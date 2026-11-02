//! Vaani's look: neutral greys with a single rose accent, in light and dark. Inter for all
//! text (OFL, shipped in assets/fonts and installed per user). Flat colours, small radii,
//! hairline borders, one type scale.
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
    bars: [(0.96, 0.61, 0.76), (0.96, 0.61, 0.76), (0.96, 0.61, 0.76)],
    dot: (0.96, 0.50, 0.69),
    done: (0.58, 0.89, 0.78),
};
const LIGHT_PILL: PillColors = PillColors {
    bars: [(0.84, 0.34, 0.56), (0.84, 0.34, 0.56), (0.84, 0.34, 0.56)],
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
@define-color v_bg #1c1c1e;
@define-color v_side #232325;
@define-color v_surface #2a2a2d;
@define-color v_surface2 #333336;
@define-color v_border #38383b;
@define-color v_border2 #48484c;
@define-color v_hover #343437;
@define-color v_active #3d3d41;
@define-color v_keyshadow #161618;
@define-color v_fg #f5f5f7;
@define-color v_muted #a1a1a6;
@define-color v_subtle #6e6e73;
@define-color v_accent #f0789f;
@define-color v_accent_strong #f58fb0;
@define-color v_lilac #c9a6ee;
@define-color v_g1 #f0789f;
@define-color v_g2 #f0789f;
@define-color v_g3 #f0789f;
@define-color v_on_grad #2a1320;
@define-color v_ok #5fd38d;
@define-color v_warn #f2c14e;
@define-color v_danger #ff6b6b;
@define-color v_pill_bg rgba(36, 36, 38, 0.96);
@define-color v_pill_border rgba(255, 255, 255, 0.08);
@define-color v_on_accent #ffffff;
@define-color v_field #1c1c1e;
";

const LIGHT_VARS: &str = "
@define-color v_bg #f5f5f7;
@define-color v_side #ececef;
@define-color v_surface #ffffff;
@define-color v_surface2 #f2f2f4;
@define-color v_border #e2e2e6;
@define-color v_border2 #d1d1d6;
@define-color v_hover #e6e6ea;
@define-color v_active #dcdce0;
@define-color v_keyshadow #d1d1d6;
@define-color v_fg #1d1d1f;
@define-color v_muted #6e6e73;
@define-color v_subtle #98989d;
@define-color v_accent #d6457a;
@define-color v_accent_strong #c2386c;
@define-color v_lilac #8f62cf;
@define-color v_g1 #d6457a;
@define-color v_g2 #d6457a;
@define-color v_g3 #d6457a;
@define-color v_on_grad #ffffff;
@define-color v_ok #1f8a4c;
@define-color v_warn #9a6700;
@define-color v_danger #d70015;
@define-color v_pill_bg rgba(250, 250, 252, 0.97);
@define-color v_pill_border rgba(0, 0, 0, 0.08);
@define-color v_on_accent #ffffff;
@define-color v_field #ffffff;
";

const CSS: &str = r#"
/* Spacing: 4 / 8 / 12 / 16 / 24 / 32. Radii: 6 controls, 10 cards. One accent colour. */

/* ---------- floating pill ---------- */
window.vaani-overlay { background: transparent; }
.vaani-pill {
  background-color: @v_pill_bg;
  border: 1px solid @v_pill_border;
  border-radius: 999px;
  padding: 0 16px 0 12px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.28);
}
.vaani-pill label {
  font-family: "Inter", "Noto Sans", sans-serif;
  color: @v_fg; font-weight: 600; font-size: 13px; font-feature-settings: "tnum";
}
.vaani-pill label.dim { color: @v_muted; font-weight: 500; font-size: 12px; }

/* ---------- settings window ---------- */
window.vaani-settings, window.vaani-settings * {
  font-family: "Inter", "Noto Sans", "Ubuntu", sans-serif;
}
window.vaani-settings { background-color: @v_bg; color: @v_fg; font-size: 14px; }
window.vaani-settings headerbar {
  background-color: @v_side; background-image: none;
  border-bottom: 1px solid @v_border; box-shadow: none; color: @v_fg; min-height: 40px;
}
window.vaani-settings headerbar .title { font-weight: 600; font-size: 13px; }
window.vaani-settings headerbar button {
  color: @v_muted; background: none; border: none; min-height: 18px; min-width: 18px; padding: 4px;
  border-radius: 6px;
}
window.vaani-settings headerbar button:hover { color: @v_fg; background-color: @v_hover; }

/* sidebar */
window.vaani-settings .sidebar, window.vaani-settings .sidebar list {
  background-color: @v_side; border: none;
}
window.vaani-settings .sidebar { padding: 12px 8px; }
window.vaani-settings .sidebar row {
  border-radius: 6px; padding: 7px 12px; margin: 1px 0;
  color: @v_muted; font-weight: 500; font-size: 13.5px;
}
window.vaani-settings .sidebar row:hover { background-color: @v_hover; color: @v_fg; }
window.vaani-settings .sidebar row:selected {
  background-color: alpha(@v_accent, 0.13); color: @v_accent_strong; font-weight: 600;
}
window.vaani-settings .sidebar row label { color: inherit; }
window.vaani-settings .sidebar row, window.vaani-settings .sidebar row:focus,
window.vaani-settings .sidebar row:focus-visible { outline: none; outline-width: 0; }
window.vaani-settings separator { background-color: @v_border; min-width: 1px; min-height: 1px; }
window.vaani-settings stack.pages, window.vaani-settings scrolledwindow, window.vaani-settings viewport {
  background-color: @v_bg;
}

