//! Top-level command line: `--version`, `--help`, the headless commands, and
//! the options the app itself accepts. Anything else is refused so a typo
//! does not silently open the window.

const USAGE: &str = "\
Usage: orbit-tape [--indicator-path <file.ind>]...
       orbit-tape <command> [options]

Native crypto charting for COSMIC. With no command, opens the app.

Commands:
  scan      Run an indicator over a coin list and report alert signals
  watch     Run the saved alerts without the app (systemd user service)
  mcp       MCP server for AI assistants (stdio)
  report    Run the daily routine through Claude Code

Options:
  --indicator-path <file.ind>  Load an extra .ind indicator (repeatable)
  -h, --help                   Show this help
  -V, --version                Show the version

Run 'orbit-tape <command> --help' for a command's options.";

#[derive(Debug, PartialEq, Eq)]
pub enum Launch<'a> {
    /// Open the app; `--indicator-path` is read later by the indicator loader.
    App,
    Help,
    Version,
    /// A headless command and the arguments after it.
    Command(&'a str, &'a [String]),
    Error(String),
}

const COMMANDS: [&str; 4] = ["scan", "watch", "mcp", "report"];

pub fn parse(arguments: &[String]) -> Launch<'_> {
    if let Some(first) = arguments.first()
        && COMMANDS.contains(&first.as_str())
    {
        return Launch::Command(first, &arguments[1..]);
    }

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "-h" | "--help" => return Launch::Help,
            "-V" | "--version" => return Launch::Version,
            // The loader reports a missing value.
            "--indicator-path" => index += 1,
            _ if argument.starts_with("--indicator-path=") => {}
            _ if argument.starts_with('-') && argument != "-" => {
                return Launch::Error(format!("unknown option '{argument}'"));
            }
            _ if index == 0 => return Launch::Error(format!("unknown command '{argument}'")),
            _ => return Launch::Error(format!("unexpected argument '{argument}'")),
        }
        index += 1;
    }
    Launch::App
}

pub fn version_line() -> String {
    let (version, sha) = crate::version::app_build_version_parts();
    match sha {
        Some(sha) => format!("orbit-tape {version} ({sha})"),
        None => format!("orbit-tape {version}"),
    }
}

/// Handles help, version and errors; returns the exit code, or `None` when
/// the caller should go on (open the app or run a command).
pub fn handle_simple(launch: &Launch<'_>) -> Option<i32> {
    match launch {
        Launch::Help => {
            println!("{USAGE}");
            Some(0)
        }
        Launch::Version => {
            println!("{}", version_line());
            Some(0)
        }
        Launch::Error(message) => {
            eprintln!("orbit-tape: {message}. Try 'orbit-tape --help'.");
            Some(2)
        }
        Launch::App | Launch::Command(..) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn no_arguments_opens_the_app() {
        assert_eq!(parse(&[]), Launch::App);
    }

    #[test]
    fn indicator_paths_open_the_app() {
        let list = args(&[
            "--indicator-path",
            "a.ind",
            "--indicator-path=b.ind",
            "--indicator-path",
            "-weird-name.ind",
        ]);
        assert_eq!(parse(&list), Launch::App);
    }

    #[test]
    fn help_and_version() {
        for flag in ["-h", "--help"] {
            assert_eq!(parse(&args(&[flag])), Launch::Help);
        }
        for flag in ["-V", "--version"] {
            assert_eq!(parse(&args(&[flag])), Launch::Version);
        }
        assert_eq!(
            parse(&args(&["--indicator-path", "a.ind", "--version"])),
            Launch::Version
        );
    }

    #[test]
    fn commands_pass_their_arguments_through() {
        let list = args(&["scan", "--help", "--symbols", "BTC"]);
        assert_eq!(parse(&list), Launch::Command("scan", &list[1..]));
        for command in COMMANDS {
            let list = args(&[command]);
            assert_eq!(parse(&list), Launch::Command(command, &[]));
        }
    }

    #[test]
    fn unknown_arguments_are_refused() {
        assert_eq!(
            parse(&args(&["-v"])),
            Launch::Error("unknown option '-v'".into())
        );
        assert_eq!(
            parse(&args(&["--verbose"])),
            Launch::Error("unknown option '--verbose'".into())
        );
        assert_eq!(
            parse(&args(&["scna"])),
            Launch::Error("unknown command 'scna'".into())
        );
        assert_eq!(
            parse(&args(&["--indicator-path", "a.ind", "extra"])),
            Launch::Error("unexpected argument 'extra'".into())
        );
    }
}
