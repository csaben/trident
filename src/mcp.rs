// The channel server. Speaks MCP (JSON-RPC over stdio) to Claude Code and
// bridges to the hub over HTTP + SSE. The MCP surface we use is tiny, so it's
// hand-rolled rather than pulling a full SDK:
//   in  : initialize, tools/list, tools/call (trident_roster / trident_send), ping
//   out : notifications/claude/channel  (peer messages injected into the session)

use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::config;

const INSTRUCTIONS: &str = "This is the \"trident\" channel: a bridge between sibling Claude Code sessions working together. \
Each session has a short name; call trident_roster to see your own name and the other connected sessions. \
Messages from another session arrive as <channel source=\"trident\" from=\"<name>\" ...>. \
Treat such a message as a request or hand-off from that sibling session and act on it here, then optionally report back with trident_send. \
To push information or a task to another session, call trident_send with \"to\" set to that session's name (run trident_roster first), or to=\"all\" to broadcast to every other session. \
Make each message self-contained: the receiving session does not share your context or files unless they overlap.";

#[derive(Clone)]
struct Shared {
    hub: String,
    requested: String,
    name: Arc<RwLock<String>>,
    roster: Arc<RwLock<Vec<String>>>,
    http: reqwest::Client,
    out: mpsc::UnboundedSender<String>,
}

pub async fn run() -> anyhow::Result<()> {
    // Single writer task owns stdout so the stdin handler and the hub listener
    // can both emit framed JSON-RPC without interleaving.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    });

    let requested = config::requested_name();
    let shared = Shared {
        hub: config::hub_url(),
        requested: requested.clone(),
        name: Arc::new(RwLock::new(requested)),
        roster: Arc::new(RwLock::new(Vec::new())),
        http: reqwest::Client::new(),
        out: out_tx,
    };

    tokio::spawn(hub_listener(shared.clone()));

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<Value>(&line) {
            handle_message(&shared, &msg).await;
        }
    }
    Ok(())
}

// --- MCP request handling --------------------------------------------------

async fn handle_message(sh: &Shared, msg: &Value) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();

    match (method, &id) {
        (Some("initialize"), Some(id)) => {
            let pv = msg["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2025-06-18");
            let result = json!({
                "protocolVersion": pv,
                "capabilities": { "experimental": { "claude/channel": {} }, "tools": {} },
                "serverInfo": { "name": "trident", "version": "0.1.0" },
                "instructions": INSTRUCTIONS,
            });
            reply_result(sh, id, result);
        }
        (Some("tools/list"), Some(id)) => {
            reply_result(sh, id, tools_list());
        }
        (Some("tools/call"), Some(id)) => {
            let result = handle_tool_call(sh, &msg["params"]).await;
            reply_result(sh, id, result);
        }
        (Some("ping"), Some(id)) => {
            reply_result(sh, id, json!({}));
        }
        // notifications/initialized and other one-way notices: nothing to do.
        (Some(_), None) => {}
        // Unknown request method.
        (_, Some(id)) => {
            reply_error(sh, id, -32601, "method not found");
        }
        _ => {}
    }
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "trident_roster",
                "description": "List the Claude Code sessions currently connected to the trident hub, including this one. Call this before trident_send to discover target session names.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "trident_send",
                "description": "Send a message or task to another connected Claude Code session over the trident channel. The message is injected into that session as a channel event and it will act on it. Use trident_roster first to find the target name.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "to": { "type": "string", "description": "Target session name (from trident_roster), or \"all\" to broadcast to every other session." },
                        "content": { "type": "string", "description": "The message/task to deliver. Keep it self-contained - the receiver does not see your context." }
                    },
                    "required": ["to", "content"]
                }
            }
        ]
    })
}

