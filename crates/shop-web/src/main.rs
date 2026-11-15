//! `shop.web`: serves `web/` (the page compiled against `sdk/ts/shop_gateway.ts`) and forwards `POST /rpc/<Op>` to
//! the gateway's HTTP listener, so the browser talks to one origin. It holds no rights: the page can only do what
//! the gateway's public ops allow.
use shop_wire::http::{Request, Response};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn ctype(p: &str) -> &'static str {
    match p.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn forward(gw: &str, r: &Request) -> Response {
    let res = async {
        let mut s = tokio::net::TcpStream::connect(gw).await?;
        let mut head = format!("POST {} HTTP/1.1\r\nhost: {gw}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n", r.path, r.body.len());
        for h in ["x-shop-schema", "x-shop-session"] {
            if let Some(v) = r.headers.get(h) {
                head.push_str(&format!("{h}: {v}\r\n"));
            }
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).await?;
        s.write_all(&r.body).await?;
        let mut b = Vec::new();
        s.read_to_end(&mut b).await?;
        Ok::<_, std::io::Error>(b)
    }
    .await;
    let Ok(b) = res else { return Response::new(502, "text/plain", "gateway unreachable") };
    let split = b.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(b.len());
    let head = String::from_utf8_lossy(&b[..split]);
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(502);
    let body = b.get(split + 4..).unwrap_or_default().to_vec();
    Response::new(status, if status == 200 { "application/json" } else { "text/plain" }, body)
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.web").await;
    let root = Arc::new(PathBuf::from(cell.args.get("root", "web")));
    let gw = Arc::new(cell.dir.one("shop.gateway.http"));
    let addr = cell.dir.one("shop.web.http");
    eprintln!("{} on http://{addr} serving {}", cell.instance(), root.display());
    shop_wire::http::serve(shop_cell::listen(&addr).await, move |r: Request| {
        let (root, gw) = (root.clone(), gw.clone());
        async move {
            if r.path.starts_with("/rpc/") {
                return forward(&gw, &r).await;
            }
            if r.path == "/config.json" {
                return Response::json(200, &serde_json::json!({ "mode": "live", "gateway": "" })).header("cache-control", "no-store");
            }
            let p = r.path.split('?').next().unwrap_or("/");
            let p = if p == "/" { "/index.html" } else { p };
            if p.contains("..") {
                return Response::new(400, "text/plain", "bad path");
            }
            match tokio::fs::read(root.join(&p[1..])).await {
                Ok(b) => Response::new(200, ctype(p), b).header("cache-control", "no-cache"),
                Err(_) => Response::new(404, "text/plain", "not found"),
            }
        }
    })
    .await;
}
