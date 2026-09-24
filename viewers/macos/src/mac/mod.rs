//! AppKit: the application, its menus and its window. The window's contents are one view,
//! [`view::DiskView`], drawn by [`draw`].

mod draw;
mod view;

use ::std::cell::OnceCell;
use ::std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSEventModifierFlags, NSMenu, NSMenuItem, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSPoint, NSRect, NSSize, NSString,
};

use libdiskonaut::ScanOptions;
use view::DiskView;

/// What the command line asked for.
struct Options {
    folder: Option<PathBuf>,
    apparent: bool,
}

fn options() -> Options {
    let mut options = Options {
        folder: None,
        apparent: false,
    };
    for arg in ::std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-a" | "--apparent-size") => options.apparent = true,
            Some("-h" | "--help") => {
                println!(
                    "diskonaut-mac [-a|--apparent-size] [FOLDER]\n\n\
                     A macOS window on where the disk space went. Without a folder, asks for one."
                );
                ::std::process::exit(0);
            }
            // Launch Services once passed a process serial number to apps opened from Finder.
            Some(flag) if flag.starts_with("-psn_") => {}
            Some(flag) if flag.starts_with('-') => {
                eprintln!("diskonaut-mac: unknown option {flag} (see --help)");
                ::std::process::exit(2);
            }
            _ => options.folder = Some(PathBuf::from(arg)),
        }
    }
    options
}

pub struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    view: OnceCell<Retained<DiskView>>,
    folder: OnceCell<PathBuf>,
}

define_class!(
    // SAFETY: NSObject may be subclassed; `Delegate` has no `Drop` impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DiskonautAppDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the signatures match `NSApplicationDelegate`'s.
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            // Needed when started from a terminal rather than from Finder.
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
            if let Some(window) = self.ivars().window.get() {
                window.makeKeyAndOrderFront(None);
            }
            let Some(view) = self.ivars().view.get() else {
                return;
            };
            match self.ivars().folder.get() {
                Some(folder) => view.start_scan(folder.clone()),
                None => unsafe {
                    let _: () = msg_send![&**view, scanFolder: Option::<&NSObject>::None];
                },
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            window: OnceCell::new(),
            view: OnceCell::new(),
            folder: OnceCell::new(),
        });
        unsafe { msg_send![super(this), init] }
    }
}

pub fn run() {
    let options = options();
    let mtm = MainThreadMarker::new().expect("AppKit runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&menu_bar(mtm, &app)));

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1180.0, 760.0));
    // SAFETY: the window is kept by the delegate, so it must not release itself when closed.
    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window
    };
    window.setTitle(&NSString::from_str("diskonaut"));
    window.setContentMinSize(NSSize::new(520.0, 340.0));
    window.center();
    window.setFrameAutosaveName(&NSString::from_str("diskonaut main window"));
    window.setAcceptsMouseMovedEvents(true);

    let scan_options = ScanOptions {
        show_apparent_size: options.apparent,
        ..ScanOptions::default()
    };
    let view = DiskView::new(mtm, frame, scan_options);
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));

    let ivars = delegate.ivars();
    let _ = ivars.window.set(window);
    let _ = ivars.view.set(view);
    if let Some(folder) = options.folder {
        let _ = ivars.folder.set(folder);
    }
    app.run();
}

