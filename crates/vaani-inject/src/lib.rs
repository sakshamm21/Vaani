//! Text insertion on X11: own the CLIPBOARD with our text, press Ctrl+V (Ctrl+Shift+V in
//! terminals) through XTest, wait until the app has actually read the text, then put the
//! user's previous clipboard back (every target, images included).
//!
//! X11 clipboards live in the owning process, so a background thread keeps serving the
//! restored clipboard until someone copies something new.
//!
//! "Never lose words": if no app reads the pasted text, it is typed key by key instead
//! (XTest, with a spare keycode remapped for characters the keyboard layout lacks). If that
//! fails too, the text stays on the clipboard and the caller tells the user to paste it.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use x11rb::CURRENT_TIME;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, Window, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("cannot connect to the X server: {0}")]
    Connect(String),
    #[error("X11 error: {0}")]
    X11(String),
    #[error("the insertion thread stopped")]
    Gone,
}

fn x<E: std::fmt::Display>(e: E) -> InjectError {
    InjectError::X11(e.to_string())
}

/// How text gets into the focused app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Paste, and type the text if the app doesn't take the paste.
    #[default]
    Auto,
    /// Paste only; leave the text on the clipboard if the app doesn't take it.
    Paste,
    /// Type key by key; never touch the clipboard.
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The focused app read our text after `after`.
    Pasted {
        after: Duration,
        terminal: bool,
        app: String,
    },
    /// Typed key by key (the app ignored the paste, or typing was asked for).
    Typed { app: String, chars: usize },
    /// Nobody read it; the text is left on the clipboard.
    LeftOnClipboard { app: String },
}

enum Cmd {
    Insert(String, Mode, Sender<Result<Outcome, InjectError>>),
    SetClipboard(String),
    FocusedApp(Sender<String>),
    Backspace(usize, Sender<Result<(), InjectError>>),
    ReadClipboard(Sender<String>),
}

#[derive(Clone)]
pub struct Injector {
    tx: Sender<Cmd>,
    /// Written to after each command so the service thread wakes from poll().
    wake: std::sync::Arc<std::os::unix::net::UnixStream>,
}

