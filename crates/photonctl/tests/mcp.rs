//! `photonctl mcp` as an agent sees it: json-rpc lines on stdin and stdout.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// send these messages, close stdin, and read every answer.
fn talk(env: &[(&str, &str)], msgs: &[Value]) -> Vec<Value> {
    let home = std::env::temp_dir().join(format!("photonctl-mcp-{}", std::process::id()));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_photonctl"));
    cmd.arg("mcp").env("PHOTON_HOME", &home).env_remove("PHOTON_SESSION").stdin(Stdio::piped()).stdout(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for m in msgs {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out: Vec<Value> = BufReader::new(child.stdout.take().unwrap()).lines().map(|l| serde_json::from_str(&l.unwrap()).unwrap()).collect();
    assert!(child.wait().unwrap().success(), "it ends cleanly when stdin closes");
    out
}

#[test]
fn an_agent_can_start_it_list_its_tools_and_hear_why_a_call_failed() {
    let out = talk(
        &[],
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            // no session: refused as a tool error, without touching a daemon.
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "photon_surface", "arguments": {"op": "remove", "surface": "x"}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "nonsense"}),
            // what loses work stays with people, refused before any daemon is asked.
            json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "photon_window", "arguments": {"cmd": "session.delete", "args": {"session": "x"}}}}),
        ],
    );
    assert_eq!(out.len(), 5, "a notification gets no answer: {out:?}");
    assert_eq!(out[4]["result"]["isError"], true);
    assert!(out[4]["result"]["content"][0]["text"].as_str().unwrap().contains("for the person to do"));
    assert_eq!(out[0]["result"]["serverInfo"]["name"], "photon");
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");

    let names: Vec<&str> = out[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["photon_window", "photon_window_state", "photon_commands", "photon_layout", "photon_terminal", "photon_surface"]);
    let surface = out[1]["result"]["tools"][5]["description"].as_str().unwrap();
    assert!(surface.contains("progress") && surface.contains("<surface-event"), "the kinds and the event are explained");

    assert_eq!(out[2]["id"], 3);
    assert_eq!(out[2]["result"]["isError"], true);
    assert!(out[2]["result"]["content"][0]["text"].as_str().unwrap().contains("wasn't started by photon"));

    assert_eq!(out[3]["error"]["code"], -32601);
}
