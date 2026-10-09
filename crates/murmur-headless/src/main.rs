//! `murmur-headless --reply-fd N`: one headless episode (docs/HEADLESS.md), driven by
//! eval/headless/sim.py: the episode spec on stdin's first line, then requests on stdin and
//! answers on descriptor N. Evaluation only.

fn main() {
    let mut args = std::env::args().skip(1);
    let mut reply_fd: Option<u32> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--reply-fd" => reply_fd = args.next().and_then(|s| s.parse().ok()),
            other => {
                eprintln!("murmur-headless: unknown argument {other:?} (usage: murmur-headless --reply-fd N)");
                std::process::exit(2);
            }
        }
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
