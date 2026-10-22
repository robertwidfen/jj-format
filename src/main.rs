use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::{event::DisableMouseCapture, execute};
use regex::{Captures, Regex};
use std::io::stdout;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};

#[cfg(windows)]
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(not(windows))]
fn normalize_path(path: &str) -> &str {
    path
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

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Print the jj command and diagnostic information
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

fn main() -> io::Result<()> {
    let args = Args::parse();
    let stdin = io::stdin();

    // without a pager argument, output is collected and shown with the internal "minus" pager
    let mut buffer: Vec<u8> = Vec::new();
    let mut child = None;

    let fd: &mut dyn Write = if args.pager.is_empty() {
        &mut buffer
    } else {
        let pager_program = &args.pager[0];
        let pager_flags = &args.pager[1..];
        let cmd = Command::new(pager_program)
            .args(pager_flags)
            .stdin(Stdio::piped())
            .spawn()?;
        child = Some(cmd);
        child.as_mut().unwrap().stdin.as_mut().unwrap()
    };

    let re_diff = make_re(RE_DIFF);
    let re_status = make_re(RE_STATUS);
    let re_conflict = make_re(RE_CONFLICT);
    let re_change = make_re(RE_CHANGE);
    let re_commit_id = make_re(RE_COMMIT_ID);
    let re_reset = Regex::new(r"\u{1b}\[0m").unwrap();

    for line in stdin.lock().lines().map_while(Result::ok) {
        if let Some(captures) = re_diff.captures(&line)
            && let status = get(&captures, "status")
        {
            let path = normalize_path(get(&captures, "path"));
            match status {
                s if s.starts_with("Removed") => {
                    writeln!(fd, "{BG_GREY}{RED}D {path}{CLEAR_LINE}{RESET}")?;
                }
                s if s.starts_with("Created conflict in") => {
                    writeln!(fd, "{BG_GREY}{RED}C {path}{CLEAR_LINE}{RESET}")?;
                }
                s if s.starts_with("Resolved conflict in") => {
                    writeln!(fd, "{BG_GREY}{BLUE}R {path}{CLEAR_LINE}{RESET}")?;
                }
                s if s.starts_with("Added regular") => {
                    writeln!(fd, "{BG_GREY}{GREEN}A {path}{CLEAR_LINE}{RESET}")?;
                }
                s if s.starts_with("Added executable") => {
                    writeln!(
                        fd,
                        "{BG_GREY}{GREEN}A {path}{RESET_FG}+x{CLEAR_LINE}{RESET}"
                    )?;
                }
                s if s.starts_with("Added symlink") => {
                    writeln!(
                        fd,
                        "{BG_GREY}{GREEN}A {path}{RESET_FG}->{CLEAR_LINE}{RESET}"
                    )?;
                }
                s if s.starts_with("Modified ") => {
                    if line.contains("=>") {
                        let old_path = normalize_path(get(&captures, "old_path"));
                        let new_path = normalize_path(get(&captures, "new_path"));
                        writeln!(
                            fd,
                            "{BG_GREY}{MAGENTA}R {new_path} {LIGHT_GRAY}⇐ {old_path}{CLEAR_LINE}{RESET}"
                        )?;
                    } else {
                        writeln!(fd, "{BG_GREY}{BLUE}M {path}{CLEAR_LINE}{RESET}")?;
                    }
                }
                s if s.starts_with("Executable ") => {
                    writeln!(fd, "{BG_GREY}{BLUE}x {path}{RESET_FG}-x{CLEAR_LINE}{RESET}")?;
                }
                s if s.starts_with("Non-executable ") => {
                    writeln!(fd, "{BG_GREY}{BLUE}X {path}{RESET_FG}+x{CLEAR_LINE}{RESET}")?;
                }
                _ => {
                    panic!("Unhandled diff match {:?}", line);
                }
            }
        } else if let Some(captures) = re_conflict.captures(&line) {
            let path = normalize_path(get(&captures, "path"));
            let sides = get(&captures, "sides");
            writeln!(fd, "{RED}C {path}{RESET} {sides}-sided{CLEAR_LINE}{RESET}")?;
        } else if let Some(captures) = re_status.captures(&line) {
            let status = get(&captures, "status");
            let path = normalize_path(get(&captures, "path"));
            if args.test {
                println!("status: {:?} {:?}", status, path);
            }
            match status {
                "A" => writeln!(fd, "{GREEN}A {path}{CLEAR_LINE}{RESET}")?,
                "M" => writeln!(fd, "{BLUE}M {path}{CLEAR_LINE}{RESET}")?,
                "D" => writeln!(fd, "{RED}D {path}{CLEAR_LINE}{RESET}")?,
                "R" => {
                    if path.contains("=>") {
                        let new_path = normalize_path(get(&captures, "new_path"));
                        let old_path = normalize_path(get(&captures, "old_path"));
                        writeln!(
                            fd,
                            "{MAGENTA}R {new_path} {LIGHT_GRAY}⇐ {old_path}{CLEAR_LINE}{RESET}"
                        )?;
                    } else {
                        writeln!(fd, "{BG_GREY}{BLUE}M {path}{CLEAR_LINE}{RESET}")?;
                    }
                }
                "C" => writeln!(fd, "{RED}C {path}{CLEAR_LINE}{RESET}")?,
                "?" => writeln!(fd, "{MAGENTA}? {path}{CLEAR_LINE}{RESET}")?,
                _ => {
                    panic!("Unhandled status match {:?}", line);
                }
            }
        } else if let Some(captures) = re_change.captures(&line) {
            let mut bg_line = re_reset
                .replace_all(&line, &format!("\u{1b}[0m{BG_BLUE}"))
                .to_string();
            let summary = get(&captures, "summary");
            if !summary.is_empty() {
                let mut new_summary = summary.to_string();
                if let Some((i_warning, _)) = summary.char_indices().nth(args.summary_error_len + 1)
                {
                    new_summary.insert_str(i_warning, RED);
                }
                if let Some((i_error, _)) = summary.char_indices().nth(args.summary_warning_len + 1)
                {
                    new_summary.insert_str(i_error, YELLOW);
                }
                bg_line = bg_line.replace(summary, &new_summary);
            }
            writeln!(fd, "{BG_BLUE}{bg_line}{CLEAR_LINE}{RESET}")?;
        } else if re_commit_id.is_match(&line) {
            let line = re_reset
                .replace_all(&line, &format!("\u{1b}[0m{BG_BLUE}"))
                .to_string();
            writeln!(fd, "{BG_BLUE}{line}{CLEAR_LINE}{RESET}")?;
        } else {
            writeln!(fd, "{}", line)?;
        }
    }

    if let Some(mut child_process) = child {
        drop(child_process.stdin.take());
        child_process.wait()?;
    } else {
        let text = String::from_utf8_lossy(&buffer);
        let text = text.trim_end_matches('\n');
        let pager = minus::Pager::new();

        // disable mouse capture
        if !args.mouse_support {
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let _ = execute!(stdout(), DisableMouseCapture);
            });
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
                writeln!(out, "{sgr}{row}\u{1b}\\[0m")?;
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
    }

    Ok(())
}

