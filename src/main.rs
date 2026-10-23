use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::{event::DisableMouseCapture, execute};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::cmp::Reverse;
use std::io::stdout;
use std::io::{self, BufRead, Write};
use std::process::{Command, ExitCode, Stdio};

// both variants return the same type, so code type checked on one platform also compiles on the other
#[cfg(windows)]
fn normalize_path(path: &str) -> Cow<'_, str> {
    Cow::Owned(path.replace('\\', "/"))
}

#[cfg(not(windows))]
fn normalize_path(path: &str) -> Cow<'_, str> {
    Cow::Borrowed(path)
}

const RED: &str = "\u{1b}[31m";
const YELLOW: &str = "\u{1b}[33m";
const GREEN: &str = "\u{1b}[32m";
const BLUE: &str = "\u{1b}[34m";
const MAGENTA: &str = "\u{1b}[35m";

// [48;2; means background color, [38;2; means foreground color
// remaining values are RGB values

// const RED: &str = "\u{1b}[38;2;224;108;117m";
// const GREEN: &str = "\u{1b}[38;2;152;195;121m";
// const BLUE: &str = "\u{1b}[38;2;97;175;239m";
// const MAGENTA: &str = "\u{1b}[38;2;198;120;221m";

const LIGHT_GRAY: &str = "\u{1b}[38;2;120;120;120m";
const BG_GREY: &str = "\u{1b}[48;2;40;44;52m";
const BG_BLUE: &str = "\u{1b}[48;2;30;44;52m";

const RESET: &str = "\u{1b}[0m";
const RESET_FG: &str = "\u{1b}[39m";
// const RESET_BG: &str = "\u{1b}[49m";
// make background to go to end of line
const CLEAR_LINE: &str = "\u{1b}[K";

/// Kinds of file changes; each one has the same mark and color in all output formats
#[derive(Clone, Copy, Debug, PartialEq)]
enum FileChange {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    Untracked,
    ExecutableSet,
    ExecutableUnset,
    Conflict,
    Resolved,
}

