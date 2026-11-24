//! `orbit-tape report`: runs the daily routine once with Claude Code
//! (`claude -p` and the `daily_report` prompt), saves the report and sends it
//! to the desktop and/or ntfy. Meant to run from the systemd user timer
//! `orbit-tape-report.timer`, which is off until the user enables it.
//!
//! Claude gets only this program's MCP server (`orbit-tape mcp`, through
//! `--mcp-config` and `--strict-mcp-config`) and may use only its tools, so
//! the run does not depend on how the server is registered in Claude Code.

use crate::mcp::report::{DAILY_REPORT, REPORT_TOOLS};
use data::config::alert_store::{AlertStore, Channel};
use data::config::plan::Plan;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const USAGE: &str = "\
Usage: orbit-tape report [options]

Runs the daily routine once with Claude Code: `claude -p` with the
daily_report prompt and the Orbit Tape tools only. The report is saved to
reports/YYYY-MM-DD.md in the data folder and sent to the desktop and/or
ntfy. A failed run is sent too, with the reason.

Needs the plan file (see docs/plan.example.toml) and Claude Code logged in.
Each run uses the Claude plan or API key that `claude` is logged in with.

Options:
  --channel desktop|ntfy|both   Where to send it. Default: the plan's
                                report.alert_channel.
  --no-send                     Save and print the report, send nothing.
  --claude PATH                 The claude program. Default: $ORBIT_TAPE_CLAUDE,
                                else claude on PATH, else ~/.local/bin/claude.
  --timeout MINUTES             Stop Claude after this long. Default 15.
  --print-prompt                Print the prompt and exit.

Claude may use only get_plan, market_snapshot, roman_setups, spot_status,
list_alerts and create_alert: it cannot change, pause or delete alerts.

Daily at 11:05 local time:
  systemctl --user enable --now orbit-tape-report.timer
Another time: systemctl --user edit orbit-tape-report.timer
Logs: journalctl --user -u orbit-tape-report";

const DEFAULT_TIMEOUT_MINUTES: u64 = 15;
/// ntfy's message limit is 4096 bytes; leave room for the note.
const NTFY_BYTES: usize = 3800;
const DESKTOP_CHARS: usize = 900;

#[derive(Debug, Default, PartialEq)]
struct Options {
    channel: Option<Channel>,
    send: bool,
    claude: Option<PathBuf>,
    timeout: Option<Duration>,
    print_prompt: bool,
    help: bool,
}

fn parse_channel(text: &str) -> Result<Channel, String> {
    match text.trim().to_lowercase().as_str() {
        "desktop" => Ok(Channel::Desktop),
        "ntfy" => Ok(Channel::Ntfy),
        "both" => Ok(Channel::Both),
        other => Err(format!(
            "channel must be desktop, ntfy or both, got '{other}'"
        )),
    }
}

fn parse_options(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        send: true,
        ..Options::default()
    };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "--help" | "-h" => options.help = true,
            "--no-send" => options.send = false,
            "--print-prompt" => options.print_prompt = true,
            "--channel" => options.channel = Some(parse_channel(&value("--channel")?)?),
            "--claude" => options.claude = Some(PathBuf::from(value("--claude")?)),
            "--timeout" => {
                let text = value("--timeout")?;
                let minutes: u64 = text
                    .parse()
                    .ok()
                    .filter(|minutes| *minutes > 0)
                    .ok_or_else(|| format!("--timeout takes whole minutes, got '{text}'"))?;
                options.timeout = Some(Duration::from_secs(minutes * 60));
            }
            other => return Err(format!("unknown argument '{other}' (see --help)")),
        }
    }
    Ok(options)
}

