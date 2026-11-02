//! Vaani's look: soft rose and orchid on warm ink (dark) or blush paper (light), with
//! Unbounded for display type and Plus Jakarta Sans for everything else (both OFL, shipped
//! in assets/fonts and installed per user). Pastel gradients keep it easy on the eyes; the
//! chunky display type and the gradient hero make it stand out.
//!
//! The window always uses Vaani's own palette on top of GTK's built-in theme, so it looks the
//! same on every distro. Light or dark follows the desktop unless picked in Settings.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk4::gdk;
use vaani_core::config::Theme;

type Rgb = (f64, f64, f64);

/// Colours the pill draws with cairo. Pastel on dark, deeper on light for contrast.
pub struct PillColors {
    pub bars: [Rgb; 3],
    pub dot: Rgb,
    pub done: Rgb,
}

const DARK_PILL: PillColors = PillColors {
    bars: [(1.0, 0.70, 0.78), (0.96, 0.56, 0.72), (0.77, 0.60, 0.91)],
    dot: (0.96, 0.50, 0.69),
    done: (0.58, 0.89, 0.78),
};
const LIGHT_PILL: PillColors = PillColors {
    bars: [(0.94, 0.48, 0.66), (0.84, 0.34, 0.56), (0.60, 0.44, 0.84)],
    dot: (0.84, 0.34, 0.56),
    done: (0.14, 0.57, 0.44),
};

static DARK: AtomicBool = AtomicBool::new(true);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn pill_colors() -> &'static PillColors {
    if is_dark() { &DARK_PILL } else { &LIGHT_PILL }
}

const DARK_VARS: &str = "
@define-color v_bg #17121a;
@define-color v_side #1c1620;
@define-color v_surface #231b28;
@define-color v_surface2 #2d2333;
@define-color v_border #3b2e42;
@define-color v_border2 #4d3c55;
@define-color v_hover #362a3d;
@define-color v_active #413248;
@define-color v_keyshadow #100c12;
@define-color v_fg #fcf1f6;
@define-color v_muted #c4abbb;
@define-color v_subtle #8d7488;
@define-color v_accent #f59bc2;
@define-color v_accent_strong #f5a9c9;
@define-color v_lilac #c9a6ee;
@define-color v_g1 #ffb8cb;
@define-color v_g2 #f494bc;
@define-color v_g3 #c7a0ec;
@define-color v_on_grad #2a1320;
@define-color v_ok #93e4c8;
@define-color v_warn #f6d58e;
@define-color v_danger #ff7a90;
@define-color v_pill_bg rgba(28, 22, 32, 0.96);
@define-color v_pill_border rgba(245, 155, 194, 0.40);
";

const LIGHT_VARS: &str = "
@define-color v_bg #fff7fa;
@define-color v_side #fcecf2;
@define-color v_surface #ffffff;
@define-color v_surface2 #fbe7ef;
@define-color v_border #f1d4e1;
@define-color v_border2 #e5bccf;
@define-color v_hover #f8dce8;
@define-color v_active #f3cddd;
@define-color v_keyshadow #e5bccf;
@define-color v_fg #2b1621;
@define-color v_muted #7f5d6e;
@define-color v_subtle #b0909f;
@define-color v_accent #d6578f;
@define-color v_accent_strong #c2447d;
@define-color v_lilac #8f62cf;
@define-color v_g1 #ffc4d5;
@define-color v_g2 #f6a3c5;
@define-color v_g3 #d2b4f2;
@define-color v_on_grad #2a1320;
@define-color v_ok #23926f;
@define-color v_warn #a87612;
@define-color v_danger #d93a5a;
@define-color v_pill_bg rgba(255, 247, 250, 0.97);
@define-color v_pill_border rgba(214, 87, 143, 0.35);
";

