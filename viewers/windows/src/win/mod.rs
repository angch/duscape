//! The window: Win32 input turned into [`Viewer`] calls, and GDI drawing of what it says
//! ([`paint`]). Nothing here decides anything a test would want to check; that is the viewer's,
//! in `diskonaut-viewer`, shared with the macOS and Linux windows.
//!
//! The viewer works in points; the window multiplies by the screen's DPI scale to draw and
//! divides to hit-test, so every size here is the other viewers' too.
//!
//! Work off the window's thread — the scan, rescans, previews — reports by posting one
//! [`AppMsg`], boxed, to the window. One that arrives while a modal loop runs (a message box, the
//! context menu) is queued and handled when the handler that opened it returns, so a scan that
//! finishes under a dialog is not lost.

mod paint;

use ::std::cell::{Cell, RefCell};
use ::std::collections::VecDeque;
use ::std::ffi::{OsString, c_void};
use ::std::os::windows::ffi::OsStringExt;
use ::std::path::{Path, PathBuf};
use ::std::ptr::{null, null_mut};
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use diskonaut_scan::rescan::{Outcome, Rescanner};
use diskonaut_viewer::menu::{Action, Entry, Platform};
use diskonaut_viewer::scan;
use diskonaut_viewer::state::{Direction, Hit, Jump, Mods, Preview, ROW, Rect, Viewer};
use libdiskonaut::model::SizeKind;
use libdiskonaut::preview::{Reader, Ready};
use libdiskonaut::{DirSummary, FileTree, ScanOptions};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetDC, GetDeviceCaps, InvalidateRect, LOGPIXELSY, ReleaseDC, ScreenToClient,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F5, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::Shell::{
    BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW, SHGetPathFromIDListW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CS_DBLCLKS, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IDC_ARROW, IDI_APPLICATION, IDYES, KillTimer, LoadCursorW, LoadIconW,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW,
    SetProcessDPIAware, SetTimer, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CHAR, WM_CREATE, WM_DESTROY,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
    WM_RBUTTONUP, WM_SIZE, WM_TIMER, WM_XBUTTONUP, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::cli::Opt;
use crate::elevate;
use crate::preview::{Picture, PreviewRequest, prepare_picture};

/// A report from a thread off the window's.
const WM_APP_MSG: u32 = WM_APP + 1;
/// Ticks while the status bar has a message to take down.
const FLASH_TIMER: usize = 1;
/// Fires once after a burst of outline batches, to lay the live view out for them.
const OUTLINE_TIMER: usize = 2;
/// How long after a batch the live view is laid out — the elevated scan of a volume sends
/// dozens of batches a second, and each relayout took 15–25 ms, so laid out per batch the
/// window answered nothing until the scan ended.
const OUTLINE_MS: u32 = 100;
/// How many rows one notch of the wheel scrolls the list.
const WHEEL_ROWS: isize = 3;

/// What a thread off the window's reports.
enum AppMsg {
    /// More of the live outline.
    Summaries(Vec<DirSummary>),
    /// The finished tree.
    Scanned(Box<FileTree>),
    Rescanned(u64, Outcome),
    Preview(u64, Ready<Picture>),
}

/// Post `message` to the window at `hwnd`. If it cannot be posted — the window is gone — the
/// message is dropped here rather than leaked.
fn post(hwnd: usize, message: AppMsg) {
    let raw = Box::into_raw(Box::new(message));
    // SAFETY: the pointer is reclaimed by the window procedure, or here if it never gets there.
    if unsafe { PostMessageW(hwnd as HWND, WM_APP_MSG, 0, raw as LPARAM) } == 0 {
        drop(unsafe { Box::from_raw(raw) });
    }
}

/// What the window holds.
struct Window {
    /// Its own handle, set once the window exists.
    hwnd: usize,
    viewer: Viewer,
    options: ScanOptions,
    reader: Option<Reader<PreviewRequest>>,
    /// Cleared when the window closes, which stops the scan and any rescan.
    running: Arc<AtomicBool>,
    /// Pixels per point: the screen's DPI over 96.
    scale: f64,
    /// The decoded picture behind `viewer.preview`, when that is a picture.
    picture: Option<Picture>,
    /// The breadcrumbs as last painted, in points, each with the depth it goes up to.
    crumbs: Vec<(Rect, usize)>,
    fonts: paint::Fonts,
    /// Outline batches have come in since the view was last laid out; `OUTLINE_TIMER` is set.
    outline_behind: bool,
}

/// The preview panel's caption line and, under it, the area for the picture or text: in points.
pub(crate) fn preview_parts(info: Rect) -> (Rect, Rect) {
    let inner = info.inset(6.0, 6.0);
    let caption = Rect::new(inner.x, inner.y, inner.w, ROW);
    let body = Rect::new(
        inner.x,
        caption.bottom() + 2.0,
        inner.w,
        (inner.bottom() - caption.bottom() - 2.0).max(0.0),
    );
    (caption, body)
}

impl Window {
    fn points(&self, px: i32) -> f64 {
        f64::from(px) / self.scale
    }

    fn pixels(&self, points: f64) -> i32 {
        (points * self.scale).round() as i32
    }

    /// The client area changed: lay the viewer out for it.
    fn on_size(&mut self, hwnd: HWND) {
        let client = client_rect(hwnd);
        self.viewer.resize(
            self.points(client.right - client.left),
            self.points(client.bottom - client.top),
        );
        self.changed(hwnd);
    }

    /// After anything that may have changed what is shown: ask for the preview of the entry in
    /// hand, retitle, and redraw.
    fn changed(&mut self, hwnd: HWND) {
        // The picture is prepared at the pixels it will take, so a resize asks for it again.
        if let Some(info) = self.viewer.layout.info {
            let (_, body) = preview_parts(info);
            let pixels = (
                u32::try_from(self.pixels(body.w).max(1)).unwrap_or(1),
                u32::try_from(self.pixels(body.h).max(1)).unwrap_or(1),
            );
            if let Some((generation, path)) = self.viewer.wanted_preview_sized(Some(pixels))
                && let Some(reader) = &self.reader
            {
                self.picture = None;
                reader.request(PreviewRequest {
                    generation,
                    path,
                    max_pixels: pixels,
                });
            }
        }
        if !matches!(self.viewer.preview, Preview::Picture(_)) {
            self.picture = None;
        }
        set_title(
            hwnd,
            &format!(
                "{} — {} — diskonaut",
                self.viewer.title(),
                self.viewer.subtitle()
            ),
        );
        if self.viewer.message_left().is_some() {
            // SAFETY: a timer on our own window, killed once the message is down.
            unsafe { SetTimer(hwnd, FLASH_TIMER, 250, None) };
        }
        invalidate(hwnd);
    }

    fn on_app_message(&mut self, hwnd: HWND, message: AppMsg) {
        match message {
            // Into the tree now, on screen when the timer fires: a burst of batches costs one
            // relayout, not one each.
            AppMsg::Summaries(summaries) => {
                self.viewer.absorb_summaries(summaries);
                if !self.outline_behind {
                    self.outline_behind = true;
                    // SAFETY: our own timer on our own window.
                    unsafe { SetTimer(hwnd, OUTLINE_TIMER, OUTLINE_MS, None) };
                }
                return;
            }
            AppMsg::Scanned(tree) => {
                self.outline_caught_up(hwnd);
                self.viewer.finish_scan(*tree);
            }
            AppMsg::Rescanned(id, outcome) => self.viewer.rescan_done(id, outcome),
            AppMsg::Preview(generation, ready) => {
                let (preview, picture) = match ready {
                    Ready::Info(info) => (Preview::Info(info), None),
                    Ready::Text(lines) => (Preview::Text(lines), None),
                    // A binary file is shown as its first bytes, not described.
                    Ready::Binary { info, dump } => (Preview::Hex { info, dump }, None),
                    Ready::Picture(picture) => {
                        (Preview::Picture(picture.description.clone()), Some(picture))
                    }
                };
                if !self.viewer.preview_ready(generation, preview) {
                    return;
                }
                self.picture = picture;
            }
        }
        self.changed(hwnd);
    }

    /// The outline timer's turn, or the finished tree's: nothing is behind any more.
    fn outline_caught_up(&mut self, hwnd: HWND) {
        if self.outline_behind {
            self.outline_behind = false;
            // SAFETY: our own timer.
            unsafe { KillTimer(hwnd, OUTLINE_TIMER) };
        }
    }

    fn on_key(&mut self, hwnd: HWND, key: u16) {
        let shift = key_down(VK_SHIFT);
        let ctrl = key_down(VK_CONTROL);
        match key {
            VK_UP => self.viewer.arrow(Direction::Up, shift),
            VK_DOWN => self.viewer.arrow(Direction::Down, shift),
            VK_LEFT => self.viewer.arrow(Direction::Left, shift),
            VK_RIGHT => self.viewer.arrow(Direction::Right, shift),
            VK_PRIOR => self.viewer.jump(Jump::PageUp, shift),
            VK_NEXT => self.viewer.jump(Jump::PageDown, shift),
            VK_HOME => self.viewer.jump(Jump::Home, shift),
            VK_END => self.viewer.jump(Jump::End, shift),
            VK_TAB => self.viewer.toggle_focus(),
            VK_RETURN => {
                self.viewer.enter_selected();
            }
            VK_ESCAPE | VK_BACK => {
                self.viewer.go_up();
            }
            VK_DELETE => self.delete(hwnd),
            VK_F5 if shift => self.viewer.rescan_all(),
            VK_F5 => self.viewer.rescan_selected(),
            // Ctrl+C copies the relative path, Ctrl+Shift+C the absolute one; Ctrl+A marks all.
            0x43 if ctrl => {
                self.viewer.copy_paths(shift);
            }
            0x41 if ctrl => self.viewer.mark_all(),
            _ => return,
        }
        self.changed(hwnd);
    }

    /// Keys that are characters, as the keyboard layout makes them.
    fn on_char(&mut self, hwnd: HWND, character: u16) {
        match char::from_u32(u32::from(character)) {
            Some('+' | '=') => self.viewer.zoom_in(),
            Some('-') => self.viewer.zoom_out(),
            Some('0') => self.viewer.reset_zoom(),
            Some('a') => self.viewer.toggle_size(),
            Some('r') => self.viewer.rescan_selected(),
            Some('R') => self.viewer.rescan_all(),
            Some('s' | 'S') => self.viewer.toggle_sidebar(),
            Some('d') => self.delete(hwnd),
            _ => return,
        }
        self.changed(hwnd);
    }

    /// The breadcrumb under a point, as the depth it leads up to.
    fn crumb_at(&self, x: f64, y: f64) -> Option<usize> {
        self.crumbs
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|&(_, depth)| depth)
    }

    fn on_click(&mut self, hwnd: HWND, x: i32, y: i32, double: bool) {
        let (x, y) = (self.points(x), self.points(y));
        if let Some(depth) = self.crumb_at(x, y) {
            self.viewer.go_to_depth(depth);
            return self.changed(hwnd);
        }
        // A folder row's expander opens it in place; a double click there is one click.
        if let Hit::Expander(index) = self.viewer.hit(x, y) {
            if !double {
                self.viewer.toggle_row(index);
            }
            return self.changed(hwnd);
        }
        if double {
            if self.viewer.click(x, y, Mods::default()).is_some() {
                self.viewer.enter_selected();
            }
        } else {
            let mods = Mods {
                toggle: key_down(VK_CONTROL),
                range: key_down(VK_SHIFT),
            };
            if self.viewer.click(x, y, mods).is_none() {
                if !matches!(self.viewer.hit(x, y), Hit::SmallFiles) {
                    return;
                }
                self.viewer
                    .say("The entries too small for a tile are all in the list");
            }
        }
        self.changed(hwnd);
    }

    /// Right-click: the entry under the pointer comes into hand — unless it is one of several
    /// marked, which stay marked — and a menu of what can be done with it opens.
    fn on_context_menu(&mut self, hwnd: HWND, x: i32, y: i32) {
        let (px, py) = (self.points(x), self.points(y));
        if self.viewer.context_click(px, py) {
            self.changed(hwnd);
        }
        let entries = self.viewer.context_menu(&PLATFORM);
        if entries.is_empty() {
            return;
        }
        // SAFETY: the menu is created, shown and destroyed here; every string outlives its call.
        let chosen = unsafe {
            let menu = CreatePopupMenu();
            for (index, entry) in entries.iter().enumerate() {
                match entry {
                    Entry::Separator => {
                        AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    }
                    Entry::Item {
                        action,
                        label,
                        enabled,
                    } => {
                        let text = match key_hint(*action) {
                            Some(hint) => wide(&format!("{label}\t{hint}")),
                            None => wide(label),
                        };
                        let flags = if *enabled {
                            MF_STRING
                        } else {
                            MF_STRING | MF_GRAYED
                        };
                        // Ids from 1: 0 is what a dismissed menu returns.
                        AppendMenuW(menu, flags, index + 1, text.as_ptr());
                    }
                }
            }
            let mut point = POINT { x, y };
            ClientToScreen(hwnd, &mut point);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                null(),
            );
            DestroyMenu(menu);
            usize::try_from(chosen).unwrap_or(0)
        };
        let action = chosen
            .checked_sub(1)
            .and_then(|index| entries.get(index))
            .and_then(Entry::chosen);
        match action {
            Some(action) => self.act(hwnd, action),
            // Dismissed: the entry it moved to is still in hand, and any paint the menu's loop
            // deflected is owed.
            None => return invalidate(hwnd),
        }
        self.changed(hwnd);
    }

    /// Carry out a context menu's choice.
    fn act(&mut self, hwnd: HWND, action: Action) {
        let failed = match action {
            Action::Open => self
                .viewer
                .open_in_hand()
                .and_then(|path| libdiskonaut::launch::open(&path).err()),
            Action::Reveal => {
                let paths = self.viewer.target_paths();
                let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
                libdiskonaut::launch::reveal(&paths).err()
            }
            Action::CopyPath | Action::CopyFullPath => {
                self.viewer.copy_paths(action == Action::CopyFullPath);
                None
            }
            Action::Rescan => {
                self.viewer.rescan_selected();
                None
            }
            Action::RescanAll => {
                self.viewer.rescan_all();
                None
            }
            Action::Delete | Action::Trash => {
                self.delete(hwnd);
                None
            }
            // Not offered here (`PLATFORM`).
            Action::QuickLook | Action::CopyPathname => None,
        };
        if let Some(error) = failed {
            self.viewer.say(error);
        }
    }

    fn on_wheel(&mut self, hwnd: HWND, delta: i16, x: i32, y: i32) {
        let (x, y) = (self.points(x), self.points(y));
        let layout = self.viewer.layout;
        if layout.list.is_some_and(|list| list.contains(x, y)) {
            let rows = if delta > 0 { -WHEEL_ROWS } else { WHEEL_ROWS };
            self.viewer.scroll_list(rows);
            self.viewer.hover_at(x, y);
            invalidate(hwnd);
        } else if layout.treemap.contains(x, y) {
            if delta > 0 {
                self.viewer.zoom_in();
            } else {
                self.viewer.zoom_out();
            }
            self.changed(hwnd);
        }
    }

    fn on_mouse_move(&mut self, hwnd: HWND, x: i32, y: i32) {
        if self.viewer.hover_at(self.points(x), self.points(y)) {
            invalidate(hwnd);
        }
    }

    /// Delete what is marked, or the entry in hand, once the user has said yes.
    fn delete(&mut self, hwnd: HWND) {
        if self.viewer.scanning {
            self.viewer.say("Deleting waits for the scan to finish");
            return;
        }
        let files = self.viewer.targets();
        if files.is_empty() {
            return;
        }
        if let Some(refusal) = Viewer::refusal(&files) {
            message(hwnd, &refusal, MB_OK | MB_ICONERROR);
            return;
        }
        if message(
            hwnd,
            &Viewer::delete_prompt(&files),
            MB_YESNO | MB_ICONWARNING,
        ) != IDYES
        {
            return;
        }
        if let Err(error) = self.viewer.delete(&files) {
            self.changed(hwnd);
            message(hwnd, &error, MB_OK | MB_ICONERROR);
        }
    }

    /// Start the scan, the preview thread and rescans, reporting to `hwnd`.
    fn start(&mut self, hwnd: HWND, root: PathBuf) {
        let window = hwnd as usize;
        self.reader = Some(Reader::spawn(prepare_picture, move |generation, ready| {
            post(window, AppMsg::Preview(generation, ready));
        }));
        self.viewer.enable_rescans(Rescanner::new(
            self.options,
            Arc::clone(&self.running),
            move |id, outcome| post(window, AppMsg::Rescanned(id, outcome)),
        ));
        self.viewer.set_clipboard(libdiskonaut::clipboard::copy);
        scan::spawn(
            root,
            self.options,
            Arc::clone(&self.running),
            move |batch| post(window, AppMsg::Summaries(batch)),
            move |tree| {
                if let Some(tree) = tree {
                    post(window, AppMsg::Scanned(Box::new(tree)));
                }
            },
        );
    }
}