/// The menu bar. Commands go to the first responder, which is the view; the standard ones
/// (hide, quit, minimise, full screen) to the application and the window further along.
fn menu_bar(mtm: MainThreadMarker, app: &NSApplication) -> Retained<NSMenu> {
    let command = NSEventModifierFlags::Command;
    let option = NSEventModifierFlags::Option;
    let control = NSEventModifierFlags::Control;
    // U+F700 and U+F701 are AppKit's ↑ and ↓ keys; U+0008 is Backspace (⌫).
    let (up, down, backspace) = ("\u{f700}", "\u{f701}", "\u{8}");

    let bar = NSMenu::new(mtm);
    let app_menu = submenu(mtm, &bar, "diskonaut");
    add(
        mtm,
        &app_menu,
        "About diskonaut",
        sel!(orderFrontStandardAboutPanel:),
        "",
        command,
    );
    separator(mtm, &app_menu);
    let services = submenu(mtm, &app_menu, "Services");
    app.setServicesMenu(Some(&services));
    separator(mtm, &app_menu);
    add(mtm, &app_menu, "Hide diskonaut", sel!(hide:), "h", command);
    add(
        mtm,
        &app_menu,
        "Hide Others",
        sel!(hideOtherApplications:),
        "h",
        command | option,
    );
    add(
        mtm,
        &app_menu,
        "Show All",
        sel!(unhideAllApplications:),
        "",
        command,
    );
    separator(mtm, &app_menu);
    add(
        mtm,
        &app_menu,
        "Quit diskonaut",
        sel!(terminate:),
        "q",
        command,
    );

    let file = submenu(mtm, &bar, "File");
    add(mtm, &file, "Scan Folder…", sel!(scanFolder:), "o", command);
    separator(mtm, &file);
    add(mtm, &file, "Open", sel!(openSelected:), down, command);
    add(
        mtm,
        &file,
        "Enclosing Folder",
        sel!(enclosingFolder:),
        up,
        command,
    );
    add(mtm, &file, "Quick Look", sel!(quickLook:), "y", command);
    add(
        mtm,
        &file,
        "Show in Finder",
        sel!(showInFinder:),
        "r",
        command | option,
    );
    separator(mtm, &file);
    add(
        mtm,
        &file,
        "Move to Trash",
        sel!(moveToTrash:),
        backspace,
        command,
    );
    add(
        mtm,
        &file,
        "Delete Immediately…",
        sel!(deleteImmediately:),
        backspace,
        command | option,
    );
    separator(mtm, &file);
    add(
        mtm,
        &file,
        "Close Window",
        sel!(performClose:),
        "w",
        command,
    );

    let edit = submenu(mtm, &bar, "Edit");
    add(mtm, &edit, "Copy Path", sel!(copy:), "c", command);
    add(
        mtm,
        &edit,
        "Copy as Pathname",
        sel!(copyAsPathname:),
        "c",
        command | option,
    );
    add(mtm, &edit, "Mark All", sel!(selectAll:), "a", command);

    let view = submenu(mtm, &bar, "View");
    add(
        mtm,
        &view,
        "Show Apparent Sizes",
        sel!(toggleApparentSize:),
        "",
        command,
    );
    separator(mtm, &view);
    add(mtm, &view, "Zoom In", sel!(zoomIn:), "+", command);
    add(mtm, &view, "Zoom Out", sel!(zoomOut:), "-", command);
    add(mtm, &view, "Actual Size", sel!(resetZoom:), "0", command);
    separator(mtm, &view);
    add(
        mtm,
        &view,
        "Rescan Folder",
        sel!(rescanFolder:),
        "r",
        command,
    );
    // A capital letter is its own Shift.
    add(
        mtm,
        &view,
        "Rescan Everything",
        sel!(rescanEverything:),
        "R",
        command,
    );
    separator(mtm, &view);
    add(
        mtm,
        &view,
        "Hide Sidebar",
        sel!(toggleSidebar:),
        "s",
        command | control,
    );
    add(
        mtm,
        &view,
        "Enter Full Screen",
        sel!(toggleFullScreen:),
        "f",
        command | control,
    );

    let window = submenu(mtm, &bar, "Window");
    add(
        mtm,
        &window,
        "Minimize",
        sel!(performMiniaturize:),
        "m",
        command,
    );
    add(mtm, &window, "Zoom", sel!(performZoom:), "", command);
    app.setWindowsMenu(Some(&window));
    bar
}

fn submenu(mtm: MainThreadMarker, parent: &NSMenu, title: &str) -> Retained<NSMenu> {
    let title = NSString::from_str(title);
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
    let item = NSMenuItem::new(mtm);
    item.setTitle(&title);
    item.setSubmenu(Some(&menu));
    parent.addItem(&item);
    menu
}

fn add(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    action: Sel,
    key: &str,
    modifiers: NSEventModifierFlags,
) {
    // SAFETY: each action is implemented along the responder chain: by the view, the window or
    // the application.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    item.setKeyEquivalentModifierMask(modifiers);
    menu.addItem(&item);
}

fn separator(mtm: MainThreadMarker, menu: &NSMenu) {
    menu.addItem(&NSMenuItem::separatorItem(mtm));
}
