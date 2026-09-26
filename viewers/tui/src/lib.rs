mod app;
mod bench;
mod cli;
mod clipboard;
mod config;
mod error;
mod input;
mod messages;
mod preview;
mod state;
mod ui;

/// musl's own allocator is the scan's bottleneck; see the `tikv-jemallocator` dependency.
#[cfg(all(target_env = "musl", target_pointer_width = "64"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use ::std::io;
use ::std::path::PathBuf;
use ::std::process;
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::sync::mpsc;
use ::std::sync::mpsc::{Receiver, SyncSender};
use ::std::thread::park_timeout;
use ::std::{thread, time};
use clap::Parser;
use cli::Opt;
use diskonaut_scan::parallel;
use error::Error;
use libdiskonaut::{Outline, ScanOptions};

use ::ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as BackEvent};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor::Show, execute};

use app::{App, UiMode};
use config::DiskonautConfig;
use input::{TerminalEvents, needs_quit_delay};
use messages::{Event, Instruction, handle_events};

/// Number of scanned entries batched into one message to the UI thread.
///
/// A whole-disk scan produces millions of entries; a channel round-trip per entry, as this once
/// did, costs more than reading the filesystem.
const SCAN_BATCH_SIZE: usize = 4096;

/// The program entry point, shared by both binaries (`diskonaut-angch` and its `diskonaut` alias).
pub fn run() {
    if let Err(err) = try_main() {
        println!("Error: {}", err);
        process::exit(2);
    }
}
fn get_stdout() -> io::Result<io::Stdout> {
    Ok(io::stdout())
}

/// Put the terminal back the way it was found: cooked mode, mouse released, main screen, cursor
/// visible.
/// Idempotent, and it ignores errors because it runs on the way out when there is nothing left to
/// do about them. Leaving the alternate screen is what brings back the shell's screen and puts the
/// cursor back where the prompt left it; without it the prompt resumes wherever the last frame
/// drew.
fn restore_terminal() {
    let _ = disable_raw_mode();
    // A picture in the preview would otherwise stay in the terminal's memory.
    if preview::kitty_known() {
        let _ = preview::kitty_delete(&mut io::stdout());
    }
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}

/// Restores the terminal when it drops — on a normal quit, an early `?` error, or a panic unwinding
/// through `start`. Without this, any exit that skips the teardown leaves the shell in raw mode
/// with a hidden cursor, which needs `reset` to fix.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn try_main() -> Result<(), Error> {
    let opts = Opt::parse();

    let config_path = opts
        .config
        .clone()
        .or_else(config::default_config_path)
        .unwrap_or_else(|| PathBuf::from("config"));
    let diskonaut_config =
        DiskonautConfig::load(opts.config.as_deref()).map_err(|source| Error::Config {
            path: config_path.clone(),
            source,
        })?;
    let keybinds = diskonaut_config
        .keybinds()
        .map_err(|source| Error::Config {
            path: config_path,
            source,
        })?;
    let show_apparent_size = opts.apparent_size || diskonaut_config.base.apparent_size;
    let scan_options = ScanOptions {
        parallel: !opts.single_thread,
        threads: opts.threads,
        show_apparent_size,
        max_depth: opts.max_depth,
        one_file_system: opts.one_file_system,
        hard_link_threshold: opts.hard_link_threshold,
        read_device: !opts.no_device_read,
    };

    if opts.benchmark {
        let folder = opts.resolve_folder()?;
        bench::run(
            &folder,
            opts.bench_stage,
            scan_options,
            opts.bench_repeat,
            opts.bench_shards,
            opts.bench_shard_depth,
            opts.bench_profile,
        );
        return Ok(());
    }

    match get_stdout() {
        Ok(stdout) => {
            enable_raw_mode()?;
            // The guard restores the terminal on every ordinary way out; the panic hook does it
            // before the message prints, so a panic in any thread cannot leave the shell wedged.
            let _guard = TerminalGuard;
            execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
            // Asked now, in raw mode and before anything reads stdin, because it may query the
            // terminal and read the answer; on the alternate screen, so a terminal that prints
            // the query instead of answering it leaves nothing behind once `App::new` clears it.
            preview::graphics_protocol();
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                default_hook(info);
            }));
            let terminal_backend = CrosstermBackend::new(stdout);
            let terminal_events = TerminalEvents {};
            let folder = opts.resolve_folder()?;
            start(
                terminal_backend,
                Box::new(terminal_events),
                folder,
                scan_options,
                keybinds,
            );
        }
        Err(_) => return Err(Error::NoStdout),
    }
    Ok(())
}

