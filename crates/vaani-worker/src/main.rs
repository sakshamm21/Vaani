//! `vaani-worker`: the supervised child process that runs local speech models (see the
//! library for details).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(vaani_worker::run(&args));
}
