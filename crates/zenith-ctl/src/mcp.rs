//! The Model Context Protocol, over stdio: one tool per command in the table.
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;
use zenith_agent::control::ControlClient;
use zenith_agent::{ArgKind, COMMANDS, CommandSpec};

const PROTOCOL_VERSION: &str = "2025-06-18";

fn input_schema(c: &CommandSpec) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for a in c.args {
        let kind = match a.kind {
            ArgKind::Int => "integer",
            ArgKind::Number => "number",
            ArgKind::Text => "string",
            ArgKind::Flag => "boolean",
        };
        properties.insert(a.name.to_string(), json!({ "type": kind, "description": a.help }));
        if a.required {
            required.push(json!(a.name));
        }
    }
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

pub fn tools_list() -> Value {
    let tools: Vec<Value> = COMMANDS
        .iter()
        .map(|c| {
            let description = if c.needs_gpu {
                format!("{} (only when the game itself is what is running)", c.help)
            } else {
                c.help.to_string()
            };
            json!({
                "name": c.name,
                "description": description,
                "inputSchema": input_schema(c),
                "annotations": { "readOnlyHint": !c.acts },
            })
        })
        .collect();
    json!({ "tools": tools })
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Answers one JSON-RPC request. `None` for notifications, which get no answer.
pub fn handle(request: &Value, client: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or_default();
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "zenith", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Zenith is a voxel sandbox game. These tools play it: call state to see where you are, wait_ready after joining, then move, mine, build and craft. Commands block until done and reply with the result.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or_default();
            let args = request["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
            Ok(call_tool(name, &args, client))
        }
        // Notifications need no answer; anything else with an id gets told it does not exist.
        _ if id.is_none() => return None,
        other => Err(json!({ "code": -32601, "message": format!("no method {other}") })),
    };
    id.as_ref()?;
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

fn call_tool(name: &str, args: &Value, client: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) -> Value {
    let Some(spec) = zenith_agent::commands::find(name) else {
        return json!({ "isError": true, "content": [{ "type": "text", "text": format!("no tool called {name}") }] });
    };
    match client(spec.name, args) {
        Ok(answer) if answer["ok"] == json!(true) => {
            let result = &answer["result"];
            // A screenshot comes back as the picture itself, not just where it was saved.
            if spec.name == "screenshot"
                && let Some(bytes) = result["path"].as_str().and_then(|p| std::fs::read(p).ok())
            {
                return json!({ "content": [
                        { "type": "image", "data": base64(&bytes), "mimeType": "image/png" },
                        { "type": "text", "text": result.to_string() },
                    ] });
            }
            json!({ "content": [{ "type": "text", "text": result.to_string() }] })
        }
        Ok(answer) => {
            let message = answer["error"].as_str().unwrap_or("the command failed").to_string();
            json!({ "isError": true, "content": [{ "type": "text", "text": message }] })
        }
        Err(e) => {
            json!({ "isError": true, "content": [{ "type": "text", "text": format!("Is the game running? {e}") }] })
        }
    }
}

/// Serves until stdin closes.
pub fn serve(endpoint: &Path) -> Result<(), String> {
    let mut connection: Option<ControlClient> = None;
    let endpoint = endpoint.to_path_buf();
    let mut call = |name: &str, args: &Value| -> Result<Value, String> {
        if connection.is_none() {
            connection = Some(ControlClient::connect(&endpoint, Duration::from_secs(1))?);
        }
        let client = connection.as_mut().ok_or("no connection")?;
        match client.call(name, args) {
            Ok(v) => Ok(v),
            Err(e) => {
                // The game may have been restarted: try once more from scratch next time.
                connection = None;
                Err(e)
            }
        }
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle(&request, &mut call),
            Err(e) => {
                Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }))
            }
        };
        if let Some(answer) = answer {
            writeln!(stdout, "{answer}").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offline(_: &str, _: &Value) -> Result<Value, String> {
        Err("nothing is running".into())
    }

    #[test]
    fn initialize_says_who_this_is_and_what_it_offers() {
        let a = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}), &mut offline)
            .expect("an answer");
        assert_eq!(a["id"], 1);
        assert_eq!(a["result"]["serverInfo"]["name"], "zenith");
        assert!(a["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn every_command_is_a_tool_with_a_real_schema() {
        let tools = tools_list();
        let list = tools["tools"].as_array().expect("tools");
        assert_eq!(list.len(), COMMANDS.len());
        let goto = list.iter().find(|t| t["name"] == "goto").expect("goto");
        assert_eq!(goto["inputSchema"]["properties"]["x"]["type"], "number");
        assert_eq!(goto["inputSchema"]["required"], json!(["x", "y", "z"]));
        assert_eq!(goto["annotations"]["readOnlyHint"], false);
        let state = list.iter().find(|t| t["name"] == "state").expect("state");
        assert_eq!(state["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn notifications_get_no_answer_and_unknown_methods_an_error() {
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &mut offline).is_none());
        let a = handle(&json!({"jsonrpc":"2.0","id":2,"method":"nope"}), &mut offline).expect("an answer");
        assert_eq!(a["error"]["code"], -32601);
    }

    #[test]
    fn a_tool_call_reports_success_failure_and_absence_as_mcp_wants() {
        let mut client = |name: &str, args: &Value| -> Result<Value, String> {
            assert_eq!(name, "block");
            match args["x"].as_i64() {
                Some(1) => Ok(json!({"ok": true, "result": {"name": "stone"}})),
                _ => Ok(json!({"ok": false, "error": "that chunk is not loaded"})),
            }
        };
        let call = |x: i64| json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"block","arguments":{"x":x,"y":0,"z":0}}});
        let good = handle(&call(1), &mut client).expect("an answer");
        assert!(good["result"]["content"][0]["text"].as_str().expect("text").contains("stone"));
        assert!(good["result"].get("isError").is_none());
        let bad = handle(&call(2), &mut client).expect("an answer");
        assert_eq!(bad["result"]["isError"], true);
        let nothing = handle(&call(1), &mut offline).expect("an answer");
        assert!(nothing["result"]["content"][0]["text"].as_str().expect("text").contains("Is the game running"));
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
