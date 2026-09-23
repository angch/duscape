//! A Windows GUI treemap for diskonaut-angch.
//!
//! The scan and the squarify layout come straight from `libdiskonaut` — the same fast native walker
//! the terminal app uses — so this crate is only a window and some GDI drawing. It is deliberately
//! built on `windows-sys` (raw bindings, tiny binary) rather than a GUI framework, to keep the
//! executable small.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("diskonaut-gui is a Windows-only GUI. Use `diskonaut` on this platform.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    gui::run();
}

#[cfg(windows)]
mod gui {
    use ::std::ffi::{OsString, c_void};
    use ::std::os::windows::ffi::OsStringExt;
    use ::std::path::PathBuf;
    use ::std::ptr::{null, null_mut};
    use ::std::time::Instant;

    use libdiskonaut::model::FileToDelete;
    use libdiskonaut::scan::parallel;
    use libdiskonaut::tiles::{Area, Board, FileType};
    use libdiskonaut::{DisplayCount, DisplaySize, FileTree, ScanOptions};

    use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
        DEFAULT_GUI_FONT, DeleteDC, DeleteObject, EndPaint, FillRect, FrameRect, GetStockObject,
        HDC, InvalidateRect, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
        TRANSPARENT, TextOutW,
    };
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_DELETE, VK_DOWN, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
    };
    use windows_sys::Win32::UI::Shell::{
        BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW, SHGetPathFromIDListW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW,
        GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW, IDC_ARROW, IDI_APPLICATION,
        IDYES, LoadCursorW, LoadIconW, MB_ICONWARNING, MB_YESNO, MSG, MessageBoxW, PostMessageW,
        PostQuitMessage, RegisterClassW, SW_SHOW, SetProcessDPIAware, SetWindowLongPtrW,
        SetWindowTextW, ShowWindow, TranslateMessage, WM_APP, WM_CREATE, WM_DESTROY, WM_KEYDOWN,
        WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WM_SIZE, WNDCLASSW,
        WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    /// Pixel size of a virtual layout cell. The 2.5:1 ratio matches the treemap's internal
    /// `HEIGHT_WIDTH_RATIO`, so scaling cells back to pixels yields square-looking tiles, and the
    /// treemap's 8x3-cell minimum becomes a readable ~80x12-pixel minimum.
    const CELL_W: i32 = 10;
    const CELL_H: i32 = 4;
    const TOP_BAR: i32 = 30;
    const BOTTOM_BAR: i32 = 24;
    const BACK_BTN: RECT = RECT {
        left: 6,
        top: 4,
        right: 74,
        bottom: TOP_BAR - 4,
    };

    /// Progress ping from the scan thread, `wparam` = entries scanned so far.
    const WM_APP_PROGRESS: u32 = WM_APP + 1;
    /// Scan finished; `lparam` is a `Box<FileTree>` raw pointer to adopt.
    const WM_APP_DONE: u32 = WM_APP + 2;

    struct AppState {
        root: PathBuf,
        tree: Option<FileTree>,
        board: Option<Board>,
        scanning: bool,
        scanned_entries: usize,
        scan_secs: f64,
        scan_start: Instant,
        hover: Option<usize>,
    }

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
    }

    /// A stable colour per tile: folders in blues, files in warm tones, cycling by index.
    fn tile_color(index: usize, is_dir: bool) -> COLORREF {
        const FOLDERS: [(u8, u8, u8); 4] = [
            (52, 101, 164),
            (60, 120, 190),
            (44, 88, 140),
            (72, 138, 210),
        ];
        const FILES: [(u8, u8, u8); 4] = [
            (120, 120, 120),
            (150, 130, 96),
            (110, 140, 110),
            (150, 110, 110),
        ];
        let (r, g, b) = if is_dir {
            FOLDERS[index % FOLDERS.len()]
        } else {
            FILES[index % FILES.len()]
        };
        rgb(r, g, b)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn in_rect(r: &RECT, x: i32, y: i32) -> bool {
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    }

    fn client_rect(hwnd: HWND) -> RECT {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe { GetClientRect(hwnd, &mut rect) };
        rect
    }

    /// The treemap's pixel region: the client area minus the top and bottom bars.
    fn treemap_region(client: &RECT) -> (i32, i32) {
        let w = (client.right - client.left).max(0);
        let h = (client.bottom - client.top - TOP_BAR - BOTTOM_BAR).max(0);
        (w, h)
    }

    fn relayout(state: &mut AppState, hwnd: HWND) {
        let client = client_rect(hwnd);
        let (w, h) = treemap_region(&client);
        if let Some(board) = state.board.as_mut() {
            let cols = (w / CELL_W).max(1) as u16;
            let rows = (h / CELL_H).max(1) as u16;
            board.change_area(&Area {
                x: 0,
                y: 0,
                width: cols,
                height: rows,
            });
        }
    }

    /// Map a pixel point to a tile index within the current board, if any.
    fn tile_at(board: &Board, px: i32, py: i32) -> Option<usize> {
        let cx = (px / CELL_W) as u16;
        let cy = ((py - TOP_BAR) / CELL_H) as u16;
        board
            .tiles
            .iter()
            .position(|t| cx >= t.x && cx < t.x + t.width && cy >= t.y && cy < t.y + t.height)
    }

    fn set_title(hwnd: HWND, text: &str) {
        let w = wide(text);
        unsafe { SetWindowTextW(hwnd, w.as_ptr()) };
    }

    fn refresh_title(state: &AppState, hwnd: HWND) {
        if state.scanning {
            set_title(
                hwnd,
                &format!(
                    "Scanning {} — {} entries…",
                    state.root.display(),
                    DisplayCount(state.scanned_entries as u64)
                ),
            );
        } else if let Some(tree) = &state.tree {
            let rate = if state.scan_secs > 0.0 {
                (state.scanned_entries as f64 / state.scan_secs) as u64
            } else {
                0
            };
            set_title(
                hwnd,
                &format!(
                    "{} — {}  |  {} entries in {:.2}s ({} entries/s)",
                    tree.get_current_path().display(),
                    DisplaySize(tree.get_current_folder_size() as f64),
                    DisplayCount(state.scanned_entries as u64),
                    state.scan_secs,
                    DisplayCount(rate),
                ),
            );
        }
    }

    fn draw_text(hdc: HDC, x: i32, y: i32, color: COLORREF, s: &str) {
        let text: Vec<u16> = s.encode_utf16().collect();
        if text.is_empty() {
            return;
        }
        unsafe {
            SetTextColor(hdc, color);
            TextOutW(hdc, x, y, text.as_ptr(), text.len() as i32);
        }
    }

    fn fill(hdc: HDC, r: &RECT, color: COLORREF) {
        let brush = unsafe { CreateSolidBrush(color) };
        unsafe {
            FillRect(hdc, r, brush);
            DeleteObject(brush as _);
        }
    }

    fn paint(state: &AppState, hwnd: HWND) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        let client = client_rect(hwnd);
        let width = client.right - client.left;
        let height = client.bottom - client.top;

        // Draw to an off-screen bitmap, then blit once — no flicker on resize or hover.
        let mem = unsafe { CreateCompatibleDC(hdc) };
        let bmp = unsafe { CreateCompatibleBitmap(hdc, width.max(1), height.max(1)) };
        let old_bmp = unsafe { SelectObject(mem, bmp as _) };
        let old_font = unsafe { SelectObject(mem, GetStockObject(DEFAULT_GUI_FONT)) };
        unsafe { SetBkMode(mem, TRANSPARENT as i32) };

        // Background.
        fill(mem, &client, rgb(24, 24, 24));

        // Treemap or scanning message.
        if let Some(board) = &state.board {
            for (index, tile) in board.tiles.iter().enumerate() {
                let rect = RECT {
                    left: tile.x as i32 * CELL_W,
                    top: TOP_BAR + tile.y as i32 * CELL_H,
                    right: (tile.x + tile.width) as i32 * CELL_W,
                    bottom: TOP_BAR + (tile.y + tile.height) as i32 * CELL_H,
                };
                let is_dir = tile.file_type == FileType::Folder;
                fill(mem, &rect, tile_color(index, is_dir));
                let border = unsafe { CreateSolidBrush(rgb(20, 20, 20)) };
                unsafe {
                    FrameRect(mem, &rect, border);
                    DeleteObject(border as _);
                }
                if rect.right - rect.left > 60 && rect.bottom - rect.top > 18 {
                    let label = format!(
                        "{}  {}",
                        tile.name.to_string_lossy(),
                        DisplaySize(tile.size as f64)
                    );
                    draw_text(mem, rect.left + 4, rect.top + 3, rgb(240, 240, 240), &label);
                }
            }
            // The "small files" placeholder: everything too small to draw, as one marked block.
            if let Some((cx, cy)) = board.unrenderable_tile_coordinates {
                let (w, _h) = treemap_region(&client);
                let rect = RECT {
                    left: cx as i32 * CELL_W,
                    top: TOP_BAR + cy as i32 * CELL_H,
                    right: w,
                    bottom: height - BOTTOM_BAR,
                };
                fill(mem, &rect, rgb(60, 60, 60));
                let border = unsafe { CreateSolidBrush(rgb(20, 20, 20)) };
                unsafe {
                    FrameRect(mem, &rect, border);
                    DeleteObject(border as _);
                }
                draw_text(
                    mem,
                    rect.left + 4,
                    rect.top + 3,
                    rgb(200, 200, 200),
                    "small files",
                );
            }
            // Selected tile: a bright frame, drawn thick by insetting.
            if let Some(tile) = board.currently_selected() {
                let yellow = unsafe { CreateSolidBrush(rgb(255, 214, 10)) };
                for inset in 0..2 {
                    let rect = RECT {
                        left: tile.x as i32 * CELL_W + inset,
                        top: TOP_BAR + tile.y as i32 * CELL_H + inset,
                        right: (tile.x + tile.width) as i32 * CELL_W - inset,
                        bottom: TOP_BAR + (tile.y + tile.height) as i32 * CELL_H - inset,
                    };
                    unsafe { FrameRect(mem, &rect, yellow) };
                }
                unsafe { DeleteObject(yellow as _) };
            }
        } else if state.scanning {
            draw_text(
                mem,
                16,
                TOP_BAR + 16,
                rgb(220, 220, 220),
                &format!(
                    "Scanning {}…  {} entries",
                    state.root.display(),
                    DisplayCount(state.scanned_entries as u64)
                ),
            );
        }

        // Top bar: back button and the current path.
        let top = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: TOP_BAR,
        };
        fill(mem, &top, rgb(40, 40, 40));
        fill(mem, &BACK_BTN, rgb(70, 70, 70));
        draw_text(
            mem,
            BACK_BTN.left + 10,
            BACK_BTN.top + 4,
            rgb(235, 235, 235),
            "◄ Up",
        );
        if let Some(tree) = &state.tree {
            draw_text(
                mem,
                BACK_BTN.right + 12,
                8,
                rgb(210, 210, 210),
                &tree.get_current_path().display().to_string(),
            );
        }

        // Bottom bar: the hovered tile's details.
        let bottom = RECT {
            left: 0,
            top: height - BOTTOM_BAR,
            right: width,
            bottom: height,
        };
        fill(mem, &bottom, rgb(40, 40, 40));
        let status = match (state.board.as_ref(), state.hover) {
            (Some(board), Some(i)) => board.tiles.get(i).map(|t| {
                let kind = if t.file_type == FileType::Folder { "folder" } else { "file" };
                format!("{}  —  {}  ({})", t.name.to_string_lossy(), DisplaySize(t.size as f64), kind)
            }),
            _ => None,
        }
        .unwrap_or_else(|| {
            "Left-click a folder to open · right-click or ◄Up to go back · arrows select · Del deletes"
                .to_string()
        });
        draw_text(mem, 8, height - BOTTOM_BAR + 4, rgb(210, 210, 210), &status);

        unsafe {
            BitBlt(hdc, 0, 0, width, height, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            SelectObject(mem, old_font);
            DeleteObject(bmp as _);
            DeleteDC(mem);
            EndPaint(hwnd, &ps);
        }
    }

    fn enter_selected(state: &mut AppState, hwnd: HWND) {
        let name = match state.board.as_ref().and_then(|b| b.currently_selected()) {
            Some(t) if t.file_type == FileType::Folder => t.name.clone(),
            _ => return,
        };
        if let (Some(tree), Some(board)) = (state.tree.as_mut(), state.board.as_mut()) {
            tree.enter_folder(&name);
            board.change_files(tree.get_current_folder());
            board.reset_selected_index();
            board.move_to_largest_folder();
        }
        relayout(state, hwnd);
        refresh_title(state, hwnd);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn go_up(state: &mut AppState, hwnd: HWND) {
        let moved = match (state.tree.as_mut(), state.board.as_mut()) {
            (Some(tree), Some(board)) => {
                let moved = tree.leave_folder();
                if moved {
                    board.change_files(tree.get_current_folder());
                    board.reset_selected_index();
                }
                moved
            }
            _ => false,
        };
        if moved {
            relayout(state, hwnd);
            refresh_title(state, hwnd);
            unsafe { InvalidateRect(hwnd, null(), 1) };
        }
    }

    fn delete_selected(state: &mut AppState, hwnd: HWND) {
        let Some(tile) = state.board.as_ref().and_then(|b| b.currently_selected()) else {
            return;
        };
        let Some(tree) = state.tree.as_ref() else {
            return;
        };
        let sizes = tree
            .item_in_current_folder(&tile.name)
            .map(libdiskonaut::FileOrFolder::sizes)
            .unwrap_or_default();
        let mut path_to_file = tree.current_folder_names.clone();
        path_to_file.push(tile.name.clone());
        let to_delete = FileToDelete {
            path_in_filesystem: tree.path_in_filesystem.clone(),
            path_to_file,
            file_type: tile.file_type,
            num_descendants: tile.descendants,
            size: tile.size,
            sizes,
        };

        let prompt = wide(&format!(
            "Delete {}?\n\n{} will be permanently removed from disk.",
            to_delete.full_path().display(),
            DisplaySize(to_delete.size as f64),
        ));
        let caption = wide("Confirm delete");
        let answer = unsafe {
            MessageBoxW(
                hwnd,
                prompt.as_ptr(),
                caption.as_ptr(),
                MB_YESNO | MB_ICONWARNING,
            )
        };
        if answer != IDYES {
            return;
        }

        if let (Some(tree), Some(board)) = (state.tree.as_mut(), state.board.as_mut()) {
            tree.delete_file(&to_delete);
            board.change_files(tree.get_current_folder());
            board.reset_selected_index();
        }
        relayout(state, hwnd);
        refresh_title(state, hwnd);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn on_key(state: &mut AppState, hwnd: HWND, vk: u16) {
        let mut moved = true;
        if let Some(board) = state.board.as_mut() {
            match vk {
                x if x == VK_LEFT => board.move_selected_left(),
                x if x == VK_RIGHT => board.move_selected_right(),
                x if x == VK_UP => board.move_selected_up(),
                x if x == VK_DOWN => board.move_selected_down(),
                x if x == VK_RETURN => {
                    enter_selected(state, hwnd);
                    return;
                }
                x if x == VK_BACK => {
                    go_up(state, hwnd);
                    return;
                }
                x if x == VK_DELETE => {
                    delete_selected(state, hwnd);
                    return;
                }
                _ => moved = false,
            }
        } else {
            moved = false;
        }
        if moved {
            refresh_title(state, hwnd);
            unsafe { InvalidateRect(hwnd, null(), 1) };
        }
    }

    fn on_lclick(state: &mut AppState, hwnd: HWND, x: i32, y: i32) {
        if in_rect(&BACK_BTN, x, y) {
            go_up(state, hwnd);
            return;
        }
        let hit = state.board.as_ref().and_then(|b| tile_at(b, x, y));
        if let Some(index) = hit {
            if let Some(board) = state.board.as_mut() {
                board.set_selected_index(&index);
            }
            let is_dir = state
                .board
                .as_ref()
                .and_then(|b| b.tiles.get(index))
                .map(|t| t.file_type == FileType::Folder)
                .unwrap_or(false);
            if is_dir {
                enter_selected(state, hwnd);
            } else {
                unsafe { InvalidateRect(hwnd, null(), 1) };
            }
        }
    }

    fn on_mousemove(state: &mut AppState, hwnd: HWND, x: i32, y: i32) {
        let hit = state.board.as_ref().and_then(|b| tile_at(b, x, y));
        if hit != state.hover {
            state.hover = hit;
            // Only the status bar changes, so repaint just that strip.
            let client = client_rect(hwnd);
            let strip = RECT {
                left: 0,
                top: client.bottom - BOTTOM_BAR,
                right: client.right,
                bottom: client.bottom,
            };
            unsafe { InvalidateRect(hwnd, &strip, 0) };
        }
    }

    fn adopt_tree(state: &mut AppState, hwnd: HWND, tree: FileTree) {
        state.scanned_entries = tree.get_total_descendants() as usize;
        state.scan_secs = state.scan_start.elapsed().as_secs_f64();
        state.board = Some(Board::new(tree.get_current_folder()));
        state.tree = Some(tree);
        state.scanning = false;
        // Lay the tiles out at the real window size first; only then can the initial selection
        // land on a real tile — `move_to_largest_folder` needs a populated board.
        relayout(state, hwnd);
        if let Some(board) = state.board.as_mut() {
            board.move_to_largest_folder();
        }
        refresh_title(state, hwnd);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    thread_local! {
        /// Set while a handler holds `&mut AppState`. `SetWindowTextW` and a modal `MessageBoxW`
        /// synchronously re-enter `wndproc`; forming a second `&mut` to the same `AppState` then
        /// would be undefined behaviour, so re-entrant calls take the guarded path below instead.
        /// Kept outside `AppState` so it is never covered by that `&mut`.
        static IN_HANDLER: ::std::cell::Cell<bool> = const { ::std::cell::Cell::new(false) };
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_CREATE {
            let create = lparam as *const CREATESTRUCTW;
            let state = unsafe { (*create).lpCreateParams } as isize;
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state) };
            return 0;
        }

        let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;

        // WM_APP_DONE owns a `Box<FileTree>`. Reclaim it on any path that will not adopt it — the
        // window already gone, or a re-entrant call — so the scanned tree never leaks.
        if msg == WM_APP_DONE && (state_ptr.is_null() || IN_HANDLER.with(::std::cell::Cell::get)) {
            drop(unsafe { Box::from_raw(lparam as *mut FileTree) });
            return 0;
        }
        if state_ptr.is_null() {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        if msg == WM_DESTROY {
            drop(unsafe { Box::from_raw(state_ptr) });
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        // A re-entrant call must not form a second `&mut AppState`. The default proc touches none
        // of our state, so let it handle the message (e.g. WM_SETTEXT, or WM_PAINT under a modal).
        if IN_HANDLER.with(::std::cell::Cell::get) {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }

        IN_HANDLER.with(|f| f.set(true));
        // A panic must not unwind across the `extern "system"` frame (UB under the unwind profile).
        // Catch it, then fall back to the default proc.
        let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
            dispatch(unsafe { &mut *state_ptr }, hwnd, msg, wparam, lparam)
        }))
        .unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) });
        IN_HANDLER.with(|f| f.set(false));
        result
    }

    /// The message handlers, run with an exclusive `&mut AppState` and behind the re-entrancy guard.
    fn dispatch(
        state: &mut AppState,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_SIZE => {
                relayout(state, hwnd);
                unsafe { InvalidateRect(hwnd, null(), 1) };
                0
            }
            WM_PAINT => {
                paint(state, hwnd);
                0
            }
            WM_LBUTTONDOWN => {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                on_lclick(state, hwnd, x, y);
                0
            }
            WM_RBUTTONDOWN => {
                go_up(state, hwnd);
                0
            }
            WM_MOUSEMOVE => {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                on_mousemove(state, hwnd, x, y);
                0
            }
            WM_KEYDOWN => {
                on_key(state, hwnd, wparam as u16);
                0
            }
            WM_APP_PROGRESS => {
                state.scanned_entries = wparam;
                refresh_title(state, hwnd);
                0
            }
            WM_APP_DONE => {
                let tree = *unsafe { Box::from_raw(lparam as *mut FileTree) };
                adopt_tree(state, hwnd, tree);
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// The classic folder chooser. Returns `None` if the user cancels.
    fn pick_folder() -> Option<PathBuf> {
        let title = wide("Choose a folder to scan");
        let mut display = [0u16; 260];
        let mut info: BROWSEINFOW = unsafe { std::mem::zeroed() };
        info.pszDisplayName = display.as_mut_ptr();
        info.lpszTitle = title.as_ptr();
        info.ulFlags = BIF_RETURNONLYFSDIRS;
        let pidl = unsafe { SHBrowseForFolderW(&info) };
        if pidl.is_null() {
            return None;
        }
        let mut path = [0u16; 260];
        let ok = unsafe { SHGetPathFromIDListW(pidl, path.as_mut_ptr()) };
        unsafe { CoTaskMemFree(pidl as *const c_void) };
        if ok == 0 {
            return None;
        }
        let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
        Some(PathBuf::from(OsString::from_wide(&path[..len])))
    }

    /// Scan `root` on a worker thread, pinging the window with progress and, finally, the tree.
    fn spawn_scan(hwnd: HWND, root: PathBuf) {
        let hwnd = hwnd as usize;
        std::thread::spawn(move || {
            let hwnd = hwnd as HWND;
            let mut entries = 0usize;
            let mut since_ping = 0usize;
            let built = parallel::build_tree(
                &root,
                ScanOptions::default(),
                parallel::SHARDS,
                parallel::SHARD_DEPTH,
                |dir| {
                    entries += dir.len();
                    since_ping += dir.len();
                    if since_ping >= 4096 {
                        since_ping = 0;
                        unsafe { PostMessageW(hwnd, WM_APP_PROGRESS, entries, 0) };
                    }
                    true
                },
            );
            if let Some((tree, _failed, _timings, _small)) = built {
                let boxed = Box::into_raw(Box::new(tree));
                unsafe { PostMessageW(hwnd, WM_APP_DONE, 0, boxed as LPARAM) };
            }
        });
    }

    pub fn run() {
        unsafe { SetProcessDPIAware() };

        let root = match std::env::args_os().nth(1) {
            Some(arg) => PathBuf::from(arg),
            None => match pick_folder() {
                Some(path) => path,
                None => return,
            },
        };

        let state = Box::new(AppState {
            root: root.clone(),
            tree: None,
            board: None,
            scanning: true,
            scanned_entries: 0,
            scan_secs: 0.0,
            scan_start: Instant::now(),
            hover: None,
        });
        let state_ptr = Box::into_raw(state);

        unsafe {
            let hinstance = GetModuleHandleW(null());
            let class_name = wide("DiskonautGuiWindow");
            let wnd_class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&wnd_class);

            let title = wide("diskonaut-gui");
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1100,
                720,
                null_mut(),
                null_mut(),
                hinstance,
                state_ptr as *const _,
            );
            if hwnd.is_null() {
                drop(Box::from_raw(state_ptr));
                return;
            }

            refresh_title(&*state_ptr, hwnd);
            ShowWindow(hwnd, SW_SHOW);
            spawn_scan(hwnd, root);

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
