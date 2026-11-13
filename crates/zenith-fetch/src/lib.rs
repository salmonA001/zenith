//! Getting files from the web, and checking that they are the files that were meant.
//!
//! Requests go through the system's own `curl`, which Windows 10 and later, macOS and nearly every
//! Linux have. It brings the system's certificates with it, so nothing here carries TLS code. Every
//! request goes through the `Http` trait, so that tests can answer in its place.
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// How this program introduces itself to servers, which some (such as Modrinth's) require.
pub const USER_AGENT: &str = concat!("zenith/", env!("CARGO_PKG_VERSION"), " (open-source voxel game and mod tools)");

/// Something that can fetch a URL. `Curl` does it for real; `Canned` answers from a list.
pub trait Http: Send + Sync {
    /// The body of a URL, refused if it is larger than `max_bytes`.
    fn get(&self, url: &str, headers: &[(&str, &str)], max_bytes: u64) -> Result<Vec<u8>, String>;
}

/// The system's `curl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Curl;

impl Http for Curl {
    fn get(&self, url: &str, headers: &[(&str, &str)], max_bytes: u64) -> Result<Vec<u8>, String> {
        check_url(url)?;
        let mut command = Command::new("curl");
        command.args(["--fail", "--silent", "--show-error", "--location", "--max-redirs", "5", "--max-time", "300"]);
        command.args(["--proto", "=https,http", "--proto-redir", "=https", "--user-agent", USER_AGENT]);
        command.arg("--max-filesize").arg(max_bytes.to_string());
        for (name, value) in headers {
            if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
                return Err(format!("a header that cannot be sent: {name}"));
            }
            command.arg("--header").arg(format!("{name}: {value}"));
        }
        let output = command
            .arg(url)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run curl, which downloads need: {e}"))?;
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if why.is_empty() { format!("{url}: the download failed") } else { format!("{url}: {why}") });
        }
        if output.stdout.len() as u64 > max_bytes {
            return Err(format!("{url} is larger than the {max_bytes} bytes allowed"));
        }
        Ok(output.stdout)
    }
}

/// Answers given in advance, for tests: the same bytes for the same URL, an error for any other.
#[derive(Default)]
pub struct Canned {
    answers: Mutex<HashMap<String, Vec<u8>>>,
    asked: Mutex<Vec<String>>,
}

impl Canned {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn answer(&self, url: &str, body: impl Into<Vec<u8>>) {
        if let Ok(mut answers) = self.answers.lock() {
            answers.insert(url.to_string(), body.into());
        }
    }

    /// Every URL asked for so far, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

impl Http for Canned {
    fn get(&self, url: &str, _headers: &[(&str, &str)], max_bytes: u64) -> Result<Vec<u8>, String> {
        check_url(url)?;
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(url.to_string());
        }
        let body = self
            .answers
            .lock()
            .ok()
            .and_then(|a| a.get(url).cloned())
            .ok_or_else(|| format!("{url}: 404 Not Found"))?;
        if body.len() as u64 > max_bytes {
            return Err(format!("{url} is larger than the {max_bytes} bytes allowed"));
        }
        Ok(body)
    }
}

/// Only HTTPS goes out to the world. Plain HTTP is allowed to this machine alone, which tests use.
pub fn check_url(url: &str) -> Result<(), String> {
    if url.len() > 4096 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("that is not a URL that can be fetched".into());
    }
    let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| url.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/'])));
    let secure = url.strip_prefix("https://").is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'));
    if secure || local { Ok(()) } else { Err(format!("{url}: only https:// addresses can be fetched")) }
}

/// A path from a page's address: what the browser would make of a relative link on it.
pub fn resolve(base: &str, link: &str) -> Result<String, String> {
    if link.starts_with("https://") || link.starts_with("http://") {
        check_url(link)?;
        return Ok(link.to_string());
    }
    if link.is_empty()
        || link.starts_with(['/', '\\'])
        || link.contains(':')
        || link.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(format!("{link} is not a path that can be followed from {base}"));
    }
    let folder = base.split(['?', '#']).next().unwrap_or(base);
    let folder = &folder[..folder.rfind('/').map_or(folder.len(), |i| i + 1)];
    let joined = format!("{folder}{link}");
    check_url(&joined)?;
    Ok(joined)
}

/// Text made safe to put in a URL's query.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha1_hex(data: &[u8]) -> String {
    hex(&Sha1::digest(data))
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn sha512_hex(data: &[u8]) -> String {
    hex(&Sha512::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_leaves_this_machine() {
        assert!(check_url("https://example.com/a.json").is_ok());
        assert!(check_url("http://127.0.0.1:8080/a.json").is_ok());
        assert!(check_url("http://localhost/a").is_ok());
        assert!(check_url("http://example.com/a.json").is_err());
        assert!(check_url("http://localhost.evil.com/a").is_err(), "a lookalike of localhost");
        assert!(check_url("file:///etc/passwd").is_err());
        assert!(check_url("https://").is_err());
        assert!(check_url("https:///nohost").is_err());
        assert!(check_url("https://a.com/x y").is_err());
        assert!(check_url("-o/etc/x").is_err(), "nothing curl would take as an option");
    }

    #[test]
    fn links_resolve_as_a_browser_would_and_never_climb_out() {
        let base = "https://example.com/mods/zenith-catalogue.json";
        assert_eq!(resolve(base, "packs/neon.json").as_deref(), Ok("https://example.com/mods/packs/neon.json"));
        assert_eq!(resolve(base, "https://cdn.example.com/x.json").as_deref(), Ok("https://cdn.example.com/x.json"));
        assert!(resolve(base, "../secret.json").is_err());
        assert!(resolve(base, "/root.json").is_err());
        assert!(resolve(base, "file:///etc/passwd").is_err());
        assert!(resolve(base, "http://example.com/plain.json").is_err());
    }

    #[test]
    fn escaping_leaves_only_safe_characters() {
        assert_eq!(escape("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(escape("[\"x\"]"), "%5B%22x%22%5D");
    }

    #[test]
    fn hashes_match_the_published_test_vectors() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(sha512_hex(b"abc").starts_with("ddaf35a193617aba"));
    }

    #[test]
    fn canned_answers_what_it_was_given_and_remembers_what_was_asked() {
        let http = Canned::new();
        http.answer("https://a.com/x", "hello");
        assert_eq!(http.get("https://a.com/x", &[], 100), Ok(b"hello".to_vec()));
        assert!(http.get("https://a.com/x", &[], 2).is_err(), "too large");
        assert!(http.get("https://a.com/y", &[], 100).is_err());
        assert_eq!(http.asked(), ["https://a.com/x", "https://a.com/x", "https://a.com/y"]);
    }
}
