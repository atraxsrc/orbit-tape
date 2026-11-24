//! `orbit-tape report` with a stand-in for `claude`: the prompt goes in on
//! stdin, the report is saved per day, failures exit 1 with the reason.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn folder(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("orbit-tape-report-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn report(data: &Path, claude: &Path, plan: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-tape"))
        .args(["report", "--no-send", "--claude"])
        .arg(claude)
        .env("ORBIT_TAPE_DATA_PATH", data)
        .env("ORBIT_TAPE_PLAN", plan)
        .output()
        .unwrap()
}

fn example_plan() -> String {
    format!("{}/docs/plan.example.toml", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_report_is_claudes_answer_saved_for_the_day() {
    let dir = folder("ok");
    // Answers with its strict-config flag and the prompt's first words.
    let claude = script(
        &dir,
        r#"strict=no; for a in "$@"; do [ "$a" = --strict-mcp-config ] && strict=yes; done
echo "strict: $strict"; head -c 29 -"#,
    );
    let out = report(&dir, &claude, &example_plan());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "strict: yes\nRun the daily trading routine");
    let today = chrono::Local::now().date_naive();
    let saved = std::fs::read_to_string(dir.join(format!("reports/{today}.md"))).unwrap();
    assert_eq!(saved.trim(), stdout.trim());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_run_exits_1_with_the_reason() {
    let dir = folder("fail");
    let claude = script(&dir, "cat >/dev/null; echo 'Invalid API key' >&2; exit 3");
    let out = report(&dir, &claude, &example_plan());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Invalid API key"), "{stderr}");
    assert!(
        !dir.join("reports").read_dir().unwrap().any(|_| true),
        "nothing saved"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_plan_no_run() {
    let dir = folder("noplan");
    let claude = script(&dir, "echo should not run");
    let out = report(&dir, &claude, &dir.join("missing.toml").to_string_lossy());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("plan.example.toml"), "{stderr}");
    assert!(out.stdout.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
