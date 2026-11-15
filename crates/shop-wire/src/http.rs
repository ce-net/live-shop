//! A small HTTP/1.1 listener for the two cells that face a browser (`shop.gateway`, `shop.web`).
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Response {
    pub status: u16,
    pub ctype: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn new(status: u16, ctype: &'static str, body: impl Into<Vec<u8>>) -> Response {
        Response { status, ctype, body: body.into(), headers: vec![] }
    }
    pub fn json(status: u16, v: &serde_json::Value) -> Response {
        Response::new(status, "application/json", serde_json::to_vec(v).unwrap())
    }
    pub fn header(mut self, k: &str, v: &str) -> Response {
        self.headers.push((k.into(), v.into()));
        self
    }
}

fn reason(s: u16) -> &'static str {
    match s {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    }
}

pub async fn serve<F, Fut>(listener: TcpListener, f: F)
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let f = Arc::new(f);
    loop {
        let Ok((s, _)) = listener.accept().await else { continue };
        let f = f.clone();
        tokio::spawn(async move {
            let (r, mut w) = s.into_split();
            let mut r = BufReader::new(r);
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("/").to_string();
                let mut headers = HashMap::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let n: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                if n > 1 << 20 {
                    return;
                }
                let mut body = vec![0u8; n];
                if r.read_exact(&mut body).await.is_err() {
                    return;
                }
                let close = headers.get("connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
                let resp = f(Request { method, path, headers, body }).await;
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\n",
                    resp.status,
                    reason(resp.status),
                    resp.ctype,
                    resp.body.len()
                );
                for (k, v) in &resp.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                if w.write_all(head.as_bytes()).await.is_err() || w.write_all(&resp.body).await.is_err() {
                    return;
                }
                if close {
                    return;
                }
            }
        });
    }
}
