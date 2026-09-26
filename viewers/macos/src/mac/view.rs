//! The window's one view. It owns the `Viewer`, turns AppKit events and menu commands into calls
//! on it, draws it (`draw`), and does what only AppKit can: dialogs, the Trash, the pasteboard,
//! Finder and Quick Look.
//!
//! Everything here runs on the main thread. The scan, rescans and the previewer run on threads
//! of their own and come back through [`on_main`].
//!
//! The `Viewer` sits in a `RefCell`. A modal dialog or panel runs the event loop inside the call
//! that opened it, and so can draw, or deliver a scan's findings, meanwhile; no borrow is ever
//! held across one, which is why each command below reads what it needs, lets go, asks, and then
//! borrows again.

use ::std::cell::{Cell, OnceCell, RefCell};
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSBitmapImageFileType,
    NSControlStateValueOff, NSControlStateValueOn, NSDragOperation, NSDraggingDestination,
    NSDraggingInfo, NSEvent, NSEventModifierFlags, NSImage, NSMenu, NSMenuItem, NSModalResponseOK,
    NSOpenPanel, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString, NSResponder,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSDictionary, NSFileManager, NSInteger, NSPoint, NSRect,
    NSSize, NSString, NSURL,
};
use objc2_quick_look_ui::{
    QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource, QLPreviewPanelDelegate,
};

use super::draw::{Frame, draw};
use diskonaut_scan::rescan::{Outcome, Rescanner};
use diskonaut_viewer::menu::{Action, Entry, Platform};
use diskonaut_viewer::preview::{Loaded, Previewer};
use diskonaut_viewer::scan;
use diskonaut_viewer::state::{Direction, Hit, Jump, Mods, Preview, ROW, Rect, Viewer, drop_later};
use libdiskonaut::format::quote_path_for_shell;
use libdiskonaut::model::SizeKind;
use libdiskonaut::{DirSummary, DisplayCount, DisplaySize, FileToDelete, FileTree, ScanOptions};

pub struct Ivars {
    /// Boxed: the tree holds 128-bit sizes, and an Objective-C object's fields cannot be aligned
    /// to 16 bytes.
    viewer: RefCell<Option<Box<Viewer>>>,
    /// The decoded picture the preview shows, when it shows one.
    image: RefCell<Option<Retained<NSImage>>>,
    /// Where the breadcrumbs were drawn, and the depth each goes up to.
    crumbs: RefCell<Vec<(Rect, usize)>>,
    previewer: OnceCell<Previewer>,
    /// Cleared to stop the current scan and its rescans, when another folder is scanned.
    running: RefCell<Arc<AtomicBool>>,
    /// Counts scans, so that findings of one that has been replaced are dropped.
    scans: Cell<u64>,
    options: Cell<ScanOptions>,
    /// Scrolling not yet a whole row.
    scrolled: Cell<f64>,
    /// A pinch not yet a whole zoom step.
    magnified: Cell<f64>,
    /// Outline batches taken into the tree and not yet laid out: one relayout is queued behind
    /// whatever batches are already waiting on the main queue, and does for them all.
    outline_behind: Cell<bool>,
    /// The context menu last opened, for a script to choose from or close (`script`): while it
    /// is open, it reads events itself.
    context_menu: RefCell<Option<Retained<NSMenu>>>,
}

