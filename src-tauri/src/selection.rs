//! Grabs the text selected in whatever app was in front when the quick-add
//! hotkey was pressed: sends Ctrl+Insert (copy — unlike Ctrl+C it never
//! interrupts a program in a terminal), reads the clipboard if it changed,
//! then puts the user's previous clipboard contents back.
//!
//! The user's clipboard must never be lost:
//! - if it can't be saved (busy, too big, too slow to render), nothing is
//!   copied at all and the popup opens empty;
//! - if the copy doesn't arrive in time (nothing selected, or a slow app such
//!   as remote desktop), the saved clipboard stays pending for 1.5 s and is
//!   put back if that app's copy lands late;
//! - while such a copy is pending, the hotkey doesn't copy again, so there is
//!   never more than one copy in flight.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, GlobalFree, HANDLE, HGLOBAL, HWND},
        Graphics::Gdi::{CopyEnhMetaFileW, DeleteEnhMetaFile, HENHMETAFILE},
        System::{
            DataExchange::{
                CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
                GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
                KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_CONTROL,
                VK_INSERT, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
            },
            WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsHungAppWindow, IsWindow},
        },
    },
};

const CF_UNICODETEXT: u32 = 13;
const CF_ENHMETAFILE: u32 = 14;
/// Formats whose handle isn't an HGLOBAL. CF_BITMAP is covered by the CF_DIB
/// Windows synthesizes; CF_ENHMETAFILE is copied as a metafile.
const NON_MEMORY_FORMATS: [u32; 7] = [2, 3, 9, 0x80, 0x82, 0x83, 0x8E];
/// Window classes of consoles/terminals: nothing is ever sent there.
const CONSOLE_CLASSES: [&str; 3] = ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS", "PseudoConsoleWindow"];
const MAX_CHARS: usize = 500;
/// Saving the clipboard renders delayed formats (a big Excel range…); past
/// these limits we don't touch the clipboard at all.
const SAVE_BUDGET: Duration = Duration::from_millis(600);
const SAVE_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Modifiers a copy has to step around, left and right apart, and whether
/// each is an extended key.
const MODIFIERS: [(VIRTUAL_KEY, bool); 8] = [
    (VK_LMENU, false),
    (VK_RMENU, true),
    (VK_LCONTROL, false),
    (VK_RCONTROL, true),
    (VK_LSHIFT, false),
    (VK_RSHIFT, false),
    (VK_LWIN, true),
    (VK_RWIN, true),
];
/// Unassigned virtual key used as a mask (see send_copy).
const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
/// How long the copy we sent is waited for before the popup opens without it.
const COPY_WAIT: Duration = Duration::from_millis(250);
/// How long the popup waits to read the copied text (the clipboard restore
/// itself goes on longer, in the background).
const READ_WAIT: Duration = Duration::from_millis(600);
/// How long a late copy from a slow app is still expected.
const LATE_WINDOW: Duration = Duration::from_millis(1500);

/// Our copy of an enhanced metafile; freed unless handed to the clipboard.
struct Emf(HENHMETAFILE);
// A metafile handle is process-wide and only used by one thread at a time.
unsafe impl Send for Emf {}
impl Drop for Emf {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteEnhMetaFile(Some(self.0));
        }
    }
}

enum Saved {
    Memory(u32, Vec<u8>),
    Metafile(Emf),
}

/// The user's clipboard, saved by a capture whose copy didn't arrive in time.
struct Pending {
    saved: Vec<Saved>,
    generation: u64,
}
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
/// Captures and late restores never interleave.
static LOCK: Mutex<()> = Mutex::new(());
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// A restore is still being retried in the background (clipboard was busy):
/// no new capture may touch the clipboard until it's done.
static RESTORING: AtomicBool = AtomicBool::new(false);

/// When a deferred restore may still write: checked with the clipboard open,
/// so nothing can change between the check and the write.
#[derive(Clone, Copy)]
enum Check {
    /// The clipboard still holds exactly what it held at this sequence number.
    Unchanged(u32),
    /// The current content is the late copy of this process (see `is_late_copy`).
    LateCopyFrom(u32),
    /// Same, and nothing has been written since this sequence number (a
    /// deferred restore must not approve content that arrived later).
    LateCopyAt(u32, u32),
}

impl Check {
    fn holds(self) -> bool {
        let seq_now = || unsafe { GetClipboardSequenceNumber() };
        match self {
            Check::Unchanged(seq) => seq_now() == seq,
            Check::LateCopyFrom(target) => is_late_copy(target),
            Check::LateCopyAt(target, seq) => seq_now() == seq && is_late_copy(target),
        }
    }
}