/// What the context menu offers here beyond every viewer's items: Explorer, and deleting for
/// good (there is no Recycle Bin yet).
const PLATFORM: Platform = Platform {
    reveal: "Show in Explorer",
    quick_look: false,
    pathname: false,
    trash: false,
};

/// The key that does what a menu item does, shown beside it.
fn key_hint(action: Action) -> Option<&'static str> {
    Some(match action {
        Action::Open => "Enter",
        Action::CopyPath => "Ctrl+C",
        Action::CopyFullPath => "Ctrl+Shift+C",
        Action::Rescan => "r",
        Action::RescanAll => "R",
        Action::Delete => "Del",
        _ => return None,
    })
}

fn key_down(key: u16) -> bool {
    // SAFETY: no preconditions; the high bit says the key is down.
    unsafe { GetKeyState(i32::from(key)) < 0 }
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn client_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `rect` is a valid out-pointer for the call.
    unsafe { GetClientRect(hwnd, &mut rect) };
    rect
}

fn invalidate(hwnd: HWND) {
    // SAFETY: our own window; FALSE, since every paint covers the whole client area.
    unsafe { InvalidateRect(hwnd, null(), 0) };
}

fn set_title(hwnd: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: NUL-terminated and alive for the call. Re-enters the window procedure with
    // WM_SETTEXT, which the re-entrancy guard hands to the default procedure.
    unsafe { SetWindowTextW(hwnd, text.as_ptr()) };
}

