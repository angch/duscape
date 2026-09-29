//! What a window opened with no folder offers: the volumes, each with how full it is, and the
//! home folder — chosen with a click or the keyboard — in place of a folder dialog. The
//! state and the layout in points are here; a viewer draws the rows and starts the scan.

use ::std::path::{Path, PathBuf};

use libduscape::DisplaySize;
use libduscape::format::without_verbatim_prefix;
use libduscape::os::{Volume, volumes};

use crate::state::Rect;

/// One thing to scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub target: Target,
    /// The first line: the mount point, or the folder's name.
    pub title: String,
    /// The second: the device and filesystem, or the folder's path.
    pub detail: String,
    /// Bytes in use and in all, for the bar; none for a folder.
    pub fullness: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Scan(PathBuf),
    /// The platform's folder dialog, for a folder not listed.
    Dialog,
    /// Back to the scan the chooser was opened over.
    Cancel,
}

/// The height of a row, the gap between rows, and the band above them, in points.
pub const ROW: f64 = 54.0;
pub const GAP: f64 = 8.0;
pub const HEADING: f64 = 64.0;
/// The rows' width at most, and the margin around them.
pub const WIDTH: f64 = 640.0;
pub const MARGIN: f64 = 24.0;

/// The rows laid out in a window: each one's rectangle and its index in the choices, as many
/// as fit, and where the heading goes.
pub struct ChooserLayout {
    pub heading: Rect,
    pub rows: Vec<(Rect, usize)>,
}

pub struct Chooser {
    pub choices: Vec<Choice>,
    /// The row the keyboard is on.
    pub cursor: usize,
    /// The row under the pointer.
    pub hover: Option<usize>,
    /// The rows' top, for a list taller than the window.
    pub scroll: usize,
}

impl Chooser {
    /// The volumes mounted now, the home folder, and the folder dialog if the viewer has one.
    #[must_use]
    pub fn new(with_dialog: bool) -> Self {
        Self::from_parts(volumes(), home(), with_dialog)
    }

    #[must_use]
    pub fn from_parts(volumes: Vec<Volume>, home: Option<PathBuf>, with_dialog: bool) -> Self {
        let mut choices: Vec<Choice> = volumes
            .into_iter()
            .map(|volume| {
                let detail = match (volume.label.is_empty(), volume.filesystem.is_empty()) {
                    (false, false) => format!("{} · {}", volume.label, volume.filesystem),
                    (false, true) => volume.label,
                    (true, false) => volume.filesystem,
                    (true, true) => String::new(),
                };
                Choice {
                    title: volume.path.display().to_string(),
                    detail,
                    fullness: Some((volume.used, volume.total)),
                    target: Target::Scan(volume.path),
                }
            })
            .collect();
        if let Some(home) = home {
            choices.push(Choice {
                title: "Home folder".to_string(),
                detail: home.display().to_string(),
                fullness: None,
                target: Target::Scan(home),
            });
        }
        if with_dialog {
            choices.push(Choice {
                title: "Another folder…".to_string(),
                detail: "Choose one in a dialog".to_string(),
                fullness: None,
                target: Target::Dialog,
            });
        }
        Chooser {
            choices,
            cursor: 0,
            hover: None,
            scroll: 0,
        }
    }

    /// With a way back: the chooser opened over a scan — from the path bar's button — ends
    /// with a Cancel row that returns to it, as Escape does then instead of closing the window.
    #[must_use]
    pub fn with_cancel(mut self, back_to: &Path) -> Self {
        self.choices.push(Choice {
            title: "Cancel".to_string(),
            detail: format!(
                "Back to {}",
                without_verbatim_prefix(&back_to.to_string_lossy())
            ),
            fullness: None,
            target: Target::Cancel,
        });
        self
    }

    /// Whether the chooser can be left for a scan already on screen.
    #[must_use]
    pub fn cancellable(&self) -> bool {
        self.choices
            .iter()
            .any(|choice| choice.target == Target::Cancel)
    }

    /// The rows in `bounds`: centred, at most [`WIDTH`] wide, from the cursor's page.
    #[must_use]
    pub fn layout(&self, bounds: Rect) -> ChooserLayout {
        let width = (bounds.w - 2.0 * MARGIN).clamp(0.0, WIDTH);
        let x = bounds.x + (bounds.w - width) / 2.0;
        let heading = Rect::new(x, bounds.y + MARGIN, width, HEADING - MARGIN);
        let top = bounds.y + HEADING;
        let room = (bounds.bottom() - MARGIN - top).max(0.0);
        let per_page = (((room + GAP) / (ROW + GAP)).floor() as usize).max(1);
        let rows = self
            .choices
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(per_page)
            .enumerate()
            .map(|(slot, (index, _))| {
                let y = top + slot as f64 * (ROW + GAP);
                (Rect::new(x, y, width, ROW), index)
            })
            .collect();
        ChooserLayout { heading, rows }
    }

    /// The words for a row: what it is, what it is on, and how full — `12.3G of 50.0G` and the
    /// share used, for the bar.
    #[must_use]
    pub fn words(&self, index: usize) -> (String, String, String, Option<f64>) {
        let choice = &self.choices[index];
        match choice.fullness {
            Some((used, total)) => (
                choice.title.clone(),
                choice.detail.clone(),
                format!(
                    "{} of {}",
                    DisplaySize(used as f64),
                    DisplaySize(total as f64)
                ),
                Some(if total > 0 {
                    (used as f64 / total as f64).clamp(0.0, 1.0)
                } else {
                    0.0
                }),
            ),
            None => (
                choice.title.clone(),
                choice.detail.clone(),
                String::new(),
                None,
            ),
        }
    }