define_class!(
    // SAFETY: NSView may be subclassed; `DiskView` has no `Drop` impl.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DiskonautView"]
    #[ivars = Ivars]
    pub struct DiskView;

    unsafe impl NSObjectProtocol for DiskView {}

    // SAFETY: every signature below matches AppKit's declaration of the method it overrides or
    // implements.
    impl DiskView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: the superclass's method, with the argument it declares.
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.with(|viewer| viewer.resize(size.width, size.height));
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Ok(viewer) = self.ivars().viewer.try_borrow() else {
                return;
            };
            let image = self.ivars().image.borrow();
            let key_window = self.window().is_some_and(|window| window.isKeyWindow());
            let bounds = self.bounds();
            let crumbs = draw(&Frame {
                viewer: viewer.as_deref(),
                image: image.as_deref(),
                key_window,
                bounds: Rect::new(0.0, 0.0, bounds.size.width, bounds.size.height),
            });
            *self.ivars().crumbs.borrow_mut() = crumbs;
        }

        // ------------------------------------------------------------ keyboard

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if !self.key(event) {
                // SAFETY: the superclass's method, with the argument it declares.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        // ------------------------------------------------------------ mouse

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            let crumb = self
                .ivars()
                .crumbs
                .borrow()
                .iter()
                .find(|(rect, _)| rect.contains(x, y))
                .map(|&(_, depth)| depth);
            if let Some(depth) = crumb {
                self.update(|viewer| viewer.go_to_depth(depth));
                return;
            }
            let flags = event.modifierFlags();
            let mods = Mods {
                toggle: flags.contains(NSEventModifierFlags::Command),
                range: flags.contains(NSEventModifierFlags::Shift),
            };
            let double = event.clickCount() == 2 && mods == Mods::default();
            let clicked = self.update(|viewer| {
                match viewer.hit(x, y) {
                    // A folder row's expander opens it in place; the second click of a double
                    // is not a second toggle.
                    Hit::Expander(index) => {
                        if event.clickCount() == 1 {
                            viewer.toggle_row(index);
                        }
                        return None;
                    }
                    Hit::SmallFiles => viewer
                        .say("Entries too small for a tile of their own: they are all in the list"),
                    _ => {}
                }
                viewer.click(x, y, mods)?;
                // A double click opens a folder — the row in hand, nested or not; a file is
                // shown in Quick Look.
                Some(double && !viewer.enter_selected())
            });
            if clicked.flatten() == Some(true) {
                self.toggle_quick_look();
            } else {
                self.reload_quick_look();
            }
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
            let (x, y) = self.point(event);
            let entries = self
                .update(|viewer| {
                    if viewer.context_click(x, y) {
                        viewer.context_menu(&PLATFORM)
                    } else {
                        Vec::new()
                    }
                })
                .unwrap_or_default();
            let menu = (!entries.is_empty()).then(|| context_menu(self.mtm(), &entries));
            self.ivars().context_menu.replace(menu.clone());
            menu
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            if self.with(|viewer| viewer.hover_at(x, y)) == Some(true) {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            // Nothing is under a pointer that has gone: the row, the tile and the nested tile.
            if self.with(|viewer| viewer.hover_at(-1.0, -1.0)) == Some(true) {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            let (over_list, over_treemap) = self
                .with(|viewer| {
                    let layout = viewer.layout;
                    (
                        layout.list.is_some_and(|list| list.contains(x, y)),
                        layout.treemap.contains(x, y),
                    )
                })
                .unwrap_or_default();
            // A mouse's wheel over the treemap zooms; a trackpad's two fingers do not, as
            // they scroll everywhere else — a pinch zooms instead (`magnifyWithEvent:`).
            if over_treemap && !event.hasPreciseScrollingDeltas() {
                let delta = event.scrollingDeltaY();
                if delta > 0.0 {
                    self.update(Viewer::zoom_in);
                } else if delta < 0.0 {
                    self.update(Viewer::zoom_out);
                }
                return;
            }
            if !over_list {
                return;
            }
            // A trackpad reports points; a wheel, lines of about three rows.
            let delta = if event.hasPreciseScrollingDeltas() {
                -event.scrollingDeltaY() / ROW
            } else {
                -event.scrollingDeltaY() * 3.0
            };
            let scrolled = self.ivars().scrolled.get() + delta;
            let rows = scrolled.trunc();
            self.ivars().scrolled.set(scrolled - rows);
            if rows != 0.0 {
                self.with(|viewer| viewer.scroll_list(rows as isize));
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            if self.with(|viewer| viewer.layout.treemap.contains(x, y)) != Some(true) {
                return;
            }
            // A step for each quarter of magnification, however the pinch arrives.
            let pinched = self.ivars().magnified.get() + event.magnification() * 4.0;
            let steps = pinched.trunc();
            self.ivars().magnified.set(pinched - steps);
            for _ in 0..(steps.abs() as usize) {
                if steps > 0.0 {
                    self.update(Viewer::zoom_in);
                } else {
                    self.update(Viewer::zoom_out);
                }
            }
        }

        /// The mouse's back (thumb) button goes up a folder.
        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            if event.buttonNumber() == 3 {
                self.update(Viewer::go_up);
            }
        }

        // ------------------------------------------------------------ menu commands

        #[unsafe(method(scanFolder:))]
        fn scan_folder(&self, _sender: Option<&AnyObject>) {
            let panel = NSOpenPanel::openPanel(self.mtm());
            panel.setCanChooseDirectories(true);
            panel.setCanChooseFiles(false);
            panel.setAllowsMultipleSelection(false);
            panel.setPrompt(Some(&NSString::from_str("Scan")));
            panel.setMessage(Some(&NSString::from_str("Choose a folder to scan")));
            if panel.runModal() == NSModalResponseOK
                && let Some(path) = panel.URLs().firstObject().and_then(|url| url.to_file_path())
            {
                self.start_scan(path);
            }
        }

        #[unsafe(method(openSelected:))]
        fn open_selected(&self, _sender: Option<&AnyObject>) {
            let file = self.update(Viewer::open_in_hand);
            if let Some(url) = file.flatten().and_then(NSURL::from_file_path) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }

        #[unsafe(method(enclosingFolder:))]
        fn enclosing_folder(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::go_up);
        }

        #[unsafe(method(showInFinder:))]
        fn show_in_finder(&self, _sender: Option<&AnyObject>) {
            let urls: Vec<Retained<NSURL>> = self
                .paths_or_folder()
                .into_iter()
                .filter_map(NSURL::from_file_path)
                .collect();
            let urls: Vec<&NSURL> = urls.iter().map(|url| &**url).collect();
            NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_slice(&urls));
        }

        #[unsafe(method(quickLook:))]
        fn quick_look(&self, _sender: Option<&AnyObject>) {
            self.toggle_quick_look();
        }

        #[unsafe(method(moveToTrash:))]
        fn move_to_trash(&self, _sender: Option<&AnyObject>) {
            self.remove(false);
        }

        #[unsafe(method(deleteImmediately:))]
        fn delete_immediately(&self, _sender: Option<&AnyObject>) {
            self.remove(true);
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.copy_paths(true);
        }

        #[unsafe(method(copyAsPathname:))]
        fn copy_as_pathname(&self, _sender: Option<&AnyObject>) {
            self.copy_paths(false);
        }

        /// The context menu's Copy Path: relative to the folder the app was started in, where
        /// it was started from a terminal, as the other viewers copy.
        #[unsafe(method(copyRelativePath:))]
        fn copy_relative_path(&self, _sender: Option<&AnyObject>) {
            let Some((text, label)) = self.with(|viewer| viewer.copied_paths(false)).flatten() else {
                return;
            };
            let message = if put_on_pasteboard(&text) {
                format!("{label} {text}")
            } else {
                "Could not copy to the clipboard".to_string()
            };
            self.update(|viewer| viewer.say(message));
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::mark_all);
        }

        #[unsafe(method(toggleApparentSize:))]
        fn toggle_apparent_size(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::toggle_size);
        }

        #[unsafe(method(zoomIn:))]
        fn zoom_in(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::zoom_in);
        }

        #[unsafe(method(zoomOut:))]
        fn zoom_out(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::zoom_out);
        }

        #[unsafe(method(resetZoom:))]
        fn reset_zoom(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::reset_zoom);
        }

        #[unsafe(method(rescanFolder:))]
        fn rescan_folder(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::rescan_selected);
        }

        #[unsafe(method(rescanEverything:))]
        fn rescan_everything(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::rescan_all);
        }

        #[unsafe(method(toggleSidebar:))]
        fn toggle_sidebar(&self, _sender: Option<&AnyObject>) {
            self.update(Viewer::toggle_sidebar);
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.validate(item)
        }

        // ------------------------------------------------------------ Quick Look

        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: Option<&QLPreviewPanel>) -> bool {
            true
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel_control(&self, panel: Option<&QLPreviewPanel>) {
            if let Some(panel) = panel {
                // SAFETY: the panel is told to let go of this view in `endPreviewPanelControl:`,
                // and the view lives as long as the window.
                unsafe {
                    panel.setDataSource(Some(ProtocolObject::from_ref(self)));
                    panel.setDelegate(Some(self));
                }
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel_control(&self, panel: Option<&QLPreviewPanel>) {
            if let Some(panel) = panel {
                // SAFETY: clearing both is always allowed.
                unsafe {
                    panel.setDataSource(None);
                    panel.setDelegate(None);
                }
            }
        }
    }

    unsafe impl QLPreviewPanelDataSource for DiskView {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn number_of_preview_items(&self, _panel: Option<&QLPreviewPanel>) -> NSInteger {
            self.quick_look_paths().len() as NSInteger
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn preview_item(
            &self,
            _panel: Option<&QLPreviewPanel>,
            index: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            self.quick_look_paths()
                .into_iter()
                .nth(index as usize)
                .and_then(NSURL::from_file_path)
                .map(ProtocolObject::from_retained)
        }
    }

    unsafe impl NSWindowDelegate for DiskView {}

    unsafe impl QLPreviewPanelDelegate for DiskView {
        /// Keys pressed in the panel move through the folder here, and the panel follows.
        #[unsafe(method(previewPanel:handleEvent:))]
        fn preview_panel_handle_event(
            &self,
            _panel: Option<&QLPreviewPanel>,
            event: Option<&NSEvent>,
        ) -> bool {
            match event {
                Some(event) if event.r#type() == objc2_app_kit::NSEventType::KeyDown => {
                    self.key(event)
                }
                _ => false,
            }
        }
    }

    // A folder dropped on the window is scanned.
    unsafe impl NSDraggingDestination for DiskView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if dropped_folder(sender).is_some() {
                NSDragOperation::Generic
            } else {
                NSDragOperation::None
            }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            match dropped_folder(sender) {
                Some(path) => {
                    self.start_scan(path);
                    true
                }
                None => false,
            }
        }
    }
);