const CSS: &str = r#"
/* ---------- floating pill ---------- */
window.vaani-overlay { background: transparent; }
.vaani-pill {
  background-color: @v_pill_bg;
  border: 1px solid @v_pill_border;
  border-radius: 999px;
  padding: 0 16px 0 12px;
  box-shadow: 0 8px 28px alpha(@v_accent, 0.22), 0 4px 14px rgba(0, 0, 0, 0.25);
}
.vaani-pill label {
  font-family: "Plus Jakarta Sans", "Inter", "Noto Sans", sans-serif;
  color: @v_fg; font-weight: 700; font-size: 13px; font-feature-settings: "tnum";
}
.vaani-pill label.dim { color: @v_accent; font-weight: 600; font-size: 12px; }

/* ---------- settings window ---------- */
window.vaani-settings, window.vaani-settings * {
  font-family: "Plus Jakarta Sans", "Inter", "Noto Sans", "Ubuntu", sans-serif;
}
window.vaani-settings {
  background-color: @v_bg;
  color: @v_fg;
}
window.vaani-settings headerbar {
  background-color: @v_side;
  background-image: none;
  border-bottom: 1px solid @v_border;
  box-shadow: none;
  color: @v_fg;
  min-height: 42px;
}
window.vaani-settings headerbar .title {
  font-family: "Unbounded", sans-serif; font-weight: 700; font-size: 13px; letter-spacing: 1px;
}
window.vaani-settings headerbar button {
  color: @v_muted; background: none; border: none; min-height: 18px; min-width: 18px; padding: 4px;
}
window.vaani-settings headerbar button:hover { color: @v_fg; background-color: @v_hover; }

/* sidebar */
window.vaani-settings .sidebar, window.vaani-settings .sidebar list {
  background-color: @v_side;
  border: none;
}
window.vaani-settings .sidebar { padding: 14px 10px; }
window.vaani-settings .sidebar row {
  border-radius: 12px; padding: 10px 14px; margin: 2px 0;
  color: @v_muted; font-weight: 600; font-size: 14px;
  transition: all 150ms ease;
}
window.vaani-settings .sidebar row:hover { background-color: @v_surface; color: @v_fg; }
window.vaani-settings .sidebar row:selected {
  background-image: linear-gradient(90deg, alpha(@v_accent, 0.22), alpha(@v_g3, 0.08));
  color: @v_fg;
  box-shadow: inset 3px 0 @v_accent;
}
window.vaani-settings .sidebar row label { color: inherit; }
window.vaani-settings separator { background-color: @v_border; min-width: 1px; min-height: 1px; }
window.vaani-settings stack.pages, window.vaani-settings scrolledwindow, window.vaani-settings viewport {
  background-color: @v_bg;
}

/* page */
window.vaani-settings .page { padding: 28px 36px 40px 36px; }
window.vaani-settings .page-title {
  font-family: "Unbounded", sans-serif; font-size: 26px; font-weight: 800;
  letter-spacing: -0.5px; margin-bottom: 18px; color: @v_fg;
}
window.vaani-settings .page-sub { color: @v_muted; font-size: 14px; margin-bottom: 18px; }
window.vaani-settings .group-title {
  font-family: "Unbounded", sans-serif;
  font-weight: 700; font-size: 11px; letter-spacing: 1.5px;
  color: @v_accent_strong; margin: 26px 0 10px 4px;
}

/* cards and rows */
window.vaani-settings .card {
  border-radius: 18px;
  border: 1px solid @v_border;
  background-color: @v_surface;
}
window.vaani-settings list.card { background-color: @v_surface; }
window.vaani-settings list.card > row { background: transparent; }
window.vaani-settings .card .row, window.vaani-settings .row { padding: 14px 18px; min-height: 40px; }
window.vaani-settings .row-title { font-weight: 700; font-size: 14px; color: @v_fg; }
window.vaani-settings .row-sub { color: @v_muted; font-size: 12.5px; }