fn start<B>(
    terminal_backend: B,
    terminal_events: Box<dyn Iterator<Item = BackEvent> + Send>,
    path: PathBuf,
    scan_options: ScanOptions,
    keybinds: config::Keybinds,
) where
    B: Backend + Send + 'static,
{
    let (event_sender, event_receiver): (SyncSender<Event>, Receiver<Event>) =
        mpsc::sync_channel(1);
    let (instruction_sender, instruction_receiver): (
        SyncSender<Instruction>,
        Receiver<Instruction>,
    ) = mpsc::sync_channel(100);

    let running = Arc::new(AtomicBool::new(true));
    let loaded = Arc::new(AtomicBool::new(false));

    // Constructed before any thread reads stdin: `App::new` clears the terminal, which
    // queries the cursor position by writing a CPR request and reading its reply straight off
    // stdin. If `stdin_handler` were already polling stdin for input events, it could steal that
    // reply out from under the query, leaving it blocked until the next real keypress arrived and
    // the screen showing nothing in the meantime.
    let mut app = App::new(
        terminal_backend,
        path.clone(),
        event_sender,
        keybinds.clone(),
        scan_options.show_apparent_size,
    );
    enable_background_work(&mut app, &instruction_sender, scan_options, &running);

    let threads = [
        spawn("event_executer", {
            let instruction_sender = instruction_sender.clone();
            move || handle_events(event_receiver, instruction_sender)
        }),
        spawn(
            "stdin_handler",
            stdin_handler(
                terminal_events,
                instruction_sender.clone(),
                running.clone(),
                keybinds,
            ),
        ),
        spawn(
            "hd_scanner",
            scanner(
                path,
                scan_options,
                instruction_sender.clone(),
                running.clone(),
                loaded.clone(),
            ),
        ),
        spawn(
            "loading_loop",
            loading_loop(instruction_sender.clone(), running.clone(), loaded),
        ),
        spawn(
            "ticker",
            ticker(instruction_sender, running.clone(), app.ticker_pace()),
        ),
    ];

    app.start(instruction_receiver);
    running.store(false, Ordering::Release);

    for thread in threads {
        thread.join().unwrap();
    }
}

/// A named thread. Every thread of the app has a name, for what `top` and a debugger show.
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
        .unwrap()
}

/// The previewer, rescans and the second pass: threads the app starts as it needs them, each
/// reporting back as an [`Instruction`].
fn enable_background_work<B>(
    app: &mut App<B>,
    instruction_sender: &SyncSender<Instruction>,
    scan_options: ScanOptions,
    running: &Arc<AtomicBool>,
) where
    B: Backend + Send + 'static,
{
    {
        let instruction_sender = instruction_sender.clone();
        let previewer = preview::Previewer::spawn(move |generation, preview| {
            let _ = instruction_sender.send(Instruction::PreviewReady(generation, preview));
        });
        let pictures = preview::pictures();
        let graphics: Box<dyn preview::Graphics> = match pictures {
            preview::Pictures::Kitty => Box::new(preview::KittyGraphics::default()),
            preview::Pictures::Sixel => Box::new(preview::SixelGraphics::default()),
            _ => Box::new(preview::NoGraphics),
        };
        app.enable_previews(previewer, pictures, graphics);
    }
    {
        let instruction_sender = instruction_sender.clone();
        app.enable_rescans(diskonaut_scan::rescan::Rescanner::new(
            scan_options,
            running.clone(),
            move |id, outcome| {
                let _ = instruction_sender.send(Instruction::Rescanned(id, outcome));
            },
        ));
    }
    {
        let instruction_sender = instruction_sender.clone();
        app.enable_refining(diskonaut_scan::rescan::Refiner::new(
            diskonaut_scan::thread_count(scan_options),
            running.clone(),
            move |generation, found, left| {
                let _ = instruction_sender.send(Instruction::Refined(generation, found, left));
            },
        ));
    }
}

