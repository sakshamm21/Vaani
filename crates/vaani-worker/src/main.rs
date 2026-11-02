//! `vaani-worker`: the supervised child process that will load local models and run VAD and
//! speech-to-text, talking to the main app over a Unix socket.
//!
//! Stub until M1. It only answers `--version` so the supervisor and `vaani doctor` can find it.

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version") | Some("-V") => println!("vaani-worker {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!(
                "vaani-worker is started by the vaani app; it is not meant to be run by hand."
            );
            std::process::exit(2);
        }
    }
}
