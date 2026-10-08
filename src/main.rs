use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use regex::Regex;
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

    /// Optional pager command and its arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pager: Vec<String>,
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
    let re_change = Regex::new(r"^(│ )*(\x1b\[1m\x1b\[38;\d+;\d+m)?[@◆○]").unwrap();
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
        Regex::new(r"\x1b\[38;5;\dm([A-Z?]) (\{(?:(.+) => (.+)\})|.+?)\x1b\[39m").unwrap();

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
        // assumes no wrapped lines, corrected on first key press
        let rows = crossterm::terminal::size().map_or(25, |(_, r)| r as usize);
        pager
            .set_prompt(page_prompt(0, rows, text.lines().count()))
            .map_err(io::Error::other)?;
        pager
            .set_input_classifier(Box::new(PageIndicator {
                inner: minus::input::HashedEventRegister::default(),
                pager: pager.clone(),
            }))
            .map_err(io::Error::other)?;
        pager.push_str(text).map_err(io::Error::other)?;
        minus::page_all(pager).map_err(io::Error::other)?;
    }

    Ok(())
}
