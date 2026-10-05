//! The volume's change log on macOS: FSEvents, replayed since an id to say which directories
//! changed after a saved scan ([`crate::cache`]).
//!
//! CoreServices is opened with `dlopen` when a replay is asked for, not linked: the viewers
//! keep every framework out of their load commands (`viewers/macos/src/mac/appkit.rs`), since
//! each one loaded costs the start. The stream is served on a dispatch queue of its own; the
//! callback sends each batch over a channel to the caller, which waits until the log says
//! `HistoryDone`, or gives up after `give_up` — a log three weeks deep on a small tree can
//! take longer to replay than the tree takes to walk.
//!
//! What the events mean here: a path is a directory whose contents changed (the stream is
//! made without file events, so a file's change comes as its directory), `MustScanSubDirs`
//! that the log lost events under it (dropped, or the log wrapped), `RootChanged` that the
//! watched root itself moved, `EventIdsWrapped` that the ids started over. Any of the last
//! two, or the root to be walked whole, means the saved scan is not brought up to date.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crate::cache::Changes;

const MUST_SCAN_SUBDIRS: u32 = 0x1;
const USER_DROPPED: u32 = 0x2;
const KERNEL_DROPPED: u32 = 0x4;
const EVENT_IDS_WRAPPED: u32 = 0x8;
const HISTORY_DONE: u32 = 0x10;
const ROOT_CHANGED: u32 = 0x20;

type StreamRef = *mut c_void;
type Callback = extern "C" fn(StreamRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

/// CoreServices' and CoreFoundation's entry points, resolved once.
struct Api {
    get_current_event_id: unsafe extern "C" fn() -> u64,
    copy_uuid_for_device: unsafe extern "C" fn(i32) -> *const c_void,
    stream_create: unsafe extern "C" fn(
        *const c_void,
        Callback,
        *const Context,
        *const c_void,
        u64,
        f64,
        u32,
    ) -> StreamRef,
    set_dispatch_queue: unsafe extern "C" fn(StreamRef, *mut c_void),
    start: unsafe extern "C" fn(StreamRef) -> u8,
    stop: unsafe extern "C" fn(StreamRef),
    invalidate: unsafe extern "C" fn(StreamRef),
    release: unsafe extern "C" fn(StreamRef),
    string_with_bytes:
        unsafe extern "C" fn(*const c_void, *const u8, isize, u32, u8) -> *const c_void,
    array_create: unsafe extern "C" fn(
        *const c_void,
        *const *const c_void,
        isize,
        *const c_void,
    ) -> *const c_void,
    /// `kCFTypeArrayCallBacks`, a constant in CoreFoundation's data: read only.
    type_array_callbacks: usize,
    cf_release: unsafe extern "C" fn(*const c_void),
    uuid_create_string: unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void,
    string_get_cstring: unsafe extern "C" fn(*const c_void, *mut c_char, isize, u32) -> u8,
}

// SAFETY: libdispatch's, in libSystem, with the signatures `dispatch/queue.h` declares.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

const UTF8: u32 = 0x0800_0100;

impl Api {
    fn load() -> Option<&'static Api> {
        use std::sync::OnceLock;
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(Api::open).as_ref()
    }

    fn open() -> Option<Api> {
        let path = c"/System/Library/Frameworks/CoreServices.framework/CoreServices";
        // SAFETY: a NUL-terminated path; dlopen returns null on failure.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is open and `name` NUL-terminated; a symbol not found is null.
        let sym = |name: &CStr| unsafe { libc::dlsym(handle, name.as_ptr()) };
        macro_rules! function {
            ($name:literal, $signature:ty) => {{
                let pointer = sym($name);
                if pointer.is_null() {
                    return None;
                }
                // SAFETY: the symbol is CoreServices' or CoreFoundation's, whose signature
                // is the one given (`FSEvents.h`, `CFString.h`, `CFArray.h`, `CFUUID.h`),
                // and a function pointer transmutes from the address `dlsym` gave.
                unsafe { std::mem::transmute::<*mut c_void, $signature>(pointer) }
            }};
        }
        let callbacks = sym(c"kCFTypeArrayCallBacks");
        if callbacks.is_null() {
            return None;
        }
        Some(Api {
            get_current_event_id: function!(
                c"FSEventsGetCurrentEventId",
                unsafe extern "C" fn() -> u64
            ),
            copy_uuid_for_device: function!(
                c"FSEventsCopyUUIDForDevice",
                unsafe extern "C" fn(i32) -> *const c_void
            ),
            stream_create: function!(
                c"FSEventStreamCreate",
                unsafe extern "C" fn(
                    *const c_void,
                    Callback,
                    *const Context,
                    *const c_void,
                    u64,
                    f64,
                    u32,
                ) -> StreamRef
            ),
            set_dispatch_queue: function!(
                c"FSEventStreamSetDispatchQueue",
                unsafe extern "C" fn(StreamRef, *mut c_void)
            ),
            start: function!(c"FSEventStreamStart", unsafe extern "C" fn(StreamRef) -> u8),
            stop: function!(c"FSEventStreamStop", unsafe extern "C" fn(StreamRef)),
            invalidate: function!(c"FSEventStreamInvalidate", unsafe extern "C" fn(StreamRef)),
            release: function!(c"FSEventStreamRelease", unsafe extern "C" fn(StreamRef)),
            string_with_bytes: function!(
                c"CFStringCreateWithBytes",
                unsafe extern "C" fn(*const c_void, *const u8, isize, u32, u8) -> *const c_void
            ),
            array_create: function!(
                c"CFArrayCreate",
                unsafe extern "C" fn(
                    *const c_void,
                    *const *const c_void,
                    isize,
                    *const c_void,
                ) -> *const c_void
            ),
            type_array_callbacks: callbacks as usize,
            cf_release: function!(c"CFRelease", unsafe extern "C" fn(*const c_void)),
            uuid_create_string: function!(
                c"CFUUIDCreateString",
                unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_void
            ),
            string_get_cstring: function!(
                c"CFStringGetCString",
                unsafe extern "C" fn(*const c_void, *mut c_char, isize, u32) -> u8
            ),
        })
    }
}

