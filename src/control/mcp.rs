//! Minimal MCP (Model Context Protocol) stdio server.
//!
//! `yt-tui mcp` is spawned by an MCP client (Claude Code / Desktop). It translates
//! `tools/call` into one request line on the running TUI's control socket.

use crate::paths::Paths;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

const DEFAULT_PROTOCOL: &str = "2024-11-05";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

fn tools() -> Vec<Value> {
    let vid = json!({ "video_id": { "type": "string", "description": "YouTube video id, from `search`" } });
    vec![
        tool("state", "Current playback state: track, position, volume, shuffle, repeat, queue.", json!({}), &[]),
        tool("queue", "The play queue and the index of the track currently playing.", json!({}), &[]),
        tool("play", "Resume playback.", json!({}), &[]),
        tool("pause", "Pause playback.", json!({}), &[]),
        tool("toggle", "Toggle between play and pause.", json!({}), &[]),
        tool("next", "Skip to the next track.", json!({}), &[]),
        tool("previous", "Go to the previous track.", json!({}), &[]),
        tool(
            "seek",
            "Seek to an absolute position in the current track.",
            json!({ "seconds": { "type": "number", "minimum": 0 } }),
            &["seconds"],
        ),
        tool(
            "volume",
            "Set the volume.",
            json!({ "percent": { "type": "number", "minimum": 0, "maximum": 100 } }),
            &["percent"],
        ),
        tool(
            "mute",
            "Mute or unmute.",
            json!({ "muted": { "type": "boolean", "description": "default true" } }),
            &[],
        ),
        tool(
            "shuffle",
            "Enable or disable shuffle.",
            json!({ "enabled": { "type": "boolean" } }),
            &["enabled"],
        ),
        tool(
            "repeat",
            "Set the repeat mode.",
            json!({ "mode": { "type": "string", "enum": ["off", "all", "one"] } }),
            &["mode"],
        ),
        tool(
            "search",
            "Search YouTube Music. Returns tracks (with video_id for play_track/queue_add), albums, artists or playlists depending on filter.",
            json!({
                "query": { "type": "string" },
                "filter": { "type": "string", "enum": ["songs", "albums", "artists", "playlists"], "description": "default songs" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "default 10" }
            }),
            &["query"],
        ),
        tool("play_track", "Play a track immediately.", vid.clone(), &["video_id"]),
        tool(
            "play_tracks",
            "Replace the queue with these tracks and start playing the first.",
            json!({ "video_ids": { "type": "array", "items": { "type": "string" } } }),
            &["video_ids"],
        ),
        tool("queue_add", "Append a track to the end of the queue.", vid.clone(), &["video_id"]),
        tool("queue_play_next", "Insert a track right after the current one.", vid, &["video_id"]),
        tool(
            "queue_remove",
            "Remove the track at a queue index (see `queue`).",
            json!({ "index": { "type": "integer", "minimum": 0 } }),
            &["index"],
        ),
        tool(
            "queue_jump",
            "Jump to and play the track at a queue index (see `queue`).",
            json!({ "index": { "type": "integer", "minimum": 0 } }),
            &["index"],
        ),
        tool("queue_clear", "Clear upcoming tracks from the queue.", json!({}), &[]),
    ]
}

#[cfg(unix)]
fn call_socket(paths: &Paths, request: &Value) -> Result<Value> {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock = paths.control_socket();
    let mut stream = UnixStream::connect(&sock).map_err(|e| {
        anyhow::anyhow!(
            "cannot reach yt-tui ({e}). Start yt-tui and set `[control] enabled = true` in config.toml."
        )
    })?;
    // Searches hit the network, so allow a generous reply window.
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    if reply.is_empty() {
        anyhow::bail!("yt-tui closed the connection");
    }
    Ok(serde_json::from_str(&reply)?)
}

#[cfg(not(unix))]
fn call_socket(_paths: &Paths, _request: &Value) -> Result<Value> {
    anyhow::bail!("control socket is only supported on unix platforms")
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn handle_call(paths: &Paths, params: &Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return tool_result("missing tool name".into(), true);
    };
    if !tools().iter().any(|t| t["name"] == name) {
        return tool_result(format!("unknown tool '{name}'"), true);
    }
    let mut req = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    req.insert("cmd".into(), json!(name));
    match call_socket(paths, &Value::Object(req)) {
        Ok(resp) if resp["ok"] == true => tool_result(
            serde_json::to_string_pretty(&resp["data"]).unwrap_or_default(),
            false,
        ),
        Ok(resp) => tool_result(
            resp["error"].as_str().unwrap_or("unknown error").to_string(),
            true,
        ),
        Err(e) => tool_result(e.to_string(), true),
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Returns the response to send, or `None` for notifications.
fn handle_message(paths: &Paths, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str)?;
    // Notifications carry no id and get no reply.
    let id = msg.get("id").cloned()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "yt-tui", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => handle_call(paths, &params),
        other => return Some(error_response(id, -32601, &format!("method not found: {other}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Blocks serving MCP over stdin/stdout until stdin closes.
pub fn run(paths: &Paths) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle_message(paths, &msg),
            Err(e) => Some(error_response(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{r}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        let d = std::env::temp_dir().join("yt-tui-mcp-test");
        Paths { config_dir: d.clone(), data_dir: d.clone(), cache_dir: d }
    }

    #[test]
    fn handshake_and_tools() {
        let p = paths();
        let init = handle_message(&p, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle_message(&p, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let list = handle_message(&p, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<_> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"search") && names.contains(&"pause"));
        assert!(!names.iter().any(|n| n.contains("login") || n.contains("cookie")));
    }

    #[test]
    fn call_without_running_app_is_tool_error() {
        let r = handle_message(&paths(), &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pause"}})).unwrap();
        assert_eq!(r["result"]["isError"], true);
        let r = handle_message(&paths(), &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"logout"}})).unwrap();
        assert_eq!(r["result"]["isError"], true);
    }
}