/* page */
window.vaani-settings .page { padding: 28px 40px 40px 40px; }
window.vaani-settings .page-title { font-size: 22px; font-weight: 700; letter-spacing: -0.3px; color: @v_fg; margin-bottom: 4px; }
window.vaani-settings .page-sub { color: @v_muted; font-size: 13px; margin-bottom: 8px; }
window.vaani-settings .group-title {
  font-weight: 600; font-size: 12px; color: @v_muted; margin: 24px 0 8px 2px;
}

/* cards and rows */
window.vaani-settings .card {
  border-radius: 10px; border: 1px solid @v_border; background-color: @v_surface;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
}
window.vaani-settings list.card { background-color: @v_surface; }
window.vaani-settings list.card > row { background: transparent; }
window.vaani-settings .card .row, window.vaani-settings .row { padding: 12px 16px; min-height: 36px; }
window.vaani-settings .row-title { font-weight: 500; font-size: 14px; color: @v_fg; }
window.vaani-settings .row-sub { color: @v_muted; font-size: 12.5px; }

/* status card (top of Overview) */
window.vaani-settings .hero {
  border-radius: 10px; padding: 16px 20px; margin-top: 16px;
  background-color: @v_surface; border: 1px solid @v_border;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
}
window.vaani-settings .hero-title { font-size: 16px; font-weight: 700; color: @v_fg; }
window.vaani-settings .hero .hero-sub { font-size: 13.5px; color: @v_muted; }

/* stats */
window.vaani-settings .stat {
  border-radius: 10px; padding: 12px 16px; border: 1px solid @v_border; background-color: @v_surface;
}
window.vaani-settings .stat-num { font-size: 22px; font-weight: 700; color: @v_fg; font-feature-settings: "tnum"; }
window.vaani-settings .stat-label { color: @v_muted; font-size: 12px; }

/* badges, keycaps */
window.vaani-settings .badge {
  border-radius: 4px; padding: 2px 8px; font-size: 11.5px; font-weight: 600;
  background-color: alpha(@v_ok, 0.14); color: @v_ok;
}
window.vaani-settings .badge.accent { background-color: alpha(@v_warn, 0.16); color: @v_warn; }
window.vaani-settings .badge.star { background-color: alpha(@v_accent, 0.14); color: @v_accent_strong; }
window.vaani-settings .keycap {
  font-family: "JetBrains Mono", "Ubuntu Mono", "DejaVu Sans Mono", monospace;
  font-weight: 600; font-size: 12.5px;
  padding: 4px 10px; border-radius: 6px; color: @v_fg; background-color: @v_surface2;
  border: 1px solid @v_border2; border-bottom-width: 2px;
}