/// A message box over `hwnd`; returns which button closed it.
fn message(hwnd: HWND, text: &str, style: u32) -> i32 {
    let text = wide(text);
    let caption = wide("diskonaut");
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe { MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), style) }
}

fn low_word(value: isize) -> i32 {
    i32::from(value as u16 as i16)
}

fn high_word(value: isize) -> i32 {
    i32::from((value >> 16) as u16 as i16)
}

thread_local! {
    /// Set while a handler holds `&mut Window`. `SetWindowTextW`, a message box or the context
    /// menu re-enter the window procedure; a second `&mut` to the same `Window` would be
    /// undefined behaviour, so re-entrant calls go to the default procedure instead.
    static IN_HANDLER: Cell<bool> = const { Cell::new(false) };
    /// Reports that arrived while a handler held the window, to handle once it lets go — in the
    /// order they came, since the outline's last folders come before the finished tree.
    static PENDING: RefCell<VecDeque<AppMsg>> = const { RefCell::new(VecDeque::new()) };
    /// A WM_PAINT arrived while a handler held the window (a modal loop pumps messages), and the
    /// default procedure validated the area without drawing it: paint again once the handler
    /// lets go, or the window stays stale where the dialog was.
    static REPAINT: Cell<bool> = const { Cell::new(false) };
    /// WM_DESTROY arrived while a handler held the window — the window was closed from the
    /// taskbar or by another process with a dialog up. The `Window` is freed once the handler
    /// on the stack has let go of it, never under its `&mut`.
    static DESTROY: Cell<bool> = const { Cell::new(false) };
}

