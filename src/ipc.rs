//! The thin-client ↔ daemon protocol.
//!
//! Messages are newline-delimited JSON over a per-workspace unix socket. The client
//! (`lsq diagnostics ...`) connects, sends one [`Request`], reads one [`Response`], exits.
//! If no daemon is listening, the client spawns one and waits for it to come up.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Diagnostics for an absolute file path.
    Diagnostics { file: String },
    /// Resolve a selector (FILE:LINE:COL, FILE:LINE, FILE@NAME) to an exact position.
    Resolve { selector: String },
    /// Rename the symbol at a selector across the workspace.
    Rename { selector: String, new_name: String, apply: bool },
    /// Liveness probe.
    Ping,
    /// Ask the daemon to shut down.
    Shutdown,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { result: Value },
    Err { message: String },
}

/// Per-workspace socket path: `lsq-<hash(root)>.sock` in the temp dir. One daemon per root.
pub fn socket_path(root: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("lsq-{}.sock", root_hash(root)))
}

/// Companion log file the spawned daemon writes its stderr to (for debugging).
pub fn log_path(root: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("lsq-{}.log", root_hash(root)))
}

fn root_hash(root: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Send a request to the workspace daemon, spawning it on demand. Returns the result
/// payload, or the daemon's error.
pub fn query(root: &Path, req: &Request) -> Result<Value> {
    let path = socket_path(root);
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            spawn_daemon(root)?;
            connect_with_retry(&path, Duration::from_secs(20))?
        }
    };

    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    write_msg(&mut writer, req)?;
    let resp: Response = read_msg(&mut reader)?.ok_or_else(|| anyhow!("daemon closed connection"))?;
    match resp {
        Response::Ok { result } => Ok(result),
        Response::Err { message } => Err(anyhow!(message)),
    }
}

fn spawn_daemon(root: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(root))?;
    std::process::Command::new(exe)
        .arg("__daemon")
        .current_dir(root) // daemon derives its socket path from its cwd → must match ours
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .context("spawning daemon")?;
    Ok(())
}

fn connect_with_retry(path: &Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e).context("daemon did not come up in time"),
        }
    }
}

/// Write one newline-delimited JSON message.
pub fn write_msg<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()?;
    Ok(())
}

/// Read one newline-delimited JSON message. `Ok(None)` on clean EOF.
pub fn read_msg<R: BufRead, T: for<'de> Deserialize<'de>>(r: &mut R) -> Result<Option<T>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(line.trim_end())?))
}