/// The log's id now: what a scan starting this moment is stamped with. `None` where
/// CoreServices cannot be loaded.
#[must_use]
pub fn current_event_id() -> Option<u64> {
    let api = Api::load()?;
    // SAFETY: no arguments; the function reads the log's counter.
    Some(unsafe { (api.get_current_event_id)() })
}

/// The log's identity for `device`: changes when the log is reset, `None` when the volume
/// keeps no log (then nothing since a saved scan can be known, and nothing is saved).
#[must_use]
pub fn log_uuid(device: u64) -> Option<String> {
    let api = Api::load()?;
    let device = i32::try_from(device).ok()?;
    // SAFETY: a device number; the result is a CFUUID the caller owns, or null.
    let uuid = unsafe { (api.copy_uuid_for_device)(device) };
    if uuid.is_null() {
        return None;
    }
    // SAFETY: `uuid` is a live CFUUID; the string is owned by the caller and released below.
    let string = unsafe { (api.uuid_create_string)(std::ptr::null(), uuid) };
    let mut buffer = [0 as c_char; 64];
    // SAFETY: `string` is a live CFString and the buffer's length is passed with it.
    let ok = unsafe { (api.string_get_cstring)(string, buffer.as_mut_ptr(), 64, UTF8) };
    let words = if ok != 0 {
        // SAFETY: CFStringGetCString NUL-terminates what it wrote within the buffer.
        Some(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    };
    // SAFETY: both were created above and are released exactly once.
    unsafe {
        (api.cf_release)(string);
        (api.cf_release)(uuid);
    }
    words
}

/// `kern.osversion`: an OS update rewrites the sealed volume without events.
#[must_use]
pub fn system_version() -> String {
    let mut buffer = [0 as c_char; 64];
    let mut len = buffer.len();
    // SAFETY: the name is NUL-terminated, the buffer and its length go together, no new value.
    let got = unsafe {
        libc::sysctlbyname(
            c"kern.osversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if got != 0 {
        return String::new();
    }
    // SAFETY: sysctl wrote a NUL-terminated string within `len`.
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// What a replay came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay {
    /// The directories named since the id, relative to the root.
    Changes(Changes),
    /// The ids started over, or the root itself moved: nothing since is known.
    Lost,
    /// `HistoryDone` never came, or CoreServices could not be loaded.
    GaveUp,
}

struct Batch {
    paths: Vec<PathBuf>,
    flags: Vec<u32>,
    ids: Vec<u64>,
}

/// One event of a replay ([`events`]): the directory as the log spells it, the event's id, and
/// whether the log lost events under it (`MustScanSubDirs`, dropped), so it is to be walked
/// whole rather than listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub path: PathBuf,
    pub id: u64,
    pub whole: bool,
}

/// What a replay of events came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Events {
    /// Every event since the id, up to the one current when the replay began.
    Events(Vec<Event>),
    /// The ids started over, or the root itself moved: nothing since is known.
    Lost,
    /// `HistoryDone` never came, or CoreServices could not be loaded.
    GaveUp,
}