/// The window is gone: free what it held, stop its threads, and end the message loop.
fn destroy(state: *mut Window, hwnd: HWND) {
    // SAFETY: the pointer came from `Box::into_raw` in `run`, and is cleared here once; no
    // handler holds a `&mut` to it (the caller checks the guard).
    let window = unsafe { Box::from_raw(state) };
    window.running.store(false, Ordering::Release);
    drop(window);
    // SAFETY: `hwnd` is the window being destroyed; clearing its user data ends every use of the pointer.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    PENDING.with(|pending| pending.borrow_mut().clear());
    // SAFETY: no preconditions.
    unsafe { PostQuitMessage(0) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CREATE {
        // SAFETY: WM_CREATE's lparam is the CREATESTRUCTW whose create parameter is our window.
        let create = lparam as *const CREATESTRUCTW;
        let state = unsafe { (*create).lpCreateParams } as isize;
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state) };
        return 0;
    }
    // SAFETY: GWLP_USERDATA holds the `Window` pointer from WM_CREATE until WM_DESTROY zeroes it.
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Window;
    if msg == WM_APP_MSG {
        // SAFETY: every WM_APP_MSG carries a `Box<AppMsg>` from `post`.
        let message = unsafe { Box::from_raw(lparam as *mut AppMsg) };
        if state.is_null() {
            return 0;
        }
        if IN_HANDLER.with(Cell::get) {
            PENDING.with(|pending| pending.borrow_mut().push_back(*message));
            return 0;
        }
        return run_handler(state, |window| {
            window.on_app_message(hwnd, *message);
            0
        });
    }
    if state.is_null() {
        // SAFETY: the message's own arguments, passed on unchanged.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    if msg == WM_DESTROY {
        if IN_HANDLER.with(Cell::get) {
            DESTROY.with(|flag| flag.set(true));
        } else {
            destroy(state, hwnd);
        }
        return 0;
    }
    if IN_HANDLER.with(Cell::get) {
        if msg == WM_PAINT {
            REPAINT.with(|flag| flag.set(true));
        }
        // SAFETY: the message's own arguments, passed on unchanged.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    if !handles(msg) {
        // Straight to the system, outside the guard: `DefWindowProcW` runs the frame's own
        // loops in here — dragging an edge, the title bar or a system-menu command — and the
        // WM_SIZE and WM_PAINT they send arrive re-entrantly. Behind the guard those were
        // dropped, so the window's contents never followed a resize.
        // SAFETY: the message's own arguments, passed on unchanged.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    run_handler(state, |window| dispatch(window, hwnd, msg, wparam, lparam))
}