    /// The row at a point, by the last layout.
    #[must_use]
    pub fn hit(&self, bounds: Rect, x: f64, y: f64) -> Option<usize> {
        self.layout(bounds)
            .rows
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|&(_, index)| index)
    }

    /// The pointer moved. Returns whether the row under it changed.
    pub fn hover_at(&mut self, bounds: Rect, x: f64, y: f64) -> bool {
        let hover = self.hit(bounds, x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// The cursor a row down or up, kept on the page.
    pub fn arrow(&mut self, down: bool, bounds: Rect) {
        if self.choices.is_empty() {
            return;
        }
        self.cursor = if down {
            (self.cursor + 1).min(self.choices.len() - 1)
        } else {
            self.cursor.saturating_sub(1)
        };
        let per_page = self.layout(bounds).rows.len().max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + per_page {
            self.scroll = self.cursor + 1 - per_page;
        }
    }

    /// What the row in hand, or the one clicked, would scan.
    #[must_use]
    pub fn target(&self, index: usize) -> Option<&Target> {
        self.choices.get(index).map(|choice| &choice.target)
    }

    #[must_use]
    pub fn heading(&self) -> &'static str {
        if self.choices.is_empty() {
            "Nothing to scan: no volume could be read"
        } else {
            "Choose what to scan"
        }
    }
}

/// The user's home folder, if the environment names one that exists.
#[must_use]
pub fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    ::std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|path| path.is_dir() && path != Path::new("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(path: &str, used: u64, total: u64) -> Volume {
        Volume {
            path: PathBuf::from(path),
            label: "/dev/sda1".to_string(),
            filesystem: "ext4".to_string(),
            total,
            used,
        }
    }

    #[test]
    fn the_volumes_then_home_then_the_dialog_each_a_row_with_its_words() {
        let chooser = Chooser::from_parts(
            vec![volume("/", 30_000_000_000, 100_000_000_000)],
            Some(PathBuf::from("/home/me")),
            true,
        );
        assert_eq!(chooser.choices.len(), 3);
        let (title, detail, size, share) = chooser.words(0);
        assert_eq!((title.as_str(), detail.as_str()), ("/", "/dev/sda1 · ext4"));
        assert_eq!(size, "27.9G of 93.1G");
        assert!((share.unwrap() - 0.3).abs() < 1e-9);
        assert_eq!(chooser.words(1).2, "", "a folder has no bar");
        assert_eq!(chooser.target(2), Some(&Target::Dialog));
        assert_eq!(
            chooser.target(1),
            Some(&Target::Scan(PathBuf::from("/home/me")))
        );
    }

    #[test]
    fn rows_are_laid_out_centred_and_the_keyboard_pages_them() {
        let mut chooser = Chooser::from_parts(
            (0..10)
                .map(|i| volume(&format!("/mnt/{i}"), 1, 2))
                .collect(),
            None,
            false,
        );
        let bounds = Rect::new(0.0, 0.0, 1000.0, 300.0);
        let layout = chooser.layout(bounds);
        assert_eq!(layout.rows[0].0.w, WIDTH);
        assert!((layout.rows[0].0.x - 180.0).abs() < 1e-9, "centred");
        let per_page = layout.rows.len();
        assert!(per_page > 1 && per_page < 10, "as many as fit: {per_page}");
        let (rect, index) = layout.rows[1];
        assert_eq!(chooser.hit(bounds, rect.x + 1.0, rect.y + 1.0), Some(index));
        assert!(chooser.hover_at(bounds, rect.x + 1.0, rect.y + 1.0));
        assert!(!chooser.hover_at(bounds, rect.x + 2.0, rect.y + 2.0));
        for _ in 0..9 {
            chooser.arrow(true, bounds);
        }
        assert_eq!(chooser.cursor, 9);
        assert_eq!(
            chooser.scroll,
            10 - per_page,
            "scrolled to keep the cursor on the page"
        );
        assert!(
            chooser.layout(bounds).rows.iter().any(|&(_, i)| i == 9),
            "the cursor's row is on screen"
        );
        chooser.arrow(true, bounds);
        assert_eq!(chooser.cursor, 9, "stays at the end");
    }

    /// Opened over a scan, the chooser ends with a Cancel row back to it; opened with no
    /// folder there is nothing to go back to.
    #[test]
    fn a_chooser_over_a_scan_ends_with_cancel() {
        let fresh = Chooser::from_parts(vec![volume("/", 1, 2)], None, false);
        assert!(!fresh.cancellable());
        let over = Chooser::from_parts(vec![volume("/", 1, 2)], None, true)
            .with_cancel(Path::new("/home/me"));
        assert!(over.cancellable());
        let last = over.choices.len() - 1;
        assert_eq!(over.target(last), Some(&Target::Cancel));
        assert_eq!(
            over.words(last),
            (
                "Cancel".to_string(),
                "Back to /home/me".to_string(),
                String::new(),
                None
            )
        );
        assert_eq!(
            over.target(1),
            Some(&Target::Dialog),
            "the other rows as before"
        );
    }
}