extern "C" fn deliver(
    _stream: StreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    // SAFETY: `info` is the `Sender<Batch>` boxed by `replay`, alive until the stream is
    // invalidated and released, after which no callback runs.
    let sender = unsafe { &*info.cast::<Sender<Batch>>() };
    // SAFETY: `paths` is `count` C strings, as FSEvents documents for a stream made without
    // CF types.
    let paths = unsafe { std::slice::from_raw_parts(paths.cast::<*const c_char>(), count) };
    // SAFETY: `flags` is `count` words, one an event.
    let flags = unsafe { std::slice::from_raw_parts(flags, count) };
    // SAFETY: `ids` is `count` event ids, one an event.
    let ids = unsafe { std::slice::from_raw_parts(ids, count) };
    let batch = Batch {
        paths: paths
            .iter()
            .map(|&path| {
                // SAFETY: each path is a NUL-terminated string for this call's duration.
                let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
                PathBuf::from(std::ffi::OsStr::from_encoded_bytes_unchecked_safe(bytes))
            })
            .collect(),
        flags: flags.to_vec(),
        ids: ids.to_vec(),
    };
    let _ = sender.send(batch);
}

/// `OsStr` from the bytes of a C path: every byte string is a Unix `OsStr`.
trait FromBytes {
    fn from_encoded_bytes_unchecked_safe(bytes: &[u8]) -> &std::ffi::OsStr;
}

impl FromBytes for std::ffi::OsStr {
    fn from_encoded_bytes_unchecked_safe(bytes: &[u8]) -> &std::ffi::OsStr {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(bytes)
    }
}

/// Replay the log for `root` since `since`, for at most `give_up`: the directories named,
/// relative to `root`.
#[must_use]
pub fn replay(root: &Path, since: u64, give_up: Duration) -> Replay {
    match events(root, since, give_up) {
        Events::Events(events) => Replay::Changes(changes(root, &events)),
        Events::Lost => Replay::Lost,
        Events::GaveUp => Replay::GaveUp,
    }
}

/// `events` as the directories to list and to walk again, relative to `root`. A path outside
/// the root is the root itself in another spelling, or a volume-level event: all of it walked.
fn changes(root: &Path, events: &[Event]) -> Changes {
    let mut listed = std::collections::HashSet::new();
    let mut walked = std::collections::HashSet::new();
    for event in events {
        // Paths come with a trailing separator, as directories.
        match event.path.strip_prefix(root) {
            Ok(relative) if event.whole => walked.insert(relative.to_path_buf()),
            Ok(relative) => listed.insert(relative.to_path_buf()),
            Err(_) => walked.insert(PathBuf::new()),
        };
    }
    Changes {
        listed: listed.into_iter().collect(),
        walked: walked.into_iter().collect(),
        events: events.len() as u64,
    }
}

/// Every event of the log for `root` since `since`, with its id, for at most `give_up`. Only
/// the history: events after the id current when the replay began are left out. While a busy
/// machine replays a long history, live events arrive faster than they are taken, and the log
/// drops them with `MustScanSubDirs` on the root — 91 to 288 of them in a replay of ten million
/// ids here (2026-10-05), each saying "walk it all"; a history replayed since a stamp needs none
/// of them, since whatever happens after is replayed from the next stamp.
#[must_use]
pub fn events(root: &Path, since: u64, give_up: Duration) -> Events {
    events_while(root, since, give_up, &|| true)
}