/// Selected text of the foreground app, if any, or why there's none (for the
/// hotkey log). Blocks for up to ~1.3 s (usually a few hundred ms), so call it
/// off the main thread.
/// `owner` is one of our windows (needed to restore the clipboard).
pub fn capture(owner: HWND) -> Result<String, &'static str> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || is_ours(fg) || is_console(fg) {
        return Err("no app to copy from");
    }
    let _serial = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if RESTORING.load(Ordering::SeqCst) || PENDING.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return Err("previous copy unsettled"); // don't start another
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let target = pid_of(fg);
    // No waiting for the hotkey's Alt to be let go: send_copy works around
    // whatever is still held.
    if clipboard_owner_hung() {
        return Err("clipboard owner hung"); // reading its data would block
    }

    // No copy unless the user's clipboard is safely saved.
    let (saved, saved_seq) = with_clipboard(owner, Duration::from_millis(300), save_all).flatten().ok_or("clipboard not saved")?;
    // The window may have changed while we waited; never send keys elsewhere.
    let now_fg = unsafe { GetForegroundWindow() };
    if now_fg != fg || is_console(now_fg) {
        return Err("app in front changed");
    }
    let before = unsafe { GetClipboardSequenceNumber() };
    if before != saved_seq {
        return Err("clipboard changed meanwhile"); // our copy would be stale
    }
    send_copy();
    // Apps answer a copy within tens of ms; nothing by now usually means
    // nothing was selected (a slow app's late copy is still handled below).
    let deadline = Instant::now() + COPY_WAIT;
    while unsafe { GetClipboardSequenceNumber() } == before {
        if Instant::now() > deadline {
            *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending { saved, generation });
            let owner = owner.0 as isize;
            thread::spawn(move || late_restore(owner, before, generation, target));
            return Err("nothing copied");
        }
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(30)); // let the app finish writing all formats

    // Read the selection and put the user's clipboard back in one session:
    // nothing can slip in between. A clipboard manager may hold the clipboard
    // just after a copy, so the session runs on its own thread with the full
    // patience, while the popup waits for the text only READ_WAIT; a later
    // read still restores the clipboard, its text is just dropped. New
    // captures wait until it's done (RESTORING).
    // Once the popup has opened without it, the user may copy something new:
    // from then on the session restores only over our own copy, unchanged.
    RESTORING.store(true, Ordering::SeqCst);
    let copied = unsafe { GetClipboardSequenceNumber() };
    let gave_up = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let owner_raw = owner.0 as isize;
    let late = gave_up.clone();
    thread::spawn(move || {
        let owner = HWND(owner_raw as *mut _);
        let mut slot = Some(saved);
        let text = with_clipboard(owner, Duration::from_millis(2500), |_| {
            let text = read_selection(());
            let saved = slot.take().expect("one session");
            if !late.load(Ordering::SeqCst) || Check::Unchanged(copied).holds() {
                restore_all(saved);
            } // else: the user's newer copy stays; ours is dropped
            text
        })
        .flatten();
        let _ = tx.send(text);
        match slot {
            // Clipboard stayed busy: restore later, but only over that same copy
            // (restore_later clears RESTORING when it's done).
            Some(saved) => restore_later(owner, saved, Check::Unchanged(unsafe { GetClipboardSequenceNumber() })),
            None => RESTORING.store(false, Ordering::SeqCst),
        }
    });
    let text = match rx.recv_timeout(READ_WAIT) {
        Ok(text) => text,
        Err(_) => {
            gave_up.store(true, Ordering::SeqCst);
            return Err("clipboard busy");
        }
    };

    let text = text.ok_or("copy not readable")?.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("empty selection");
    }
    Ok(text.chars().take(MAX_CHARS).collect())
}

