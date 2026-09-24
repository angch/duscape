//! Where the time goes while a tree is built: phase timers and counters, for `--bench-profile`.
//!
//! Off by default and costing one predictable branch per counted event; [`enable`] before the
//! trees are made turns it on for the process. A [`FileTree`](super::FileTree) then keeps a
//! [`BuildProfile`] of its own work, merges the ones of trees merged into it, and hands it over
//! with `take_profile`. The counters that live below the tree — name comparisons in a folder, the
//! ledger's ancestor walks — are process-wide, since a folder does not know which tree it is in.

use ::std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use ::std::time::Duration;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn profiling on for every tree made from now on.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Whether profiling is on.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Names compared while looking an entry up in a folder by name.
pub static NAME_COMPARES: AtomicU64 = AtomicU64::new(0);
/// Name indexes built by folders that grew past the scanning threshold.
pub static INDEX_BUILDS: AtomicU64 = AtomicU64::new(0);
/// Steps taken up the directory tree by the ledger's ancestor walks (`shared_depth`,
/// `charge_ancestors`).
pub static ANCESTOR_STEPS: AtomicU64 = AtomicU64::new(0);
/// Directories the ledger compared a new link's directory with (list mode).
pub static LINK_COMPARISONS: AtomicU64 = AtomicU64::new(0);

/// Add to a process-wide counter, if profiling is on.
#[inline]
pub fn count(counter: &AtomicU64, by: u64) {
    if enabled() {
        counter.fetch_add(by, Ordering::Relaxed);
    }
}

fn take(counter: &AtomicU64) -> u64 {
    counter.swap(0, Ordering::Relaxed)
}

/// What one tree's build cost, phase by phase.
///
/// Durations are the thread's own time in each phase; merged across parallel builders they add
/// up to more than the wall clock, which is the point of the comparison.
#[derive(Clone, Debug, Default)]
pub struct BuildProfile {
    pub directories: u64,
    pub entries: u64,
    /// Finding (or making) the folder a directory's entries go in, down the path from the root,
    /// and adding the sizes along it.
    pub resolve: Duration,
    /// The pass over a directory's entries adding up sizes, for directories with no shared
    /// blocks in them: the per-entry floor.
    pub sizes: Duration,
    /// The same pass for directories holding shared blocks, which also charges them to the
    /// ledger, or notes them for later.
    pub ledger: Duration,
    /// Directories with shared blocks in them.
    pub ledger_directories: u64,
    /// Taking the names buffer and placing the entries in the folder.
    pub place: Duration,
    /// Folders stepped through while resolving directories: the sum of their depths.
    pub resolve_steps: u64,
    /// Shared-block sightings charged inline, or noted for the replay.
    pub sightings: u64,
    /// The replay of noted sightings after a parallel build: all of it, and its two halves.
    pub replay: Duration,
    pub replay_charge: Duration,
    pub replay_take_back: Duration,
    pub replay_sightings: u64,
    /// Sightings the replay found already charged somewhere, so took back from some folders.
    pub replay_taken_back: u64,
    /// Directories the replay went through, interning each in the ledger.
    pub replay_directories: u64,
    /// The process-wide counters, collected by [`Self::collect_globals`].
    pub name_compares: u64,
    pub index_builds: u64,
    pub ancestor_steps: u64,
    pub link_comparisons: u64,
}

impl BuildProfile {
    /// Fold another tree's profile into this one.
    pub fn merge(&mut self, other: &Self) {
        self.directories += other.directories;
        self.entries += other.entries;
        self.resolve += other.resolve;
        self.sizes += other.sizes;
        self.ledger += other.ledger;
        self.ledger_directories += other.ledger_directories;
        self.place += other.place;
        self.resolve_steps += other.resolve_steps;
        self.sightings += other.sightings;
        self.replay += other.replay;
        self.replay_charge += other.replay_charge;
        self.replay_take_back += other.replay_take_back;
        self.replay_sightings += other.replay_sightings;
        self.replay_taken_back += other.replay_taken_back;
        self.replay_directories += other.replay_directories;
        self.name_compares += other.name_compares;
        self.index_builds += other.index_builds;
        self.ancestor_steps += other.ancestor_steps;
        self.link_comparisons += other.link_comparisons;
    }

    /// Take the process-wide counters into this profile and reset them, so that the next build
    /// starts from zero.
    pub fn collect_globals(&mut self) {
        self.name_compares += take(&NAME_COMPARES);
        self.index_builds += take(&INDEX_BUILDS);
        self.ancestor_steps += take(&ANCESTOR_STEPS);
        self.link_comparisons += take(&LINK_COMPARISONS);
    }

    /// The profile as a few lines for a report, indented by `indent`.
    #[must_use]
    pub fn report(&self, indent: &str) -> String {
        let secs = |d: Duration| d.as_secs_f64();
        let per = |n: u64, of: u64| if of == 0 { 0.0 } else { n as f64 / of as f64 };
        let build = self.resolve + self.sizes + self.ledger + self.place;
        let mut out = String::new();
        out += &format!(
            "{indent}build profile: {} directories, {} entries, {:.3}s of builder time\n",
            self.directories,
            self.entries,
            secs(build)
        );
        out += &format!(
            "{indent}  resolve {:.3}s  {:.1} folders/dir  {:.1} name compares/dir  {} indexes built\n",
            secs(self.resolve),
            per(self.resolve_steps, self.directories),
            per(self.name_compares, self.directories),
            self.index_builds
        );
        out += &format!(
            "{indent}  place   {:.3}s  {:.0} ns/entry\n",
            secs(self.place),
            per(
                self.place.as_nanos().try_into().unwrap_or(u64::MAX),
                self.entries
            )
        );
        out += &format!(
            "{indent}  sizes   {:.3}s  {:.0} ns/entry over the {} directories with no shared blocks\n",
            secs(self.sizes),
            per(
                self.sizes.as_nanos().try_into().unwrap_or(u64::MAX),
                self.entries.saturating_sub(self.ledger_entries())
            ),
            self.directories - self.ledger_directories
        );
        out += &format!(
            "{indent}  ledger  {:.3}s  {} directories, {} sightings  {:.1} link comparisons and {:.1} ancestor steps each\n",
            secs(self.ledger),
            self.ledger_directories,
            self.sightings,
            per(
                self.link_comparisons,
                self.sightings.max(self.replay_sightings)
            ),
            per(
                self.ancestor_steps,
                self.sightings.max(self.replay_sightings)
            )
        );
        if self.replay_sightings > 0 {
            out += &format!(
                "{indent}  replay  {:.3}s  charge {:.3}s  take back {:.3}s  {} directories, {} sightings, {} taken back\n",
                secs(self.replay),
                secs(self.replay_charge),
                secs(self.replay_take_back),
                self.replay_directories,
                self.replay_sightings,
                self.replay_taken_back
            );
        }
        out
    }

    /// Entries in the directories that held shared blocks: unknown exactly, so estimated from
    /// their share of directories. Only for the per-entry figure of the `sizes` line.
    fn ledger_entries(&self) -> u64 {
        if self.directories == 0 {
            return 0;
        }
        self.entries / self.directories * self.ledger_directories
    }
}