pub fn run(arguments: &[String]) -> i32 {
    let options = match parse_options(arguments) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("orbit-tape report: {error}");
            return 1;
        }
    };
    if options.help {
        println!("{USAGE}");
        return 0;
    }
    if options.print_prompt {
        println!("{DAILY_REPORT}");
        return 0;
    }
    crate::alert_channels::set_proxy(crate::alert_channels::saved_proxy());
    let today = chrono::Local::now().date_naive();
    let title = format!("Orbit Tape daily report {today}");

    // The plan first: without it there is nothing to run, and its channel
    // says where to send the failure.
    let plan = Plan::load_from(&data::config::plan::path());
    let channel = options.channel.unwrap_or_else(|| {
        plan.as_ref()
            .ok()
            .and_then(|plan| parse_channel(&plan.report.alert_channel).ok())
            .unwrap_or(Channel::Both)
    });
    let result = plan
        .and_then(|_| find_claude(options.claude.as_deref()))
        .and_then(|claude| {
            run_claude(
                &claude,
                options
                    .timeout
                    .unwrap_or(Duration::from_secs(DEFAULT_TIMEOUT_MINUTES * 60)),
            )
        });
    match result {
        Ok(report) => {
            let saved = match save(&report, today) {
                Ok(path) => {
                    eprintln!("orbit-tape report: saved {}", path.display());
                    Some(path)
                }
                Err(error) => {
                    eprintln!("orbit-tape report: could not save the report: {error}");
                    None
                }
            };
            println!("{report}");
            if options.send {
                send(&title, &report, saved.as_deref(), channel);
            }
            0
        }
        Err(error) => {
            eprintln!("orbit-tape report: {error}");
            if options.send {
                send(&format!("{title} failed"), &error, None, channel);
            }
            1
        }
    }
}

/// The claude program: the option, `$ORBIT_TAPE_CLAUDE`, claude on PATH, or
/// the installer's usual places (a systemd user service has a short PATH).
fn find_claude(given: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = given
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("ORBIT_TAPE_CLAUDE").map(PathBuf::from))
    {
        return Ok(path);
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("claude"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let usual = home.iter().flat_map(|home| {
        [
            home.join(".local/bin/claude"),
            home.join(".claude/local/claude"),
        ]
    });
    on_path
        .into_iter()
        .chain(usual)
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "Claude Code (claude) was not found: install it, or pass --claude PATH or set \
             ORBIT_TAPE_CLAUDE"
                .to_owned()
        })
}

/// The arguments for `claude -p`: only this program's MCP server, only the
/// routine's tools, plain text out. The prompt goes on stdin.
fn claude_args(orbit_tape: &Path) -> Vec<String> {
    let config = serde_json::json!({
        "mcpServers": {
            "orbit-tape": {
                "type": "stdio",
                "command": orbit_tape.to_string_lossy(),
                "args": ["mcp"],
            }
        }
    });
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "text",
        "--mcp-config",
        &config.to_string(),
        "--strict-mcp-config",
        "--allowedTools",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.push(REPORT_TOOLS.join(","));
    args
}

fn run_claude(claude: &Path, timeout: Duration) -> Result<String, String> {
    let orbit_tape = std::env::current_exe()
        .map_err(|error| format!("could not find this program's path: {error}"))?;
    // Run from the reports folder, so no project's CLAUDE.md or settings apply.
    let folder = data::data_path(Some("reports"));
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    let mut child = Command::new(claude)
        .args(claude_args(&orbit_tape))
        .current_dir(&folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", claude.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(DAILY_REPORT.as_bytes())
            .map_err(|error| format!("could not send the prompt to claude: {error}"))?;
    }
    // Read both pipes on threads so a full pipe never blocks the child.
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = reader(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = reader(child.stderr.take().map(|p| Box::new(p) as _));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "claude did not finish within {} minutes and was stopped",
                    timeout.as_secs() / 60
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(error) => return Err(format!("could not wait for claude: {error}")),
        }
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if !status.success() {
        let detail = last_lines(if err.trim().is_empty() { &out } else { &err }, 5);
        return Err(format!("claude exited with {status}: {detail}"));
    }
    let report = out.trim().to_owned();
    if report.is_empty() {
        return Err("claude gave an empty report".to_owned());
    }
    Ok(report)
}

fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

/// Saves the day's report (one per day; a rerun replaces it), readable by the
/// user only: it holds their plan's numbers.
fn save(report: &str, date: chrono::NaiveDate) -> std::io::Result<PathBuf> {
    save_in(&data::data_path(Some("reports")), report, date)
}