const RE_DIFF: &str = r"
ESC\[38;5;(?<color>\d+)m
(?<status>
  ((Removed|Added|Modified) (((regular|executable) file)|symlink))|
  ((Resolved conflict|Created conflict) in)|
  ((Non-e|E)xecutable file became (non-)?executable at)
) +
(?<path>[^(]+)( \((?<old_path>.+) => (?<new_path>.+)\))?:
ESC\[39m
";

const RE_STATUS: &str = r"
ESC\[38;5;(?<color>\d+)m(?<status>[MADRC?]) +
(?<path>([^ {]*)\{(?:(?<old_path>.+) => (?<new_path>.+)\})|.+?)ESC\[39m
";

const RE_CONFLICT: &str = r"
(?<path>[^ ]+)\s+
ESC\[38;5;(?<color>\d+)m(?<sides>\d+)-sided conflictESC\[39m
";

const RE_CHANGE: &str = r"
^.*(ESC\[1mESC\[38;5;(?<color>\d+)m)?(?<mark>[@◆○×])(ESC\[0m)? +
.*ESC\[1mESC\[38;5;5m(?<change_id>[a-z]+)
(ESC\[0m *(?<summary>[^ ESC]+[^ESC]+[^ ESC]+))?
.*
";

const RE_COMMIT_ID: &str = r"
^.*(?<mark>[@◆○×]).*
Commit ID:
.*ESC\[38;5;4m(?<commit_id>[a-z0-9]+)ESC\[39m
.*
";

fn replace_esc(s: &str) -> String {
    s.replace("ESC", "\u{1b}")
}

fn make_re(re: &str) -> Regex {
    let re_nl_space = Regex::new(r"\n\s*").unwrap();
    let re = &replace_esc(&re_nl_space.replace_all(re, "").to_string());
    Regex::new(&re).unwrap()
}

fn get<'a>(captures: &'a Captures<'a>, name: &str) -> &'a str {
    captures.name(name).map_or("", |m| m.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_re_diff() {
        let re_diff = make_re(RE_DIFF);
        println!("re_diff: {:?}", re_diff);

        for (line, (color, status, path, new_path, old_path)) in vec![
            (
                "ESC[38;5;3mCreated conflict in src/main.rs:ESC[39m",
                ("3", "Created conflict in", "src/main.rs", "", ""),
            ),
            (
                "ESC[38;5;3mModified regular file n (m => n):ESC[39m",
                ("3", "Modified regular file", "n", "n", "m"),
            ),
            (
                "ESC[38;5;3mRemoved regular file src/main.rs:ESC[39m",
                ("3", "Removed regular file", "src/main.rs", "", ""),
            ),
            (
                "ESC[38;5;3mAdded regular file src/main.rs:ESC[39m",
                ("3", "Added regular file", "src/main.rs", "", ""),
            ),
        ] {
            let line = replace_esc(line);
            println!("Testing {:?}", line);
            assert!(re_diff.is_match(&line));
            let captures = re_diff.captures(&line).unwrap();
            assert_eq!(get(&captures, "color"), color);
            assert_eq!(get(&captures, "status"), status);
            assert_eq!(get(&captures, "path"), path);
            assert_eq!(get(&captures, "new_path"), new_path);
            assert_eq!(get(&captures, "old_path"), old_path);
        }
    }

    #[test]
    fn test_re_status() {
        let re_status = make_re(RE_STATUS);
        println!("re_status: {}", re_status);

        for (line, (color, status, path)) in vec![
            ("ESC[38;5;2mA README.mdESC[39m", ("2", "A", "README.md")),
            ("ESC[38;5;6mA README.mdESC[39m", ("6", "A", "README.md")),
            ("ESC[38;5;2mM src/main.rsESC[39m", ("2", "M", "src/main.rs")),
            ("ESC[38;5;6mM src/main.rsESC[39m", ("6", "M", "src/main.rs")),
        ] {
            let line = replace_esc(line);
            println!("Testing {:?}", line);
            assert!(re_status.is_match(&line));

            let captures = re_status.captures(&line).unwrap();
            assert_eq!(get(&captures, "color"), color);
            assert_eq!(get(&captures, "status"), status);
            assert_eq!(get(&captures, "path"), path);
        }
    }

    #[test]
    fn test_re_conflict() {
        let re_conflict = make_re(RE_CONFLICT);
        println!("re_conflict: {:?}", re_conflict);

        for (line, (path, sides)) in vec![(
            "src/main.rs    ESC[38;5;3m2-sided conflictESC[39m",
            ("src/main.rs", "2"),
        )] {
            let line = replace_esc(line);
            println!("Testing {:?}", line);
            assert!(re_conflict.is_match(&line));

            let captures = re_conflict.captures(&line).unwrap();
            assert_eq!(get(&captures, "path"), path);
            assert_eq!(get(&captures, "sides"), sides);
        }
    }

    #[test]
    fn test_re_change() {
        let re_change = make_re(RE_CHANGE);
        println!("re_change: '{:?}'", re_change);
        for (line, (mark, summary)) in vec![
            (
                "│ ESC[1mESC[38;5;1m@ESC[0m  4 ESC[1mESC[38;5;5mmESC[0m    summary2",
                ("@", "summary2"),
            ),
            (
                "ESC[1mESC[38;5;14m◆ESC[0m  ESC[38;5;2m-ESC[39m ESC[1mESC[38;5;5mzESC[0m    ESC[38;5;2m-ESC[39m",
                ("◆", ""),
            ),
            ("○  4 ESC[1mESC[38;5;5mlESC[0m    summary", ("○", "summary")),
            (
                "ESC[1mESC[38;5;1m×ESC[0m  ESC[38;5;2m-ESC[39m ESC[1mESC[38;5;5mxESC[0m    summary",
                ("×", "summary"),
            ),
        ] {
            let line = replace_esc(line);
            println!("Testing {:?}", line);
            assert!(re_change.is_match(&line));

            let captures = re_change.captures(&line).unwrap();
            assert_eq!(get(&captures, "mark"), mark);
            assert_eq!(get(&captures, "summary"), summary);
        }
    }

    #[test]
    fn test_re_commit_id() {
        let re_commit_id = make_re(RE_COMMIT_ID);
        println!("re_commit_id: {:?}", re_commit_id);

        for (line, (mark, commit_id)) in vec![
            (
                "│ ESC[1mESC[38;5;1m×ESC[0m  Commit ID: ESC[38;5;4m4ed6adab1aa245ba48d0a3e749621d29cf9445cfESC[39m",
                ("×", "4ed6adab1aa245ba48d0a3e749621d29cf9445cf"),
            ),
            (
                "│ ESC[1mESC[38;5;1m@ESC[0m  Commit ID: ESC[38;5;4m09c05c575cf77b0d7a541a5fbbc461a962b168b1ESC[39m",
                ("@", "09c05c575cf77b0d7a541a5fbbc461a962b168b1"),
            ),
        ] {
            let line = replace_esc(line);
            println!("Testing {:?}", line);
            assert!(re_commit_id.is_match(&line));

            let captures = re_commit_id.captures(&line).unwrap();
            assert_eq!(get(&captures, "mark"), mark);
            assert_eq!(get(&captures, "commit_id"), commit_id);
        }
    }
}