thread_local! {
    /// The window's view, for work coming back from other threads to find.
    static VIEW: OnceCell<Retained<DiskView>> = const { OnceCell::new() };
}

/// Run `work` on the main thread with the view. Should the viewer be borrowed at that moment,
/// it goes back on the queue rather than be lost.
pub fn on_main(work: impl FnOnce(&DiskView) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        let Some(view) = VIEW.with(|cell| cell.get().cloned()) else {
            return;
        };
        if view.ivars().viewer.try_borrow_mut().is_err() {
            on_main(work);
            return;
        }
        work(&view);
    });
}

impl DiskView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, options: ScanOptions) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            viewer: RefCell::new(None),
            image: RefCell::new(None),
            crumbs: RefCell::new(Vec::new()),
            previewer: OnceCell::new(),
            running: RefCell::new(Arc::new(AtomicBool::new(false))),
            scans: Cell::new(0),
            options: Cell::new(options),
            scrolled: Cell::new(0.0),
            magnified: Cell::new(0.0),
            outline_behind: Cell::new(false),
            context_menu: RefCell::new(None),
        });
        // SAFETY: the superclass's designated initialiser, on the instance just allocated.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // SAFETY: the view owns the area, and `InVisibleRect` keeps it matched to the view.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::ActiveInKeyWindow
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&area);
        // SAFETY: reading AppKit's pasteboard type constant.
        let file_url = unsafe { NSPasteboardTypeFileURL };
        view.registerForDraggedTypes(&NSArray::from_slice(&[file_url]));
        VIEW.with(|cell| {
            let _ = cell.set(view.clone());
        });
        let _ = view
            .ivars()
            .previewer
            .set(Previewer::spawn(|generation, loaded| {
                on_main(move |view| view.preview_arrived(generation, loaded));
            }));
        view
    }

    /// Run `change` on the viewer, if there is one and it is free.
    fn with<R>(&self, change: impl FnOnce(&mut Viewer) -> R) -> Option<R> {
        let mut viewer = self.ivars().viewer.try_borrow_mut().ok()?;
        viewer.as_deref_mut().map(change)
    }

    /// Run `change` on the viewer, then bring the window up to date with it.
    fn update<R>(&self, change: impl FnOnce(&mut Viewer) -> R) -> Option<R> {
        let result = self.with(change);
        self.changed();
        result
    }

    /// After any change: the title, the preview, and a redraw.
    fn changed(&self) {
        let Ok(mut slot) = self.ivars().viewer.try_borrow_mut() else {
            return;
        };
        let Some(viewer) = slot.as_deref_mut() else {
            return;
        };
        if let Some(window) = self.window() {
            window.setTitle(&NSString::from_str(&viewer.title()));
            window.setSubtitle(&NSString::from_str(&viewer.subtitle()));
            let url = NSURL::from_directory_path(viewer.tree.get_current_path());
            window.setRepresentedURL(url.as_deref());
        }
        if let Some((generation, path)) = viewer.wanted_preview() {
            self.ivars().image.replace(None);
            if let Some(previewer) = self.ivars().previewer.get() {
                previewer.request(generation, path);
            }
        } else if viewer.preview == Preview::None {
            self.ivars().image.replace(None);
        }
        drop(slot);
        self.setNeedsDisplay(true);
    }

    fn point(&self, event: &NSEvent) -> (f64, f64) {
        let point: NSPoint = self.convertPoint_fromView(event.locationInWindow(), None);
        (point.x, point.y)
    }

    /// A key, whether pressed here or in the Quick Look panel. Returns whether it meant anything.
    fn key(&self, event: &NSEvent) -> bool {
        let flags = event.modifierFlags();
        if flags.intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control) {
            return false;
        }
        let extend = flags.contains(NSEventModifierFlags::Shift);
        let characters = event
            .charactersIgnoringModifiers()
            .map(|characters| characters.to_string())
            .unwrap_or_default();
        // Hardware key codes for the keys that type no character of their own.
        match event.keyCode() {
            123 => self.update(|viewer| viewer.arrow(Direction::Left, extend)),
            124 => self.update(|viewer| viewer.arrow(Direction::Right, extend)),
            125 => self.update(|viewer| viewer.arrow(Direction::Down, extend)),
            126 => self.update(|viewer| viewer.arrow(Direction::Up, extend)),
            116 => self.update(|viewer| viewer.jump(Jump::PageUp, extend)),
            121 => self.update(|viewer| viewer.jump(Jump::PageDown, extend)),
            115 => self.update(|viewer| viewer.jump(Jump::Home, extend)),
            119 => self.update(|viewer| viewer.jump(Jump::End, extend)),
            // Return and keypad Enter.
            36 | 76 => self.update(|viewer| {
                viewer.enter_selected();
            }),
            // Esc and Backspace.
            53 | 51 => self.update(|viewer| {
                viewer.go_up();
            }),
            48 => self.update(Viewer::toggle_focus),
            // Forward delete.
            117 => {
                self.remove(false);
                Some(())
            }
            49 => {
                self.toggle_quick_look();
                return true;
            }
            _ => match characters.as_str() {
                "a" => self.update(Viewer::toggle_size),
                "+" | "=" => self.update(Viewer::zoom_in),
                "-" | "_" => self.update(Viewer::zoom_out),
                "0" => self.update(Viewer::reset_zoom),
                "r" => self.update(Viewer::rescan_selected),
                "R" => self.update(Viewer::rescan_all),
                "d" => {
                    self.remove(false);
                    Some(())
                }
                _ => return false,
            },
        };
        self.reload_quick_look();
        true
    }

    // ---------------------------------------------------------------- scanning

    /// Scan `root`, in place of whatever the window showed.
    pub fn start_scan(&self, root: PathBuf) {
        let root = root.canonicalize().unwrap_or(root);
        // The walk reports a missing root as an empty folder; say what is wrong instead.
        if !root.is_dir() {
            self.alert(
                NSAlertStyle::Warning,
                &format!("“{}” is not a folder", root.display()),
                "Choose a folder to scan with File ▸ Scan Folder…, or drop one on the window.",
                &["OK"],
            );
            return;
        }
        let running = Arc::new(AtomicBool::new(true));
        self.ivars()
            .running
            .replace(Arc::clone(&running))
            .store(false, Ordering::Release);
        let scan_id = self.ivars().scans.get() + 1;
        self.ivars().scans.set(scan_id);
        let mut options = self.ivars().options.get();

        let old = self.ivars().viewer.borrow_mut().take();
        // The size shown, and the side panel, carry over from the folder scanned before.
        let (kind, sidebar) = match &old {
            Some(old) => (old.tree.shown, old.sidebar),
            None if options.show_apparent_size => (SizeKind::Apparent, true),
            None => (SizeKind::Disk, true),
        };
        options.show_apparent_size = kind == SizeKind::Apparent;
        if let Some(mut old) = old {
            old.cancel_rescans();
            drop_later(::std::mem::ManuallyDrop::into_inner(old.tree));
        }
        let mut viewer = Viewer::new(&root, kind, scan_id);
        viewer.sidebar = sidebar;
        viewer.set_tree_view(true);
        viewer.enable_rescans(Rescanner::new(
            options,
            Arc::clone(&running),
            move |id, outcome| {
                on_main(move |view| view.rescan_done(scan_id, id, outcome));
            },
        ));
        let bounds = self.bounds();
        viewer.resize(bounds.size.width, bounds.size.height);
        self.ivars().viewer.replace(Some(Box::new(viewer)));
        self.ivars().image.replace(None);
        scan::spawn(
            root,
            options,
            running,
            move |summaries| on_main(move |view| view.scan_batch(scan_id, summaries)),
            move |tree| on_main(move |view| view.scan_done(scan_id, tree)),
        );
        self.changed();
    }

    fn scan_batch(&self, scan_id: u64, summaries: Vec<DirSummary>) {
        let current = self.with(|viewer| {
            let current = viewer.scan_id == scan_id;
            if current {
                viewer.absorb_summaries(summaries);
            }
            current
        });
        if current == Some(true) && !self.ivars().outline_behind.replace(true) {
            on_main(|view| {
                view.ivars().outline_behind.set(false);
                view.update(Viewer::catch_up);
            });
        }
    }

    fn scan_done(&self, scan_id: u64, tree: Option<FileTree>) {
        let Some(tree) = tree else {
            return;
        };
        let current = self.with(|viewer| viewer.scan_id == scan_id) == Some(true);
        if !current {
            drop_later(tree);
            return;
        }
        self.update(|viewer| viewer.finish_scan(tree));
        if super::script::run_from_environment() {
            return;
        }
        if let Some(path) = ::std::env::var_os("DISKONAUT_MAC_SNAPSHOT") {
            // Long enough for the preview of what is in hand to arrive.
            ::std::thread::spawn(move || {
                ::std::thread::sleep(::std::time::Duration::from_millis(500));
                on_main(move |view| {
                    view.snapshot(Path::new(&path));
                    NSApplication::sharedApplication(view.mtm()).terminate(None);
                });
            });
        }
    }

    /// Draw the window's contents into a PNG at `path`: a way to look at the drawing without
    /// screen access, for development (`DISKONAUT_MAC_SNAPSHOT=out.png diskonaut-mac FOLDER`).
    pub fn snapshot(&self, path: &Path) {
        let bounds = self.bounds();
        let Some(bitmap) = self.bitmapImageRepForCachingDisplayInRect(bounds) else {
            return;
        };
        self.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);
        // SAFETY: no properties are passed.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        };
        if let Some(png) = png {
            png.writeToFile_atomically(&NSString::from_str(&path.to_string_lossy()), true);
        }
    }

    fn rescan_done(&self, scan_id: u64, id: u64, outcome: Outcome) {
        let current = self.with(|viewer| viewer.scan_id == scan_id) == Some(true);
        if current {
            self.update(|viewer| viewer.rescan_done(id, outcome));
            self.reload_quick_look();
        } else if let Outcome::Scanned(tree, ..) = outcome {
            drop_later(tree);
        }
    }

    fn preview_arrived(&self, generation: u64, loaded: Loaded) {
        let (preview, image) = match loaded {
            Loaded::Info(info) => (Preview::Info(info), None),
            Loaded::Text(lines) => (Preview::Text(lines), None),
            Loaded::Binary { info, dump } => (Preview::Hex { info, dump }, None),
            Loaded::Picture { bytes, caption } => {
                let data = NSData::from_vec(bytes);
                match NSImage::initWithData(NSImage::alloc(), &data) {
                    Some(image) if image.size().width > 0.0 => match pixels(&image) {
                        // Decoding happens when it is drawn, so an enormous picture — a few
                        // megabytes that expand to gigabytes — is refused before then.
                        Some((w, h)) if w.saturating_mul(h) > MAX_PICTURE_PIXELS => (
                            Preview::Info(format!("{caption}, too large to preview")),
                            None,
                        ),
                        _ => (Preview::Picture(caption), Some(image)),
                    },
                    _ => (
                        Preview::Info(format!("{caption}, which this Mac cannot decode")),
                        None,
                    ),
                }
            }
        };
        if self.with(|viewer| viewer.preview_ready(generation, preview)) == Some(true) {
            self.ivars().image.replace(image);
            self.setNeedsDisplay(true);
        }
    }

    /// Choose `title` from the context menu last opened, or with `None` close it. Returns
    /// whether there was such an item.
    pub fn choose_from_context_menu(&self, title: Option<&str>) -> bool {
        let Some(menu) = self.ivars().context_menu.take() else {
            return false;
        };
        let chosen = match title {
            Some(title) => {
                let index = menu.indexOfItemWithTitle(&NSString::from_str(title));
                if index >= 0 {
                    menu.performActionForItemAtIndex(index);
                }
                index >= 0
            }
            None => true,
        };
        menu.cancelTracking();
        chosen
    }

    /// What the viewer holds, as `name: value` lines, for a script to compare (`script`).
    pub fn state_for_script(&self) -> String {
        let window = self.window();
        let title = window
            .as_ref()
            .map(|window| window.title().to_string())
            .unwrap_or_default();
        let app = NSApplication::sharedApplication(self.mtm());
        let key_window = app
            .keyWindow()
            .map(|window| {
                // Without the prefix of the subclass key-value observing makes at run time.
                let class = window.class().name().to_string_lossy().into_owned();
                let class = class.trim_start_matches("NSKVONotifying_").to_string();
                format!("{} ({class})", window.title())
            })
            .unwrap_or_default();
        let key_window = format!("{key_window}, active {}", app.isActive());
        let lossy = |names: &[::std::ffi::OsString]| {
            names
                .iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let viewer = self.ivars().viewer.borrow();
        let Some(viewer) = viewer.as_deref() else {
            return format!("viewer: none\ntitle: {title}\n");
        };
        let listing: Vec<_> = viewer
            .board
            .listing()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        // The tree's rows, and the one in hand, each as its path from the folder shown.
        let row_path = |path: &[::std::ffi::OsString]| {
            path.iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        };
        let rows: Vec<String> = viewer
            .rows()
            .iter()
            .map(|row| row_path(&row.path))
            .collect();
        let cursor = viewer
            .cursor_entry()
            .map(|row| row_path(&row.path))
            .unwrap_or_default();
        let (status, totals) = viewer.status();
        [
            ("title", title),
            ("key window", key_window),
            ("path", lossy(&viewer.tree.current_folder_names)),
            ("listing", lossy(&listing)),
            ("selected", lossy(viewer.selected.as_slice())),
            ("rows", rows.join(" | ")),
            ("cursor", cursor),
            ("marked", lossy(&viewer.marked)),
            ("focus", format!("{:?}", viewer.focus)),
            ("zoom", viewer.board.zoom_level.to_string()),
            ("apparent", viewer.showing_apparent().to_string()),
            ("sidebar", viewer.sidebar.to_string()),
            ("scanning", viewer.scanning.to_string()),
            ("preview", format!("{:?}", viewer.preview)),
            ("image", self.ivars().image.borrow().is_some().to_string()),
            ("status", status),
            ("totals", totals),
        ]
        .iter()
        .map(|(name, value)| format!("{name}: {value}\n"))
        .collect()
    }

    // ---------------------------------------------------------------- acting on entries

    /// What a command acts on: the marked entries or the one in hand, else the folder shown.
    fn paths_or_folder(&self) -> Vec<PathBuf> {
        self.with(|viewer| {
            let paths = viewer.target_paths();
            if paths.is_empty() {
                vec![viewer.tree.get_current_path()]
            } else {
                paths
            }
        })
        .unwrap_or_default()
    }

    fn copy_paths(&self, quoted: bool) {
        let paths = self.paths_or_folder();
        if paths.is_empty() {
            return;
        }
        // Quoted for the shell, like the terminal viewer's copies; or plain, one per line, like
        // Finder's Copy as Pathname.
        let text = if quoted {
            paths
                .iter()
                .map(|path| quote_path_for_shell(path))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            paths
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let copied = put_on_pasteboard(&text);
        let message = match (copied, paths.len()) {
            (false, _) => "Could not copy to the clipboard".to_string(),
            (true, 1) => format!("Copied {text}"),
            (true, n) => format!("Copied {} paths", DisplayCount(n as u64)),
        };
        self.update(|viewer| viewer.say(message));
    }

    /// Move what is marked or in hand to the Trash, or delete it for good, once confirmed.
    fn remove(&self, permanently: bool) {
        let Some((files, scanning, scan_id)) =
            self.with(|viewer| (viewer.targets(), viewer.scanning, viewer.scan_id))
        else {
            return;
        };
        if files.is_empty() {
            if scanning {
                self.update(|viewer| viewer.say("Deleting waits until the scan has finished"));
            }
            return;
        }
        if let Some(name) = libdiskonaut::delete::refused(&files) {
            self.alert(
                NSAlertStyle::Warning,
                "This cannot be deleted",
                &format!("NTFS metadata belongs to the filesystem: {name}"),
                &["OK"],
            );
            return;
        }
        let (question, verb) = confirmation(&files, permanently);
        let detail = describe_files(&files, permanently);
        let style = if permanently {
            NSAlertStyle::Critical
        } else {
            NSAlertStyle::Warning
        };
        if self.alert(style, &question, &detail, &[verb, "Cancel"]) != NSAlertFirstButtonReturn {
            return;
        }

        let mut removed = Vec::new();
        let mut failures = Vec::new();
        for file in files {
            let result = if permanently {
                libdiskonaut::delete::remove(&file).map_err(|error| error.to_string())
            } else {
                trash(&file.full_path())
            };
            match result {
                Ok(()) => removed.push(file),
                Err(error) => failures.push((file, error)),
            }
        }
        let size: u128 = removed.iter().map(|file| file.size).sum();
        self.update(|viewer| {
            // A scan of another folder that began meanwhile has a tree these are not in.
            if viewer.scan_id != scan_id {
                return;
            }
            viewer.removed(&removed, permanently);
            if !removed.is_empty() {
                let items = match removed.len() {
                    1 => "1 item".to_string(),
                    n => format!("{} items", DisplayCount(n as u64)),
                };
                viewer.say(if permanently {
                    format!("Deleted {items}, freeing {}", DisplaySize(size as f64))
                } else {
                    format!("Moved {items} ({}) to the Trash", DisplaySize(size as f64))
                });
            }
        });
        self.reload_quick_look();
        if let Some((file, error)) = failures.first() {
            let name = file.full_path().display().to_string();
            let more = match failures.len() {
                1 => String::new(),
                n => format!(
                    "\n\n{} more could not be removed either.",
                    DisplayCount(n as u64 - 1)
                ),
            };
            self.alert(
                NSAlertStyle::Warning,
                &format!("Could not remove {name}"),
                &format!("{error}{more}"),
                &["OK"],
            );
        }
    }

    /// An app-modal alert. Returns which button closed it (`NSAlertFirstButtonReturn` + n).
    fn alert(
        &self,
        style: NSAlertStyle,
        message: &str,
        detail: &str,
        buttons: &[&str],
    ) -> NSInteger {
        let alert = NSAlert::new(self.mtm());
        alert.setAlertStyle(style);
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(detail));
        for button in buttons {
            alert.addButtonWithTitle(&NSString::from_str(button));
        }
        alert.runModal()
    }

    fn validate(&self, item: &NSMenuItem) -> bool {
        let Some(action) = item.action() else {
            return true;
        };
        let Ok(viewer) = self.ivars().viewer.try_borrow() else {
            return false;
        };
        let Some(viewer) = viewer.as_deref() else {
            return action == sel!(scanFolder:);
        };
        let has_target = !viewer.target_names().is_empty();
        match action {
            a if a == sel!(moveToTrash:) || a == sel!(deleteImmediately:) => {
                has_target && !viewer.scanning
            }
            a if a == sel!(openSelected:) || a == sel!(quickLook:) => has_target,
            a if a == sel!(enclosingFolder:) => viewer.depth() > 0,
            a if a == sel!(rescanFolder:) || a == sel!(rescanEverything:) => viewer.can_rescan(),
            a if a == sel!(toggleApparentSize:) => {
                item.setState(if viewer.showing_apparent() {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                true
            }
            a if a == sel!(toggleSidebar:) => {
                let title = if viewer.sidebar {
                    "Hide Sidebar"
                } else {
                    "Show Sidebar"
                };
                item.setTitle(&NSString::from_str(title));
                true
            }
            _ => true,
        }
    }

    // ---------------------------------------------------------------- Quick Look

    fn quick_look_paths(&self) -> Vec<PathBuf> {
        self.with(|viewer| viewer.target_paths())
            .unwrap_or_default()
    }

    fn toggle_quick_look(&self) {
        let mtm = self.mtm();
        // SAFETY: the shared panel is AppKit's, used on the main thread.
        unsafe {
            let Some(panel) = QLPreviewPanel::sharedPreviewPanel(mtm) else {
                return;
            };
            if QLPreviewPanel::sharedPreviewPanelExists(mtm) && panel.isVisible() {
                panel.orderOut(None);
            } else {
                panel.makeKeyAndOrderFront(None);
            }
        }
    }

    /// Show what is now in hand in the Quick Look panel, if it is open.
    fn reload_quick_look(&self) {
        let mtm = self.mtm();
        // SAFETY: as in `toggle_quick_look`; the panel is not created here if it does not exist.
        unsafe {
            if QLPreviewPanel::sharedPreviewPanelExists(mtm)
                && let Some(panel) = QLPreviewPanel::sharedPreviewPanel(mtm)
                && panel.isVisible()
            {
                panel.reloadData();
            }
        }
    }
}

/// Pictures with more pixels than this are described rather than drawn: 1 GiB decoded, at four
/// bytes a pixel — the limit `libdiskonaut::preview::decode_picture` sets for the terminal viewer.
const MAX_PICTURE_PIXELS: isize = (1 << 30) / 4;

/// A picture's size in pixels, from its header: the largest of its representations.
fn pixels(image: &NSImage) -> Option<(isize, isize)> {
    image
        .representations()
        .iter()
        .map(|rep| (rep.pixelsWide(), rep.pixelsHigh()))
        .max_by_key(|&(w, h)| w.saturating_mul(h))
}

/// The question and the button a removal asks with.
fn confirmation(files: &[FileToDelete], permanently: bool) -> (String, &'static str) {
    let what = match files {
        [one] => format!(
            "“{}”",
            one.path_to_file
                .last()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        many => format!("{} items", DisplayCount(many.len() as u64)),
    };
    if permanently {
        (format!("Delete {what} immediately?"), "Delete")
    } else {
        (format!("Move {what} to the Trash?"), "Move to Trash")
    }
}

fn describe_files(files: &[FileToDelete], permanently: bool) -> String {
    let size: u128 = files.iter().map(|file| file.size).sum();
    let mut words = match files {
        [one] => {
            let contents = match one.num_descendants {
                Some(count) if one.file_type == libdiskonaut::FileType::Folder => {
                    format!(", a folder of {} items", DisplayCount(count))
                }
                _ => String::new(),
            };
            format!(
                "{}{contents}\n\n{}",
                DisplaySize(size as f64),
                one.full_path().display()
            )
        }
        many => {
            let names: Vec<String> = many
                .iter()
                .take(5)
                .filter_map(|file| file.path_to_file.last())
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            let more = if many.len() > 5 {
                format!(" and {} more", DisplayCount(many.len() as u64 - 5))
            } else {
                String::new()
            };
            format!(
                "{} in all: {}{more}",
                DisplaySize(size as f64),
                names.join(", ")
            )
        }
    };
    if permanently {
        words += "\n\nThis can’t be undone.";
    }
    words
}

/// Move `path` to the Trash — a link itself, never what it points to.
fn trash(path: &Path) -> Result<(), String> {
    let url = NSURL::from_file_path(path).ok_or_else(|| "not a file path".to_string())?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| error.localizedDescription().to_string())
}

/// The folder being dragged over the window, if it is one folder.
fn dropped_folder(sender: &ProtocolObject<dyn NSDraggingInfo>) -> Option<PathBuf> {
    let pasteboard = sender.draggingPasteboard();
    // SAFETY: reading AppKit's pasteboard type constant.
    let url = pasteboard.stringForType(unsafe { NSPasteboardTypeFileURL })?;
    let url = NSURL::URLWithString(&url)?;
    let path = url.to_file_path()?;
    path.is_dir().then_some(path)
}

/// The menu a right-click (or Control-click) on an entry opens.
/// What the context menu offers here beyond every viewer's items (`diskonaut_viewer::menu`).
const PLATFORM: Platform = Platform {
    reveal: "Show in Finder",
    quick_look: true,
    pathname: true,
    trash: true,
};

/// The context menu, as `Viewer::context_menu` has it: each item sent along the responder chain
/// to the command the menu bar sends, enabled as the shared menu says.
fn context_menu(mtm: MainThreadMarker, entries: &[Entry]) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    for entry in entries {
        let Entry::Item {
            action,
            label,
            enabled,
        } = entry
        else {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            continue;
        };
        let selector = match action {
            Action::Open => sel!(openSelected:),
            Action::QuickLook => sel!(quickLook:),
            Action::Reveal => sel!(showInFinder:),
            Action::CopyPath => sel!(copyRelativePath:),
            Action::CopyFullPath => sel!(copy:),
            Action::CopyPathname => sel!(copyAsPathname:),
            Action::Rescan => sel!(rescanFolder:),
            Action::RescanAll => sel!(rescanEverything:),
            Action::Trash => sel!(moveToTrash:),
            Action::Delete => sel!(deleteImmediately:),
        };
        // SAFETY: each action is a method of `DiskView`, found along the responder chain.
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(label),
                Some(selector),
                &NSString::from_str(""),
            )
        };
        item.setEnabled(*enabled);
    }
    menu
}

/// `text` on the general pasteboard, as a string. Whether it took.
fn put_on_pasteboard(text: &str) -> bool {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: reading AppKit's pasteboard type constant.
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
}
