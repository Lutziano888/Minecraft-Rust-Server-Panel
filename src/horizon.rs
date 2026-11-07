//! Sprechen mit horizon-server: über sein CLI und seine Control-API
//! (`.horizon/api.json` im Serverordner: Adresse auf 127.0.0.1 und Token).

use serde_json::Value;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
};

/// `horizon-server --dir <dir> <args…>`; gibt stdout (und bei Fehler stderr) zurück.
pub async fn cli(
    program: &Path,
    dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.arg("--dir")
        .arg(dir)
        .args(args)
        .env("HORIZON_NO_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("horizon-server {} hat zu lange gebraucht", args.join(" ")))?
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if out.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        let msg = [stderr, stdout]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        Err(if msg.is_empty() {
            format!("horizon-server beendet mit {}", out.status)
        } else {
            msg
        })
    }
}

/// Ein Aufruf der Control-API eines laufenden Supervisors.
pub async fn api(
    dir: &Path,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Value, String> {
    let endpoint = tokio::fs::read_to_string(dir.join(".horizon/api.json"))
        .await
        .map_err(|_| "Server läuft nicht (keine API)".to_string())?;
    let endpoint: Value = serde_json::from_str(&endpoint).map_err(|e| e.to_string())?;
    let address = endpoint["address"].as_str().ok_or("api.json ohne Adresse")?;
    let token = endpoint["token"].as_str().ok_or("api.json ohne Token")?;
    tokio::time::timeout(timeout, request(address, token, method, path, body))
        .await
        .map_err(|_| "Zeitüberschreitung bei der Server-API".to_string())?
}

async fn request(
    address: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|_| "Server-API nicht erreichbar".to_string())?;
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\n\
         Accept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n",
        payload.len()
    );
    if body.is_some() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| e.to_string())?;
    let (status, body) = parse_response(&raw)?;
    let value: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        Err(value["error"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {status}: {value}")))
    }
}

/// Statuszeile und Körper einer HTTP/1.1-Antwort (Content-Length oder chunked).
pub fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("kaputte Antwort der Server-API")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut body = raw[split + 4..].to_vec();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or("kaputte Statuszeile")?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        body = dechunk(&body);
    }
    Ok((status, body))
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_text = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or(0);
        data = &data[line_end + 2..];
        if size == 0 || data.len() < size {
            out.extend_from_slice(&data[..size.min(data.len())]);
            break;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[(size + 2).min(data.len())..];
    }
    out
}

/// Die Ausgabe eines Konsolenbefehls als Zeilen, egal wie die Antwort aussieht.
pub fn reply_lines(reply: &Value) -> Vec<String> {
    for key in ["output", "lines", "messages", "reply"] {
        match &reply[key] {
            Value::String(s) => return s.lines().map(str::to_owned).collect(),
            Value::Array(a) => {
                return a
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))
                    .collect()
            }
            _ => {}
        }
    }
    match reply {
        Value::String(s) => s.lines().map(str::to_owned).collect(),
        Value::Object(o) if o.len() == 1 && o.contains_key("ok") => vec![],
        Value::Null => vec![],
        other => vec![other.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
        let (s, b) = parse_response(raw).unwrap();
        assert_eq!(s, 200);
        assert_eq!(b, b"{\"ok\":true}");
    }

    #[test]
    fn parses_chunked() {
        let raw = b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let (s, b) = parse_response(raw).unwrap();
        assert_eq!(s, 404);
        assert_eq!(b, b"{\"a\":1}");
    }

    #[test]
    fn reply_shapes() {
        assert_eq!(reply_lines(&json!({"output": "a\nb"})), vec!["a", "b"]);
        assert_eq!(reply_lines(&json!({"ok": true, "lines": ["x"]})), vec!["x"]);
        assert!(reply_lines(&json!({"ok": true})).is_empty());
    }
}
