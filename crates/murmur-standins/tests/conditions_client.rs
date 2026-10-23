//! The tidewater client against a mock `POST /v1/sample` server.

use std::sync::{Arc, Mutex};

use murmur_standins::conditions::{Slot, sample_once, surfaces_at};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `/v1/sample` with one sample per point: in area (depth 10 m) west of -122.5, outside
/// it east of there. `status` lets a test make it fail. Records every request body.
async fn mock(status: Arc<Mutex<u16>>, seen: Arc<Mutex<Vec<Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let status = status.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // keep-alive: serve requests until the client closes
                loop {
                    let (head_end, len) = loop {
                        if let Some(i) = find(&buf, b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                            let len = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            break (i + 4, len);
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    while buf.len() < head_end + len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body: Value = serde_json::from_slice(&buf[head_end..head_end + len])
                        .unwrap_or(Value::Null);
                    buf.drain(..head_end + len);
                    seen.lock().expect("lock").push(body.clone());
                    let code = *status.lock().expect("lock");
                    let reply = if code == 200 {
                        let samples: Vec<Value> = body["points"]
                            .as_array()
                            .map(|ps| {
                                ps.iter()
                                    .map(|p| {
                                        let (lat, lon) = (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0));
                                        let inside = lon < -122.5;
                                        json!({
                                            "lat": lat, "lon": lon,
                                            "elevation_m": if inside { json!(-9.0) } else { Value::Null },
                                            "water_level_m": 1.0,
                                            "water_depth_m": if inside { json!(10.0) } else { Value::Null },
                                            "current_east": if inside { 0.5 } else { 0.0 },
                                            "current_north": -0.25,
                                            "wind_east": 3.0, "wind_north": 0.0,
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        json!({"t": body["t"], "samples": samples})
                    } else {
                        json!({"error": "t is outside the scenario's time range"})
                    };
                    let data = reply.to_string();
                    let resp = format!(
                        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{data}",
                        data.len()
                    );
                    if sock.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://{addr}/v1/sample")
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn batches_all_reported_positions_and_keeps_the_last_sample_on_error() {
    let status = Arc::new(Mutex::new(200));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = mock(status.clone(), seen.clone()).await;
    let client = reqwest::Client::new();

    let slots: Vec<Arc<Slot>> = (0..3).map(|_| Arc::new(Slot::default())).collect();
    slots[0].report(37.80, -122.60, 0.0);
    slots[2].report(37.81, -122.40, 40.0); // outside the mock's area
    // slot 1 hasn't reported: not asked for

    sample_once(&client, &url, 1_781_431_200.5, &slots)
        .await
        .expect("first query");
    {
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 1, "one request for all vehicles");
        assert_eq!(seen[0]["t"], json!(1_781_431_200.5));
        assert_eq!(
            seen[0]["points"],
            json!([[37.80, -122.60, 0.0], [37.81, -122.40, 40.0]])
        );
    }
    let a = slots[0].latest().expect("sample for slot 0");
    assert!(a.in_area());
    assert!(a.navigable(2.5));
    assert_eq!(a.current_east(), 0.5);
    assert!(slots[1].latest().is_none());
    let c = slots[2].latest().expect("sample for slot 2");
    assert!(!c.in_area());
    assert!(!c.navigable(0.5));

    // the service refuses (e.g. time outside the scenario): report it, keep what we had
    *status.lock().expect("lock") = 400;
    slots[0].report(37.85, -122.70, 0.0);
    let err = sample_once(&client, &url, 1_781_431_201.5, &slots)
        .await
        .expect_err("400 is an error");
    assert!(err.contains("400"), "{err}");
    assert_eq!(slots[0].latest().map(|s| s.lat), Some(37.80));

    // and picks up again when the service does
    *status.lock().expect("lock") = 200;
    sample_once(&client, &url, 1_781_431_202.5, &slots)
        .await
        .expect("recovered");
    assert_eq!(slots[0].latest().map(|s| s.lat), Some(37.85));
}

/// The surface under each point, in one request: the water over a seabed (the mock's 1 m water
/// level over -9 m), nothing outside the area.
#[tokio::test]
async fn surfaces_are_the_water_or_ground_under_each_point() {
    let status = Arc::new(Mutex::new(200));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = mock(status, seen.clone()).await;
    let client = reqwest::Client::new();
    let s = surfaces_at(
        &client,
        &url,
        1_781_431_200.0,
        &[(37.80, -122.60), (37.81, -122.40)],
    )
    .await
    .expect("surfaces");
    assert_eq!(s, [Some(1.0), None]);
    assert_eq!(seen.lock().expect("lock").len(), 1, "one request");
    let ground = murmur_standins::conditions::Sample {
        elevation_m: Some(3.0),
        water_level_m: Some(0.4),
        ..Default::default()
    };
    assert_eq!(ground.surface_m(), Some(3.0));
}

#[tokio::test]
async fn unreachable_service_is_an_error_not_a_hang() {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .expect("client");
    let slots = vec![Arc::new(Slot::default())];
    slots[0].report(37.8, -122.6, 0.0);
    // port 9 (discard) on localhost: nothing listens there
    let r = sample_once(&client, "http://127.0.0.1:9/v1/sample", 0.0, &slots).await;
    assert!(r.is_err());
    assert!(slots[0].latest().is_none());
}
