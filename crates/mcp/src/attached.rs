//! A running FigCraft app, reached through its loopback control port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use figcraft_agent_tools::Backend;
use serde_json::{Value, json};

/// The longest reply line we read from the app (512 MiB). The app's window captures alone reach
/// 256 MiB as base64 (192 MiB PNGs); anything longer is refused without reading the rest.
const MAX_REPLY: usize = 512 * 1024 * 1024;

/// A running FigCraft app, reached through its loopback control port (`figcraft --control PORT`).
pub struct Attached {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
    /// [`MAX_REPLY`]; smaller in tests.
    max_reply: usize,
    /// How long the app is asked to wait for each request; `None`: its own 60 s.
    timeout: Option<Duration>,
}

impl Attached {
    /// Connect to `addr` (e.g. `127.0.0.1:7979`), failing fast when nothing is listening.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let mut r = Self { addr: addr.to_string(), conn: None, next_id: 1, max_reply: MAX_REPLY, timeout: None };
        r.reconnect()?;
        Ok(r)
    }

    /// Ask the app to wait up to `timeout` for each request instead of its 60 s (it caps this; an
    /// app from before this ignores it and answers within its 60 s), and read its answers as long.
    pub fn with_timeout(mut self, timeout: Duration) -> std::io::Result<Self> {
        self.timeout = Some(timeout);
        self.reconnect()?;
        Ok(self)
    }

    /// Connect to `127.0.0.1:port`.
    pub fn port(port: u16) -> std::io::Result<Self> {
        Self::connect(&format!("127.0.0.1:{port}"))
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn reconnect(&mut self) -> std::io::Result<()> {
        self.conn = None;
        let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, format!("cannot resolve {}", self.addr));
        for sa in self.addr.to_socket_addrs()? {
            match TcpStream::connect_timeout(&sa, Duration::from_millis(800)) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    // The app answers within its timeout; leave headroom.
                    s.set_read_timeout(Some(self.timeout.unwrap_or(Duration::from_secs(60)).saturating_add(Duration::from_secs(30)))).ok();
                    let read = s.try_clone()?;
                    self.conn = Some((BufReader::new(read), s));
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// The app already closed this connection (it drops ones idle past its socket timeout), or
    /// left bytes on it nobody asked for. Checked before a request is written, when replacing the
    /// connection can't repeat anything.
    fn stale(&mut self) -> bool {
        let Some((reader, writer)) = self.conn.as_mut() else { return false };
        if !reader.buffer().is_empty() || writer.set_nonblocking(true).is_err() {
            return true;
        }
        // Nothing to read on a healthy idle connection: the peek would block.
        let idle = matches!(writer.peek(&mut [0]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
        writer.set_nonblocking(false).is_err() || !idle
    }

    fn roundtrip(&mut self, line: &str) -> std::io::Result<String> {
        let max_reply = self.max_reply;
        let Some((reader, writer)) = self.conn.as_mut() else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "not connected to the app"));
        };
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let mut reply = Vec::new();
        if reader.take((max_reply + 1) as u64).read_until(b'\n', &mut reply)? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "control channel closed"));
        }
        if reply.last() == Some(&b'\n') {
            reply.pop();
        }
        if reply.len() > max_reply {
            let mib = max_reply / (1024 * 1024);
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("reply larger than {mib} MiB")));
        }
        String::from_utf8(reply).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

impl Backend for Attached {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({"id": id, "method": method, "params": params});
        if let (Some(t), Some(fields)) = (self.timeout, line.as_object_mut()) {
            fields.insert("timeoutMs".into(), json!(u64::try_from(t.as_millis()).unwrap_or(u64::MAX)));
        }
        let line = line.to_string();
        if self.conn.is_none() || self.stale() {
            self.reconnect().map_err(|e| {
                format!("FigCraft app at {} is not reachable: {e} (request was not sent; start it with `figcraft --control PORT`)", self.addr)
            })?;
        }
        // A lost reply does not mean the app failed to apply the request.
        let reply = self.roundtrip(&line).map_err(|e| {
            self.conn = None;
            format!(
                "FigCraft control call `{method}` at {} failed: {e}; the operation may have completed. Inspect the document before retrying.",
                self.addr
            )
        })?;
        let v: Value = serde_json::from_str(reply.trim()).map_err(|e| format!("bad reply from the app: {e}"))?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(v.get("error").and_then(Value::as_str).unwrap_or("unknown error").to_string())
        }
    }

    fn has_ui(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("attached to the FigCraft app at {}", self.addr)
    }
}

#[cfg(test)]
#[path = "attached_tests.rs"]
mod tests;