impl Injector {
    pub fn start() -> Result<Self, InjectError> {
        let (tx, rx) = unbounded();
        let (ready_tx, ready_rx) = bounded(1);
        let (wake, wake_rx) = std::os::unix::net::UnixStream::pair().map_err(x)?;
        wake_rx.set_nonblocking(true).map_err(x)?;
        thread::Builder::new()
            .name("inject".into())
            .spawn(move || match Service::new() {
                Ok(mut s) => {
                    let _ = ready_tx.send(Ok(()));
                    s.run(rx, wake_rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(x)?;
        ready_rx.recv().map_err(|_| InjectError::Gone)??;
        Ok(Self {
            tx,
            wake: std::sync::Arc::new(wake),
        })
    }

    fn send(&self, cmd: Cmd) -> Result<(), InjectError> {
        self.tx.send(cmd).map_err(|_| InjectError::Gone)?;
        let _ = (&*self.wake).write(&[1]);
        Ok(())
    }

    /// Insert text into the focused app. Blocks until pasted or typed.
    pub fn insert(&self, text: &str) -> Result<Outcome, InjectError> {
        self.insert_with(text, Mode::Auto)
    }

    /// Insert text with an explicit [`Mode`].
    pub fn insert_with(&self, text: &str, mode: Mode) -> Result<Outcome, InjectError> {
        let (tx, rx) = bounded(1);
        self.send(Cmd::Insert(text.to_string(), mode, tx))?;
        rx.recv().map_err(|_| InjectError::Gone)?
    }

    /// Just put text on the clipboard (e.g. "copy" from history).
    pub fn set_clipboard(&self, text: &str) {
        let _ = self.send(Cmd::SetClipboard(text.to_string()));
    }

    /// WM_CLASS of the focused window, lowercased (e.g. "firefox", "gnome-terminal").
    /// Press BackSpace `n` times in the focused window ("scratch that").
    pub fn backspace(&self, n: usize) -> Result<(), InjectError> {
        let (tx, rx) = bounded(1);
        self.send(Cmd::Backspace(n, tx))?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| InjectError::X11("backspace timed out".into()))?
    }

    /// The clipboard's text, or an empty string (for snippets with `{clipboard}`).
    pub fn clipboard_text(&self) -> String {
        let (tx, rx) = bounded(1);
        if self.send(Cmd::ReadClipboard(tx)).is_err() {
            return String::new();
        }
        rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()
    }

    pub fn focused_app(&self) -> String {
        let (tx, rx) = bounded(1);
        if self.send(Cmd::FocusedApp(tx)).is_err() {
            return String::new();
        }
        rx.recv_timeout(Duration::from_millis(500))
            .unwrap_or_default()
    }
}

/// target atom -> (property type, bytes)
type Offer = BTreeMap<Atom, (Atom, Vec<u8>)>;

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    timestamp: Atom,
    multiple: Atom,
    save_targets: Atom,
    utf8: Atom,
    text: Atom,
    text_plain: Atom,
    text_plain_utf8: Atom,
    incr: Atom,
    prop: Atom,
    wm_class: Atom,
}

struct Service {
    conn: RustConnection,
    root: Window,
    win: Window,
    a: Atoms,
    /// What we are serving right now while we own the clipboard.
    offer: Offer,
    owned: bool,
    max_prop: usize,
}

const TERMINALS: &[&str] = &[
    "terminal",
    "konsole",
    "kitty",
    "alacritty",
    "xterm",
    "rxvt",
    "tilix",
    "terminator",
    "wezterm",
    "ghostty",
    "foot",
    "st-256color",
    "guake",
    "tilda",
    "yakuake",
    "terminology",
    "sakura",
    "tabby",
    "contour",
    "blackbox",
    "ptyxis",
    "warp",
    "cool-retro-term",
];

/// Whether a window class (lowercase) belongs to a terminal emulator.
pub fn is_terminal(class: &str) -> bool {
    TERMINALS.iter().any(|t| class.contains(t))
}

impl Service {
    fn new() -> Result<Self, InjectError> {
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| InjectError::Connect(e.to_string()))?;
        let root = conn.setup().roots[screen].root;
        let atom = |n: &str| -> Result<Atom, InjectError> {
            Ok(conn
                .intern_atom(false, n.as_bytes())
                .map_err(x)?
                .reply()
                .map_err(x)?
                .atom)
        };
        let a = Atoms {
            clipboard: atom("CLIPBOARD")?,
            targets: atom("TARGETS")?,
            timestamp: atom("TIMESTAMP")?,
            multiple: atom("MULTIPLE")?,
            save_targets: atom("SAVE_TARGETS")?,
            utf8: atom("UTF8_STRING")?,
            text: atom("TEXT")?,
            text_plain: atom("text/plain")?,
            text_plain_utf8: atom("text/plain;charset=utf-8")?,
            incr: atom("INCR")?,
            prop: atom("VAANI_SELECTION")?,
            wm_class: AtomEnum::WM_CLASS.into(),
        };
        let win = conn.generate_id().map_err(x)?;
        conn.create_window(
            0,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(x)?;
        conn.flush().map_err(x)?;
        // Leave headroom under the request size limit for the property header.
        let max_prop = conn.maximum_request_bytes().saturating_sub(64);
        Ok(Self {
            conn,
            root,
            win,
            a,
            offer: Offer::new(),
            owned: false,
            max_prop,
        })
    }

    fn run(&mut self, rx: Receiver<Cmd>, mut wake: std::os::unix::net::UnixStream) {
        loop {
            let mut drain = [0u8; 64];
            while matches!(wake.read(&mut drain), Ok(n) if n > 0) {}
            match rx.try_recv() {
                Ok(Cmd::Insert(text, mode, reply)) => {
                    let r = self.insert(&text, mode);
                    let _ = reply.send(r);
                }
                Ok(Cmd::SetClipboard(text)) => {
                    let offer = self.text_offer(&text);
                    let _ = self.own(offer);
                }
                Ok(Cmd::FocusedApp(reply)) => {
                    let _ = reply.send(self.focused_class());
                }
                Ok(Cmd::Backspace(n, reply)) => {
                    let _ = reply.send(self.backspace(n));
                }
                Ok(Cmd::ReadClipboard(reply)) => {
                    let _ = reply.send(self.clipboard_text());
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
            match self.conn.poll_for_event() {
                Ok(Some(ev)) => {
                    self.handle_background(ev);
                }
                Ok(None) => wait_readable(&[self.conn.stream().as_fd(), wake.as_fd()], 1000),
                Err(e) => {
                    tracing::error!("clipboard service lost the X connection: {e}");
                    return;
                }
            }
        }
    }

    /// Serve requests while idle; drop our offer when someone else copies.
    fn handle_background(&mut self, ev: Event) {
        match ev {
            Event::SelectionRequest(req) => {
                let _ = self.serve(&req);
            }
            Event::SelectionClear(c) if c.selection == self.a.clipboard => {
                self.owned = false;
                self.offer.clear();
            }
            _ => {}
        }
    }

    fn text_offer(&self, text: &str) -> Offer {
        let mut o = Offer::new();
        let bytes = text.as_bytes().to_vec();
        for t in [
            self.a.utf8,
            self.a.text_plain_utf8,
            AtomEnum::STRING.into(),
            self.a.text,
            self.a.text_plain,
        ] {
            let ty = if t == self.a.text_plain_utf8 {
                t
            } else {
                self.a.utf8
            };
            o.insert(t, (ty, bytes.clone()));
        }
        o
    }

    fn own(&mut self, offer: Offer) -> Result<(), InjectError> {
        self.offer = offer;
        self.conn
            .set_selection_owner(self.win, self.a.clipboard, CURRENT_TIME)
            .map_err(x)?;
        let owner = self
            .conn
            .get_selection_owner(self.a.clipboard)
            .map_err(x)?
            .reply()
            .map_err(x)?
            .owner;
        self.owned = owner == self.win;
        Ok(())
    }

    fn insert(&mut self, text: &str, mode: Mode) -> Result<Outcome, InjectError> {
        let app = self.focused_class();
        let terminal = is_terminal(&app);
        if mode == Mode::Type {
            let chars = self.type_text(text)?;
            return Ok(Outcome::Typed { app, chars });
        }

        // 1. Save the user's clipboard (or keep what we already serve for them).
        let saved = if self.owned {
            std::mem::take(&mut self.offer)
        } else {
            self.save_clipboard().unwrap_or_default()
        };

        // 2. Offer our text, give apps a moment, then press paste. Some apps (Scintilla-based
        //    editors such as Nextpad++, Wine apps) prefetch the clipboard the instant its owner
        //    changes and paste from that cache, so a Ctrl+V sent within a millisecond pastes
        //    nothing. 40 ms is invisible to the user and leaves a wide margin (10 ms was enough).
        self.own(self.text_offer(text))?;
        let mut prefetched = false;
        self.serve_for(Duration::from_millis(40), &mut prefetched)?;
        self.wait_modifiers_released();
        let started = Instant::now();
        let mut read_at = self.paste_and_wait(terminal, &app, Duration::from_millis(1500))?;

        // An app that has only just opened can drop the first paste key while its text box is
        // still taking focus. Try once more once focus has settled: a paste lands whole or not
        // at all, while typed keys sent too early lose the start of the text.
        if read_at.is_none() && !prefetched && self.owned && mode == Mode::Auto {
            self.wait_focus_settled();
            read_at = self.paste_and_wait(terminal, &app, Duration::from_millis(700))?;
        }

        let read_at = match read_at {
            Some(t) => t,
            // The app pasted from its own prefetched copy; we can't see that happen, so give
            // it a generous moment before putting the old clipboard back.
            None if prefetched && self.owned => {
                let mut ignored = false;
                self.serve_for(Duration::from_millis(400), &mut ignored)?;
                tracing::debug!(%app, "no read after paste; assuming the app used its prefetched copy");
                Instant::now()
            }
            None if mode == Mode::Auto => {
                tracing::info!(%app, "the app ignored the paste; typing the text instead");
                self.wait_focus_settled();
                let typed = self.type_text(text);
                self.restore(saved)?;
                return match typed {
                    Ok(chars) => Ok(Outcome::Typed { app, chars }),
                    Err(e) => {
                        tracing::warn!(%app, "typing failed too: {e}");
                        let offer = self.text_offer(text);
                        self.own(offer)?;
                        Ok(Outcome::LeftOnClipboard { app })
                    }
                };
            }
            None => {
                tracing::warn!(%app, "no app read the dictated text; leaving it on the clipboard");
                return Ok(Outcome::LeftOnClipboard { app });
            }
        };
        let after = read_at.duration_since(started);

        // 4. Put the old clipboard back.
        self.restore(saved)?;
        Ok(Outcome::Pasted {
            after,
            terminal,
            app,
        })
    }

    /// Give the clipboard back to what the user had before we took it.
    fn restore(&mut self, saved: Offer) -> Result<(), InjectError> {
        if self.owned {
            if saved.is_empty() {
                self.offer.clear();
                let _ = self
                    .conn
                    .set_selection_owner(x11rb::NONE, self.a.clipboard, CURRENT_TIME);
                self.owned = false;
            } else {
                self.own(saved)?;
            }
            let _ = self.conn.flush();
        }
        Ok(())
    }

    /// Type `text` into the focused window with XTest. Characters on the current layout use
    /// their own key (with Shift if needed); anything else (accents, scripts, emoji) goes
    /// through a spare keycode remapped to that character for the moment it is pressed.
    fn type_text(&mut self, text: &str) -> Result<usize, InjectError> {
        // A typed Return in a terminal would run the command, so never type one there.
        let flat;
        let text = if is_terminal(&self.focused_class()) && text.contains('\n') {
            flat = text.replace('\n', " ");
            flat.as_str()
        } else {
            text
        };
        self.wait_modifiers_released();
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(x)?
            .reply()
            .map_err(x)?;
        let per = usize::from(map.keysyms_per_keycode).max(1);
        // keysym -> (keycode, needs shift), from the first two levels only.
        let mut direct: BTreeMap<u32, (u8, bool)> = BTreeMap::new();
        let mut spare = None;
        for (i, syms) in map.keysyms.chunks(per).enumerate() {
            let code = min + i as u8;
            if syms.iter().all(|&s| s == 0) {
                // Prefer a high, unused keycode so we never shadow a real key.
                spare = Some(code);
                continue;
            }
            for (level, &sym) in syms.iter().take(2).enumerate() {
                if sym != 0 {
                    direct.entry(sym).or_insert((code, level == 1));
                }
            }
        }
        let shift = self.keycode(0xffe1)?;
        let mut remapped = false;
        let mut count = 0;
        for ch in text.chars() {
            let sym = keysym_for(ch);
            let (code, shifted) = match direct.get(&sym) {
                Some(&k) => k,
                None => {
                    let Some(code) = spare else {
                        return Err(InjectError::X11(format!("no key to type {ch:?}")));
                    };
                    let mut syms = vec![0u32; per];
                    syms[0] = sym;
                    if per > 1 {
                        syms[1] = sym;
                    }
                    self.conn
                        .change_keyboard_mapping(1, code, per as u8, &syms)
                        .map_err(x)?;
                    // Let clients see the new mapping before the key arrives.
                    self.conn.get_input_focus().map_err(x)?.reply().map_err(x)?;
                    thread::sleep(Duration::from_millis(8));
                    remapped = true;
                    (code, false)
                }
            };
            if shifted {
                self.conn
                    .xtest_fake_input(2, shift, CURRENT_TIME, self.root, 0, 0, 0)
                    .map_err(x)?;
            }
            self.conn
                .xtest_fake_input(2, code, CURRENT_TIME, self.root, 0, 0, 0)
                .map_err(x)?;
            self.conn
                .xtest_fake_input(3, code, CURRENT_TIME, self.root, 0, 0, 0)
                .map_err(x)?;
            if shifted {
                self.conn
                    .xtest_fake_input(3, shift, CURRENT_TIME, self.root, 0, 0, 0)
                    .map_err(x)?;
            }
            self.conn.flush().map_err(x)?;
            count += 1;
            // A short pause keeps slow apps (Wine, remote desktops) from dropping keys.
            thread::sleep(Duration::from_millis(if remapped { 6 } else { 2 }));
        }
        if let (true, Some(code)) = (remapped, spare) {
            let _ = self
                .conn
                .change_keyboard_mapping(1, code, per as u8, &vec![0u32; per]);
            let _ = self.conn.flush();
        }
        Ok(count)
    }

    /// Answer clipboard requests for `dur`; sets `read` if anyone fetched the text itself.
    fn serve_for(&mut self, dur: Duration, read: &mut bool) -> Result<(), InjectError> {
        let end = Instant::now() + dur;
        while Instant::now() < end && self.owned {
            match self.conn.poll_for_event().map_err(x)? {
                Some(Event::SelectionRequest(req)) => {
                    let served = self.serve(&req)?;
                    tracing::debug!(
                        requestor = req.requestor,
                        target = req.target,
                        served,
                        "clipboard read before paste"
                    );
                    if served && req.target != self.a.targets && req.target != self.a.timestamp {
                        *read = true;
                    }
                }
                Some(Event::SelectionClear(_)) => self.owned = false,
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
        Ok(())
    }

    /// Wait (up to 400 ms) until Shift/Ctrl/Alt/Super are physically up, so our Ctrl+V is not
    /// turned into something else by a key the user is still holding.
    /// Press paste and serve the clipboard until the app has read it. Only reads after the key
    /// press count: clipboard managers and file managers read every new clipboard right away,
    /// so a read before the key press says nothing about the paste.
    fn paste_and_wait(
        &mut self,
        terminal: bool,
        app: &str,
        limit: Duration,
    ) -> Result<Option<Instant>, InjectError> {
        self.send_paste(terminal)?;
        tracing::debug!(%app, terminal, "paste key sent");
        let deadline = Instant::now() + limit;
        let mut read_at: Option<Instant> = None;
        while Instant::now() < deadline {
            // Keep serving a while after the first read: some apps ask several times.
            if read_at.is_some_and(|t| t.elapsed() > Duration::from_millis(300)) {
                break;
            }
            match self.conn.poll_for_event().map_err(x)? {
                Some(Event::SelectionRequest(req)) => {
                    let served = self.serve(&req)?;
                    tracing::debug!(
                        requestor = req.requestor,
                        target = req.target,
                        served,
                        "clipboard read after paste"
                    );
                    if served && req.target != self.a.targets && req.target != self.a.timestamp {
                        read_at.get_or_insert_with(Instant::now);
                    }
                }
                Some(Event::SelectionClear(_)) => {
                    // Someone copied during the paste; leave their content alone.
                    self.owned = false;
                    break;
                }
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
        Ok(read_at)
    }

    /// Wait (up to 600 ms) until keyboard focus has stayed on one window for 150 ms, so keys
    /// are not sent to a window that is still opening.
    fn wait_focus_settled(&self) {
        let focus = || {
            self.conn
                .get_input_focus()
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.focus)
        };
        let end = Instant::now() + Duration::from_millis(600);
        let mut last = focus();
        let mut since = Instant::now();
        while Instant::now() < end {
            thread::sleep(Duration::from_millis(25));
            let now = focus();
            if now != last {
                last = now;
                since = Instant::now();
            } else if since.elapsed() >= Duration::from_millis(150) {
                return;
            }
        }
    }

    fn wait_modifiers_released(&self) {
        let end = Instant::now() + Duration::from_millis(400);
        while Instant::now() < end {
            let mask = self
                .conn
                .query_pointer(self.root)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| u16::from(r.mask))
                .unwrap_or(0);
            // Shift 1, Control 4, Mod1 (Alt) 8, Mod4 (Super) 64
            if mask & (1 | 4 | 8 | 64) == 0 {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn backspace(&self, n: usize) -> Result<(), InjectError> {
        self.wait_modifiers_released();
        let k = self.keycode(0xff08)?; // BackSpace
        for _ in 0..n {
            for kind in [2, 3] {
                self.conn
                    .xtest_fake_input(kind, k, CURRENT_TIME, self.root, 0, 0, 0)
                    .map_err(x)?;
            }
            self.conn.flush().map_err(x)?;
            thread::sleep(Duration::from_millis(3));
        }
        Ok(())
    }

    fn clipboard_text(&self) -> String {
        if self.owned {
            return self
                .offer
                .get(&self.a.utf8)
                .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
        }
        match self.fetch(self.a.utf8) {
            Ok(Some((_, bytes, 8))) => String::from_utf8_lossy(&bytes).into_owned(),
            _ => String::new(),
        }
    }

    fn keycode(&self, sym: u32) -> Result<u8, InjectError> {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(x)?
            .reply()
            .map_err(x)?;
        let per = usize::from(map.keysyms_per_keycode).max(1);
        map.keysyms
            .chunks(per)
            .position(|s| s.contains(&sym))
            .map(|i| min + i as u8)
            .ok_or_else(|| InjectError::X11(format!("no keycode for keysym 0x{sym:x}")))
    }

    fn send_paste(&self, terminal: bool) -> Result<(), InjectError> {
        let mut keys = vec![self.keycode(0xffe3)?]; // Control_L
        if terminal {
            keys.push(self.keycode(0xffe1)?); // Shift_L
        }
        keys.push(self.keycode(0x76)?); // v
        for &k in &keys {
            self.conn
                .xtest_fake_input(2, k, CURRENT_TIME, self.root, 0, 0, 0)
                .map_err(x)?;
        }
        for &k in keys.iter().rev() {
            self.conn
                .xtest_fake_input(3, k, CURRENT_TIME, self.root, 0, 0, 0)
                .map_err(x)?;
        }
        self.conn.flush().map_err(x)?;
        Ok(())
    }

    fn serve(&self, req: &SelectionRequestEvent) -> Result<bool, InjectError> {
        let property = if req.property == x11rb::NONE {
            req.target
        } else {
            req.property
        };
        let ok = if req.selection != self.a.clipboard || !self.owned {
            false
        } else if req.target == self.a.targets {
            let mut list = vec![self.a.targets, self.a.timestamp];
            list.extend(self.offer.keys().copied());
            self.conn
                .change_property32(
                    PropMode::REPLACE,
                    req.requestor,
                    property,
                    AtomEnum::ATOM,
                    &list,
                )
                .map_err(x)?;
            true
        } else if let Some((ty, data)) = self.offer.get(&req.target) {
            if data.len() > self.max_prop {
                false // would need INCR; rare for text, skipped
            } else {
                self.conn
                    .change_property8(PropMode::REPLACE, req.requestor, property, *ty, data)
                    .map_err(x)?;
                true
            }
        } else {
            false
        };
        let ev = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if ok { property } else { x11rb::NONE },
        };
        self.conn
            .send_event(false, req.requestor, EventMask::NO_EVENT, ev)
            .map_err(x)?;
        self.conn.flush().map_err(x)?;
        Ok(ok)
    }

    fn fetch(&self, target: Atom) -> Result<Option<(Atom, Vec<u8>, u8)>, InjectError> {
        self.conn
            .convert_selection(
                self.win,
                self.a.clipboard,
                target,
                self.a.prop,
                CURRENT_TIME,
            )
            .map_err(x)?;
        self.conn.flush().map_err(x)?;
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            match self.conn.poll_for_event().map_err(x)? {
                Some(Event::SelectionNotify(n)) if n.target == target => {
                    if n.property == x11rb::NONE {
                        return Ok(None);
                    }
                    let r = self
                        .conn
                        .get_property(true, self.win, self.a.prop, AtomEnum::ANY, 0, u32::MAX / 4)
                        .map_err(x)?
                        .reply()
                        .map_err(x)?;
                    if r.type_ == self.a.incr {
                        return Ok(None); // large item (e.g. big image); not preserved
                    }
                    return Ok(Some((r.type_, r.value, r.format)));
                }
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
        Ok(None)
    }

    fn save_clipboard(&self) -> Result<Offer, InjectError> {
        let mut saved = Offer::new();
        let owner = self
            .conn
            .get_selection_owner(self.a.clipboard)
            .map_err(x)?
            .reply()
            .map_err(x)?
            .owner;
        if owner == x11rb::NONE {
            return Ok(saved);
        }
        let Some((_, raw, 32)) = self.fetch(self.a.targets)? else {
            return Ok(saved);
        };
        let skip = [
            self.a.targets,
            self.a.timestamp,
            self.a.multiple,
            self.a.save_targets,
        ];
        for t in raw
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .filter(|t| !skip.contains(t))
        {
            if let Some((ty, data, 8)) = self.fetch(t)? {
                saved.insert(t, (ty, data));
            }
        }
        Ok(saved)
    }

    fn focused_class(&self) -> String {
        let Ok(Ok(focus)) = self.conn.get_input_focus().map(|c| c.reply()) else {
            return String::new();
        };
        let mut w = focus.focus;
        for _ in 0..8 {
            if w == x11rb::NONE || w == 1 || w == self.root {
                break;
            }
            if let Ok(Ok(p)) = self
                .conn
                .get_property(false, w, self.a.wm_class, AtomEnum::STRING, 0, 256)
                .map(|c| c.reply())
            {
                if !p.value.is_empty() {
                    // WM_CLASS is "instance\0Class\0"; use the class.
                    let parts: Vec<&[u8]> = p
                        .value
                        .split(|b| *b == 0)
                        .filter(|s| !s.is_empty())
                        .collect();
                    let class = parts.last().copied().unwrap_or(&[]);
                    return String::from_utf8_lossy(class).to_ascii_lowercase();
                }
            }
            match self.conn.query_tree(w).map(|c| c.reply()) {
                Ok(Ok(t)) => w = t.parent,
                _ => break,
            }
        }
        String::new()
    }
}

/// Block until any of `fds` is readable or `timeout_ms` passes (no busy polling).
/// The X keysym that types `ch`.
fn keysym_for(ch: char) -> u32 {
    match ch {
        '\n' => 0xff0d, // Return
        '\t' => 0xff09, // Tab
        '\u{8}' => 0xff08,
        c if (' '..='~').contains(&c) => c as u32,
        // Latin-1 keysyms equal their code points.
        c if ('\u{a0}'..='\u{ff}').contains(&c) => c as u32,
        c => 0x0100_0000 + c as u32,
    }
}

fn wait_readable(fds: &[std::os::fd::BorrowedFd<'_>], timeout_ms: i32) {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let mut p: Vec<PollFd<'_>> = fds
        .iter()
        .map(|fd| PollFd::new(fd, PollFlags::IN))
        .collect();
    let t = Timespec {
        tv_sec: i64::from(timeout_ms / 1000),
        tv_nsec: i64::from(timeout_ms % 1000) * 1_000_000,
    };
    let _ = rustix::event::poll(&mut p, Some(&t));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms() {
        assert_eq!(keysym_for('a'), 0x61);
        assert_eq!(keysym_for('\n'), 0xff0d);
        assert_eq!(keysym_for('é'), 0xe9);
        assert_eq!(keysym_for('न'), 0x0100_0928);
        assert_eq!(keysym_for('😀'), 0x0101_f600);
    }

    #[test]
    fn terminal_detection() {
        for t in [
            "gnome-terminal-server",
            "xfce4-terminal",
            "kitty",
            "alacritty",
            "org.wezfurlong.wezterm",
            "xterm",
            "tilix",
        ] {
            assert!(is_terminal(t), "{t}");
        }
        for t in ["firefox", "code", "libreoffice-writer", "xed", "slack"] {
            assert!(!is_terminal(t), "{t}");
        }
    }
}
