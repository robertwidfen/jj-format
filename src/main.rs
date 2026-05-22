use regex::Regex;
use std::env;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};

const RED: &str = "\x1b[38;2;224;108;117m";
const GREEN: &str = "\x1b[38;2;152;195;121m";
const BLUE: &str = "\x1b[38;2;97;175;239m";
const MAGENTA: &str = "\x1b[38;2;198;120;221m";
const LIGHT_GRAY: &str = "\x1b[38;2;120;120;120m";
const RESET: &str = "\x1b[0m";
const BG_GREY: &str = "\x1b[48;2;40;44;52m";
const CLEAR_LINE: &str = "\x1b[K";

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let re_diff_file = Regex::new(
        r"\x1b\[38;5;3m(Removed|Added|Modified) (((regular|executable) file)|symlink) (.+?( \((.+) => (.+)\))?):\x1b\[39m",
    )
    .unwrap();
    let re_exec_file = Regex::new(
        r"\x1b\[38;5;3m(Non-e|E)xecutable file became (non-)?executable at (.+?):\x1b\[39m",
    )
    .unwrap();
    let re_status_file = Regex::new(r"\x1b\[38;5;6mR \{(.+) => (.+)\}\x1b\[39m").unwrap();

    let args: Vec<String> = env::args().collect();

    let mut stdout = io::stdout();
    let mut child = None;

    let fd: &mut dyn Write = if args.len() == 1 {
        &mut stdout
    } else {
        let pager_program = &args[1];
        let pager_flags = &args[2..];
        let cmd = Command::new(pager_program)
            .args(pager_flags)
            .stdin(Stdio::piped())
            .spawn()?;
        child = Some(cmd);
        child.as_mut().unwrap().stdin.as_mut().unwrap()
    };

    for line in stdin.lock().lines().map_while(Result::ok) {
        if let Some(captures) = re_diff_file.captures(&line)
            && let action = captures.get(1).map_or("", |m| m.as_str())
        {
            match action {
                "Removed" => {
                    let path = captures.get(5).map_or("", |m| m.as_str());
                    writeln!(fd, "{BG_GREY}{RED}D {path}{CLEAR_LINE}{RESET}")?;
                }
                "Added" => {
                    let path = captures.get(5).map_or("", |m| m.as_str());
                    writeln!(fd, "{BG_GREY}{GREEN}A {path}{CLEAR_LINE}{RESET}")?;
                }
                "Modified" => {
                    let path = captures.get(5).map_or("", |m| m.as_str());
                    if path.contains("=>") {
                        let new_path = captures.get(7).map_or("", |m| m.as_str());
                        let old_path = captures.get(8).map_or("", |m| m.as_str());
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
            let path = captures.get(3).map_or("", |m| m.as_str());
            let x = if line.contains("became executable") {
                "X"
            } else {
                "x"
            };
            writeln!(fd, "{BG_GREY}{BLUE}{x} {path}{CLEAR_LINE}{RESET}")?;
        } else if let Some(captures) = re_status_file.captures(&line) {
            let old_path = captures.get(1).map_or("", |m| m.as_str());
            let new_path = captures.get(2).map_or("", |m| m.as_str());
            writeln!(
                fd,
                "{MAGENTA}R {new_path} {LIGHT_GRAY}<= {old_path}{CLEAR_LINE}{RESET}"
            )?;
        } else {
            writeln!(fd, "{}", line)?;
        }
    }

    if let Some(mut child_process) = child {
        drop(child_process.stdin.take());
        child_process.wait()?;
    }

    Ok(())
}