impl FileChange {
    /// Returns (color, mark)
    fn style(self) -> (&'static str, &'static str) {
        match self {
            FileChange::Added => (GREEN, "A"),
            FileChange::Deleted => (RED, "D"),
            FileChange::Modified => (BLUE, "M"),
            FileChange::Renamed => (MAGENTA, "R"),
            FileChange::Copied => (GREEN, "C"),
            FileChange::Untracked => (MAGENTA, "?"),
            FileChange::ExecutableSet => (BLUE, "X"),
            FileChange::ExecutableUnset => (BLUE, "x"),
            // C and R are taken by copies and renames
            FileChange::Conflict => (RED, "×"),
            FileChange::Resolved => (GREEN, "✓"),
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Print in front of each line the rule that formatted it
    #[arg(short, long, default_value_t = false)]
    test: bool,

    /// Summary warning length
    #[arg(short = 'w', long, default_value_t = 50)]
    summary_warning_len: usize,

    /// Summary error length
    #[arg(short = 'e', long, default_value_t = 72)]
    summary_error_len: usize,

    /// Enable mouse support in minus pager
    #[arg(short = 'm', long, default_value_t = false)]
    mouse_support: bool,

    /// Lines to print after quitting minus:
    /// 0 to suppress output;
    /// positive number from the start of the last visible page,
    /// negative number from the end of the last visible page, may extend beyond the page;
    /// `page` prints exactly the page;
    /// `all` prints all lines.
    #[arg(short = 'l', long, default_value = "0", allow_negative_numbers = true, value_parser = parse_last_lines)]
    output_lines: OutputLines,

    /// Optional pager command and its arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pager: Vec<String>,
}

/// Rows to print after quitting minus
#[derive(Clone, Copy, Debug)]
enum OutputLines {
    /// All lines
    All,
    /// Exactly the last visible page
    Page,
    /// Positive: from the start of the page onwards, negative: up to the end of the page
    Rows(isize),
}

fn parse_last_lines(s: &str) -> Result<OutputLines, String> {
    match s {
        "all" => Ok(OutputLines::All),
        "page" => Ok(OutputLines::Page),
        n => n
            .parse()
            .map(OutputLines::Rows)
            .map_err(|e| format!("expected a number, `all` or `page`: {e}")),
    }
}

fn page_prompt(upper_mark: usize, rows: usize, line_count: usize) -> String {
    // the last terminal row is used by the prompt
    let page_rows = rows.saturating_sub(1).max(1);
    let max_mark = line_count.saturating_sub(page_rows);
    let total = line_count.div_ceil(page_rows).max(1);
    let page = if upper_mark >= max_mark {
        total
    } else {
        upper_mark / page_rows + 1
    };
    format!("{page}/{total}  q: quit  /: search  M: help")
}

/// Wraps the default key bindings of minus and updates the page indicator in the prompt
struct PageIndicator {
    inner: minus::input::HashedEventRegister<std::hash::RandomState>,
    pager: minus::Pager,
    /// Last seen (upper_mark, rows, cols), used to reprint the visible page after quitting
    view: std::sync::Arc<std::sync::Mutex<(usize, usize, usize)>>,
}

impl minus::input::InputClassifier for PageIndicator {
    fn classify_input(
        &self,
        ev: Event,
        ps: &minus::PagerState,
    ) -> Option<minus::input::InputEvent> {
        // on layouts like German "/" and "?" need shift, but minus binds them without modifiers
        let ev = match ev {
            Event::Key(mut key) if matches!(key.code, KeyCode::Char(c) if !c.is_alphabetic()) => {
                key.modifiers.remove(KeyModifiers::SHIFT);
                Event::Key(key)
            }
            ev => ev,
        };
        *self.view.lock().unwrap() = (ps.upper_mark, ps.rows, ps.cols);
        let event = self.inner.classify_input(ev, ps);
        let upper_mark = match event {
            Some(minus::input::InputEvent::UpdateUpperMark(m)) => m,
            _ => ps.upper_mark,
        };
        let _ = self.pager.set_prompt(page_prompt(
            upper_mark,
            ps.rows,
            ps.screen.formatted_lines_count(),
        ));
        event
    }

    fn format_help(&self) -> Option<String> {
        minus::input::InputClassifier::format_help(&self.inner)
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        // the pager was quit before all output was written
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("jj-format: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> io::Result<()> {
    let formatter = Formatter::new(args.summary_warning_len, args.summary_error_len, args.test);
    let stdin = io::stdin().lock();

    let child = match args.pager.split_first() {
        None => None,
        Some((pager_program, pager_flags)) => match Command::new(pager_program)
            .args(pager_flags)
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(child) => Some(child),
            Err(e) => {
                eprintln!(
                    "jj-format: cannot start pager `{pager_program}`: {e}, using built-in pager"
                );
                None
            }
        },
    };

    if let Some(mut child) = child {
        let mut pager_stdin = child.stdin.take().unwrap();
        let result = formatter.format_stream(stdin, &mut pager_stdin);
        drop(pager_stdin);
        child.wait()?;
        result
    } else {
        // without a pager, output is collected and shown with the internal "minus" pager
        let mut buffer: Vec<u8> = Vec::new();
        formatter.format_stream(stdin, &mut buffer)?;
        run_minus(args, &buffer)
    }
}

fn run_minus(args: &Args, buffer: &[u8]) -> io::Result<()> {
    let text = String::from_utf8_lossy(buffer);
    let text = text.trim_end_matches('\n');
    let pager = minus::Pager::new();

    // minus always enables mouse capture, which prevents selecting text with the terminal;
    // this hook runs right after minus has set up the terminal and drawn the first page
    if !args.mouse_support {
        pager
            .add_hook(
                minus::hooks::Hook::PostPagerStart,
                2,
                Box::new(|_| {
                    let _ = execute!(stdout(), DisableMouseCapture);
                }),
            )
            .map_err(io::Error::other)?;
    }

    let (cols, rows) =
        crossterm::terminal::size().map_or((80, 25), |(c, r)| (c as usize, r as usize));
    let view = std::sync::Arc::new(std::sync::Mutex::new((0, rows, cols)));
    let mut input = minus::input::HashedEventRegister::default();
    input.add_described_key_events(&["<"], "top", |_, _| {
        minus::input::InputEvent::UpdateUpperMark(0)
    });
    input.add_described_key_events(&[">"], "bottom", |_, _| {
        minus::input::InputEvent::UpdateUpperMark(usize::MAX - 1)
    });
    input.add_described_key_events(&["b"], "page up", |_, ps| {
        minus::input::InputEvent::UpdateUpperMark(ps.upper_mark.saturating_sub(ps.rows - 1))
    });
    input.add_described_key_events(&["f"], "page down", |_, ps| {
        minus::input::InputEvent::UpdateUpperMark(ps.upper_mark.saturating_add(ps.rows - 1))
    });
    pager
        .set_prompt(page_prompt(0, rows, text.lines().count()))
        .map_err(io::Error::other)?;
    pager
        .set_input_classifier(Box::new(PageIndicator {
            inner: input,
            pager: pager.clone(),
            view: view.clone(),
        }))
        .map_err(io::Error::other)?;
    // by default minus calls process::exit on quit, which would skip reprinting the last page
    pager
        .remove_hook(minus::hooks::Hook::PostPagerExit, 1)
        .map_err(io::Error::other)?;
    pager.push_str(text).map_err(io::Error::other)?;
    minus::page_all(pager).map_err(io::Error::other)?;

    // minus uses the alternate screen if there is more than one screen of content, so reprint the end of the
    // last visible page after quitting; rows are wrapped the same way as minus does it, upper_mark counts wrapped rows
    let (upper_mark, rows, cols) = *view.lock().unwrap();
    let wrapped: Vec<_> = text
        .lines()
        .flat_map(|line| textwrap::wrap(line, cols))
        .collect();

    // if everything fits on one screen minus prints it directly without paging
    if wrapped.len() <= rows {
        return Ok(());
    }

    // alternate screen was used - determine the range of lines to print manually
    let start = upper_mark.min(wrapped.len());
    let end = (upper_mark + rows.saturating_sub(1)).min(wrapped.len());
    let range = match args.output_lines {
        OutputLines::All => 0..wrapped.len(),
        OutputLines::Page => start..end,
        OutputLines::Rows(n) if n < 0 => end.saturating_sub(n.unsigned_abs())..end,
        OutputLines::Rows(n) => start..(start + n.unsigned_abs()).min(wrapped.len()),
    };

    // wrapping splits colored text, so carry the SGR state (colors etc.) over to each printed row
    let re_sgr = Regex::new(r"\u{1b}\[([0-9;]*)m").unwrap();
    let mut sgr = String::new();
    let mut out = stdout().lock();
    for (i, row) in wrapped[..range.end].iter().enumerate() {
        if i >= range.start {
            writeln!(out, "{sgr}{row}{RESET}")?;
        }
        for cap in re_sgr.captures_iter(row) {
            let params = &cap[1];
            if params.is_empty() || params == "0" || params.starts_with("0;") {
                sgr.clear();
            }
            if !params.is_empty() && params != "0" {
                sgr.push_str(&cap[0]);
            }
        }
    }
    Ok(())
}

/// Rewrites lines of jj output, lines it does not recognize are passed through unchanged
struct Formatter {
    re_diff: Regex,
    re_status: Regex,
    re_types: Regex,
    re_conflict: Regex,
    re_change: Regex,
    re_commit_id: Regex,
    re_working_copy: Regex,
    re_committer: Regex,
    re_graph_only: Regex,
    re_description: Regex,
    re_git_header: Regex,
    re_git_meta: Regex,
    re_reset: Regex,
    summary_warning_len: usize,
    summary_error_len: usize,
    test: bool,
}

/// What the formatter remembers between lines
#[derive(Default)]
struct State {
    description: Description,
    /// the header of a git diff is collected until its first line that is not a header line
    git: Option<GitHeader>,
}

/// Where the first line of a description is expected
#[derive(Default, Clone, Copy, PartialEq, Debug)]
enum Description {
    #[default]
    None,
    /// in the next line, after a log line without summary
    NextLine,
    /// after the next blank line, after the `Committer:` line of a detailed log or `jj show`
    AfterBlank,
}

impl Formatter {
    fn new(summary_warning_len: usize, summary_error_len: usize, test: bool) -> Self {
        Formatter {
            re_diff: make_re(RE_DIFF),
            re_status: make_re(RE_STATUS),
            re_types: make_re(RE_TYPES),
            re_conflict: make_re(RE_CONFLICT),
            re_change: make_re(RE_CHANGE),
            re_commit_id: make_re(RE_COMMIT_ID),
            re_working_copy: make_re(RE_WORKING_COPY),
            re_committer: make_re(RE_COMMITTER),
            re_graph_only: make_re("^GRAPH$"),
            re_description: make_re(RE_DESCRIPTION),
            re_git_header: make_re(RE_GIT_HEADER),
            re_git_meta: make_re(RE_GIT_META),
            re_reset: Regex::new(r"\u{1b}\[0m").unwrap(),
            summary_warning_len,
            summary_error_len,
            test,
        }
    }

    /// Formats `input` line by line into `out`; lines that are not valid UTF-8 are copied byte for byte
    fn format_stream(&self, mut input: impl BufRead, out: &mut dyn Write) -> io::Result<()> {
        let mut state = State::default();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if input.read_until(b'\n', &mut buf)? == 0 {
                if let Some(git) = state.git.take() {
                    self.write_git(out, &git)?;
                }
                return Ok(());
            }
            let content = buf.strip_suffix(b"\n").unwrap_or(&buf);
            let content = content.strip_suffix(b"\r").unwrap_or(content);
            let text = std::str::from_utf8(content).ok();

            if let Some(git) = &mut state.git {
                if let Some(captures) = text.and_then(|line| self.re_git_meta.captures(line)) {
                    git.add(&captures);
                    continue;
                }
                self.write_git(out, git)?;
                state.git = None;
            }
            if let Some(captures) = text.and_then(|line| self.re_git_header.captures(line)) {
                state.git = Some(GitHeader::new(&captures));
                continue;
            }

            match text.and_then(|line| self.format_line(line, &mut state)) {
                Some((rule, line)) => {
                    self.write_rule(out, rule)?;
                    writeln!(out, "{line}")?
                }
                None => {
                    self.write_rule(out, "")?;
                    out.write_all(&buf)?;
                    if !buf.ends_with(b"\n") {
                        out.write_all(b"\n")?;
                    }
                }
            }
        }
    }

    /// In test mode writes the name of the rule that formatted the following line
    fn write_rule(&self, out: &mut dyn Write, rule: &str) -> io::Result<()> {
        if self.test {
            write!(out, "{LIGHT_GRAY}{rule:>12} │{RESET} ")?;
        }
        Ok(())
    }

    fn write_git(&self, out: &mut dyn Write, git: &GitHeader) -> io::Result<()> {
        for line in git.format() {
            self.write_rule(out, "git")?;
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    /// Returns the name of the matching rule and the formatted line, or None if the line should be kept as it is
    fn format_line(&self, line: &str, state: &mut State) -> Option<(&'static str, String)> {
        match std::mem::take(&mut state.description) {
            Description::NextLine => {
                if let Some(captures) = self.re_description.captures(line) {
                    let summary = captures.name("summary")?;
                    let mut line = line.to_string();
                    line.replace_range(summary.range(), &self.highlight_summary(summary.as_str()));
                    return Some(("description", line));
                }
            }
            Description::AfterBlank if self.re_graph_only.is_match(line) => {
                state.description = Description::NextLine;
                return None;
            }
            _ => {}
        }

        if let Some(captures) = self.re_diff.captures(line) {
            Some(("diff", format_diff_header(&captures)))
        } else if let Some(captures) = self.re_conflict.captures(line) {
            let prefix = get(&captures, "prefix");
            let path = get(&captures, "path");
            let description = format!("{RESET_FG} {}", get(&captures, "description"));
            let line = format_path_line(prefix, FileChange::Conflict, path, &description);
            Some(("conflict", line))
        } else if let Some(captures) = self.re_status.captures(line) {
            Some(("status", format_status(&captures)))
        } else if let Some(captures) = self.re_types.captures(line) {
            Some(("types", format_types(&captures)))
        } else if let Some(captures) = self.re_change.captures(line) {
            let mut line = line.to_string();
            match captures.name("summary") {
                Some(summary) => {
                    line.replace_range(summary.range(), &self.highlight_summary(summary.as_str()))
                }
                // the default log template prints the description in the next line
                None => state.description = Description::NextLine,
            }
            Some(("change", self.highlight_line(&line)))
        } else if self.re_commit_id.is_match(line) {
            Some(("commit id", self.highlight_line(line)))
        } else if self.re_working_copy.is_match(line) {
            Some(("working copy", self.highlight_line(line)))
        } else {
            if self.re_committer.is_match(line) {
                state.description = Description::AfterBlank;
            }
            None
        }
    }

    /// Gives the line a blue background up to the end of the terminal row
    fn highlight_line(&self, line: &str) -> String {
        let line = self.re_reset.replace_all(line, format!("{RESET}{BG_BLUE}"));
        format!("{BG_BLUE}{line}{CLEAR_LINE}{RESET}")
    }

    /// Colors the characters of the summary beyond the warning and error length
    fn highlight_summary(&self, summary: &str) -> String {
        let mut marks = [
            (self.summary_warning_len, YELLOW),
            (self.summary_error_len, RED),
        ];
        // insert from the back so earlier byte positions stay valid
        marks.sort_by_key(|&(len, _)| Reverse(len));
        let mut highlighted = summary.to_string();
        for (len, color) in marks {
            if let Some((i, _)) = summary.char_indices().nth(len) {
                highlighted.insert_str(i, color);
            }
        }
        if highlighted.len() != summary.len() {
            highlighted.push_str(RESET_FG);
        }
        highlighted
    }
}

/// Formats the header line of a file in a diff, e.g. `M path`
fn file_header(prefix: &str, change: FileChange, path: &str, suffix: &str) -> String {
    let (color, mark) = change.style();
    format!("{prefix}{BG_GREY}{color}{mark} {path}{RESET_FG}{suffix}{CLEAR_LINE}{RESET}")
}

/// Formats a file header of a color-words diff
fn format_diff_header(captures: &Captures) -> String {
    let prefix = get(captures, "prefix");
    let header = |change: FileChange, path: &str, suffix: &str| {
        file_header(prefix, change, &normalize_path(path), suffix)
    };

    if let Some(path) = captures.name("conflict_path") {
        return match get(captures, "conflict_op") {
            "Resolved" => header(FileChange::Resolved, path.as_str(), ""),
            _ => header(FileChange::Conflict, path.as_str(), ""),
        };
    }
    if let Some(path) = captures.name("became_path") {
        let path = path.as_str();
        return match (get(captures, "from"), get(captures, "to")) {
            ("Executable file", "non-executable") => {
                header(FileChange::ExecutableUnset, path, " -x")
            }
            ("Non-executable file", "executable") => header(FileChange::ExecutableSet, path, " +x"),
            (from, to) => header(
                FileChange::Modified,
                path,
                &format!(" {LIGHT_GRAY}{} → {to}", from.to_lowercase()),
            ),
        };
    }
    if let Some(path) = captures.name("special_path") {
        return header(FileChange::Modified, path.as_str(), "");
    }

    let op = get(captures, "op");
    if let (Some(old_path), Some(new_path)) = (captures.name("old_path"), captures.name("new_path"))
    {
        // jj prints copies the same way as renames, so they can't be told apart here
        let old_path = normalize_path(old_path.as_str());
        return header(
            FileChange::Renamed,
            new_path.as_str(),
            &format!(" {LIGHT_GRAY}⇐ {old_path}"),
        );
    }
    let path = get(captures, "path");
    let (change, sign) = match op {
        "Added" => (FileChange::Added, "+"),
        "Removed" => (FileChange::Deleted, "-"),
        _ => (FileChange::Modified, ""),
    };
    let suffix = match get(captures, "type") {
        "regular file" => String::new(),
        "executable file" if sign.is_empty() => String::new(),
        "executable file" => format!(" {sign}x"),
        "symlink" => " ->".to_string(),
        other => format!(" {LIGHT_GRAY}({other})"),
    };
    header(change, path, &suffix)
}

/// The header of a file in a git diff: the `diff --git` line and the extended header lines after it
struct GitHeader {
    prefix: String,
    paths: String,
    /// extended header lines, e.g. `new file mode 100644`, also `---` and `+++`
    lines: Vec<String>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    added: bool,
    deleted: bool,
    /// old and new path of a rename or copy
    moved: Option<(String, String, FileChange)>,
}

impl GitHeader {
    fn new(captures: &Captures) -> Self {
        GitHeader {
            prefix: get(captures, "prefix").to_string(),
            paths: get(captures, "paths").to_string(),
            lines: Vec::new(),
            old_mode: None,
            new_mode: None,
            added: false,
            deleted: false,
            moved: None,
        }
    }

    fn add(&mut self, captures: &Captures) {
        let value = get(captures, "value").to_string();
        match get(captures, "key") {
            "new file mode" => (self.added, self.new_mode) = (true, Some(value)),
            "deleted file mode" => (self.deleted, self.old_mode) = (true, Some(value)),
            "old mode" => self.old_mode = Some(value),
            "new mode" => self.new_mode = Some(value),
            "rename from" => self.moved = Some((value, String::new(), FileChange::Renamed)),
            "copy from" => self.moved = Some((value, String::new(), FileChange::Copied)),
            "rename to" | "copy to" => {
                if let Some((_, new, _)) = &mut self.moved {
                    *new = value;
                }
            }
            _ => {}
        }
        let prefix = get(captures, "prefix");
        let line = get(captures, "line");
        self.lines
            .push(format!("{prefix}{LIGHT_GRAY}{line}{RESET}"));
    }

    /// Formats the `diff --git` line like a color-words header, followed by the dimmed extended header lines
    fn format(&self) -> Vec<String> {
        let header = |change: FileChange, path: &str, suffix: &str| {
            file_header(&self.prefix, change, &normalize_path(path), suffix)
        };
        let (old_path, new_path) = split_git_paths(&self.paths);
        let mode_suffix = |sign: &str, mode: &Option<String>| match mode.as_deref() {
            Some("100755") => format!(" {sign}x"),
            Some("120000") => " ->".to_string(),
            _ => String::new(),
        };
        let header_line = match (
            &self.moved,
            self.old_mode.as_deref(),
            self.new_mode.as_deref(),
        ) {
            (Some((old, new, change)), _, _) => {
                let old = normalize_path(old);
                header(*change, new, &format!(" {LIGHT_GRAY}⇐ {old}"))
            }
            _ if self.added => header(
                FileChange::Added,
                &new_path,
                &mode_suffix("+", &self.new_mode),
            ),
            _ if self.deleted => header(
                FileChange::Deleted,
                &old_path,
                &mode_suffix("-", &self.old_mode),
            ),
            (None, Some("100755"), Some("100644")) => {
                header(FileChange::ExecutableUnset, &new_path, " -x")
            }
            (None, Some("100644"), Some("100755")) => {
                header(FileChange::ExecutableSet, &new_path, " +x")
            }
            (None, Some(old), Some(new)) => header(
                FileChange::Modified,
                &new_path,
                &format!(" {LIGHT_GRAY}{old} → {new}"),
            ),
            _ => header(FileChange::Modified, &new_path, ""),
        };
        std::iter::once(header_line)
            .chain(self.lines.iter().cloned())
            .collect()
    }
}

/// Splits the paths of a `diff --git a/<old> b/<new>` line, which may contain spaces
fn split_git_paths(paths: &str) -> (String, String) {
    // for an unchanged path both halves are equal
    let mid = paths.len() / 2;
    if paths.len() % 2 == 1
        && paths.is_char_boundary(mid)
        && let (Some(old), Some(new)) = (
            paths[..mid].strip_prefix("a/"),
            paths[mid..].strip_prefix(" b/"),
        )
        && old == new
    {
        return (old.to_string(), new.to_string());
    }
    match paths.split_once(" b/") {
        Some((old, new)) => (old.trim_start_matches("a/").to_string(), new.to_string()),
        None => (paths.to_string(), paths.to_string()),
    }
}

/// Formats a line of `jj status` or `jj diff --summary`
fn format_status(captures: &Captures) -> String {
    let change = match get(captures, "status") {
        "A" => FileChange::Added,
        "D" => FileChange::Deleted,
        "R" => FileChange::Renamed,
        "C" => FileChange::Copied,
        "?" => FileChange::Untracked,
        _ => FileChange::Modified,
    };
    format_path_line(get(captures, "prefix"), change, get(captures, "path"), "")
}

/// Formats a line of `jj diff --types`, e.g. `FL path` (regular file became symlink)
fn format_types(captures: &Captures) -> String {
    let path = get(captures, "path");
    let types = get(captures, "types");
    let change = if expand_braces(path).is_some() {
        // like in color-words diffs copies and renames look the same
        FileChange::Renamed
    } else if types.starts_with('-') {
        FileChange::Added
    } else if types.ends_with('-') {
        FileChange::Deleted
    } else {
        FileChange::Modified
    };
    let suffix = format!(" {LIGHT_GRAY}{types}");
    format_path_line(get(captures, "prefix"), change, path, &suffix)
}

/// Formats `mark path`, a path like `a/{b => c}/x` as `mark a/c/x ⇐ a/b/x`
fn format_path_line(prefix: &str, change: FileChange, path: &str, suffix: &str) -> String {
    let (color, mark) = change.style();
    match expand_braces(path) {
        Some((old_path, new_path)) => {
            let old_path = normalize_path(&old_path);
            let new_path = normalize_path(&new_path);
            format!(
                "{prefix}{color}{mark} {new_path} {LIGHT_GRAY}⇐ {old_path}{suffix}{CLEAR_LINE}{RESET}"
            )
        }
        None => {
            let path = normalize_path(path);
            format!("{prefix}{color}{mark} {path}{suffix}{CLEAR_LINE}{RESET}")
        }
    }
}

/// Splits a jj rename/copy path like `a/{b => c}/x` into its old and new path (`a/b/x`, `a/c/x`)
fn expand_braces(path: &str) -> Option<(String, String)> {
    let (before, rest) = path.split_once('{')?;
    let (inner, after) = rest.split_once('}')?;
    let (old, new) = inner.split_once(" => ")?;
    let join = |middle: &str| {
        // an empty side like `{ => sub}/x` leaves a doubled or leading slash behind
        let path = format!("{before}{middle}{after}").replace("//", "/");
        path.strip_prefix('/').map(str::to_string).unwrap_or(path)
    };
    Some((join(old), join(new)))
}

// prefix of lines drawn inside a jj log graph, e.g. `│ ~  `, also with `ui.graph.style = "ascii"`
const GRAPH: &str = r"(?<prefix>[\s│├─╮╯╭╰┤┬┴┼~|/\\'+-]*)";

// node of a commit in a jj log graph, optionally colored, also with `ui.graph.style = "ascii"`
const NODE: &str = r"(?:ESC\[1mESC\[38;5;\d+m)?(?<mark>[@◆○◉×ox+])(?:ESC\[0m)?";

const RE_DIFF: &str = r"
^GRAPH
ESC\[38;5;\d+m
(?:
  (?<conflict_op>Created|Resolved|Modified) conflict in (?<conflict_path>.+)|
  (?<op>Added|Removed|Modified)
  \ (?<type>regular file|executable file|symlink|tree|Git submodule|conflict)
  \ (?<path>.+?)(?:\ \((?<old_path>.+)\ =>\ (?<new_path>.+)\))?|
  (?<from>[A-Z][A-Za-z\ -]*?) became (?<to>[A-Za-z\ -]+?) at (?<became_path>.+)|
  Modified (?<special_path>JJ-COMMIT-DESCRIPTION)
)
:ESC\[39m$
";

const RE_STATUS: &str = r"
^GRAPH
ESC\[38;5;\d+m(?<status>[MADRC?])\ (?<path>.+?)ESC\[39m$
";

// -: absent, F: file, L: symlink, C: conflict, G: Git submodule, T: tree
const RE_TYPES: &str = r"
^GRAPH
ESC\[38;5;\d+m(?<types>[-FLCGT]{2})\ (?<path>.+?)ESC\[39m$
";

const RE_CONFLICT: &str = r"
^GRAPH
(?<path>\S.*?)\s+
ESC\[38;5;\d+m(?<description>\d+-sided\ conflict[^ESC]*)ESC\[39m$
";

const RE_CHANGE: &str = r"
^GRAPH NODE\ +
.*?ESC\[1mESC\[38;5;5m(?<change_id>[a-z]+)
(ESC\[0m\ *(?<summary>[^\ ESC](?:[^ESC]*[^\ ESC])?))?
.*
";

const RE_COMMIT_ID: &str = r"
^GRAPH (?:NODE.*?)?
Commit\ ID:\ ESC\[38;5;4m(?<commit_id>[0-9a-f]+)ESC\[39m$
";

// the lines of `jj status` about the working copy and its parents
const RE_WORKING_COPY: &str = r"
^(?:Working\ copy\s+\(@\)|Parent\ commit\s+\(@-\))\s*:\ 
";

const RE_COMMITTER: &str = r"^GRAPH Committer:\ ";

// first line of a description in a line of its own, optionally after an `(empty)` label
const RE_DESCRIPTION: &str = r"
^GRAPH
(?:ESC\[1m)?
(?:ESC\[38;5;\d+m\(empty\)ESC\[39m\ )?
(?<summary>[^\ ESC](?:[^ESC]*[^\ ESC])?)
(?:ESC\[0m)?$
";

const RE_GIT_HEADER: &str = r"
^GRAPH ESC\[1mdiff\ --git\ (?<paths>.+)ESC\[0m$
";

const RE_GIT_META: &str = r"
^GRAPH ESC\[1m
(?<line>
  (?<key>new\ file\ mode|deleted\ file\ mode|old\ mode|new\ mode|rename\ from|rename\ to|copy\ from|copy\ to|
    similarity\ index|dissimilarity\ index|index)\ (?<value>.*)|
  ---\ .*|
  \+\+\+\ .*
)
ESC\[0m$
";

/// Replaces the placeholder `ESC` by the escape character, but not inside words like `DESCRIPTION`
fn replace_esc(s: &str) -> String {
    Regex::new(r"ESC\b")
        .unwrap()
        .replace_all(s, "\u{1b}")
        .to_string()
}

/// Compiles a regex written over several lines; newlines and indentation are removed,
/// so literal spaces at the start of a line must be escaped as `\ `
fn make_re(re: &str) -> Regex {
    let re_nl_space = Regex::new(r"\n\s*").unwrap();
    let re = re_nl_space
        .replace_all(re, "")
        .replace("GRAPH ", GRAPH)
        .replace("GRAPH", GRAPH)
        .replace("NODE", NODE);
    Regex::new(&replace_esc(&re)).unwrap()
}

fn get<'a>(captures: &'a Captures<'a>, name: &str) -> &'a str {
    captures.name(name).map_or("", |m| m.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn formatter() -> Formatter {
        Formatter::new(50, 72, false)
    }

    /// Strips SGR and erase-line sequences so expectations stay readable
    fn plain(s: &str) -> String {
        Regex::new(r"\u{1b}\[[0-9;]*[mK]")
            .unwrap()
            .replace_all(s, "")
            .to_string()
    }

    fn format_plain(line: &str) -> Option<String> {
        formatter()
            .format_line(&replace_esc(line), &mut State::default())
            .map(|(_, l)| plain(&l))
    }

    #[test]
    fn test_diff_headers() {
        for (line, expected) in [
            (
                "ESC[38;5;3mAdded regular file src/main.rs:ESC[39m",
                "A src/main.rs",
            ),
            (
                "ESC[38;5;3mAdded executable file run.sh:ESC[39m",
                "A run.sh +x",
            ),
            ("ESC[38;5;3mAdded symlink link:ESC[39m", "A link ->"),
            (
                "ESC[38;5;3mAdded Git submodule sub:ESC[39m",
                "A sub (Git submodule)",
            ),
            (
                "ESC[38;5;3mRemoved regular file src/main.rs:ESC[39m",
                "D src/main.rs",
            ),
            (
                "ESC[38;5;3mRemoved executable file run.sh:ESC[39m",
                "D run.sh -x",
            ),
            (
                "ESC[38;5;3mModified regular file a b/f(1).txt:ESC[39m",
                "M a b/f(1).txt",
            ),
            (
                "ESC[38;5;3mModified executable file run.sh:ESC[39m",
                "M run.sh",
            ),
            (
                "ESC[38;5;3mModified regular file n (m => n):ESC[39m",
                "R n ⇐ m",
            ),
            (
                "ESC[38;5;3mModified regular file a/c/x (a/b/x => a/c/x):ESC[39m",
                "R a/c/x ⇐ a/b/x",
            ),
            (
                "ESC[38;5;3mCreated conflict in src/main.rs:ESC[39m",
                "× src/main.rs",
            ),
            (
                "ESC[38;5;3mModified conflict in src/main.rs:ESC[39m",
                "× src/main.rs",
            ),
            (
                "ESC[38;5;3mResolved conflict in src/main.rs:ESC[39m",
                "✓ src/main.rs",
            ),
            (
                "ESC[38;5;3mExecutable file became non-executable at x.sh:ESC[39m",
                "x x.sh -x",
            ),
            (
                "ESC[38;5;3mNon-executable file became executable at x.sh:ESC[39m",
                "X x.sh +x",
            ),
            (
                "ESC[38;5;3mRegular file became symlink at l:ESC[39m",
                "M l regular file → symlink",
            ),
            (
                "ESC[38;5;3mSymlink became regular file at l:ESC[39m",
                "M l symlink → regular file",
            ),
            (
                "ESC[38;5;3mModified JJ-COMMIT-DESCRIPTION:ESC[39m",
                "M JJ-COMMIT-DESCRIPTION",
            ),
            (
                "│ ~  ESC[38;5;3mModified conflict in cf.txt:ESC[39m",
                "│ ~  × cf.txt",
            ),
        ] {
            assert_eq!(format_plain(line).as_deref(), Some(expected), "{line}");
        }
    }

    #[test]
    fn test_status() {
        for (line, expected) in [
            ("ESC[38;5;2mA README.mdESC[39m", "A README.md"),
            ("ESC[38;5;6mM src/main.rsESC[39m", "M src/main.rs"),
            ("ESC[38;5;1mD linkESC[39m", "D link"),
            ("ESC[38;5;5m? new.txtESC[39m", "? new.txt"),
            (
                "ESC[38;5;6mR {src => lib}/a.rsESC[39m",
                "R lib/a.rs ⇐ src/a.rs",
            ),
            (
                "ESC[38;5;6mR a/{b => c}/x.txtESC[39m",
                "R a/c/x.txt ⇐ a/b/x.txt",
            ),
            (
                "ESC[38;5;2mC {keep.txt => keep2.txt}ESC[39m",
                "C keep2.txt ⇐ keep.txt",
            ),
            (
                "   ESC[38;5;6mM dir with space/f(1).txtESC[39m",
                "   M dir with space/f(1).txt",
            ),
        ] {
            assert_eq!(format_plain(line).as_deref(), Some(expected), "{line}");
        }
    }

    #[test]
    fn test_conflict_list() {
        for (line, expected) in [
            (
                "cf.txt    ESC[38;5;3m2-sided conflictESC[39m",
                "× cf.txt 2-sided conflict",
            ),
            (
                "a b.txt    ESC[38;5;3m3-sided conflict including 1 deletionESC[39m",
                "× a b.txt 3-sided conflict including 1 deletion",
            ),
        ] {
            assert_eq!(format_plain(line).as_deref(), Some(expected), "{line}");
        }
    }

    #[test]
    fn test_unrecognized_lines_pass_through() {
        for line in [
            "plain text",
            "ESC[38;5;3mModified something else:ESC[39m",
            "Working copy changes:",
        ] {
            assert_eq!(format_plain(line), None, "{line}");
        }
    }

    #[test]
    fn test_expand_braces() {
        for (path, expected) in [
            ("{a => b}", Some(("a", "b"))),
            ("{src => lib}/a.rs", Some(("src/a.rs", "lib/a.rs"))),
            ("a/{b => c}/x", Some(("a/b/x", "a/c/x"))),
            ("{ => sub}/x", Some(("x", "sub/x"))),
            ("a/{b => }/x", Some(("a/b/x", "a/x"))),
            ("plain", None),
        ] {
            let expected = expected.map(|(o, n)| (o.to_string(), n.to_string()));
            assert_eq!(expand_braces(path), expected, "{path}");
        }
    }

    #[test]
    fn test_change_summary() {
        let summary = "first commit with a really long summary line that definitely exceeds seventy-two chars";
        let line = replace_esc(&format!("○    7 ESC[1mESC[38;5;5mkmzESC[0m  {summary}"));
        let formatted = formatter()
            .format_line(&line, &mut State::default())
            .unwrap()
            .1;
        assert_eq!(plain(&formatted), plain(&line));
        // the 51st character starts the warning, the 73rd the error
        let (ok, rest) = summary.split_at(50);
        let (warning, error) = rest.split_at(22);
        assert!(formatted.contains(&format!("{ok}{YELLOW}{warning}{RED}{error}")));

        // only the summary is highlighted, not the same text in the change id
        let line = replace_esc("@  1 ESC[1mESC[38;5;5mmESC[0m    m");
        let formatted = Formatter::new(0, 72, false)
            .format_line(&line, &mut State::default())
            .unwrap()
            .1;
        assert!(formatted.contains("\u{1b}[38;5;5mm\u{1b}[0m"));
        assert!(formatted.contains(&format!("{RESET}{BG_BLUE}    {YELLOW}m")));
    }

    #[test]
    fn test_change_lines() {
        for line in [
            "│ ESC[1mESC[38;5;1m@ESC[0m  4 ESC[1mESC[38;5;5mmESC[0m    summary2",
            "ESC[1mESC[38;5;14m◆ESC[0m  ESC[38;5;2m-ESC[39m ESC[1mESC[38;5;5mzESC[0m    ESC[38;5;2m-ESC[39m",
            "○  4 ESC[1mESC[38;5;5mlESC[0m    summary",
            "│ ESC[1mESC[38;5;1m×ESC[0m  Commit ID: ESC[38;5;4m4ed6adab1aa245ba48d0a3e749621d29cf9445cfESC[39m",
            "○ │  Commit ID: ESC[38;5;4m61bc85cb795bf16a958a7298df49680638e6c694ESC[39m",
            // jj show
            "Commit ID: ESC[38;5;4m0d9666043b916abf64db06d7a1b37726e2e29c7fESC[39m",
            // ui.graph.style = "ascii"
            "ESC[1mESC[38;5;1mxESC[0m  ESC[1mESC[38;5;5mxoESC[0mESC[38;5;8mrrrnroESC[39m user 2026-01-01",
            "| o  ESC[1mESC[38;5;5muESC[0mESC[38;5;8msqlspxoESC[39m user 2026-01-01",
            // jj status
            "Working copy  (@) : ESC[1mESC[38;5;13muESC[38;5;8msqlspxoESC[39m (no description set)ESC[0m",
            "Parent commit (@-): ESC[1mESC[38;5;5mywESC[0mESC[38;5;8mktvznESC[39m mv",
        ] {
            let line = replace_esc(line);
            let formatted = formatter()
                .format_line(&line, &mut State::default())
                .expect(&line)
                .1;
            assert!(formatted.starts_with(BG_BLUE), "{line}");
            assert_eq!(plain(&formatted), plain(&line));
        }
    }

    /// Formats `input` (with `ESC` placeholders) as a stream and returns the lines with escapes
    fn format_lines(formatter: &Formatter, input: &str) -> Vec<String> {
        let mut out = Vec::new();
        formatter
            .format_stream(replace_esc(input).as_bytes(), &mut out)
            .unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn test_description_in_next_line() {
        let f = Formatter::new(3, 5, false);
        let highlighted = format!("abc{YELLOW}de{RED}fg{RESET_FG}");
        // default log template
        let lines = format_lines(
            &f,
            "○  ESC[1mESC[38;5;5mzsESC[0mESC[38;5;8muskulnESC[39m test@example.com\n│  abcdefg\n○  ESC[1mESC[38;5;5mzsESC[0mESC[38;5;8muskulnESC[39m test@example.com\n├─╮  ESC[38;5;2m(empty)ESC[39m abcdefg\n│ abcdefg",
        );
        assert_eq!(lines[1], format!("│  {highlighted}"));
        assert!(lines[3].ends_with(&format!("ESC[39m {highlighted}").replace("ESC", "\u{1b}")));
        // only the line right after the log line is a description
        assert_eq!(lines[4], "│ abcdefg");

        // detailed log and jj show
        let lines = format_lines(
            &f,
            "│  Committer: Test User <test@example.com>\n│\n│      abcdefg\n│\n│      abcdefg",
        );
        assert_eq!(lines[2], format!("│      {highlighted}"));
        assert_eq!(lines[4], "│      abcdefg");

        // no description
        let lines = format_lines(
            &f,
            "Committer: x\n\n    ESC[38;5;3m(no description set)ESC[39m",
        );
        assert_eq!(
            lines[2],
            replace_esc("    ESC[38;5;3m(no description set)ESC[39m")
        );
    }

    #[test]
    fn test_git_headers() {
        // (graph prefix, header lines which jj prints in bold, expected plain output)
        let cases: [(&str, &[&str], &[&str]); 9] = [
            (
                "",
                &[
                    "diff --git a/added.txt b/added.txt",
                    "new file mode 100644",
                    "index 0000000000..3e757656cf",
                    "--- /dev/null",
                    "+++ b/added.txt",
                ],
                &[
                    "A added.txt",
                    "new file mode 100644",
                    "index 0000000000..3e757656cf",
                    "--- /dev/null",
                    "+++ b/added.txt",
                ],
            ),
            (
                "",
                &["diff --git a/run b/run", "new file mode 100755"],
                &["A run +x", "new file mode 100755"],
            ),
            (
                "",
                &["diff --git a/link b/link", "deleted file mode 120000"],
                &["D link ->", "deleted file mode 120000"],
            ),
            (
                "",
                &[
                    "diff --git a/exe.sh b/exe.sh",
                    "old mode 100755",
                    "new mode 100644",
                ],
                &["x exe.sh -x", "old mode 100755", "new mode 100644"],
            ),
            (
                "",
                &["diff --git a/l b/l", "old mode 120000", "new mode 100644"],
                &["M l 120000 → 100644", "old mode 120000", "new mode 100644"],
            ),
            (
                "",
                &[
                    "diff --git a/src/a.rs b/lib/a.rs",
                    "rename from src/a.rs",
                    "rename to lib/a.rs",
                ],
                &[
                    "R lib/a.rs ⇐ src/a.rs",
                    "rename from src/a.rs",
                    "rename to lib/a.rs",
                ],
            ),
            (
                "",
                &["diff --git a/k b/k 2", "copy from k", "copy to k 2"],
                &["C k 2 ⇐ k", "copy from k", "copy to k 2"],
            ),
            (
                "",
                &["diff --git a/a b/f.txt b/a b/f.txt", "index 1..2 100644"],
                &["M a b/f.txt", "index 1..2 100644"],
            ),
            (
                "│  ",
                &["diff --git a/x b/x", "index 1..2 100644"],
                &["│  M x", "│  index 1..2 100644"],
            ),
        ];
        for (prefix, header, expected) in cases {
            let input: Vec<_> = header
                .iter()
                .map(|l| format!("{prefix}ESC[1m{l}ESC[0m"))
                .collect();
            // the header ends at the first line that is no header line, or at the end of the input
            for end in ["", "\nESC[38;5;6m@@ -1 +1 @@ESC[39m"] {
                let input = input.join("\n") + end;
                let lines: Vec<_> = format_lines(&formatter(), &input)
                    .iter()
                    .map(|l| plain(l))
                    .collect();
                let mut expected = expected.to_vec();
                if !end.is_empty() {
                    expected.push("@@ -1 +1 @@");
                }
                assert_eq!(lines, expected, "{input}");
            }
        }
    }

    #[test]
    fn test_rules_in_test_mode() {
        let lines = format_lines(
            &Formatter::new(50, 72, true),
            "plain\nESC[38;5;2mA a.txtESC[39m\nESC[1mdiff --git a/x b/xESC[0m\nESC[1mindex 1..2 100644ESC[0m\nlast",
        );
        let lines: Vec<_> = lines.iter().map(|l| plain(l)).collect();
        assert_eq!(
            lines,
            [
                "             │ plain",
                "      status │ A a.txt",
                "         git │ M x",
                "         git │ index 1..2 100644",
                "             │ last",
            ]
        );
    }

    #[test]
    fn test_split_git_paths() {
        for (paths, (old, new)) in [
            ("a/x b/x", ("x", "x")),
            ("a/a b/f.txt b/a b/f.txt", ("a b/f.txt", "a b/f.txt")),
            ("a/src/a.rs b/lib/a.rs", ("src/a.rs", "lib/a.rs")),
        ] {
            assert_eq!(
                split_git_paths(paths),
                (old.to_string(), new.to_string()),
                "{paths}"
            );
        }
    }

    #[test]
    fn test_types() {
        for (line, expected) in [
            ("ESC[38;5;6m-F added.txtESC[39m", "A added.txt -F"),
            ("ESC[38;5;6mL- linkESC[39m", "D link L-"),
            ("ESC[38;5;6mFF keep.txtESC[39m", "M keep.txt FF"),
            ("ESC[38;5;6mFL exe.shESC[39m", "M exe.sh FL"),
            (
                "ESC[38;5;6mFF {src => lib}/a.rsESC[39m",
                "R lib/a.rs ⇐ src/a.rs FF",
            ),
        ] {
            assert_eq!(format_plain(line).as_deref(), Some(expected), "{line}");
        }
    }

    #[test]
    fn test_non_utf8_is_kept() {
        let input = b"\x1b[38;5;3mModified regular file bin.txt:\x1b[39m\n\xff\xfe latin1\n\x1b[38;5;2mA after.txt\x1b[39m";
        let mut out = Vec::new();
        formatter().format_stream(&input[..], &mut out).unwrap();
        let out_text = String::from_utf8_lossy(&out);
        let lines: Vec<_> = out_text.lines().map(plain).collect();
        assert_eq!(
            lines,
            ["M bin.txt", "\u{fffd}\u{fffd} latin1", "A after.txt"]
        );
        let raw = b"\xff\xfe latin1\n";
        assert!(out.windows(raw.len()).any(|w| w == raw));
    }

    /// Runs every `tests/fixtures/*.in` (raw `jj --color=always` output) through the formatter and compares
    /// with the `.out` file next to it; set `BLESS=1` to (re)write the `.out` files
    #[test]
    fn test_fixtures() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let bless = std::env::var_os("BLESS").is_some();
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let input_path = entry.unwrap().path();
            if input_path.extension().is_none_or(|e| e != "in") {
                continue;
            }
            count += 1;
            let input = std::fs::read(&input_path).unwrap();
            let mut actual = Vec::new();
            formatter().format_stream(&input[..], &mut actual).unwrap();
            let expected_path = input_path.with_extension("out");
            if bless {
                std::fs::write(&expected_path, &actual).unwrap();
            } else {
                let expected = std::fs::read(&expected_path).unwrap_or_default();
                assert!(
                    actual == expected,
                    "{} differs, rerun with BLESS=1 to update:\n{}",
                    input_path.display(),
                    plain(&String::from_utf8_lossy(&actual))
                );
            }
        }
        assert!(count > 0, "no fixtures found in {}", dir.display());
    }
}
