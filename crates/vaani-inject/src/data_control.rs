// The data-control clipboard service, written once for both protocol versions: the wlr one
// (sway, Hyprland, older KDE) and the standard ext one (KDE Plasma 6 and newer compositors).
// It is included by `wayland.rs` with `Manager`, `Device`, `DcOffer`, `Source` and the
// `dc_*` event modules named for one of them.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use std::collections::HashMap;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};
use xkbcommon::xkb;

use super::{Offer, SAVE_LIMIT, Sel, TEXT_TYPES, focused_app, poll_in, read_keymap, text_offer, w};
use crate::keys::Keys;
use crate::{Cmd, InjectError, Mode, Outcome, is_terminal};

#[derive(Default)]
struct State {
    /// mime types announced for each offer object
    offers: HashMap<wayland_client::backend::ObjectId, Vec<String>>,
    clipboard: Option<DcOffer>,
    primary: Option<DcOffer>,
    /// What our live sources serve.
    serving: Vec<(Source, Sel, Offer)>,
    /// Set when an app fetched real data (not just the type list) from one of our sources.
    read_at: Option<Instant>,
    /// A source of ours was replaced by someone else's copy.
    cancelled: bool,
    keymap: Option<xkb::Keymap>,
    has_keyboard: bool,
}

pub(crate) struct Service {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    manager: Manager,
    device: Device,
    keys: Keys,
}

/// Whether this compositor offers data-control (checked before choosing this backend).
pub(crate) fn available() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _)) = registry_queue_init::<State>(&conn) else {
        return false;
    };
    globals.contents().with_list(|l| {
        l.iter()
            .any(|g| g.interface == Manager::interface().name)
    })
}