/* hero */
window.vaani-settings .hero {
  border-radius: 24px; padding: 26px 28px;
  background-image: linear-gradient(120deg, @v_g1, @v_g2 50%, @v_g3);
  box-shadow: 0 18px 40px -20px alpha(@v_accent, 0.7);
}
window.vaani-settings .hero label { color: @v_on_grad; }
window.vaani-settings .hero-title {
  font-family: "Unbounded", sans-serif; font-size: 24px; font-weight: 800; letter-spacing: -0.5px;
}
window.vaani-settings .hero .hero-sub { font-size: 14.5px; font-weight: 500; color: alpha(@v_on_grad, 0.85); }
window.vaani-settings .hero .badge {
  background-color: alpha(@v_on_grad, 0.12); color: @v_on_grad;
}
window.vaani-settings .hero button.suggested-action {
  background-image: none; background-color: @v_on_grad; color: #fff4f8;
  box-shadow: 0 6px 18px -6px rgba(0,0,0,0.6);
}
window.vaani-settings .hero button.suggested-action:hover { background-color: shade(@v_on_grad, 1.4); }
window.vaani-settings .hero progressbar trough { background-color: alpha(@v_on_grad, 0.15); }
window.vaani-settings .hero progressbar progress { background-image: none; background-color: @v_on_grad; }
window.vaani-settings .hero progressbar text { color: @v_on_grad; font-weight: 700; }

/* stat tiles */
window.vaani-settings .stat {
  border-radius: 18px; padding: 16px 18px; border: 1px solid @v_border; background-color: @v_surface;
}
window.vaani-settings .stat-num { font-family: "Unbounded", sans-serif; font-size: 26px; font-weight: 800; }
window.vaani-settings .stat-num.c1 { color: @v_accent; }
window.vaani-settings .stat-num.c2 { color: @v_lilac; }
window.vaani-settings .stat-num.c3 { color: @v_ok; }
window.vaani-settings .stat-label { color: @v_muted; font-size: 12px; font-weight: 600; }

/* badges, keycaps */
window.vaani-settings .badge {
  border-radius: 999px; padding: 3px 12px; font-size: 11.5px; font-weight: 800;
  background-color: alpha(@v_ok, 0.16); color: @v_ok;
}
window.vaani-settings .badge.accent { background-color: alpha(@v_warn, 0.18); color: @v_warn; }
window.vaani-settings .badge.star { background-color: alpha(@v_accent, 0.16); color: @v_accent; }
window.vaani-settings .keycap {
  font-family: "Unbounded", sans-serif; font-weight: 700; font-size: 13px;
  padding: 6px 14px; border-radius: 10px; color: @v_fg;
  background-color: @v_surface2;
  border: 1px solid @v_border; border-bottom: 3px solid @v_keyshadow;
}

