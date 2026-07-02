//! JSON-RPC framing for LSP. The entire wire format is:
//!
//!     Content-Length: <N>\r\n
//!     \r\n
//!     <N bytes of UTF-8 JSON>
//!
//! That's it. Kept tiny and synchronous so it's trivial to unit-test without a live server.

use std::io::{self, BufRead, Write};

/// Write one framed JSON message.
pub fn write_message<W: Write>(w: &mut W, body: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(body).map_err(invalid_data)?;
    write!(w, "Content-Length: {}\r\n\r\n", bytes.len())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read one framed JSON message. Returns `Ok(None)` on a clean EOF.
pub fn read_message<R: BufRead>(r: &mut R) -> io::Result<Option<serde_json::Value>> {
    // --- headers, terminated by a blank line ---
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None); // EOF before any header
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // blank line ends the header block
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                v.trim()
                    .parse()
                    .map_err(|_| invalid_data("bad Content-Length"))?,
            );
        }
        // any other header (e.g. Content-Type) is ignored
    }

    // --- body ---
    let len = content_length.ok_or_else(|| invalid_data("missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let val = serde_json::from_slice(&buf).map_err(invalid_data)?;
    Ok(Some(val))
}

fn invalid_data<E>(e: E) -> io::Error
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    io::Error::new(io::ErrorKind::InvalidData, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn round_trips_a_message() {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        // framing is shaped correctly
        let wire = String::from_utf8(buf.clone()).unwrap();
        assert!(wire.starts_with("Content-Length: "));
        assert!(wire.contains("\r\n\r\n"));

        // and it reads back byte-identical
        let got = read_message(&mut Cursor::new(buf)).unwrap().unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn clean_eof_is_none() {
        let got = read_message(&mut Cursor::new(Vec::<u8>::new())).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn ignores_extra_headers() {
        let wire = b"Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let got = read_message(&mut Cursor::new(&wire[..])).unwrap().unwrap();
        assert_eq!(got, json!({}));
    }
}