/* buttons */
window.vaani-settings button {
  border-radius: 6px; padding: 5px 12px; min-height: 24px;
  background-image: none; background-color: @v_surface2; color: @v_fg;
  border: 1px solid @v_border; box-shadow: none; font-weight: 500; text-shadow: none;
}
window.vaani-settings button:hover { background-color: @v_hover; border-color: @v_border2; }
window.vaani-settings button:active { background-color: @v_active; }
window.vaani-settings button:disabled { color: @v_subtle; }
window.vaani-settings button.suggested-action {
  background-color: @v_accent; color: @v_on_accent; border-color: @v_accent; font-weight: 600;
}
window.vaani-settings button.suggested-action:hover { background-color: @v_accent_strong; border-color: @v_accent_strong; }
window.vaani-settings button.destructive-action { background-color: @v_danger; color: #ffffff; border-color: @v_danger; }
window.vaani-settings button.image-button { padding: 5px; }
window.vaani-settings button.link, window.vaani-settings button.link label {
  background: none; border: none; color: @v_accent_strong; font-weight: 500; padding: 0;
}
window.vaani-settings button.link:hover label { text-decoration: underline; }

/* dropdowns */
window.vaani-settings dropdown > button { min-width: 120px; }
window.vaani-settings popover > contents {
  background-color: @v_surface; color: @v_fg; border-radius: 8px;
  border: 1px solid @v_border2; box-shadow: 0 8px 24px rgba(0,0,0,0.22); padding: 4px;
}
window.vaani-settings popover listview, window.vaani-settings popover list { background: transparent; }
window.vaani-settings popover row { border-radius: 4px; padding: 5px 8px; color: @v_fg; }
window.vaani-settings popover row:hover, window.vaani-settings popover row:selected {
  background-color: @v_hover;
}
window.vaani-settings popover arrow { background-color: @v_surface; }

/* entries */
window.vaani-settings entry, window.vaani-settings passwordentry, window.vaani-settings searchentry {
  border-radius: 6px; min-height: 32px; padding: 0 10px;
  background-color: @v_field; color: @v_fg; border: 1px solid @v_border2; box-shadow: none;
  caret-color: @v_fg;
}
window.vaani-settings entry:focus-within, window.vaani-settings passwordentry:focus-within,
window.vaani-settings searchentry:focus-within {
  border-color: @v_accent; box-shadow: 0 0 0 2px alpha(@v_accent, 0.25);
}
window.vaani-settings entry text, window.vaani-settings passwordentry text,
window.vaani-settings searchentry text { color: @v_fg; }
window.vaani-settings entry image, window.vaani-settings searchentry image,
window.vaani-settings passwordentry image { color: @v_muted; }

/* text view (test area) */
window.vaani-settings textview, window.vaani-settings textview text {
  background-color: transparent; color: @v_fg; font-size: 14px;
}
window.vaani-settings .tryit {
  min-height: 96px; border-radius: 10px; border: 1px solid @v_border2; background-color: @v_field;
}
window.vaani-settings .tryit:focus-within { border-color: @v_accent; box-shadow: 0 0 0 2px alpha(@v_accent, 0.2); }

/* switches */
window.vaani-settings switch {
  border-radius: 999px; background-color: @v_border2; border: none;
  min-width: 40px; min-height: 22px; padding: 2px;
}
window.vaani-settings switch:checked { background-color: @v_accent; background-image: none; }
window.vaani-settings switch slider, window.vaani-settings switch:checked slider {
  border-radius: 999px; background-image: none; background-color: #ffffff;
  min-width: 18px; min-height: 18px; margin: 0;
  border: none; outline: none; box-shadow: 0 1px 2px rgba(0,0,0,0.3);
}
window.vaani-settings switch image { color: transparent; opacity: 0; }

/* radios */
window.vaani-settings checkbutton { color: @v_fg; }
window.vaani-settings checkbutton radio {
  background-color: @v_field; border: 1px solid @v_border2; min-width: 16px; min-height: 16px;
  background-image: none; box-shadow: none;
}
window.vaani-settings checkbutton radio:checked {
  border-color: @v_accent; background-color: @v_accent; color: @v_bg;
  -gtk-icon-source: none; box-shadow: inset 0 0 0 4px @v_field;
}

/* progress bars */
window.vaani-settings progressbar trough {
  background-color: @v_surface2; border-radius: 999px; min-height: 6px; border: none;
}
window.vaani-settings progressbar progress {
  background-color: @v_accent; background-image: none; border-radius: 999px; min-height: 6px; border: none;
}
window.vaani-settings progressbar text { color: @v_muted; font-size: 11px; }

/* scrollbars */
window.vaani-settings scrollbar { background: transparent; border: none; }
window.vaani-settings scrollbar slider {
  background-color: @v_border2; border-radius: 999px; min-width: 6px; border: none;
}

/* history, about */
window.vaani-settings .history-text { font-size: 14px; color: @v_fg; }
window.vaani-settings .history-meta { color: @v_subtle; font-size: 12px; }
window.vaani-settings .about-logo {
  font-size: 28px; font-weight: 700; letter-spacing: -0.5px; color: @v_fg;
}
window.vaani-settings selection, window.vaani-settings text selection {
  background-color: alpha(@v_accent, 0.35); color: @v_fg;
}
"#;

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter.ttf");

fn font_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/fonts/vaani"))
}

/// Put our fonts where fontconfig finds them. Returns true if they were newly written
/// (fontconfig picks them up on its next rescan, at the latest on the next start).
pub fn ensure_fonts() -> bool {
    let Some(dir) = font_dir() else { return false };
    let mut wrote = false;
    // Fonts from earlier versions.
    for old in ["Unbounded.ttf", "PlusJakartaSans.ttf"] {
        if std::fs::remove_file(dir.join(old)).is_ok() {
            wrote = true;
        }
    }
    for (name, bytes) in [("Inter.ttf", INTER)] {
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
    sync_menu_icon(dark);
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

/// Cinnamon and most other panels take a window's taskbar icon from its app-menu entry
/// (matched by WM_CLASS), not from the window. Point that entry at the logo variant for the
/// current theme so the taskbar matches. Only touches the per-user entry the installer made.
fn sync_menu_icon(dark: bool) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = PathBuf::from(home)
        .join(".local/share/applications")
        .join(format!("{}.desktop", crate::keys::APP_ICON));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let want = format!(
        "Icon={}",
        if dark {
            crate::keys::APP_ICON_DARK
        } else {
            crate::keys::APP_ICON
        }
    );
    if text.lines().any(|l| l == want) {
        return;
    }
    let new: String = text
        .lines()
        .map(|l| {
            if l.starts_with("Icon=") {
                want.as_str()
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let tmp = path.with_extension("desktop.tmp");
    if std::fs::write(&tmp, new).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}
