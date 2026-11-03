//! Manual check: `cargo run -p vaani-inject --example type_text -- "text"` types into the focused window.
fn main() {
    let text = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "héllo wörld — plutôt ✓".into());
    let inj = vaani_inject::Injector::start().expect("X11");
    std::thread::sleep(std::time::Duration::from_millis(300));
    println!("{:?}", inj.insert_with(&text, vaani_inject::Mode::Type));
}
