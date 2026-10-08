## What it does

Takes jj output and applies some formatting to it before it goes to the pager.

Features:
- Use status prefixes (A, M, D, R) also in diff
- Use x (-x) and X (+x) in diff for executable change
- highlight change ID lines with blueish background to line end
- highlight filename lines of diff with grey background to line end
- highlight overlong summary (default to > 50 yellow, > 72 red)
- replace \ by / in filenames on Windows

<img src="screenshot.png" alt="jj-format screenshot" width="400">

## Install

Without a pager argument the built-in pager [minus](https://crates.io/crates/minus) is used.

Add jj-format as pager to jj config by:
```toml
[ui]
pager = "jj-format"
#pager = "jj-format less -FrfX"
```

## Usage

```
jj-format [OPTIONS] [PAGER]...
```

| Option | Description |
| --- | --- |
| `-w`, `--summary-warning-len <N>` | Highlight summary characters beyond this length in yellow (default: 50) |
| `-e`, `--summary-error-len <N>` | Highlight summary characters beyond this length in red (default: 72) |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print version |
| `[PAGER]...` | Pager command and its arguments, e.g. `less -FrfX`. Everything after the first non-option argument is passed to the pager. If omitted, the built-in pager [minus](https://crates.io/crates/minus) is used. |