/// The messages [`dispatch`] has an arm for; every other message is the system's and goes to it
/// outside the guard. An arm added to `dispatch` goes here too, or it never runs.
const HANDLED: [u32; 11] = [
    WM_SIZE,
    WM_PAINT,
    WM_LBUTTONDOWN,
    WM_LBUTTONDBLCLK,
    WM_RBUTTONUP,
    WM_XBUTTONUP,
    WM_MOUSEWHEEL,
    WM_MOUSEMOVE,
    WM_KEYDOWN,
    WM_CHAR,
    WM_TIMER,
];

/// Whether [`dispatch`] does anything with `msg`.
fn handles(msg: u32) -> bool {
    HANDLED.contains(&msg)
}

/// Run `handler` with an exclusive `&mut Window`, behind the re-entrancy guard, then the reports
/// that came in meanwhile — unless the window was destroyed meanwhile, in which case it is freed
/// now that nothing holds it. A panic must not unwind across the `extern "system"` frame.
fn run_handler(state: *mut Window, handler: impl FnOnce(&mut Window) -> LRESULT) -> LRESULT {
    IN_HANDLER.with(|flag| flag.set(true));
    // SAFETY: the guard makes this the only `&mut` to the window for the call.
    let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
        handler(unsafe { &mut *state })
    }))
    .unwrap_or(0);
    while !DESTROY.with(Cell::get) {
        let Some(message) = PENDING.with(|pending| pending.borrow_mut().pop_front()) else {
            break;
        };
        let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
            // SAFETY: still behind the guard, and the handler's `&mut` has ended.
            let window = unsafe { &mut *state };
            let hwnd = window.hwnd as HWND;
            window.on_app_message(hwnd, message);
        }));
    }
    // SAFETY: the handler's `&mut` has ended; only the handle is read.
    let hwnd = unsafe { (*state).hwnd } as HWND;
    IN_HANDLER.with(|flag| flag.set(false));
    if DESTROY.with(Cell::take) {
        destroy(state, hwnd);
    } else if REPAINT.with(Cell::take) {
        invalidate(hwnd);
    }
    result
}

