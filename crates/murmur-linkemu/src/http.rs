//! A tiny HTTP/1.1 endpoint: `/stats` (summary plus per-link), `/stats/<uid>`, `/health`.
//! Hand-rolled so the crate needs no web framework; it only ever answers GET.

use crate::geo::Terrain;
use crate::stats::{LinkStats, summary_json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::Instant;

pub async fn serve(
    addr: &str,
    links: Arc<Vec<Arc<LinkStats>>>,
    start: Instant,
    terrain: Option<Arc<dyn Terrain>>,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("http bind {addr}: {e}"))?;
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                continue;
            };
            let links = links.clone();
            let terrain = terrain.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = route(path, &links, start, terrain.as_deref());
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    Ok(())
}

fn route(
    path: &str,
    links: &[Arc<LinkStats>],
    start: Instant,
    terrain: Option<&dyn Terrain>,
) -> (&'static str, String) {
    let path = path.split('?').next().unwrap_or(path);
    let up = start.elapsed().as_secs_f64();
    match path {
        "/health" => ("200 OK", "{\"ok\":true}".into()),
        "/stats" | "/stats/summary" => {
            let mut v = summary_json(links, up, path == "/stats");
            if let Some(t) = terrain.and_then(Terrain::stats) {
                v["terrain"] = t;
            }
            ("200 OK", v.to_string())
        }
        p => match p
            .strip_prefix("/stats/")
            .and_then(|u| u.parse::<u32>().ok())
        {
            Some(uid) => match links.iter().find(|l| l.meta.uid == uid) {
                Some(l) => ("200 OK", l.json().to_string()),
                None => ("404 Not Found", "{\"error\":\"no such uid\"}".into()),
            },
            None => ("404 Not Found", "{\"error\":\"not found\"}".into()),
        },
    }
}
