//! Text insertion on Windows.
//!
//! Like the Linux path: save the user's clipboard, offer our text, press Ctrl+V with
//! `SendInput`, wait until the app has actually read the text, then put the old clipboard
//! back. The text is offered with delayed rendering, so Windows asks Vaani for it at the
//! moment an app pastes (`WM_RENDERFORMAT`); that is how Vaani knows the paste landed. It
//! is also marked so that clipboard history and cloud clipboard skip it.
//!
//! "Never lose words": if no app reads it, the text is typed instead (`KEYEVENTF_UNICODE`,
//! so every script and emoji types regardless of keyboard layout).
//!
//! All clipboard work happens on one thread that owns a hidden message-only window, because
//! Windows sends clipboard requests to the owner window's thread.

#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded, unbounded};
use windows::Win32::Foundation::{
    CloseHandle, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, SetLastError,
    WIN32_ERROR, WPARAM,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_INSERT,
    VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, PostMessageW,
    RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROYCLIPBOARD,
    WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};
use windows::core::w;

use crate::{InjectError, Mode, Outcome, is_terminal};

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_LOCALE: u32 = 16;
const CF_DIBV5: u32 = 17;
const WM_WAKE: u32 = WM_APP + 1;
/// Marks the keys Vaani sends, so its own hotkey hook ignores them (vaani-hotkey checks the
/// same value).
const VAANI_KEY_TAG: usize = 0x5641_414E;

fn err<E: std::fmt::Display>(e: E) -> InjectError {
    InjectError::Win32(e.to_string())
}

enum Cmd {
    Insert(String, Mode, Sender<Result<Outcome, InjectError>>),
    SetClipboard(String),
    FocusedApp(Sender<String>),
    Backspace(usize, Sender<Result<(), InjectError>>),
    ReadClipboard(Sender<String>),
    ReadSelection(Sender<String>),
}

#[derive(Clone)]
pub struct Injector {
    tx: Sender<Cmd>,
    /// The service thread's window, woken with a posted message after each command.
    hwnd: isize,
}

/// Whether the compositor offers a data-control clipboard (Linux only).
pub fn wayland_clipboard() -> bool {
    false
}

/// Whether /dev/uinput can be opened (Linux only).
pub fn uinput_usable() -> bool {
    false
}