/* buttons */
window.vaani-settings button {
  border-radius: 12px; padding: 7px 14px; min-height: 22px;
  background-image: none; background-color: @v_surface2; color: @v_fg;
  border: 1px solid @v_border; box-shadow: none; font-weight: 700; text-shadow: none;
  transition: all 150ms ease;
}
window.vaani-settings button:hover { background-color: @v_hover; border-color: @v_border2; }
window.vaani-settings button:active { background-color: @v_active; }
window.vaani-settings button:disabled { color: @v_subtle; }
window.vaani-settings button.suggested-action {
  background-image: linear-gradient(120deg, @v_g1, @v_g2 55%, @v_g3);
  color: @v_on_grad; border: none;
}
window.vaani-settings button.suggested-action:hover {
  box-shadow: 0 6px 18px -6px alpha(@v_accent, 0.9);
}
window.vaani-settings button.destructive-action { background-color: @v_danger; color: #fff4f8; border: none; }
window.vaani-settings button.pill { border-radius: 999px; padding: 10px 22px; font-size: 14px; }
window.vaani-settings button.image-button { padding: 7px; }
window.vaani-settings button.link, window.vaani-settings button.link label {
  background: none; border: none; color: @v_accent; font-weight: 700;
}
window.vaani-settings button.link:hover label { color: @v_accent_strong; }

/* dropdowns */
window.vaani-settings dropdown > button { min-width: 120px; }
window.vaani-settings popover > contents {
  background-color: @v_surface2; color: @v_fg; border-radius: 14px;
  border: 1px solid @v_border; box-shadow: 0 16px 40px rgba(0,0,0,0.3); padding: 6px;
}
window.vaani-settings popover listview, window.vaani-settings popover list { background: transparent; }
window.vaani-settings popover row { border-radius: 9px; padding: 6px 10px; color: @v_fg; }
window.vaani-settings popover row:hover, window.vaani-settings popover row:selected {
  background-color: alpha(@v_accent, 0.18);
}
window.vaani-settings popover arrow { background-color: @v_surface2; }

/* entries */
window.vaani-settings entry, window.vaani-settings passwordentry, window.vaani-settings searchentry {
  border-radius: 12px; min-height: 36px; padding: 0 12px;
  background-color: @v_bg; color: @v_fg; border: 1px solid @v_border; box-shadow: none;
  caret-color: @v_accent;
}
window.vaani-settings entry:focus-within, window.vaani-settings passwordentry:focus-within,
window.vaani-settings searchentry:focus-within {
  border-color: @v_accent; box-shadow: 0 0 0 3px alpha(@v_accent, 0.25);
}
window.vaani-settings entry text, window.vaani-settings passwordentry text,
window.vaani-settings searchentry text { color: @v_fg; }
window.vaani-settings entry image, window.vaani-settings searchentry image,
window.vaani-settings passwordentry image { color: @v_muted; }

/* text view (try it here) */
window.vaani-settings textview, window.vaani-settings textview text {
  background-color: transparent; color: @v_fg; font-size: 15px;
}
window.vaani-settings .tryit {
  border-radius: 18px; border: 1px dashed @v_border2; background-color: @v_surface;
}
window.vaani-settings .tryit:focus-within { border: 1px solid @v_accent; box-shadow: 0 0 0 3px alpha(@v_accent, 0.2); }

/* switches */
window.vaani-settings switch {
  border-radius: 999px; background-color: @v_surface2; border: 1px solid @v_border;
  min-width: 44px; min-height: 24px; padding: 2px;
}
window.vaani-settings switch:checked {
  background-image: linear-gradient(90deg, @v_g2, @v_g3); border-color: transparent;
}
window.vaani-settings switch slider, window.vaani-settings switch:checked slider {
  border-radius: 999px; background-image: none; background-color: #ffffff;
  min-width: 20px; min-height: 20px; margin: 0;
  border: none; outline: none; box-shadow: 0 2px 4px rgba(0,0,0,0.4);
}
window.vaani-settings switch image { color: transparent; -gtk-icon-size: 0; }

/* radios */
window.vaani-settings checkbutton { color: @v_fg; font-weight: 600; }
window.vaani-settings checkbutton radio {
  background-color: @v_bg; border: 2px solid @v_border2; min-width: 16px; min-height: 16px;
  background-image: none; box-shadow: none;
}
window.vaani-settings checkbutton radio:checked {
  border-color: @v_accent; background-color: @v_accent; color: @v_bg;
  -gtk-icon-source: none;
  box-shadow: inset 0 0 0 3px @v_bg;
}

/* progress bars */
window.vaani-settings progressbar trough {
  background-color: @v_surface2; border-radius: 999px; min-height: 8px; border: none;
}
window.vaani-settings progressbar progress {
  background-image: linear-gradient(90deg, @v_g1, @v_g2, @v_g3);
  border-radius: 999px; min-height: 8px; border: none;
}
window.vaani-settings progressbar text { color: @v_muted; font-size: 11px; font-weight: 700; }

/* scrollbars */
window.vaani-settings scrollbar { background: transparent; border: none; }
window.vaani-settings scrollbar slider {
  background-color: @v_border2; border-radius: 999px; min-width: 6px; border: none;
}

/* history */
window.vaani-settings .history-text { font-size: 14px; color: @v_fg; }
window.vaani-settings .history-meta { color: @v_subtle; font-size: 11.5px; font-weight: 600; }
window.vaani-settings .about-logo {
  font-family: "Unbounded", sans-serif; font-size: 44px; font-weight: 900; letter-spacing: -1.5px;
  color: @v_accent;
}
window.vaani-settings selection, window.vaani-settings text selection {
  background-color: alpha(@v_accent, 0.35); color: @v_fg;
}
"#;

const UNBOUNDED: &[u8] = include_bytes!("../../../assets/fonts/Unbounded.ttf");
const JAKARTA: &[u8] = include_bytes!("../../../assets/fonts/PlusJakartaSans.ttf");

fn font_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/fonts/vaani"))
}

