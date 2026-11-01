//! The control socket: one JSON request per line in, one JSON reply per line out.
//!
//! It listens on localhost only, on a random port, and a client's first line must be the token from
//! the endpoint file, which only this user can read on a normal setup. The game answers nobody
//! until `--control` asks it to.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// A request from a control client, to be handled on the game's thread.
pub struct Request {
    pub cmd: String,
    pub args: Value,
    pub reply: Sender<Value>,
}

pub struct ControlServer {
    pub requests: Receiver<Request>,
    pub port: u16,
    pub token: String,
}

/// Starts listening, and writes where to `endpoint` as `{"port":N,"token":"..."}`.
pub fn start(endpoint: &Path) -> std::io::Result<ControlServer> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = make_token();
    if let Some(parent) = endpoint.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(endpoint, json!({ "port": port, "token": token }).to_string())?;
    let (tx, requests) = mpsc::channel();
    let expected = token.clone();
    std::thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let (tx, expected) = (tx.clone(), expected.clone());
            let _ = std::thread::Builder::new().name("control-conn".into()).spawn(move || {
                let _ = serve(stream, &tx, &expected);
            });
        }
    })?;
    Ok(ControlServer { requests, port, token })
}

fn make_token() -> String {
    // Nothing here needs to be unguessable to another machine (the socket is local); it only has to
    // keep other local programs that never read the file from driving the game by accident.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64);
    let a = zenith_core::rng::mix(t, std::process::id() as u64);
    let b = zenith_core::rng::mix(a, t);
    format!("{a:016x}{b:016x}")
}

fn serve(stream: TcpStream, tx: &Sender<Request>, token: &str) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim() != format!("auth {token}") {
        writeln!(writer, "{}", json!({"ok": false, "error": "the first line must be: auth TOKEN"}))?;
        return Ok(());
    }
    writeln!(writer, "{}", json!({"ok": true}))?;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(v) => {
                let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or_default().to_string();
                let args = v.get("args").cloned().unwrap_or_else(|| json!({}));
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx.send(Request { cmd, args, reply: reply_tx }).is_err() {
                    return Ok(());
                }
                // A command waits as long as it takes: walking somewhere can take a minute.
                loop {
                    match reply_rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(a) => break a,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break json!({"ok": false, "error": "the game went away"}),
                    }
                }
            }
            Err(e) => json!({"ok": false, "error": format!("that is not JSON: {e}")}),
        };
        writeln!(writer, "{answer}")?;
    }
}

/// A client of the control socket.
pub struct ControlClient {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl ControlClient {
    /// Connects using an endpoint file, retrying for up to `wait` while the game starts.
    pub fn connect(endpoint: &Path, wait: Duration) -> Result<Self, String> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match Self::try_connect(endpoint) {
                Ok(c) => return Ok(c),
                Err(e) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    fn try_connect(endpoint: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(endpoint).map_err(|e| format!("cannot read {}: {e}", endpoint.display()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{} is not an endpoint file: {e}", endpoint.display()))?;
        let port = v["port"].as_u64().ok_or("the endpoint file has no port")? as u16;
        let token = v["token"].as_str().ok_or("the endpoint file has no token")?;
        let writer = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|e| format!("nothing answered on port {port}: {e}"))?;
        let reader = BufReader::new(writer.try_clone().map_err(|e| e.to_string())?);
        let mut c = Self { writer, reader };
        c.write_line(&format!("auth {token}"))?;
        let answer = c.read_line()?;
        if answer["ok"] != json!(true) {
            return Err(format!("refused: {answer}"));
        }
        Ok(c)
    }

    fn write_line(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.writer, "{line}").map_err(|e| e.to_string())
    }

    fn read_line(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("the game closed the connection".into());
        }
        serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))
    }

    /// Runs a command and waits for its reply.
    pub fn call(&mut self, cmd: &str, args: &Value) -> Result<Value, String> {
        self.write_line(&json!({ "cmd": cmd, "args": args }).to_string())?;
        self.read_line()
    }
}
