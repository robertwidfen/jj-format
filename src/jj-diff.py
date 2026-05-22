#!/usr/bin/env python3
import sys
import re

RED = "\033[38;2;224;108;117m"
GREEN = "\033[38;2;152;195;121m"
BLUE = "\033[38;2;97;175;239m"
MAGENTA = "\033[38;2;198;120;221m"
RESET = "\033[0m"
BG_GREY = "\033[48;2;40;44;52m"
CLEAR_LINE = "\033[K"

RE_FILE = re.compile(
    r"\033\[38;5;3m(Removed|Added|Modified) (((regular|executable) file)|symlink) (.+):\033\[39m"
)

for line in sys.stdin:
    clean_line = line.rstrip("\n")

    match_file = RE_FILE.match(clean_line)
    if not match_file:
        print(clean_line)
        continue

    action = match_file.group(1)
    if action == "Removed":
        print(f"{BG_GREY}{RED}D {match_file.group(5)}{CLEAR_LINE}{RESET}")
        continue
    elif action == "Added":
        print(f"{BG_GREY}{GREEN}A {match_file.group(5)}{CLEAR_LINE}{RESET}")
        continue
    elif action == "Modified":
        path_info = match_file.group(5)
        if "=>" in path_info:
            print(f"{BG_GREY}{MAGENTA}R {path_info}{CLEAR_LINE}{RESET}")
        else:
            print(f"{BG_GREY}{BLUE}M {path_info}{CLEAR_LINE}{RESET}")
        continue

    print(clean_line)