fn dispatch(window: &mut Window, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => window.on_size(hwnd),
        WM_PAINT => window.crumbs = paint::paint(window, hwnd),
        WM_LBUTTONDOWN => window.on_click(hwnd, low_word(lparam), high_word(lparam), false),
        WM_LBUTTONDBLCLK => window.on_click(hwnd, low_word(lparam), high_word(lparam), true),
        WM_RBUTTONUP => window.on_context_menu(hwnd, low_word(lparam), high_word(lparam)),
        // The mouse's back button.
        WM_XBUTTONUP if high_word(wparam as isize) == 1 => {
            if window.viewer.go_up() {
                window.changed(hwnd);
            }
        }
        WM_MOUSEWHEEL => {
            // The wheel reports screen coordinates.
            let mut point = POINT {
                x: low_word(lparam),
                y: high_word(lparam),
            };
            // SAFETY: `point` is a valid in-out pointer.
            unsafe { ScreenToClient(hwnd, &mut point) };
            let delta = high_word(wparam as isize) as i16;
            window.on_wheel(hwnd, delta, point.x, point.y);
        }
        WM_MOUSEMOVE => window.on_mouse_move(hwnd, low_word(lparam), high_word(lparam)),
        WM_KEYDOWN => window.on_key(hwnd, wparam as u16),
        WM_CHAR => window.on_char(hwnd, wparam as u16),
        WM_TIMER if wparam == OUTLINE_TIMER => {
            window.outline_caught_up(hwnd);
            window.viewer.catch_up();
            window.changed(hwnd);
        }
        WM_TIMER if wparam == FLASH_TIMER => {
            if window.viewer.message_left().is_none() {
                // SAFETY: our own timer.
                unsafe { KillTimer(hwnd, FLASH_TIMER) };
            }
            invalidate(hwnd);
        }
        // SAFETY: the message's own arguments, passed on unchanged.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    0
}

