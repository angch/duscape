//! A window's paints of a layout: the first in a hurry, labelling the treemap's tiles only
//! until [`LABEL_DEADLINE`]; the rest, in full. Which is which, and whether a second pass is
//! owed, is decided here for every viewer; a viewer draws, and says after [`IDLE`] without
//! input that the second pass is due.
//!
//! [`IDLE`]: crate::state::IDLE

use ::std::cell::Cell;
use ::std::time::{Duration, Instant};

use crate::state::Viewer;

/// How long a first paint of a layout may spend on the treemap's labels before it stops
/// labelling: the text is the most of a paint, and the relayout has had some 5–7 ms of the
/// frame already. The rest come in the second pass's paint, in full. It is the labels' own
/// time that counts, not the paint's: measured from the paint's start, the fills of some
/// thousands of nested tiles used the whole of it before the first label, and a frame whose
/// every label cost 1 ms went out with half of them, to be painted again with them all when
/// the second pass came — a flicker on the Linux window at every relayout.
pub const LABEL_DEADLINE: Duration = Duration::from_millis(4);

/// `DUSCAPE_PAINT_TIMES`, read once: whether a viewer prints each frame's time, and each
/// resize's layout, on stderr — how a change is checked to keep the painting fast.
#[must_use]
pub fn paint_times() -> bool {
    static ON: ::std::sync::OnceLock<bool> = ::std::sync::OnceLock::new();
    *ON.get_or_init(|| ::std::env::var_os("DUSCAPE_PAINT_TIMES").is_some())
}

/// One paint's labels: all of them, or those it has time for within [`LABEL_DEADLINE`] of
/// labelling.
pub struct LabelBudget {
    hurried: bool,
    spent: Cell<Duration>,
    skipped: Cell<bool>,
}

/// Leave to draw one label, from [`LabelBudget::allows`]: held while it is drawn, its time
/// is charged to the budget when it is dropped.
pub struct Labelling<'a> {
    budget: &'a LabelBudget,
    started: Instant,
}

impl Drop for Labelling<'_> {
    fn drop(&mut self) {
        let spent = self.budget.spent.get() + self.started.elapsed();
        self.budget.spent.set(spent);
    }
}

impl LabelBudget {
    /// For a paint starting now: `in_full`, every label.
    #[must_use]
    pub fn new(in_full: bool) -> Self {
        LabelBudget {
            hurried: !in_full,
            spent: Cell::new(Duration::ZERO),
            skipped: Cell::new(false),
        }
    }

    /// Leave to label a tile, held while it is drawn — or none, the labels having had their
    /// time, which is noted so that the second pass paints them.
    pub fn allows(&self) -> Option<Labelling<'_>> {
        if self.hurried && self.spent.get() >= LABEL_DEADLINE {
            self.skipped.set(true);
            return None;
        }
        Some(Labelling {
            budget: self,
            started: Instant::now(),
        })
    }

    /// Whether every label was drawn.
    #[must_use]
    pub fn complete(&self) -> bool {
        !self.skipped.get()
    }
}

/// Which layout ([`Viewer::layout_generation`]) a window last painted in full. A layout painted
/// in full once is painted in full again — a hover, a mark — so its labels never come and go.
#[derive(Default)]
pub struct Paints {
    in_full: u64,
}

impl Paints {
    /// Whether the paint of what `viewer` shows is to be in full.
    #[must_use]
    pub fn in_full(&self, viewer: &Viewer) -> bool {
        self.in_full == viewer.layout_generation()
    }

    /// A paint of what `viewer` shows ended, `complete` or cut at its deadline.
    pub fn painted(&mut self, viewer: &Viewer, complete: bool) {
        if complete {
            self.in_full = viewer.layout_generation();
        }
    }

    /// Whether a second pass is owed: the relayout's ([`Viewer::second_pass_owed`]), or the
    /// labels of a paint cut short.
    #[must_use]
    pub fn owed(&self, viewer: &Viewer) -> bool {
        viewer.second_pass_owed() || !self.in_full(viewer)
    }

    /// Input has stopped: the second pass, and what it lays out painted in full, whatever it
    /// takes.
    pub fn second_pass(&mut self, viewer: &mut Viewer) {
        viewer.finish_second_pass();
        self.in_full = viewer.layout_generation();
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, LabelBudget, Paints};
    use crate::state::Viewer;
    use libduscape::model::SizeKind;

    #[test]
    fn a_layout_painted_in_full_once_stays_so() {
        let mut viewer = Viewer::new(&::std::env::temp_dir(), SizeKind::Apparent, 1);
        viewer.resize(800.0, 600.0);
        let mut paints = Paints::default();
        assert!(
            !paints.in_full(&viewer),
            "a new layout's first paint may hurry"
        );
        paints.painted(&viewer, false);
        assert!(paints.owed(&viewer), "its labels are owed");
        paints.second_pass(&mut viewer);
        assert!(paints.in_full(&viewer) && !paints.owed(&viewer));
        paints.painted(&viewer, true);
        assert!(
            paints.in_full(&viewer),
            "and a hover's paint is in full too"
        );
        viewer.resize(900.0, 600.0);
        assert!(!paints.in_full(&viewer), "until the layout changes");
    }

    #[test]
    fn a_budget_in_full_allows_every_label() {
        let labels = LabelBudget::new(true);
        assert!(labels.allows().is_some() && labels.complete());
    }

    #[test]
    fn a_hurried_budget_counts_the_labels_time_not_the_paints() {
        let labels = LabelBudget::new(false);
        ::std::thread::sleep(super::LABEL_DEADLINE + Duration::from_millis(1));
        assert!(
            labels.allows().is_some(),
            "the paint's fills took the time, not the labels"
        );
        {
            let _one = labels.allows().expect("nothing spent yet");
            ::std::thread::sleep(super::LABEL_DEADLINE);
        }
        assert!(labels.allows().is_none(), "the labels had their time");
        assert!(!labels.complete(), "and the second pass is owed");
    }
}
