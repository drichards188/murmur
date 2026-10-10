//! The driver's channel: the episode spec on the first line of stdin, then one JSON request per
//! line, one JSON answer per line on the reply descriptor the driver passed (`--reply-fd N`,
//! opened through `/proc/self/fd/N`). Stdout and stderr stay the bridge's own (its `println!`
//! lines), which the driver sends to a log.
//!
//! Any request may carry `at` (situation seconds): the clock runs there first, as `step` does,
//! so a step and the request that follows it cost one round trip.
//!
//! Requests (`op`):
//! - `step` `{t}`: run to situation second `t`; answers `{t}`.
//! - `mcp` `{agent, client: [name, version], tool, args}`: answers `{is_error, structured}`, or
//!   `{protocol_error}` (an unknown tool or bad parameters, which an MCP client raises).
//! - `api` `{method, path, body, operator}`: answers `{status, body}`.
//! - `truth` `{association, log_from}`: the truth now, and the contacts' truth log from a line on
//!   (evaluation only).
//! - `fault` `{kind, ...}`: a scripted fault (`link_event` with `event`, or `stop` with `uid`).
//! - `tools`: the MCP tool names. `now`: the clock. `perf`: where the computer time went.
//! - `end`: answers `{audit}` (the log's path), then anchors and closes the log and exits.
//!
//! Every answer has `ok`; a failed request answers `{ok: false, error}` and the episode goes
//! on (the driver decides).

use std::io::{BufRead, Write};
use std::time::Instant;

use serde_json::{Value, json};

use super::Episode;
use super::perf;
use super::spec::Spec;

fn answer(out: &mut impl Write, v: &Value) -> Result<(), String> {
    let mut line = v.to_string();
    line.push('\n');
    out.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// The perf slot of a request (static names: `perf::add` keeps them).
fn slot(req: &Value) -> &'static str {
    match (req["op"].as_str(), req["tool"].as_str()) {
        (Some("mcp"), Some("get_fleet")) => "call.get_fleet",
        (Some("mcp"), Some("get_tracks")) => "call.get_tracks",
        (Some("mcp"), Some("get_inbox")) => "call.get_inbox",
        (Some("mcp"), Some("get_intents")) => "call.get_intents",
        (Some("mcp"), Some("get_changes")) => "call.get_changes",
        (Some("mcp"), _) => "call.mcp_other",
        (Some("api"), _) => "call.api",
        (Some("truth"), _) => "call.truth",
        (Some("step"), _) => "call.step",
        _ => "call.other",
    }
}

fn handle(ep: &mut Episode, req: &Value) -> Result<Value, String> {
    if let Some(at) = req["at"].as_f64() {
        let t0 = Instant::now();
        ep.step_to(at);
        perf::add("call.step", t0.elapsed());
    }
    let t0 = Instant::now();
    let out = match req["op"].as_str() {
        Some("step") => {
            let t = req["t"].as_f64().ok_or("step: t")?;
            Ok(json!({ "t": ep.step_to(t) }))
        }
        Some("mcp") => ep.mcp_call(
            req["agent"].as_str().ok_or("mcp: agent")?,
            (
                req["client"][0].as_str().unwrap_or(""),
                req["client"][1].as_str().unwrap_or(""),
            ),
            req["tool"].as_str().ok_or("mcp: tool")?,
            req["args"].clone(),
        ),
        Some("api") => ep.api_call(
            req["method"].as_str().unwrap_or("GET"),
            req["path"].as_str().ok_or("api: path")?,
            (!req["body"].is_null()).then(|| req["body"].clone()),
            req["operator"].as_str(),
        ),
        Some("truth") => ep.truth(
            req["association"].as_bool().unwrap_or(true),
            req["log_from"].as_u64().map(|i| i as usize),
        ),
        Some("fault") => ep.fault(req),
        Some("tools") => Ok(json!({ "tools": ep.tools() })),
        Some("now") => Ok(json!({ "t": ep.now_s() })),
        Some("perf") => Ok(json!({ "tasks": perf::report() })),
        Some("mcp_http") => ep.mcp_over_http(
            req["agent"].as_str().ok_or("mcp_http: agent")?,
            (
                req["client"][0].as_str().unwrap_or(""),
                req["client"][1].as_str().unwrap_or(""),
            ),
            req["tool"].as_str().ok_or("mcp_http: tool")?,
            req["args"].clone(),
        ),
        other => Err(format!("unknown op {other:?}")),
    };
    perf::add(slot(req), t0.elapsed());
    out
}

/// Serve one episode. Returns only on an error before the episode ended; `end` exits.
pub fn run_stdio(reply_fd: u32) -> Result<(), String> {
    let path = format!("/proc/self/fd/{reply_fd}");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|e| format!("{path}: {e}"))?;
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let first = lines
        .next()
        .ok_or("no episode spec on stdin")?
        .map_err(|e| e.to_string())?;
    let spec: Spec = serde_json::from_str(&first).map_err(|e| format!("spec: {e}"))?;
    let mut ep = match Episode::start(spec) {
        Ok(ep) => ep,
        Err(e) => {
            answer(&mut out, &json!({ "ok": false, "error": e }))?;
            return Err(e);
        }
    };
    answer(&mut out, &json!({ "ok": true, "t": ep.now_s(), "tools": ep.tools() }))?;
    for line in lines {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                answer(&mut out, &json!({ "ok": false, "error": format!("request: {e}") }))?;
                continue;
            }
        };
        if req["op"].as_str() == Some("end") {
            if let Some(at) = req["at"].as_f64() {
                ep.step_to(at);
            }
            answer(
                &mut out,
                &json!({ "ok": true, "audit": ep.audit_path.display().to_string(), "t": ep.now_s() }),
            )?;
            drop(out);
            ep.finish();
        }
        let reply = match handle(&mut ep, &req) {
            Ok(mut v) => {
                v["ok"] = json!(true);
                v
            }
            Err(e) => json!({ "ok": false, "error": e }),
        };
        answer(&mut out, &reply)?;
    }
    Err("the driver closed stdin before `end`".into())
}
