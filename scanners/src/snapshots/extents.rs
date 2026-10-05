//! Where a file's blocks are on the disk, for the bytes a snapshot alone holds of a file
//! written over in place since: its physical extents less the live file's, less what another
//! snapshot already counted ([`held_alone`]).
//!
//! macOS gives a file's extents through `fcntl(F_LOG2PHYS_EXT)`, one run a call, a hole as
//! offset −1 (`physical`, macOS only), no privilege needed: a 64 GB virtual machine disk of 167k runs
//! maps in 0.26 s (2026-10-05). Offsets are the container's, so a snapshot's file and the live
//! one compare. Asking opens the file, so a file only in iCloud is never asked (opening one
//! downloads it): `written_over` skips it.

use ::std::collections::BTreeMap;

/// Byte ranges, kept apart and merged: start to end (exclusive).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranges(BTreeMap<u64, u64>);

impl Ranges {
    /// The ranges of `extents`, each a start and a length.
    #[must_use]
    pub fn of(extents: &[(u64, u64)]) -> Self {
        let mut ranges = Self::default();
        for &(start, length) in extents {
            ranges.insert(start, start.saturating_add(length));
        }
        ranges
    }

    /// Add `start..end`, merged with whatever it touches.
    pub fn insert(&mut self, mut start: u64, mut end: u64) {
        if start >= end {
            return;
        }
        let touching: Vec<(u64, u64)> = self
            .0
            .range(..=end)
            .rev()
            .take_while(|&(_, &e)| e >= start)
            .map(|(&s, &e)| (s, e))
            .collect();
        for (s, e) in touching {
            self.0.remove(&s);
            start = start.min(s);
            end = end.max(e);
        }
        self.0.insert(start, end);
    }

    /// The parts of `start..end` that no range covers, in order.
    #[must_use]
    pub fn uncovered(&self, start: u64, end: u64) -> Vec<(u64, u64)> {
        let mut covering: Vec<(u64, u64)> = self
            .0
            .range(..end)
            .rev()
            .take_while(|&(_, &e)| e > start)
            .map(|(&s, &e)| (s, e))
            .collect();
        covering.reverse();
        let mut gaps = Vec::new();
        let mut at = start;
        for (s, e) in covering {
            if s > at {
                gaps.push((at, s));
            }
            at = at.max(e);
        }
        if at < end {
            gaps.push((at, end));
        }
        gaps
    }

    /// Bytes covered in all.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.iter().map(|(s, e)| e - s).sum()
    }
}

/// The bytes of `snapshot`'s extents (a start and a length each) in neither `live` nor
/// `counted`, which then holds them too: what a snapshot alone holds of a file, counted once
/// whichever of several snapshots holds them.
pub fn held_alone(snapshot: &[(u64, u64)], live: &Ranges, counted: &mut Ranges) -> u64 {
    let mut bytes = 0;
    for &(start, length) in snapshot {
        for (s, e) in live.uncovered(start, start.saturating_add(length)) {
            for (s, e) in counted.uncovered(s, e) {
                bytes += e - s;
                counted.insert(s, e);
            }
        }
    }
    bytes
}

/// The physical extents of the file at `path`, a start and a length each, holes left out.
#[cfg(target_os = "macos")]
pub fn physical(path: &::std::path::Path) -> ::std::io::Result<Vec<(u64, u64)>> {
    use ::std::os::fd::AsRawFd;
    let file = ::std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let mut extents: Vec<(u64, u64)> = Vec::new();
    let mut at = 0u64;
    while at < length {
        let mut run = libc::log2phys {
            l2p_flags: 0,
            l2p_contigbytes: i64::try_from(length - at).unwrap_or(i64::MAX),
            l2p_devoffset: i64::try_from(at).unwrap_or(i64::MAX),
        };
        // SAFETY: the descriptor is open, and `run` is the `log2phys` the call reads and fills.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_LOG2PHYS_EXT, &raw mut run) } < 0 {
            return Err(::std::io::Error::last_os_error());
        }
        let (bytes, offset) = (run.l2p_contigbytes, run.l2p_devoffset);
        let Ok(bytes) = u64::try_from(bytes) else {
            break;
        };
        if bytes == 0 {
            break;
        }
        // A hole is offset −1: no blocks.
        if let Ok(offset) = u64::try_from(offset) {
            match extents.last_mut() {
                Some((start, len)) if *start + *len == offset => *len += bytes,
                _ => extents.push((offset, bytes)),
            }
        }
        at += bytes;
    }
    Ok(extents)
}

#[cfg(test)]
mod tests {
    use super::{Ranges, held_alone};

    #[test]
    fn ranges_merge_and_say_what_they_leave_uncovered() {
        let mut ranges = Ranges::of(&[(10, 10), (30, 10)]);
        assert_eq!(ranges.uncovered(0, 50), [(0, 10), (20, 30), (40, 50)]);
        assert_eq!(ranges.uncovered(12, 18), []);
        assert_eq!(ranges.uncovered(15, 35), [(20, 30)]);
        ranges.insert(20, 30);
        assert_eq!(ranges, Ranges::of(&[(10, 30)]));
        assert_eq!(ranges.total(), 30);
        ranges.insert(5, 8);
        assert_eq!(ranges.total(), 33);
        assert_eq!(ranges.uncovered(0, 50), [(0, 5), (8, 10), (40, 50)]);
    }

    /// A file written over in its middle: the old middle is the snapshot's alone, and a second
    /// snapshot holding the same old blocks adds none.
    #[test]
    fn what_a_snapshot_alone_holds_is_counted_once() {
        let first = [(1000, 300)];
        let live = Ranges::of(&[(1000, 100), (5000, 100), (1200, 100)]);
        let mut counted = Ranges::default();
        assert_eq!(held_alone(&first, &live, &mut counted), 100);
        let second = [(1000, 100), (1100, 100), (1200, 100)];
        assert_eq!(held_alone(&second, &live, &mut counted), 0);
        let third = [(7000, 50)];
        assert_eq!(held_alone(&third, &live, &mut counted), 50);
    }
}