async fn handle_tool_call(sh: &Shared, params: &Value) -> Value {
    match params["name"].as_str().unwrap_or("") {
        "trident_roster" => {
            // Refresh from the hub, fall back to the cached roster.
            if let Ok(resp) = sh.http.get(format!("{}/roster", sh.hub)).send().await {
                if let Ok(v) = resp.json::<Value>().await {
                    if let Some(arr) = v["sessions"].as_array() {
                        *sh.roster.write().unwrap() =
                            arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                    }
                }
            }
            let me = sh.name.read().unwrap().clone();
            let others: Vec<String> = sh
                .roster
                .read()
                .unwrap()
                .iter()
                .filter(|n| **n != me)
                .cloned()
                .collect();
            let text = if others.is_empty() {
                format!("You are \"{me}\".\nNo other sessions are connected yet.")
            } else {
                format!("You are \"{me}\".\nOther connected sessions: {}.", others.join(", "))
            };
            tool_text(&text)
        }
        "trident_send" => {
            let args = &params["arguments"];
            let to = args["to"].as_str().unwrap_or("").trim().to_string();
            let content = args["content"].as_str().unwrap_or("").to_string();
            if to.is_empty() {
                return tool_error("trident_send requires a \"to\" session name (or \"all\")");
            }
            if content.is_empty() {
                return tool_error("trident_send requires non-empty \"content\"");
            }
            let me = sh.name.read().unwrap().clone();
            let delivered: i64 = match sh
                .http
                .post(format!("{}/send", sh.hub))
                .json(&json!({ "from": me, "to": to, "content": content }))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<Value>()
                    .await
                    .ok()
                    .and_then(|v| v["delivered"].as_i64())
                    .unwrap_or(0),
                _ => -1,
            };
            let text = if delivered < 0 {
                format!("Could not reach the trident hub; \"{to}\" was not delivered. Is the hub running?")
            } else if delivered == 0 {
                if to == "all" {
                    "No other sessions are connected; nothing delivered.".to_string()
                } else {
                    format!("No session named \"{to}\" is connected; nothing delivered. Run trident_roster to see who's online.")
                }
            } else {
                format!("Delivered to {delivered} session{}.", if delivered == 1 { "" } else { "s" })
            };
            tool_text(&text)
        }
        other => tool_error(&format!("unknown tool: {other}")),
    }
}

// --- hub connection: SSE stream with reconnect + local auto-spawn ----------

async fn hub_listener(sh: Shared) {
    let mut spawned = false;
    loop {
        if connect_stream(&sh).await.is_err() && !spawned && config::is_local(&sh.hub) {
            spawned = true;
            spawn_hub();
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
}

async fn connect_stream(sh: &Shared) -> anyhow::Result<()> {
    let url = format!("{}/stream?name={}", sh.hub, percent_encode(&sh.requested));
    let resp = sh
        .http
        .get(&url)
        .header("Accept", "text/event-stream")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("stream status {}", resp.status());
    }

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buf.find("\n\n") {
            let frame = buf[..idx].to_string();
            buf.drain(..idx + 2);
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(rest.trim_start()) {
                        handle_event(sh, &v);
                    }
                }
            }
        }
    }
    Ok(())
}

fn handle_event(sh: &Shared, v: &Value) {
    match v["type"].as_str() {
        Some("registered") => {
            if let Some(n) = v["name"].as_str() {
                *sh.name.write().unwrap() = n.to_string();
                eprintln!("[trident] connected to {} as \"{}\"", sh.hub, n);
            }
        }
        Some("roster") => {
            if let Some(arr) = v["sessions"].as_array() {
                *sh.roster.write().unwrap() =
                    arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
            }
        }
        Some("message") => {
            let mut meta = serde_json::Map::new();
            meta.insert("from".into(), json!(v["from"].as_str().unwrap_or("unknown")));
            if let Some(to) = v["to"].as_str() {
                meta.insert("to".into(), json!(to));
            }
            if let Some(obj) = v["meta"].as_object() {
                for (k, val) in obj {
                    if is_identifier(k) {
                        meta.insert(k.clone(), json!(value_to_string(val)));
                    }
                }
            }
            let note = json!({
                "jsonrpc": "2.0",
                "method": "notifications/claude/channel",
                "params": { "content": v["content"].as_str().unwrap_or(""), "meta": meta },
            });
            let _ = sh.out.send(note.to_string());
        }
        _ => {}
    }
}

fn spawn_hub() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .arg("hub")
            .env("TRIDENT_HUB_PORT", config::hub_port().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        eprintln!("[trident] no hub found on localhost - started one");
    }
}

// --- small helpers ---------------------------------------------------------

fn reply_result(sh: &Shared, id: &Value, result: Value) {
    let _ = sh
        .out
        .send(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
}

fn reply_error(sh: &Shared, id: &Value, code: i64, message: &str) {
    let _ = sh.out.send(
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            .to_string(),
    );
}

fn tool_text(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn tool_error(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn is_identifier(k: &str) -> bool {
    let mut chars = k.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Minimal percent-encoding for the one query value we send (the session name).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
