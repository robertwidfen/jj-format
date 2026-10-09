use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::{event::DisableMouseCapture, execute};
use regex::Regex;
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

const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";

// [48;2; means background color, [38;2; means foreground color
// remaining values are RGB values

// const RED: &str = "\x1b[38;2;224;108;117m";
// const GREEN: &str = "\x1b[38;2;152;195;121m";
// const BLUE: &str = "\x1b[38;2;97;175;239m";
// const MAGENTA: &str = "\x1b[38;2;198;120;221m";

const LIGHT_GRAY: &str = "\x1b[38;2;120;120;120m";
const BG_GREY: &str = "\x1b[48;2;40;44;52m";
const BG_BLUE: &str = "\x1b[48;2;30;44;52m";

const RESET: &str = "\x1b[0m";
// make background to go to end of line
const CLEAR_LINE: &str = "\x1b[K";

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
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
    let re_change = Regex::new(r"^(│ )*(\x1b\[1m\x1b\[38;\d+;\d+m)?[@◆○×]").unwrap();
    let re_summary = Regex::new(r"^.*\x1b\[0m\s*([^\x1b]+).*").unwrap();

    let re_diff_file = Regex::new(
        r"\x1b\[38;5;3m(Removed|Added|Modified) (((regular|executable) file)|symlink) (.+?( \((.+) => (.+)\))?):\x1b\[39m",
    )
    .unwrap();

    let re_exec_file = Regex::new(
        r"\x1b\[38;5;3m(Non-e|E)xecutable file became (non-)?executable at (.+?):\x1b\[39m",
    )
    .unwrap();

    let re_conflict_file =
        Regex::new(r"\x1b\[38;5;3m(Created conflict in) (.+?):\x1b\[39m").unwrap();

    let re_status_file =
        Regex::new(r"\x1b\[38;5;[256]m([MADRC?]) (\{(?:(.+) => (.+)\})|.+?)\x1b\[39m").unwrap();

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

    for line in stdin.lock().lines().map_while(Result::ok) {
        if let Some(captures) = re_status_file.captures(&line) {
            let status = captures.get(1).map_or("", |m| m.as_str());
            let path = normalize_path(captures.get(2).map_or("", |m| m.as_str()));

            match status {
                "A" => writeln!(fd, "{GREEN}A {path}{CLEAR_LINE}{RESET}")?,
                "M" => writeln!(fd, "{BLUE}M {path}{CLEAR_LINE}{RESET}")?,
                "D" => writeln!(fd, "{RED}D {path}{CLEAR_LINE}{RESET}")?,
                "R" => {
                    if path.contains("=>") {
                        let old_path = normalize_path(captures.get(3).map_or("", |m| m.as_str()));
                        let new_path = normalize_path(captures.get(4).map_or("", |m| m.as_str()));
                        writeln!(
                            fd,
                            "{MAGENTA}R {new_path} {LIGHT_GRAY}<= {old_path}{CLEAR_LINE}{RESET}"
                        )?;
                    } else {
                        writeln!(fd, "{BG_GREY}{BLUE}M {path}{CLEAR_LINE}{RESET}")?;
                    }
                }
                "C" => writeln!(fd, "{RED}C {path}{CLEAR_LINE}{RESET}")?,
                "?" => writeln!(fd, "{MAGENTA}? {path}{CLEAR_LINE}{RESET}")?,
                _ => writeln!(fd, "{}", line)?,
            }
        } else if let Some(_captures) = re_change.captures(&line) {
            let mut bg_line = line.replace(RESET, &format!("\x1b[0m{BG_BLUE}"));
            if let Some(captures) = re_summary.captures(&line) {
                let summary = captures.get(1).map_or("", |m| m.as_str());
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
        } else if let Some(captures) = re_diff_file.captures(&line)
            && let status = captures.get(1).map_or("", |m| m.as_str())
        {
            let path = normalize_path(captures.get(5).map_or("", |m| m.as_str()));
            match status {
                "Removed" => {
                    writeln!(fd, "{BG_GREY}{RED}D {path}{CLEAR_LINE}{RESET}")?;
                }
                "Added" => {
                    writeln!(fd, "{BG_GREY}{GREEN}A {path}{CLEAR_LINE}{RESET}")?;
                }
                "Modified" => {
                    if path.contains("=>") {
                        let new_path = normalize_path(captures.get(7).map_or("", |m| m.as_str()));
                        let old_path = normalize_path(captures.get(8).map_or("", |m| m.as_str()));
                        writeln!(
                            fd,
                            "{BG_GREY}{MAGENTA}R {new_path} {LIGHT_GRAY}<= {old_path}{CLEAR_LINE}{RESET}"
                        )?;
                    } else {
                        writeln!(fd, "{BG_GREY}{BLUE}M {path}{CLEAR_LINE}{RESET}")?;
                    }
                }
                _ => writeln!(fd, "{}", line)?,
            }
        } else if let Some(captures) = re_exec_file.captures(&line) {
            let path = normalize_path(captures.get(3).map_or("", |m| m.as_str()));
            let x = if line.contains("became executable") {
                "X"
            } else {
                "x"
            };
            writeln!(fd, "{BG_GREY}{BLUE}{x} {path}{CLEAR_LINE}{RESET}")?;
        } else if let Some(captures) = re_conflict_file.captures(&line) {
            let path = normalize_path(captures.get(2).map_or("", |m| m.as_str()));
            writeln!(fd, "{BG_GREY}{RED}C {path}{CLEAR_LINE}{RESET}")?;
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
        pager
            .set_prompt(page_prompt(0, rows, text.lines().count()))
            .map_err(io::Error::other)?;
        pager
            .set_input_classifier(Box::new(PageIndicator {
                inner: minus::input::HashedEventRegister::default(),
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
        let re_sgr = Regex::new(r"\x1b\[([0-9;]*)m").unwrap();
        let mut sgr = String::new();
        let mut out = stdout().lock();
        for (i, row) in wrapped[..range.end].iter().enumerate() {
            if i >= range.start {
                writeln!(out, "{sgr}{row}\x1b[0m")?;
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