fn save_in(folder: &Path, report: &str, date: chrono::NaiveDate) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let path = folder.join(format!("{date}.md"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(report.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(path)
}

/// `text` cut to at most `max` bytes on a character boundary, with a note
/// where the rest is.
fn shortened(text: &str, max: usize, saved: Option<&Path>) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let rest = match saved {
        Some(path) => format!("full report: {}", path.display()),
        None => "cut short".to_owned(),
    };
    format!("{}\n... ({rest})", text[..end].trim_end())
}

fn send(title: &str, body: &str, saved: Option<&Path>, channel: Channel) {
    if channel.desktop() {
        let shown = shortened(body, DESKTOP_CHARS, saved);
        // Shown here, not on a thread: the program exits right after.
        if let Err(error) = notify_rust::Notification::new()
            .appname("Orbit Tape")
            .summary(title)
            .body(&shown)
            .icon("appointment-soon")
            .timeout(notify_rust::Timeout::Milliseconds(30_000))
            .show()
        {
            eprintln!("orbit-tape report: desktop notification failed: {error}");
        }
    }
    if channel.ntfy() {
        let settings = AlertStore::load().channels.ntfy;
        if !settings.is_set() {
            eprintln!("orbit-tape report: no ntfy topic is set (Settings, Alerts)");
            return;
        }
        let outgoing = crate::alert_channels::Outgoing {
            summary: title.to_owned(),
            body: shortened(body, NTFY_BYTES, saved),
        };
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
            .and_then(|runtime| {
                runtime.block_on(crate::alert_channels::ntfy(&outgoing, &settings))
            });
        if let Err(error) = result {
            eprintln!("orbit-tape report: ntfy: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn options() {
        let options =
            parse_options(&args(&["--channel", "ntfy", "--timeout", "5", "--no-send"])).unwrap();
        assert_eq!(options.channel, Some(Channel::Ntfy));
        assert_eq!(options.timeout, Some(Duration::from_secs(300)));
        assert!(!options.send);
        assert!(parse_options(&[]).unwrap().send);
        assert!(parse_options(&args(&["--timeout", "0"])).is_err());
        assert!(parse_options(&args(&["--channel", "email"])).is_err());
        assert!(
            parse_options(&args(&["--claude"]))
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            parse_options(&args(&["--now"]))
                .unwrap_err()
                .contains("unknown")
        );
    }

    #[test]
    fn claude_gets_only_this_server_and_its_tools() {
        let args = claude_args(Path::new("/usr/bin/orbit-tape"));
        assert_eq!(args[0], "-p");
        assert!(args.contains(&"--strict-mcp-config".to_owned()));
        let at = args.iter().position(|a| a == "--allowedTools").unwrap();
        let allowed: Vec<&str> = args[at + 1].split(',').collect();
        assert_eq!(allowed.len(), 6);
        assert!(allowed.contains(&"mcp__orbit-tape__create_alert"));
        for refused in [
            "delete_alert",
            "update_alert",
            "pause_alerts",
            "check_indicator",
            "*",
        ] {
            assert!(!args[at + 1].contains(refused), "{refused}");
        }
        let at = args.iter().position(|a| a == "--mcp-config").unwrap();
        let config: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();
        assert_eq!(
            config["mcpServers"]["orbit-tape"]["command"],
            "/usr/bin/orbit-tape"
        );
        assert_eq!(config["mcpServers"]["orbit-tape"]["args"][0], "mcp");
    }

    #[test]
    fn long_reports_are_cut_on_a_character_boundary() {
        let text = "é".repeat(10); // 20 bytes
        let cut = shortened(&text, 7, Some(Path::new("/r/2027-01-04.md")));
        assert!(cut.starts_with("ééé\n"), "{cut}");
        assert!(cut.ends_with("(full report: /r/2027-01-04.md)"));
        assert_eq!(shortened("short", 7, None), "short");
    }

    #[test]
    fn reports_are_saved_per_day_for_the_user_only() {
        let folder = std::env::temp_dir().join(format!("orbit-tape-report-{}", std::process::id()));
        let date = chrono::NaiveDate::from_ymd_opt(2027, 1, 4).unwrap();
        let path = save_in(&folder, "first", date).unwrap();
        save_in(&folder, "second", date).unwrap();
        assert_eq!(path.file_name().unwrap(), "2027-01-04.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_last_lines_of_an_error() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines("only", 5), "only");
    }
}