/// A whole local volume: ask to run as administrator, and if the elevated process starts, it
/// takes over (true); declined, or elevated already, the scan goes on as it is. If the shell
/// would not start it, the scan goes on too, with a notice for the status bar.
fn handed_to_elevated(opt: &Opt, root: &Path) -> (bool, Option<String>) {
    if !elevate::wanted(root, opt.no_elevate, libdiskonaut::os::is_user_admin())
        || !elevate::is_local_disk(root)
    {
        return (false, None);
    }
    let picked = opt.folder.is_none().then_some(root);
    let args = elevate::relaunch_args(::std::env::args_os(), picked);
    match elevate::relaunch(&args) {
        Ok(()) => (true, None),
        Err(elevate::Refused::Declined) => (false, None),
        Err(elevate::Refused::Failed(code)) => (
            false,
            Some(format!(
                "Could not start as administrator (error {code}); scanning as is"
            )),
        ),
    }
}

/// The folder chooser. `None` if the user cancels.
fn pick_folder() -> Option<PathBuf> {
    let title = wide("Choose a folder to scan");
    let mut display = [0u16; 260];
    // SAFETY: zeroed is a valid BROWSEINFOW; every pointer set in it outlives the call.
    let mut info: BROWSEINFOW = unsafe { ::std::mem::zeroed() };
    info.pszDisplayName = display.as_mut_ptr();
    info.lpszTitle = title.as_ptr();
    info.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    // SAFETY: `info` is filled in and outlives the call, as do the buffers it points to.
    let pidl = unsafe { SHBrowseForFolderW(&info) };
    if pidl.is_null() {
        return None;
    }
    let mut path = [0u16; 260];
    // SAFETY: `pidl` is the list the chooser just returned; `path` has the MAX_PATH the call may fill.
    let ok = unsafe { SHGetPathFromIDListW(pidl, path.as_mut_ptr()) };
    // SAFETY: the shell allocated `pidl` for us, and it is freed once, here.
    unsafe { CoTaskMemFree(pidl as *const c_void) };
    if ok == 0 {
        return None;
    }
    let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    Some(PathBuf::from(OsString::from_wide(&path[..len])))
}