/// Settles the pending clipboard: if the target app's copy lands within the
/// late window, put the user's clipboard back; otherwise (nothing was
/// selected, or the user copied something themselves) leave it alone. The
/// first change settles it: after a copy of the user's own, their new content
/// is the clipboard to keep.
fn late_restore(owner: isize, before: u32, generation: u64, target: u32) {
    let until = Instant::now() + LATE_WINDOW;
    loop {
        let changed = unsafe { GetClipboardSequenceNumber() } != before;
        if changed || Instant::now() > until {
            // Ctrl physically down when the change shows up: the user's own copy.
            let user_copy = changed && key_down(VK_CONTROL);
            let _serial = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let mut slot = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            if !slot.as_ref().is_some_and(|p| p.generation == generation) {
                return;
            }
            let pending = slot.take().expect("checked above");
            drop(slot);
            if changed && !user_copy {
                // Opening the clipboard waits for the writer to finish; the
                // check and the write then happen in that same session.
                let owner = HWND(owner as *mut _);
                let check = Check::LateCopyFrom(target);
                let mut data = Some(pending.saved);
                let opened = with_clipboard(owner, Duration::from_millis(2500), |_| {
                    let saved = data.take().expect("one session");
                    if check.holds() {
                        restore_all(saved);
                    }
                });
                if let (None, Some(saved)) = (opened, data) {
                    let seq = unsafe { GetClipboardSequenceNumber() };
                    restore_later(owner, saved, Check::LateCopyAt(target, seq));
                }
            }
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Whether the clipboard change is the target app's late copy rather than the
/// user's own. Some apps copy without an owner window or through a helper
/// process; while our popup is in front the user can only copy inside it, and
/// those copies come from our own processes (WebView2 runs as our children).
fn is_late_copy(target: u32) -> bool {
    if unsafe { CountClipboardFormats() } == 0 {
        return false; // a clear (e.g. a password manager wiping it) is not a copy
    }
    let me = std::process::id();
    let owner = unsafe { GetClipboardOwner() }.ok().map(pid_of).filter(|&p| p != 0);
    if owner.is_some_and(|p| p == me || parent_pid(p) == Some(me)) {
        return false;
    }
    let front = pid_of(unsafe { GetForegroundWindow() });
    owner == Some(target) || owner.is_none() || front == me || front == target
}

fn parent_pid(pid: u32) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            if entry.th32ProcessID == pid {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn is_ours(hwnd: HWND) -> bool {
    pid_of(hwnd) == std::process::id()
}

fn is_console(hwnd: HWND) -> bool {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    CONSOLE_CLASSES.contains(&class.as_str())
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

/// The clipboard's owner doesn't respond: asking it for delayed data would hang.
fn clipboard_owner_hung() -> bool {
    unsafe { GetClipboardOwner() }.is_ok_and(|w| !w.0.is_null() && unsafe { IsHungAppWindow(w) }.as_bool())
}

fn key(vk: VIRTUAL_KEY, up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// A key with its scan code (for modifiers: the specific left/right one), so
/// Windows and apps that read scan codes take it for that very key.
fn modifier(vk: VIRTUAL_KEY, extended: bool, up: bool) -> INPUT {
    let mut input = key(vk, up, extended);
    input.Anonymous.ki.wScan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
    input
}

/// Ctrl+Insert, "blind" to the modifiers the user still holds (people keep
/// Alt down a while after N; a copy then would be Alt+Ctrl+Insert): in one
/// SendInput, never interleaved with real keys, the held ones are let go,
/// the copy sent, and they're pressed again, so Windows' idea of the keyboard
/// matches the user's hands (a second Alt+N is still the hotkey). Mask keys
/// around it keep any Alt or Win press/release pair from opening a menu or
/// Start.
fn send_copy() {
    let held: Vec<(VIRTUAL_KEY, bool)> = MODIFIERS.into_iter().filter(|&(vk, _)| key_down(vk)).collect();
    let mut inputs = vec![key(MASK_KEY, false, false), key(MASK_KEY, true, false)];
    inputs.extend(held.iter().map(|&(vk, ext)| modifier(vk, ext, true)));
    inputs.extend([
        modifier(VK_LCONTROL, false, false),
        // with its scan code too: remote desktop and VM clients read those
        modifier(VK_INSERT, true, false),
        modifier(VK_INSERT, true, true),
        modifier(VK_LCONTROL, false, true),
    ]);
    inputs.extend(held.iter().map(|&(vk, ext)| modifier(vk, ext, false)));
    inputs.extend([key(MASK_KEY, false, false), key(MASK_KEY, true, false)]);
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

/// Runs `f` with the clipboard open, retrying (with backoff) while another
/// app holds it, for up to `patience`.
fn with_clipboard<T>(owner: HWND, patience: Duration, f: impl FnOnce(()) -> T) -> Option<T> {
    let deadline = Instant::now() + patience;
    let mut pause = Duration::from_millis(5);
    loop {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            let out = f(());
            unsafe {
                let _ = CloseClipboard();
            }
            return Some(out);
        }
        if Instant::now() > deadline {
            return None;
        }
        thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(80));
    }
}

/// The clipboard stayed busy (clipboard managers grab it right after a copy):
/// keep trying in the background for a while, writing only if `check` still
/// holds once it's open. New captures wait until this is settled.
fn restore_later(owner: HWND, saved: Vec<Saved>, check: Check) {
    RESTORING.store(true, Ordering::SeqCst);
    let owner = owner.0 as isize;
    thread::spawn(move || {
        let owner = HWND(owner as *mut _);
        let mut data = Some(saved);
        for _ in 0..10 {
            let opened = with_clipboard(owner, Duration::from_millis(1000), |_| {
                let saved = data.take().expect("one session");
                if check.holds() {
                    restore_all(saved);
                } // else: someone copied something newer — keep that, drop ours
            });
            if opened.is_some() {
                break;
            }
        }
        RESTORING.store(false, Ordering::SeqCst);
    });
}

fn global_bytes(h: HANDLE) -> Option<Vec<u8>> {
    let g = HGLOBAL(h.0);
    unsafe {
        let size = GlobalSize(g);
        let ptr = GlobalLock(g) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(bytes)
    }
}

/// `None` unless the whole clipboard could be copied safely: too big, too slow,
/// formats that aren't plain memory (private/GDI ranges), or a format its live
/// owner failed to deliver — in all those cases we won't touch the clipboard.
fn save_all(_: ()) -> Option<(Vec<Saved>, u32)> {
    let started = Instant::now();
    let mut out = Vec::new();
    let mut total = 0usize;
    let mut failed = false;
    let mut fmt = 0u32;
    loop {
        fmt = unsafe { EnumClipboardFormats(fmt) };
        if fmt == 0 {
            break;
        }
        if started.elapsed() > SAVE_BUDGET || total > SAVE_MAX_BYTES || (0x200..=0x3FF).contains(&fmt) {
            return None;
        }
        if NON_MEMORY_FORMATS.contains(&fmt) {
            continue;
        }
        let Ok(h) = (unsafe { GetClipboardData(fmt) }) else {
            failed = true;
            continue;
        };
        if fmt == CF_ENHMETAFILE {
            let copy = unsafe { CopyEnhMetaFileW(HENHMETAFILE(h.0), PCWSTR::null()) };
            if copy.is_invalid() {
                failed = true;
            } else {
                out.push(Saved::Metafile(Emf(copy)));
            }
        } else if let Some(bytes) = global_bytes(h) {
            total += bytes.len();
            out.push(Saved::Memory(fmt, bytes));
        } else {
            failed = true;
        }
    }
    // A format missing because its owner already exited is empty anyway.
    let owner_alive = unsafe { GetClipboardOwner() }.is_ok_and(|w| !w.0.is_null() && unsafe { IsWindow(Some(w)) }.as_bool());
    if failed && owner_alive {
        return None;
    }
    // Read while still open: renders we triggered above are already counted.
    Some((out, unsafe { GetClipboardSequenceNumber() }))
}

fn restore_all(saved: Vec<Saved>) {
    unsafe {
        if EmptyClipboard().is_err() {
            return;
        }
        for item in saved {
            match item {
                Saved::Memory(fmt, bytes) => set_bytes(fmt, &bytes),
                Saved::Metafile(emf) => {
                    // On success the clipboard owns the metafile: don't free it.
                    if SetClipboardData(CF_ENHMETAFILE, Some(HANDLE(emf.0 .0))).is_ok() {
                        std::mem::forget(emf);
                    }
                }
            }
        }
        // Putting the old contents back is not a new copy: keep it out of
        // Win+V history and cloud clipboard sync.
        for name in [w!("CanIncludeInClipboardHistory"), w!("CanUploadToCloudClipboard")] {
            let fmt = RegisterClipboardFormatW(name);
            if fmt != 0 {
                set_bytes(fmt, &0u32.to_le_bytes());
            }
        }
    }
}

/// Puts one memory format on the (open) clipboard; frees the block if refused.
fn set_bytes(fmt: u32, bytes: &[u8]) {
    unsafe {
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else { return };
        let ptr = GlobalLock(g) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(g));
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(g);
        // On success the clipboard owns the memory.
        if SetClipboardData(fmt, Some(HANDLE(g.0))).is_err() {
            let _ = GlobalFree(Some(g));
        }
    }
}

/// The copied text — unless the app copied the whole current line because
/// nothing was selected (VS Code, Visual Studio do that).
fn read_selection(_: ()) -> Option<String> {
    unsafe {
        let vs = RegisterClipboardFormatW(w!("VisualStudioEditorOperationsLineCutCopyClipboardTag"));
        if vs != 0 && GetClipboardData(vs).is_ok() {
            return None;
        }
        let code = RegisterClipboardFormatW(w!("vscode-editor-data"));
        if code != 0 {
            if let Ok(h) = GetClipboardData(code) {
                let meta = global_bytes(h).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                if meta.replace(' ', "").contains("\"isFromEmptySelection\":true") {
                    return None;
                }
            }
        }
    }
    let h = unsafe { GetClipboardData(CF_UNICODETEXT) }.ok()?;
    let bytes = global_bytes(h)?;
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    Some(String::from_utf16_lossy(&wide[..end]))
}