/// [`events`], given up as soon as `keep_going` says to stop: a replay can take minutes.
#[must_use]
pub fn events_while(
    root: &Path,
    since: u64,
    give_up: Duration,
    keep_going: &dyn Fn() -> bool,
) -> Events {
    let Some(api) = Api::load() else {
        return Events::GaveUp;
    };
    let Ok(root_c) = CString::new(root.as_os_str().as_encoded_bytes()) else {
        return Events::GaveUp;
    };
    // SAFETY: no preconditions.
    let until = unsafe { (api.get_current_event_id)() };
    let (sender, receiver): (Sender<Batch>, Receiver<Batch>) = channel();
    let sender = Box::into_raw(Box::new(sender));
    let context = Context {
        version: 0,
        info: sender.cast(),
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
    };
    // SAFETY: the path bytes and length go together; the array holds the one string; every
    // CF object made here is released below; the stream's context outlives the stream, which
    // is stopped, invalidated and released before the sender is dropped.
    let stream = unsafe {
        let path = (api.string_with_bytes)(
            std::ptr::null(),
            root_c.as_ptr().cast(),
            root_c.as_bytes().len() as isize,
            UTF8,
            0,
        );
        let paths = (api.array_create)(
            std::ptr::null(),
            &raw const path,
            1,
            api.type_array_callbacks as *const c_void,
        );
        let stream = (api.stream_create)(
            std::ptr::null(),
            deliver,
            &raw const context,
            paths,
            since,
            0.0,
            0,
        );
        (api.cf_release)(paths);
        (api.cf_release)(path);
        stream
    };
    if stream.is_null() {
        // SAFETY: the box was never handed to a stream.
        drop(unsafe { Box::from_raw(sender) });
        return Events::GaveUp;
    }
    // SAFETY: a fresh serial queue for this stream; released after the stream.
    let queue = unsafe { dispatch_queue_create(c"duscape.fsevents".as_ptr(), std::ptr::null()) };
    // SAFETY: `stream` is live and not yet started.
    let started = unsafe {
        (api.set_dispatch_queue)(stream, queue);
        (api.start)(stream)
    };
    let outcome = if started == 0 {
        Events::GaveUp
    } else {
        collect(&receiver, until, give_up, keep_going)
    };
    // SAFETY: stop, invalidate and release in FSEvents' order; then no callback can run and
    // the sender is freed; the queue is released last.
    unsafe {
        (api.stop)(stream);
        (api.invalidate)(stream);
        (api.release)(stream);
        drop(Box::from_raw(sender));
        dispatch_release(queue);
    }
    outcome
}

/// Take batches until `HistoryDone`, or the time is up, keeping events up to `until`.
fn collect(
    receiver: &Receiver<Batch>,
    until: u64,
    give_up: Duration,
    keep_going: &dyn Fn() -> bool,
) -> Events {
    use std::sync::mpsc::RecvTimeoutError;
    // How often a quiet replay looks whether it is still wanted.
    const LOOK: Duration = Duration::from_millis(250);
    let deadline = Instant::now() + give_up;
    let mut events = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !keep_going() {
            return Events::GaveUp;
        }
        let batch = match receiver.recv_timeout(left.min(LOOK)) {
            Ok(batch) => batch,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Events::GaveUp,
        };
        for ((path, flags), &id) in batch.paths.into_iter().zip(&batch.flags).zip(&batch.ids) {
            if flags & HISTORY_DONE != 0 {
                return Events::Events(events);
            }
            if flags & (EVENT_IDS_WRAPPED | ROOT_CHANGED) != 0 {
                return Events::Lost;
            }
            if id > until {
                continue;
            }
            events.push(Event {
                path,
                id,
                whole: flags & (MUST_SCAN_SUBDIRS | USER_DROPPED | KERNEL_DROPPED) != 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Event, changes};

    /// The catch-up's view of a replay: under the root listed or walked, once each; outside
    /// it, the whole root walked.
    #[test]
    fn events_fold_into_the_folders_to_list_and_walk() {
        let event = |path: &str, whole: bool| Event {
            path: PathBuf::from(path),
            id: 1,
            whole,
        };
        let events = [
            event("/Users/a/x/", false),
            event("/Users/a/x/", false),
            event("/Users/a/y/", true),
            event("/Library/z/", false),
        ];
        let mut folded = changes(Path::new("/Users/a"), &events);
        folded.listed.sort();
        folded.walked.sort();
        assert_eq!(folded.listed, [PathBuf::from("x")]);
        assert_eq!(folded.walked, [PathBuf::new(), PathBuf::from("y")]);
        assert_eq!(folded.events, 4);
    }
}