/// The screen's DPI over 96: pixels per point.
fn dpi_scale() -> f64 {
    // SAFETY: the screen DC is released before returning.
    unsafe {
        let screen = GetDC(null_mut());
        let dpi = GetDeviceCaps(screen, LOGPIXELSY as i32);
        ReleaseDC(null_mut(), screen);
        if dpi > 0 { f64::from(dpi) / 96.0 } else { 1.0 }
    }
}

/// The command line and the folder to scan, resolved: `None` when there is nothing to do —
/// `--help` shown, the chooser cancelled, no such folder.
fn choose() -> Option<(Opt, PathBuf)> {
    let opt = match Opt::try_parse() {
        Ok(opt) => opt,
        Err(error) => {
            // No console to print to: `--help`, `--version` and mistakes all go in a box.
            let style = if error.use_stderr() {
                MB_OK | MB_ICONERROR
            } else {
                MB_OK | MB_ICONINFORMATION
            };
            message(null_mut(), &error.to_string(), style);
            return None;
        }
    };
    let root = opt.folder.clone().or_else(pick_folder)?;
    if !root.is_dir() {
        message(
            null_mut(),
            &format!("Not a folder: {}", root.display()),
            MB_OK | MB_ICONERROR,
        );
        return None;
    }
    let root = root.canonicalize().unwrap_or(root);
    Some((opt, root))
}

pub fn run() {
    // SAFETY: called before any window exists.
    unsafe { SetProcessDPIAware() };
    let Some((opt, root)) = choose() else {
        return;
    };
    // The root is resolved, so `.` in a volume root is the volume.
    let (handed, notice) = handed_to_elevated(&opt, &root);
    if handed {
        return;
    }
    let options = opt.scan_options();
    let shown = if options.show_apparent_size {
        SizeKind::Apparent
    } else {
        SizeKind::Disk
    };
    let scale = dpi_scale();
    let mut viewer = Viewer::new(&root, shown, 0);
    if let Some(notice) = notice {
        viewer.say(notice);
    }
    // The tree view: the list as a tree, folders open in place, drawn with their depth and an
    // expander (`paint::draw_list`); the treemap nested, tiles inside the folder tiles
    // (`paint::draw_nested`).
    viewer.set_tree_view(true);
    let window = Box::new(Window {
        hwnd: 0,
        viewer,
        options,
        reader: None,
        running: Arc::new(AtomicBool::new(true)),
        scale,
        picture: None,
        crumbs: Vec::new(),
        fonts: paint::Fonts::new(scale),
        outline_behind: false,
    });
    let state = Box::into_raw(window);

    // SAFETY: the class and window are created with valid, NUL-terminated strings; `state` is
    // owned by the window from WM_CREATE and reclaimed at WM_DESTROY, or here if creation fails.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide("DiskonautWindowsWindow");
        let class = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };
        RegisterClassW(&class);
        let title = wide("diskonaut");
        let px = |value: f64| (value * scale).round() as i32;
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            px(1200.0),
            px(780.0),
            null_mut(),
            null_mut(),
            instance,
            state as *const c_void,
        );
        if hwnd.is_null() {
            drop(Box::from_raw(state));
            return;
        }
        // Through the guard like any handler: both re-enter the window procedure.
        run_handler(state, |window| {
            window.hwnd = hwnd as usize;
            window.start(hwnd, root);
            window.on_size(hwnd);
            0
        });
        ShowWindow(hwnd, SW_SHOW);

        let mut msg: MSG = ::std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