impl Service {
    pub(crate) fn new() -> Result<Self, InjectError> {
        let conn = Connection::connect_to_env().map_err(w)?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(w)?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).map_err(w)?;
        let manager: Manager = globals.bind(&qh, 1..=MAX_VERSION, ()).map_err(w)?;
        let device = manager.get_data_device(&seat, &qh, ());
        let mut state = State::default();
        // The seat announces its keyboard, the keyboard sends its keymap, and the device
        // sends the current selection; two round trips collect all of it.
        queue.roundtrip(&mut state).map_err(w)?;
        if state.has_keyboard {
            let _keyboard = seat.get_keyboard(&qh, ());
            queue.roundtrip(&mut state).map_err(w)?;
        }
        if state.keymap.is_none() {
            tracing::warn!("the compositor sent no keymap; assuming a US layout");
            let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
            state.keymap = xkb::Keymap::new_from_names(
                &ctx,
                "",
                "",
                "us",
                "",
                None,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            );
        }
        let keys = Keys::uinput()?;
        Ok(Self {
            conn,
            queue,
            qh,
            state,
            manager,
            device,
            keys,
        })
    }

    pub(crate) fn run(&mut self, rx: Receiver<Cmd>, mut wake: UnixStream) {
        loop {
            let mut drain = [0u8; 64];
            while matches!(wake.read(&mut drain), Ok(n) if n > 0) {}
            match rx.try_recv() {
                Ok(Cmd::Insert(text, mode, reply)) => {
                    let _ = reply.send(self.insert(&text, mode));
                }
                Ok(Cmd::SetClipboard(text)) => {
                    let _ = self.own(Sel::Clipboard, text_offer(&text));
                }
                Ok(Cmd::FocusedApp(reply)) => {
                    let _ = reply.send(focused_app());
                }
                Ok(Cmd::Backspace(n, reply)) => {
                    let _ = reply.send(self.backspace(n));
                }
                Ok(Cmd::ReadClipboard(reply)) => {
                    let _ = reply.send(self.text_of(Sel::Clipboard));
                }
                Ok(Cmd::ReadSelection(reply)) => {
                    let _ = reply.send(self.text_of(Sel::Primary));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
            if let Err(e) = self.pump(Some(&wake), 1000) {
                tracing::error!("clipboard service lost the Wayland connection: {e}");
                return;
            }
        }
    }

    /// Wait up to `timeout_ms` for Wayland events (or a wake-up) and handle them.
    fn pump(&mut self, wake: Option<&UnixStream>, timeout_ms: i32) -> Result<(), InjectError> {
        self.queue.dispatch_pending(&mut self.state).map_err(w)?;
        self.conn.flush().map_err(w)?;
        if let Some(guard) = self.queue.prepare_read() {
            let readable = {
                let fd = guard.connection_fd();
                let mut fds = vec![fd];
                if let Some(s) = wake {
                    fds.push(s.as_fd());
                }
                crate::wait_readable(&fds, timeout_ms);
                poll_in(fd)
            };
            if readable {
                guard.read().map_err(w)?;
            }
        }
        self.queue.dispatch_pending(&mut self.state).map_err(w)?;
        Ok(())
    }

    /// Handle events for `dur`.
    fn serve_for(&mut self, dur: Duration) -> Result<(), InjectError> {
        let end = Instant::now() + dur;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            self.pump(None, left.as_millis().clamp(1, 50) as i32)?;
        }
        Ok(())
    }

    fn own(&mut self, sel: Sel, offer: Offer) -> Result<(), InjectError> {
        self.state.serving.retain(|(s, which, _)| {
            if *which == sel {
                s.destroy();
                false
            } else {
                true
            }
        });
        if offer.is_empty() {
            match sel {
                Sel::Clipboard => self.device.set_selection(None),
                Sel::Primary => self.device.set_primary_selection(None),
            }
        } else {
            let source = self.manager.create_data_source(&self.qh, ());
            for mime in offer.keys() {
                source.offer(mime.clone());
            }
            match sel {
                Sel::Clipboard => self.device.set_selection(Some(&source)),
                Sel::Primary => self.device.set_primary_selection(Some(&source)),
            }
            self.state.serving.push((source, sel, offer));
        }
        self.conn.flush().map_err(w)?;
        // Let the compositor announce the new selection back to us.
        self.queue.roundtrip(&mut self.state).map_err(w)?;
        Ok(())
    }

    fn owned(&self, sel: Sel) -> Option<&Offer> {
        self.state
            .serving
            .iter()
            .find(|(_, s, _)| *s == sel)
            .map(|(_, _, o)| o)
    }

    /// Everything the current selection offers, to put back after the paste.
    fn save(&mut self, sel: Sel) -> Offer {
        if let Some(o) = self.owned(sel) {
            return o.clone();
        }
        let current = match sel {
            Sel::Clipboard => self.state.clipboard.clone(),
            Sel::Primary => self.state.primary.clone(),
        };
        let Some(offer) = current else {
            return Offer::new();
        };
        let mimes = self
            .state
            .offers
            .get(&offer.id())
            .cloned()
            .unwrap_or_default();
        let mut saved = Offer::new();
        let mut total = 0;
        for mime in mimes {
            match self.receive(&offer, &mime) {
                Some(bytes) if total + bytes.len() <= SAVE_LIMIT => {
                    total += bytes.len();
                    saved.insert(mime, bytes);
                }
                Some(_) => {
                    tracing::debug!(%mime, "clipboard item too big to keep");
                }
                None => {}
            }
        }
        saved
    }

    /// Read one mime type from an offer (300 ms limit, like on X11).
    fn receive(&mut self, offer: &DcOffer, mime: &str) -> Option<Vec<u8>> {
        let (r, wr) = rustix::pipe::pipe().ok()?;
        offer.receive(mime.to_string(), wr.as_fd());
        self.conn.flush().ok()?;
        drop(wr);
        let mut file = std::fs::File::from(r);
        rustix::fs::fcntl_setfl(&file, rustix::fs::OFlags::NONBLOCK).ok()?;
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            match file.read(&mut buf) {
                Ok(0) => return Some(out),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() > deadline || out.len() > SAVE_LIMIT {
                        return None;
                    }
                    crate::wait_readable(&[file.as_fd()], 20);
                }
                Err(_) => return None,
            }
        }
    }

    fn text_of(&mut self, sel: Sel) -> String {
        let saved = self.save(sel);
        TEXT_TYPES
            .iter()
            .find_map(|t| saved.get(*t))
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    fn insert(&mut self, text: &str, mode: Mode) -> Result<Outcome, InjectError> {
        let app = focused_app();
        let terminal = is_terminal(&app);
        if mode == Mode::Type {
            let chars = self.type_text(text, terminal)?;
            return Ok(Outcome::Typed { app, chars });
        }
        let unknown = app.is_empty();

        let saved = self.save(Sel::Clipboard);
        let saved_primary = if unknown {
            self.save(Sel::Primary)
        } else {
            Offer::new()
        };
        self.own(Sel::Clipboard, text_offer(text))?;
        if unknown {
            self.own(Sel::Primary, text_offer(text))?;
        }
        // Apps prefetch the clipboard when it changes; give them a moment (see the X11 code).
        self.serve_for(Duration::from_millis(40))?;
        self.state.read_at = None;
        self.state.cancelled = false;
        let started = Instant::now();
        let mut read_at = self.paste_and_wait(terminal, unknown, Duration::from_millis(1500))?;
        if read_at.is_none() && !self.state.cancelled && mode == Mode::Auto {
            read_at = self.paste_and_wait(terminal, unknown, Duration::from_millis(700))?;
        }

        let outcome = match read_at {
            Some(t) => Outcome::Pasted {
                after: t.duration_since(started),
                terminal,
                app,
            },
            None if self.state.cancelled => {
                // Someone copied during the paste; leave their content alone.
                return Ok(Outcome::LeftOnClipboard { app });
            }
            None if mode == Mode::Auto => {
                tracing::info!(%app, "the app ignored the paste; typing the text instead");
                self.restore(saved, saved_primary, unknown)?;
                return match self.type_text(text, terminal) {
                    Ok(chars) => Ok(Outcome::Typed { app, chars }),
                    Err(e) => {
                        tracing::warn!(%app, "typing failed too: {e}");
                        self.own(Sel::Clipboard, text_offer(text))?;
                        Ok(Outcome::LeftOnClipboard { app })
                    }
                };
            }
            None => {
                tracing::warn!(%app, "no app read the dictated text; leaving it on the clipboard");
                return Ok(Outcome::LeftOnClipboard { app });
            }
        };
        self.restore(saved, saved_primary, unknown)?;
        Ok(outcome)
    }

    fn restore(&mut self, saved: Offer, primary: Offer, unknown: bool) -> Result<(), InjectError> {
        self.own(Sel::Clipboard, saved)?;
        if unknown {
            self.own(Sel::Primary, primary)?;
        }
        Ok(())
    }

    fn paste_and_wait(
        &mut self,
        terminal: bool,
        unknown: bool,
        limit: Duration,
    ) -> Result<Option<Instant>, InjectError> {
        let keys: Vec<u32> = if unknown {
            vec![self.code("Shift_L")?, self.code("Insert")?]
        } else if terminal {
            vec![
                self.code("Control_L")?,
                self.code("Shift_L")?,
                self.sym_code('v')?.0,
            ]
        } else {
            vec![self.code("Control_L")?, self.sym_code('v')?.0]
        };
        for &k in &keys {
            self.key(k, true)?;
        }
        for &k in keys.iter().rev() {
            self.key(k, false)?;
        }
        tracing::debug!(terminal, unknown, "paste key sent");
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline && !self.state.cancelled {
            // Keep serving a while after the first read: some apps ask several times.
            if self
                .state
                .read_at
                .is_some_and(|t| t.elapsed() > Duration::from_millis(300))
            {
                break;
            }
            self.pump(None, 20)?;
        }
        Ok(self.state.read_at)
    }

    fn key(&mut self, xkb_code: u32, down: bool) -> Result<(), InjectError> {
        let evdev = u16::try_from(xkb_code.saturating_sub(8)).unwrap_or(0);
        self.keys.evdev(evdev, down)
    }

    /// The key code of a named key such as "Control_L".
    fn code(&self, name: &str) -> Result<u32, InjectError> {
        let sym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
        self.find(sym)
            .map(|(c, _)| c)
            .ok_or_else(|| InjectError::Wayland(format!("no key for {name}")))
    }

    /// The key code (and whether Shift is needed) that types `ch` on the current layout.
    fn sym_code(&self, ch: char) -> Result<(u32, bool), InjectError> {
        let sym = match ch {
            '\n' => xkb::Keysym::Return,
            '\t' => xkb::Keysym::Tab,
            c => xkb::utf32_to_keysym(c as u32),
        };
        self.find(sym)
            .ok_or_else(|| InjectError::Wayland(format!("no key to type {ch:?}")))
    }

    fn find(&self, sym: xkb::Keysym) -> Option<(u32, bool)> {
        let km = self.state.keymap.as_ref()?;
        let mut found = None;
        let (min, max) = (km.min_keycode().raw(), km.max_keycode().raw());
        for code in min..=max {
            let kc = xkb::Keycode::new(code);
            for level in 0..km.num_levels_for_key(kc, 0).min(2) {
                if km.key_get_syms_by_level(kc, 0, level).contains(&sym) {
                    let hit = (code, level == 1);
                    // Prefer an unshifted key.
                    if level == 0 {
                        return Some(hit);
                    }
                    found.get_or_insert(hit);
                }
            }
        }
        found
    }

    fn type_text(&mut self, text: &str, terminal: bool) -> Result<usize, InjectError> {
        // A typed Return in a terminal would run the command, so never type one there.
        let text = if terminal {
            text.replace('\n', " ")
        } else {
            text.to_string()
        };
        // Check every character first, so nothing is half typed.
        let plan: Vec<(u32, bool)> = text
            .chars()
            .map(|c| self.sym_code(c))
            .collect::<Result<_, _>>()?;
        let shift = self.code("Shift_L")?;
        for &(code, shifted) in &plan {
            if shifted {
                self.key(shift, true)?;
            }
            self.key(code, true)?;
            self.key(code, false)?;
            if shifted {
                self.key(shift, false)?;
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(plan.len())
    }

    fn backspace(&mut self, n: usize) -> Result<(), InjectError> {
        let k = self.code("BackSpace")?;
        for _ in 0..n {
            self.key(k, true)?;
            self.key(k, false)?;
            thread::sleep(Duration::from_millis(3));
        }
        Ok(())
    }
}

// Wayland plumbing.

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            state.has_keyboard = caps.contains(wl_seat::Capability::Keyboard);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap {
            format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
            fd,
            size,
        } = event
        {
            state.keymap = read_keymap(fd, size as usize);
            tracing::debug!(ok = state.keymap.is_some(), "compositor keymap");
        }
    }
}

