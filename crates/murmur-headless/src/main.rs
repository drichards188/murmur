//! `murmur-headless --reply-fd N`: one headless episode (docs/HEADLESS.md), driven by
//! eval/headless/sim.py: the episode spec on stdin's first line, then requests on stdin and
//! answers on descriptor N. Evaluation only.
//!
//! `murmur-headless --sense [--root DIR]`: the physical sensing model's predictions, no
//! episode (`sim/sense.rs`; eval/headless/sensing.py): requests on stdin, answers on stdout,
//! the catalog and grids under DIR (default: the working directory).

fn main() {
    let mut args = std::env::args().skip(1);
    let mut reply_fd: Option<u32> = None;
    let mut sense = false;
    let mut root = std::path::PathBuf::from(".");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--reply-fd" => reply_fd = args.next().and_then(|s| s.parse().ok()),
            "--sense" => sense = true,
            "--root" => root = args.next().map(Into::into).unwrap_or(root),
            other => {
                eprintln!("murmur-headless: unknown argument {other:?} (usage: murmur-headless --reply-fd N | --sense [--root DIR])");
                std::process::exit(2);
            }
        }
    }
    if sense {
        if let Err(e) = murmur_headless::sim::sense::run_stdio(&root) {
            eprintln!("murmur-headless: {e}");
            std::process::exit(1);
        }
        return;
    }
    let Some(fd) = reply_fd else {
        eprintln!("murmur-headless: --reply-fd N is required (the driver's answer pipe)");
        std::process::exit(2);
    };
    if let Err(e) = murmur_headless::sim::server::run_stdio(fd) {
        eprintln!("murmur-headless: {e}");
        std::process::exit(1);
    }
}
