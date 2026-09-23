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
    use ::std::ffi::OsString;
    use ::std::path::PathBuf;
    use ::std::ptr::{null, null_mut};
    use ::std::time::Instant;

    use libdiskonaut::tiles::{Area, FileType, TreeMap, files_in_folder};
    use libdiskonaut::{DisplaySize, FileTree, ScanOptions, scan_into_tree};

    use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect,
        PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW,
        GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW, IDC_ARROW, LoadCursorW, MSG,
        PostQuitMessage, RegisterClassW, SW_SHOW, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
        TranslateMessage, WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_PAINT, WM_SIZE,
        WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    /// Pixel size of a virtual layout cell. The 2.5:1 ratio matches the treemap's internal
    /// `HEIGHT_WIDTH_RATIO`, so scaling cells back to pixels yields square-looking tiles, and the
    /// treemap's 8x3-cell minimum becomes a readable ~80x12-pixel minimum.
    const CELL_W: i32 = 10;
    const CELL_H: i32 = 4;

    /// One laid-out tile in pixel coordinates, ready to draw and to hit-test.
    struct GuiTile {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        name: OsString,
        size: u128,
        is_dir: bool,
    }

    struct AppState {
        tree: FileTree,
        scan_secs: f64,
        entries: u64,
        tiles: Vec<GuiTile>,
    }

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
    }

    /// A stable colour per tile: folders in blues, files in warm tones, cycling by index.
    fn tile_color(index: usize, is_dir: bool) -> COLORREF {
        const FOLDERS: [(u8, u8, u8); 4] =
            [(52, 101, 164), (60, 120, 190), (44, 88, 140), (72, 138, 210)];
        const FILES: [(u8, u8, u8); 4] =
            [(120, 120, 120), (150, 130, 96), (110, 140, 110), (150, 110, 110)];
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

    /// Lay the current folder out over a `width` x `height` pixel client area.
    fn layout(tree: &FileTree, width: i32, height: i32) -> Vec<GuiTile> {
        if width <= 0 || height <= 0 {
            return Vec::new();
        }
        let folder = tree.get_current_folder();
        let files = files_in_folder(folder, 0);
        let cols = (width / CELL_W).max(1) as u16;
        let rows = (height / CELL_H).max(1) as u16;
        let area = Area {
            x: 0,
            y: 0,
            width: cols,
            height: rows,
        };
        let mut treemap = TreeMap::new(&area);
        treemap.populate_tiles(files.iter().collect());
        treemap
            .tiles
            .into_iter()
            .map(|t| GuiTile {
                x: t.x as i32 * CELL_W,
                y: t.y as i32 * CELL_H,
                w: t.width as i32 * CELL_W,
                h: t.height as i32 * CELL_H,
                name: t.name,
                size: t.size,
                is_dir: t.file_type == FileType::Folder,
            })
            .collect()
    }

    fn relayout(state: &mut AppState, hwnd: HWND) {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe { GetClientRect(hwnd, &mut rect) };
        state.tiles = layout(&state.tree, rect.right - rect.left, rect.bottom - rect.top);
    }

    fn update_title(state: &AppState, hwnd: HWND) {
        let path = state.tree.get_current_path();
        let size = DisplaySize(state.tree.get_current_folder_size() as f64);
        let rate = if state.scan_secs > 0.0 {
            (state.entries as f64 / state.scan_secs) as u64
        } else {
            0
        };
        let title = format!(
            "{} — {}  |  scanned {} entries in {:.2}s ({} entries/s)",
            path.display(),
            size,
            state.entries,
            state.scan_secs,
            rate,
        );
        let wide = wide(&title);
        unsafe { SetWindowTextW(hwnd, wide.as_ptr()) };
    }

    fn paint(state: &AppState, hwnd: HWND) {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        let border = unsafe { CreateSolidBrush(rgb(20, 20, 20)) };
        unsafe { SetBkMode(hdc, TRANSPARENT as i32) };

        for (index, tile) in state.tiles.iter().enumerate() {
            let rect = RECT {
                left: tile.x,
                top: tile.y,
                right: tile.x + tile.w,
                bottom: tile.y + tile.h,
            };
            let fill = unsafe { CreateSolidBrush(tile_color(index, tile.is_dir)) };
            unsafe {
                FillRect(hdc, &rect, fill);
                FrameRect(hdc, &rect, border);
                DeleteObject(fill as _);
            }
            // Label only where there is room for it.
            if tile.w > 60 && tile.h > 18 {
                let name = tile.name.to_string_lossy();
                let label = format!("{}  {}", name, DisplaySize(tile.size as f64));
                let text: Vec<u16> = label.encode_utf16().collect();
                unsafe {
                    SetTextColor(hdc, rgb(240, 240, 240));
                    TextOutW(hdc, tile.x + 4, tile.y + 3, text.as_ptr(), text.len() as i32);
                }
            }
        }

        unsafe {
            DeleteObject(border as _);
            EndPaint(hwnd, &ps);
        }
    }

    fn on_click(state: &mut AppState, hwnd: HWND, x: i32, y: i32) {
        if let Some(tile) = state
            .tiles
            .iter()
            .find(|t| x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h)
        {
            if tile.is_dir {
                let name = tile.name.clone();
                state.tree.enter_folder(&name);
                relayout(state, hwnd);
                update_title(state, hwnd);
                unsafe { InvalidateRect(hwnd, null(), 1) };
            }
        }
    }

    fn on_back(state: &mut AppState, hwnd: HWND) {
        if state.tree.leave_folder() {
            relayout(state, hwnd);
            update_title(state, hwnd);
            unsafe { InvalidateRect(hwnd, null(), 1) };
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // The app state is stashed in the window's user data on WM_CREATE.
        if msg == WM_CREATE {
            let create = lparam as *const CREATESTRUCTW;
            let state = unsafe { (*create).lpCreateParams } as isize;
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state) };
            return 0;
        }

        let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;
        if state_ptr.is_null() {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        let state = unsafe { &mut *state_ptr };

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
                on_click(state, hwnd, x, y);
                0
            }
            WM_RBUTTONDOWN => {
                on_back(state, hwnd);
                0
            }
            WM_DESTROY => {
                // Reclaim and drop the leaked state box.
                drop(unsafe { Box::from_raw(state_ptr) });
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                unsafe { PostQuitMessage(0) };
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub fn run() {
        let root: PathBuf = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let start = Instant::now();
        let (tree, _failed) = scan_into_tree(&root, ScanOptions::default());
        let scan_secs = start.elapsed().as_secs_f64();
        let entries = tree.get_total_descendants();

        let state = Box::new(AppState {
            tree,
            scan_secs,
            entries,
            tiles: Vec::new(),
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
                hIcon: null_mut(),
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
                // Nothing owns the state box if the window never came up.
                drop(Box::from_raw(state_ptr));
                return;
            }

            // Fill the title and the first layout now that the window exists.
            {
                let state = &mut *state_ptr;
                relayout(state, hwnd);
                update_title(state, hwnd);
            }
            ShowWindow(hwnd, SW_SHOW);

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
