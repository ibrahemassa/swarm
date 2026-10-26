//! Live preview: a tiny local web server for the project folder. Every page it serves polls a
//! version counter and reloads itself (keeping its scroll position) when any file changes.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const RELOAD: &str = r#"<script>(()=>{const k='swarm-scroll:'+location.pathname;const y=sessionStorage.getItem(k);if(y!==null){addEventListener('load',()=>scrollTo(0,+y));sessionStorage.removeItem(k)}let v=null;setInterval(async()=>{try{const n=await (await fetch('/__swarm/v',{cache:'no-store'})).text();if(v!==null&&n!==v){sessionStorage.setItem(k,scrollY);location.reload()}v=n}catch(e){}},700)})();</script>"#;

/// Binds the first free port in 7700..7800 on localhost and serves `dir` from it on plain threads.
pub async fn start(dir: PathBuf, version: Arc<AtomicU64>) -> Option<String> {
    for port in 7700..7800u16 {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            std::thread::spawn(move || serve(l, dir, version));
            return Some(format!("http://127.0.0.1:{port}/"));
        }
    }
    None
}

fn serve(l: TcpListener, dir: PathBuf, version: Arc<AtomicU64>) {
    for s in l.incoming().flatten() {
        let (dir, version) = (dir.clone(), version.clone());
        std::thread::spawn(move || {
            let _ = handle(s, &dir, &version);
        });
    }
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn handle(mut s: TcpStream, dir: &Path, version: &AtomicU64) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = vec![0u8; 8192];
    let mut n = 0;
    while n < buf.len() {
        let k = s.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = decode(target.split(['?', '#']).next().unwrap_or("/"));

    let (status, ctype, body): (&str, &str, Vec<u8>) = if method != "GET" && method != "HEAD" {
        ("405 Method Not Allowed", "text/plain", b"GET only".to_vec())
    } else if path == "/__swarm/v" {
        ("200 OK", "text/plain", version.load(Ordering::Relaxed).to_string().into_bytes())
    } else {
        let rel = path.trim_start_matches('/');
        let rel = if rel.is_empty() || rel.ends_with('/') { format!("{rel}index.html") } else { rel.to_string() };
        let rel_path = Path::new(&rel);
        // only plain relative paths inside the project, never dot-folders like .swarm
        let safe = rel_path.components().all(|c| matches!(c, Component::Normal(p) if !p.to_string_lossy().starts_with('.')));
        let file = dir.join(rel_path);
        match (safe, std::fs::read(&file)) {
            (true, Ok(mut bytes)) => {
                let m = mime(&file);
                if m.starts_with("text/html") {
                    let html = String::from_utf8_lossy(&bytes).to_string();
                    let html = match html.rfind("</body>") {
                        Some(i) => format!("{}{RELOAD}{}", &html[..i], &html[i..]),
                        None => format!("{html}{RELOAD}"),
                    };
                    bytes = html.into_bytes();
                }
                ("200 OK", m, bytes)
            }
            _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
        }
    };
    let hdr = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(hdr.as_bytes())?;
    if method != "HEAD" {
        s.write_all(&body)?;
    }
    s.flush()
}