impl Injector {
    pub fn start() -> Result<Self, InjectError> {
        let (tx, rx) = unbounded::<Cmd>();
        let (ready_tx, ready_rx) = bounded(1);
        thread::Builder::new()
            .name("inject".into())
            .spawn(move || {
                let hwnd = match create_window() {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                tracing::info!("text insertion: Windows clipboard and SendInput");
                let _ = ready_tx.send(Ok(hwnd.0 as isize));
                let service = Service::new(hwnd);
                let mut msg = MSG::default();
                // SAFETY: standard message loop on the thread that owns `hwnd`.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                    if msg.message == WM_WAKE {
                        while let Ok(cmd) = rx.try_recv() {
                            service.handle(cmd);
                        }
                        continue;
                    }
                    // SAFETY: as above.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .map_err(err)?;
        let hwnd = ready_rx.recv().map_err(|_| InjectError::Gone)??;
        Ok(Self { tx, hwnd })
    }

    fn send(&self, cmd: Cmd) -> Result<(), InjectError> {
        self.tx.send(cmd).map_err(|_| InjectError::Gone)?;
        // SAFETY: posting a message to a window owned by our service thread.
        unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_WAKE,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(err)
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

    /// Press Backspace `n` times in the focused window ("scratch that").
    pub fn backspace(&self, n: usize) -> Result<(), InjectError> {
        let (tx, rx) = bounded(1);
        self.send(Cmd::Backspace(n, tx))?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| InjectError::Win32("backspace timed out".into()))?
    }

    /// The clipboard's text, or an empty string (for snippets with `{clipboard}`).
    pub fn clipboard_text(&self) -> String {
        let (tx, rx) = bounded(1);
        if self.send(Cmd::ReadClipboard(tx)).is_err() {
            return String::new();
        }
        rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()
    }

    /// The text currently selected in the focused app, or empty. Windows has no primary
    /// selection, so this copies it (Ctrl+C) and puts the clipboard back afterwards.
    pub fn selection_text(&self) -> String {
        let (tx, rx) = bounded(1);
        if self.send(Cmd::ReadSelection(tx)).is_err() {
            return String::new();
        }
        rx.recv_timeout(Duration::from_secs(3)).unwrap_or_default()
    }

    /// The focused app's program name, lowercased without ".exe" (e.g. "chrome", "winword").
    pub fn focused_app(&self) -> String {
        let (tx, rx) = bounded(1);
        if self.send(Cmd::FocusedApp(tx)).is_err() {
            return String::new();
        }
        rx.recv_timeout(Duration::from_millis(500))
            .unwrap_or_default()
    }
}

thread_local! {
    /// The text offered with delayed rendering, until an app asks for it.
    static PENDING: RefCell<Option<Vec<u16>>> = const { RefCell::new(None) };
    /// When an app last asked for the pending text.
    static READ_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Someone else took the clipboard while we owned it.
    static LOST: Cell<bool> = const { Cell::new(false) };
}

fn create_window() -> Result<HWND, InjectError> {
    // SAFETY: registering a window class and creating a message-only window with it; the
    // class name and window procedure live for the whole program.
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).map_err(err)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: w!("VaaniClipboard"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("VaaniClipboard"),
            w!("Vaani"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
        .map_err(err)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_RENDERFORMAT => {
            if wparam.0 as u32 == CF_UNICODETEXT {
                let text = PENDING.with(|p| p.borrow().clone());
                if let Some(text) = text {
                    if let Some(h) = global_from(bytes_of(&text)) {
                        // SAFETY: inside WM_RENDERFORMAT the clipboard is already open for
                        // us; on success Windows owns the memory.
                        if unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))) }.is_err() {
                            // SAFETY: not handed over, so it is still ours to free.
                            let _ = unsafe { GlobalFree(Some(h)) };
                        }
                    }
                    READ_AT.with(|r| r.set(Some(Instant::now())));
                }
            }
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            // Our window is going away while still owning delayed text: render it now.
            let text = PENDING.with(|p| p.borrow_mut().take());
            // SAFETY: open, check we still own it, put the text, close.
            unsafe {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if GetClipboardOwner().is_ok_and(|o| o == hwnd) {
                        if let Some(h) = text.and_then(|t| global_from(bytes_of(&t))) {
                            let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0)));
                        }
                    }
                    let _ = CloseClipboard();
                }
            }
            LRESULT(0)
        }
        WM_DESTROYCLIPBOARD => {
            LOST.with(|l| l.set(true));
            PENDING.with(|p| *p.borrow_mut() = None);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn bytes_of(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

fn utf16z(text: &str) -> Vec<u16> {
    // Windows apps expect CRLF line ends in clipboard text.
    text.replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// A movable global memory block holding `bytes`.
fn global_from(bytes: Vec<u8>) -> Option<HGLOBAL> {
    // SAFETY: allocate, copy within bounds, unlock.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).ok()?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

/// Copy the contents of a clipboard memory handle.
fn read_global(h: HANDLE) -> Option<Vec<u8>> {
    let g = HGLOBAL(h.0);
    // SAFETY: the handle came from GetClipboardData while the clipboard is open; we only
    // read `GlobalSize` bytes from the locked block.
    unsafe {
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(v)
    }
}

/// The formats that are worth putting back: text, rich text, HTML, images and copied files.
/// Others (GDI handles, private formats, the ones Windows synthesizes) are skipped; saving
/// every format would also make some apps render huge data on the spot.
fn wanted_formats() -> Vec<u32> {
    let mut v = vec![CF_UNICODETEXT, CF_LOCALE, CF_DIBV5, CF_DIB, CF_HDROP];
    for name in [
        w!("HTML Format"),
        w!("Rich Text Format"),
        w!("PNG"),
        w!("Preferred DropEffect"),
    ] {
        // SAFETY: registering a named format; returns 0 on failure.
        let id = unsafe { RegisterClipboardFormatW(name) };
        if id != 0 {
            v.push(id);
        }
    }
    v
}

type Saved = Vec<(u32, Vec<u8>)>;

/// Up to 64 MB of saved clipboard (a large copied image fits).
const SAVE_LIMIT: usize = 64 << 20;

struct Service {
    hwnd: HWND,
    wanted: Vec<u32>,
    /// Formats that keep our text out of clipboard history and the cloud clipboard.
    private_formats: Vec<u32>,
}

impl Service {
    fn new(hwnd: HWND) -> Self {
        let private_formats = [
            w!("ExcludeClipboardContentFromMonitorProcessing"),
            w!("CanIncludeInClipboardHistory"),
            w!("CanUploadToCloudClipboard"),
        ]
        .into_iter()
        // SAFETY: registering named formats.
        .map(|n| unsafe { RegisterClipboardFormatW(n) })
        .filter(|id| *id != 0)
        .collect();
        Self {
            hwnd,
            wanted: wanted_formats(),
            private_formats,
        }
    }

    fn handle(&self, cmd: Cmd) {
        match cmd {
            Cmd::Insert(text, mode, reply) => {
                let _ = reply.send(self.insert(&text, mode));
            }
            Cmd::SetClipboard(text) => {
                if let Err(e) = self.put_text(&text) {
                    tracing::warn!("could not set the clipboard: {e}");
                }
            }
            Cmd::FocusedApp(reply) => {
                let _ = reply.send(focused_app());
            }
            Cmd::Backspace(n, reply) => {
                let _ = reply.send(self.backspace(n));
            }
            Cmd::ReadClipboard(reply) => {
                let _ = reply.send(self.read_text());
            }
            Cmd::ReadSelection(reply) => {
                let _ = reply.send(self.selection());
            }
        }
    }

    /// Open the clipboard, retrying while another app holds it. Clipboard monitors (history,
    /// Remote Desktop, sync tools) open it right after every change, so allow them a second.
    fn open(&self) -> Result<Clip, InjectError> {
        let mut last = None;
        for _ in 0..100 {
            // SAFETY: opening with our own window as the owner.
            match unsafe { OpenClipboard(Some(self.hwnd)) } {
                Ok(()) => return Ok(Clip),
                Err(e) => last = Some(e),
            }
            self.pump_for(Duration::from_millis(10));
        }
        Err(InjectError::Win32(format!(
            "the clipboard is busy: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    fn save(&self) -> Saved {
        let Ok(_clip) = self.open() else {
            return Vec::new();
        };
        let mut present = Vec::new();
        let mut f = 0u32;
        loop {
            // SAFETY: enumerating while the clipboard is open.
            f = unsafe { EnumClipboardFormats(f) };
            if f == 0 {
                break;
            }
            present.push(f);
        }
        let mut saved = Vec::new();
        let mut total = 0usize;
        for f in self.wanted.iter().copied().filter(|f| present.contains(f)) {
            // The DIB is synthesized from the DIBV5 (or the other way round): one is enough.
            if f == CF_DIB && saved.iter().any(|(s, _)| *s == CF_DIBV5) {
                continue;
            }
            // SAFETY: reading a format we saw listed, while the clipboard is open.
            let Ok(h) = (unsafe { GetClipboardData(f) }) else {
                continue;
            };
            if let Some(bytes) = read_global(h) {
                total += bytes.len();
                if total > SAVE_LIMIT {
                    tracing::debug!("clipboard too large to save in full");
                    break;
                }
                saved.push((f, bytes));
            }
        }
        saved
    }

    /// Replace the clipboard with `saved` (which may be empty).
    fn restore(&self, saved: Saved) -> Result<(), InjectError> {
        PENDING.with(|p| *p.borrow_mut() = None);
        let _clip = self.open()?;
        // SAFETY: the clipboard is open; each handle is handed to Windows on success.
        unsafe {
            EmptyClipboard().map_err(err)?;
            for (f, bytes) in saved {
                if let Some(h) = global_from(bytes) {
                    if SetClipboardData(f, Some(HANDLE(h.0))).is_err() {
                        let _ = GlobalFree(Some(h));
                    }
                }
            }
        }
        LOST.with(|l| l.set(false));
        Ok(())
    }

    fn put_private_markers(&self) {
        for f in &self.private_formats {
            if let Some(h) = global_from(0u32.to_le_bytes().to_vec()) {
                // SAFETY: clipboard is open (callers hold a Clip).
                if unsafe { SetClipboardData(*f, Some(HANDLE(h.0))) }.is_err() {
                    // SAFETY: still ours.
                    let _ = unsafe { GlobalFree(Some(h)) };
                }
            }
        }
    }

    /// Offer `text` with delayed rendering, so we see when an app reads it.
    fn offer(&self, text: &str) -> Result<(), InjectError> {
        READ_AT.with(|r| r.set(None));
        let _clip = self.open()?;
        // SAFETY: clipboard open; a None handle asks for delayed rendering.
        unsafe {
            EmptyClipboard().map_err(err)?;
            // Only now: if we owned the clipboard, emptying it sent us WM_DESTROYCLIPBOARD,
            // which clears PENDING.
            PENDING.with(|p| *p.borrow_mut() = Some(utf16z(text)));
            // Delayed rendering returns a null handle on success too, which the bindings
            // report as an error: only a real error code counts.
            SetLastError(WIN32_ERROR(0));
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, None) {
                if e.code().is_err() {
                    return Err(err(e));
                }
            }
        }
        self.put_private_markers();
        LOST.with(|l| l.set(false));
        Ok(())
    }

    /// Put `text` on the clipboard right away (no delayed rendering).
    fn put_text(&self, text: &str) -> Result<(), InjectError> {
        PENDING.with(|p| *p.borrow_mut() = None);
        let _clip = self.open()?;
        let h = global_from(bytes_of(&utf16z(text)))
            .ok_or_else(|| InjectError::Win32("out of memory".into()))?;
        // SAFETY: clipboard open; Windows owns the memory on success.
        unsafe {
            EmptyClipboard().map_err(err)?;
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))) {
                let _ = GlobalFree(Some(h));
                return Err(err(e));
            }
        }
        Ok(())
    }

    fn read_text(&self) -> String {
        let Ok(_clip) = self.open() else {
            return String::new();
        };
        // SAFETY: clipboard open.
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
                return String::new();
            }
            let Ok(h) = GetClipboardData(CF_UNICODETEXT) else {
                return String::new();
            };
            read_global(h).map(|b| text_from(&b)).unwrap_or_default()
        }
    }

    /// Dispatch window messages (clipboard requests) for `dur`.
    fn pump_for(&self, dur: Duration) {
        let end = Instant::now() + dur;
        loop {
            pump();
            if Instant::now() >= end {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn insert(&self, text: &str, mode: Mode) -> Result<Outcome, InjectError> {
        let app = focused_app();
        let terminal = is_terminal(&app);
        if mode == Mode::Type {
            wait_modifiers_released();
            let chars = type_text(text)?;
            return Ok(Outcome::Typed { app, chars });
        }

        // 1. Save the user's clipboard.
        let saved = self.save();

        // 2. Offer our text. Some apps read the clipboard as soon as it changes and paste
        //    from that copy, so give them a moment before pressing paste.
        self.offer(text)?;
        self.pump_for(Duration::from_millis(40));
        let prefetched = READ_AT.with(|r| r.take()).is_some();
        wait_modifiers_released();

        // 3. Paste, and wait for the app to ask for the text.
        let started = Instant::now();
        send_paste(&app)?;
        let mut read_at = self.wait_read(Duration::from_millis(1500));
        if read_at.is_none() && !prefetched && mode == Mode::Auto && !lost() {
            // An app that has only just opened can drop the first paste key.
            self.pump_for(Duration::from_millis(150));
            send_paste(&app)?;
            read_at = self.wait_read(Duration::from_millis(700));
        }

        let read_at = match read_at {
            Some(t) => t,
            None if prefetched => {
                self.pump_for(Duration::from_millis(400));
                tracing::debug!(%app, "no read after paste; assuming the app used its prefetched copy");
                Instant::now()
            }
            None if lost() => {
                // Someone copied during the paste; leave their content alone.
                return Ok(Outcome::Pasted {
                    after: started.elapsed(),
                    terminal,
                    app,
                });
            }
            None if mode == Mode::Auto => {
                tracing::info!(%app, "the app ignored the paste; typing the text instead");
                let typed = type_text(text);
                if let Err(e) = self.restore(saved) {
                    tracing::warn!("could not put the previous clipboard back: {e}");
                }
                return match typed {
                    Ok(chars) => Ok(Outcome::Typed { app, chars }),
                    Err(e) => {
                        tracing::warn!(%app, "typing failed too: {e}");
                        self.put_text(text)?;
                        Ok(Outcome::LeftOnClipboard { app })
                    }
                };
            }
            None => {
                tracing::warn!(%app, "no app read the dictated text; leaving it on the clipboard");
                self.put_text(text)?;
                return Ok(Outcome::LeftOnClipboard { app });
            }
        };
        let after = read_at.duration_since(started);

        // 4. Put the old clipboard back once the app has its copy.
        // The text is in the app by now, so a failure here only costs the old clipboard.
        self.pump_for(Duration::from_millis(150));
        if !lost() {
            if let Err(e) = self.restore(saved) {
                tracing::warn!("could not put the previous clipboard back: {e}");
            }
        }
        Ok(Outcome::Pasted {
            after,
            terminal,
            app,
        })
    }

    fn wait_read(&self, limit: Duration) -> Option<Instant> {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            pump();
            if let Some(t) = READ_AT.with(|r| r.get()) {
                return Some(t);
            }
            if lost() {
                return None;
            }
            thread::sleep(Duration::from_millis(1));
        }
        None
    }

    fn backspace(&self, n: usize) -> Result<(), InjectError> {
        wait_modifiers_released();
        for _ in 0..n {
            key(VK_BACK, true)?;
            key(VK_BACK, false)?;
            thread::sleep(Duration::from_millis(3));
        }
        Ok(())
    }

    /// Copy the selection with Ctrl+C, read it, and put the clipboard back.
    fn selection(&self) -> String {
        // Ctrl+C interrupts the running program in a console instead of copying.
        if is_terminal(&focused_app()) {
            return String::new();
        }
        let saved = self.save();
        // SAFETY: a plain query.
        let before = unsafe { GetClipboardSequenceNumber() };
        wait_modifiers_released();
        if chord(&[VK_CONTROL], VIRTUAL_KEY(u16::from(b'C'))).is_err() {
            return String::new();
        }
        let end = Instant::now() + Duration::from_millis(600);
        let mut changed = false;
        while Instant::now() < end {
            pump();
            // SAFETY: a plain query.
            if unsafe { GetClipboardSequenceNumber() } != before {
                changed = true;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if !changed {
            return String::new();
        }
        // Let the app finish putting all its formats up.
        self.pump_for(Duration::from_millis(30));
        let text = self.read_text();
        let _ = self.restore(saved);
        text
    }
}

/// Holds the clipboard open; closes it when dropped.
struct Clip;

impl Drop for Clip {
    fn drop(&mut self) {
        // SAFETY: only created after a successful OpenClipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

fn lost() -> bool {
    LOST.with(|l| l.get())
}

fn pump() {
    let mut msg = MSG::default();
    // SAFETY: dispatching messages for this thread's windows. WM_WAKE is left for the
    // main loop (it is only posted, so it is never removed here).
    unsafe {
        while PeekMessageW(&mut msg, None, 0, WM_APP, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn text_from(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units).replace("\r\n", "\n")
}

/// The focused window's program name, lowercased without ".exe".
fn focused_app() -> String {
    // SAFETY: plain queries; the process handle is closed before returning.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(process);
        if !ok {
            return String::new();
        }
        exe_stem(&String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn exe_stem(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let name = name.to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn keyboard(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: VAANI_KEY_TAG,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), InjectError> {
    // SAFETY: a slice of fully initialised INPUT structs.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(InjectError::Win32(
            "SendInput was blocked (is the focused app running as administrator?)".into(),
        ))
    }
}

fn key(vk: VIRTUAL_KEY, down: bool) -> Result<(), InjectError> {
    let flags = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    send(&[keyboard(vk, 0, flags)])
}

/// Press `mods`, tap `k`, release `mods`.
fn chord(mods: &[VIRTUAL_KEY], k: VIRTUAL_KEY) -> Result<(), InjectError> {
    let mut v: Vec<INPUT> = mods
        .iter()
        .map(|m| keyboard(*m, 0, KEYBD_EVENT_FLAGS(0)))
        .collect();
    v.push(keyboard(k, 0, KEYBD_EVENT_FLAGS(0)));
    v.push(keyboard(k, 0, KEYEVENTF_KEYUP));
    v.extend(mods.iter().rev().map(|m| keyboard(*m, 0, KEYEVENTF_KEYUP)));
    send(&v)
}

/// Ctrl+V, or Shift+Insert in mintty (Git Bash), which doesn't paste on Ctrl+V.
fn send_paste(app: &str) -> Result<(), InjectError> {
    if app == "mintty" {
        chord(&[VK_SHIFT], VK_INSERT)
    } else {
        chord(&[VK_CONTROL], VIRTUAL_KEY(u16::from(b'V')))
    }
}

/// Wait (up to 400 ms) until Shift, Ctrl, Alt and Windows are up, so a key still held from
/// the hotkey doesn't turn the paste into another shortcut.
fn wait_modifiers_released() {
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        let held = [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            // SAFETY: a plain query; the high bit means "down".
            .any(|k| unsafe { GetAsyncKeyState(i32::from(k.0)) } < 0);
        if !held {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Type `text` key by key as Unicode characters, independent of the keyboard layout.
fn type_text(text: &str) -> Result<usize, InjectError> {
    let mut chars = 0;
    for c in text.replace("\r\n", "\n").chars() {
        match c {
            '\n' => chord(&[], VK_RETURN)?,
            '\t' => chord(&[], VK_TAB)?,
            c => {
                let mut buf = [0u16; 2];
                let units = c.encode_utf16(&mut buf);
                let mut v = Vec::with_capacity(4);
                for u in units.iter() {
                    v.push(keyboard(VIRTUAL_KEY(0), *u, KEYEVENTF_UNICODE));
                }
                for u in units.iter() {
                    v.push(keyboard(
                        VIRTUAL_KEY(0),
                        *u,
                        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    ));
                }
                send(&v)?;
            }
        }
        chars += 1;
        thread::sleep(Duration::from_millis(2));
    }
    Ok(chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_names() {
        assert_eq!(
            exe_stem(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            "chrome"
        );
        assert_eq!(
            exe_stem(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.EXE"),
            "powershell"
        );
        assert!(is_terminal(&exe_stem(
            r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal\WindowsTerminal.exe"
        )));
    }

    /// Offer text with delayed rendering, read it as an app would, then restore what was
    /// there before. Uses the real clipboard, so it runs on a desktop session only.
    #[test]
    fn offer_read_restore() {
        let hwnd = create_window().expect("window");
        let svc = Service::new(hwnd);
        svc.put_text("before").expect("put");
        let saved = svc.save();
        svc.offer("dictated ✓").expect("offer");
        assert_eq!(svc.read_text(), "dictated ✓");
        assert!(
            READ_AT.with(|r| r.get()).is_some(),
            "the read was not noticed"
        );
        svc.restore(saved).expect("restore");
        assert_eq!(svc.read_text(), "before");
    }

    #[test]
    fn clipboard_text_round_trip() {
        let units = utf16z("a\nb ✓ 😀");
        assert_eq!(text_from(&bytes_of(&units)), "a\nb ✓ 😀");
        assert!(bytes_of(&units).ends_with(&[0, 0]));
    }
}