impl Dispatch<Manager, ()> for State {
    fn event(
        _: &mut Self,
        _: &Manager,
        _: <Manager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<Device, ()> for State {
    fn event(
        state: &mut Self,
        _: &Device,
        event: dc_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use dc_device::Event;
        match event {
            Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut state.clipboard, id) {
                    forget(state, &old);
                }
            }
            Event::PrimarySelection { id } => {
                if let Some(old) = std::mem::replace(&mut state.primary, id) {
                    forget(state, &old);
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, Device, [
        dc_device::EVT_DATA_OFFER_OPCODE => (DcOffer, ()),
    ]);
}

/// Drop an offer that is no longer the selection (unless it still is the other one).
fn forget(state: &mut State, offer: &DcOffer) {
    let still = [&state.clipboard, &state.primary]
        .iter()
        .any(|o| o.as_ref().is_some_and(|o| o.id() == offer.id()));
    if !still {
        state.offers.remove(&offer.id());
        offer.destroy();
    }
}

impl Dispatch<DcOffer, ()> for State {
    fn event(
        state: &mut Self,
        offer: &DcOffer,
        event: dc_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let dc_offer::Event::Offer { mime_type } = event {
            state.offers.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<Source, ()> for State {
    fn event(
        state: &mut Self,
        source: &Source,
        event: dc_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            dc_source::Event::Send { mime_type, fd } => {
                let Some((_, sel, offer)) =
                    state.serving.iter().find(|(s, _, _)| s.id() == source.id())
                else {
                    return;
                };
                if let Some(bytes) = offer.get(&mime_type) {
                    tracing::debug!(%mime_type, ?sel, "clipboard read");
                    state.read_at.get_or_insert_with(Instant::now);
                    // Write on a thread: a slow reader must not stall the service.
                    let bytes = bytes.clone();
                    let _ = thread::Builder::new()
                        .name("clip-send".into())
                        .spawn(move || {
                            let _ = std::fs::File::from(fd).write_all(&bytes);
                        });
                }
            }
            dc_source::Event::Cancelled => {
                state.serving.retain(|(s, _, _)| s.id() != source.id());
                source.destroy();
                state.cancelled = true;
            }
            _ => {}
        }
    }
}