/// Put our fonts where fontconfig finds them. Returns true if they were newly written
/// (fontconfig picks them up on its next rescan, at the latest on the next start).
pub fn ensure_fonts() -> bool {
    let Some(dir) = font_dir() else { return false };
    let mut wrote = false;
    for (name, bytes) in [
        ("Unbounded.ttf", UNBOUNDED),
        ("PlusJakartaSans.ttf", JAKARTA),
    ] {
        let p = dir.join(name);
        if std::fs::metadata(&p)
            .map(|m| m.len() == bytes.len() as u64)
            .unwrap_or(false)
        {
            continue;
        }
        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&p, bytes).is_ok() {
            wrote = true;
        }
    }
    if wrote {
        let _ = std::process::Command::new("fc-cache")
            .arg("-f")
            .arg(&dir)
            .status();
    }
    wrote
}

thread_local! {
    static PROVIDER: RefCell<Option<gtk4::CssProvider>> = const { RefCell::new(None) };
    static APPLIED: Cell<Option<Theme>> = const { Cell::new(None) };
}

/// Whether the desktop itself is dark, judged once from its GTK theme name
/// (Mint-Y-Dark, Adwaita-dark, ...) or the GTK_THEME override.
fn desktop_is_dark() -> bool {
    static D: OnceLock<bool> = OnceLock::new();
    *D.get_or_init(|| {
        let env = std::env::var("GTK_THEME").unwrap_or_default();
        let name = gtk4::Settings::default()
            .and_then(|s| s.gtk_theme_name())
            .map(|n| n.to_string())
            .unwrap_or_default();
        let prefer = gtk4::Settings::default()
            .map(|s| s.is_gtk_application_prefer_dark_theme())
            .unwrap_or(false);
        prefer || format!("{env} {name}").to_lowercase().contains("dark")
    })
}

/// Load the stylesheet for `theme`. Cheap to call; does nothing if already applied.
pub fn apply(theme: Theme) {
    if APPLIED.with(|a| a.get()) == Some(theme) {
        return;
    }
    let dark = match theme {
        Theme::System => desktop_is_dark(),
        Theme::Light => false,
        Theme::Dark => true,
    };
    DARK.store(dark, Ordering::Relaxed);
    if let Some(s) = gtk4::Settings::default() {
        // Same base on every distro: GTK's own theme, painted with our palette.
        s.set_gtk_theme_name(Some("Adwaita"));
        s.set_gtk_application_prefer_dark_theme(dark);
    }
    let css = format!("{}{}", if dark { DARK_VARS } else { LIGHT_VARS }, CSS);
    PROVIDER.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            let provider = gtk4::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk4::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk4::STYLE_PROVIDER_PRIORITY_USER,
                );
            }
            *p = Some(provider);
        }
        if let Some(provider) = p.as_ref() {
            provider.load_from_data(&css);
        }
    });
    APPLIED.with(|a| a.set(Some(theme)));
}