/// Terminal events into instructions, with the pause a quit key needs.
fn stdin_handler(
    terminal_events: Box<dyn Iterator<Item = BackEvent> + Send>,
    instruction_sender: SyncSender<Instruction>,
    running: Arc<AtomicBool>,
    keybinds: config::Keybinds,
) -> impl FnOnce() + Send + 'static {
    move || {
        for evt in terminal_events {
            if let BackEvent::Resize(_x, _y) = evt {
                let _ = instruction_sender.send(Instruction::ResetUiMode);
                let _ = instruction_sender.send(Instruction::Render);
                continue;
            }

            let delay = matches!(&evt, BackEvent::Key(_)) && needs_quit_delay(&evt, &keybinds);
            if instruction_sender.send(Instruction::Keypress(evt)).is_err() {
                break;
            }
            if delay {
                // not ideal, but works in a pinch
                park_timeout(time::Duration::from_millis(100));
                // if we don't wait, the app won't have time to quit
                if !running.load(Ordering::Acquire) {
                    // sometimes ctrl-c doesn't shut down the app
                    // (eg. dismissing an error message)
                    // in order not to be aware of those particularities
                    // we check "running"
                    break;
                }
            }
        }
    }
}

/// The scan. It runs here, and the tree is built on `parallel::SHARDS` threads of its own; this
/// thread sends the rendering thread an outline of each directory as it goes past, so the view
/// can follow the scan, and the finished tree once the builders are merged.
fn scanner(
    path: PathBuf,
    scan_options: ScanOptions,
    instruction_sender: SyncSender<Instruction>,
    running: Arc<AtomicBool>,
    loaded: Arc<AtomicBool>,
) -> impl FnOnce() + Send + 'static {
    move || {
        let progress_sender = instruction_sender.clone();
        let progress_running = running.clone();
        let mut outline = Outline::new(path.clone(), Outline::DEFAULT_DEPTH, SCAN_BATCH_SIZE);
        let built = parallel::build_tree(
            &path,
            scan_options,
            parallel::SHARDS,
            parallel::SHARD_DEPTH,
            |directory| {
                if !progress_running.load(Ordering::Acquire) {
                    return false;
                }
                if let Some(batch) = outline.add(directory) {
                    // A failed send means the program has ended; stop rather than hang.
                    if progress_sender
                        .send(Instruction::AddScannedSummaries(batch))
                        .is_err()
                    {
                        return false;
                    }
                }
                true
            },
        );
        if running.load(Ordering::Acquire) {
            if let Some((mut tree, failed, _, small)) = built {
                let rest = outline.finish();
                if !rest.is_empty() {
                    let _ = instruction_sender.send(Instruction::AddScannedSummaries(rest));
                }
                tree.failed_to_read = failed;
                let _ = instruction_sender.send(Instruction::ScanComplete(Box::new(tree), small));
                let _ = instruction_sender.send(Instruction::StartUi);
            }
            loaded.store(true, Ordering::Release);
        }
    }
}

/// The loading indicator, toggled while the scan runs.
fn loading_loop(
    instruction_sender: SyncSender<Instruction>,
    running: Arc<AtomicBool>,
    loaded: Arc<AtomicBool>,
) -> impl FnOnce() + Send + 'static {
    move || {
        while running.load(Ordering::Acquire) && !loaded.load(Ordering::Acquire) {
            let _ = instruction_sender.send(Instruction::ToggleScanningVisualIndicator);
            let _ = instruction_sender.send(Instruction::RenderAndUpdateBoard);
            park_timeout(time::Duration::from_millis(100));
        }
    }
}

/// Drives the help line: a frame's worth apart while it slides, a few a second while it rests.
/// Dropped rather than queued when the rendering thread is behind: a late tick is worth nothing.
///
/// A slide begins on a resting tick, so the flag is looked at again a frame after each: sleeping
/// the whole resting interval would miss the start of the slide. Twice per resting interval, not
/// sixty times a second.
fn ticker(
    instruction_sender: SyncSender<Instruction>,
    running: Arc<AtomicBool>,
    fast: Arc<AtomicBool>,
) -> impl FnOnce() + Send + 'static {
    move || {
        while running.load(Ordering::Acquire) {
            if let Err(mpsc::TrySendError::Disconnected(_)) =
                instruction_sender.try_send(Instruction::Tick)
            {
                break;
            }
            park_timeout(ui::FRAME);
            if fast.load(Ordering::Acquire) {
                continue;
            }
            park_timeout(ui::IDLE_TICK.saturating_sub(ui::FRAME));
        }
    }
}
