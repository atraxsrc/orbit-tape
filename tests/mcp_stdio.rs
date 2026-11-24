//! Starts `orbit-tape mcp` as a child process and speaks the protocol over its
//! stdin and stdout: initialize, list the tools, call one that needs no network.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn mcp_server_lists_its_tools_over_stdio() {
    let data = std::env::temp_dir().join(format!("orbit-tape-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    let root = env!("CARGO_MANIFEST_DIR");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-tape"))
        .arg("mcp")
        .env("ORBIT_TAPE_DATA_PATH", &data)
        .env("ORBIT_TAPE_INDICATORS_PATH", format!("{root}/indicators"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start orbit-tape mcp");

    let mut stdin = child.stdin.take().unwrap();
    let examples = format!("{root}/examples/indicators");
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_owned(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "check_indicator", "arguments": { "path": examples } },
        })
        .to_string(),
    ];
    for request in &requests {
        writeln!(stdin, "{request}").unwrap();
    }
    stdin.flush().unwrap();

    // Read the three responses, with a watchdog so a hang fails the test.
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut responses = std::collections::BTreeMap::new();
    while responses.len() < 3 {
        let line = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("server answers within 30 s");
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if let Some(id) = message["id"].as_u64() {
            responses.insert(id, message);
        }
    }
    drop(stdin);
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&data);

    let init = &responses[&1]["result"];
    assert_eq!(init["serverInfo"]["name"], "orbit-tape");
    assert!(init["capabilities"]["tools"].is_object());

    let mut names = responses[&2]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            "alert_history",
            "check_indicator",
            "create_alert",
            "delete_alert",
            "get_candles",
            "key_levels",
            "list_alerts",
            "list_indicators",
            "pause_alerts",
            "resume_alerts",
            "run_indicator",
            "scan",
            "test_indicator",
            "update_alert",
        ]
    );
    for tool in responses[&2]["result"]["tools"].as_array().unwrap() {
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "{tool}"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
    }

    let call = &responses[&3]["result"];
    assert_eq!(call["isError"], false, "{call}");
    assert_eq!(call["content"][0]["text"], "check passed: 6 source(s)");
}
