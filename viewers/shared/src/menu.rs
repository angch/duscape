//! The context menu every desktop viewer opens on a right-click: which items, in which order,
//! with which words, and which of them can be chosen. A viewer draws the entries with its own
//! toolkit (or by hand), adds its own key hints, and carries out the [`Action`] chosen; what the
//! menu offers is decided here, once, so the three windows offer the same.
//!
//! The menu is about what a command would act on: every marked entry, or else the row in hand —
//! [`Viewer::context_click`] has put the entry under the pointer there first.

use ::std::path::PathBuf;

use libduscape::DisplayCount;
use libduscape::tiles::FileType;

use crate::state::Viewer;

/// What a menu item does. The viewer carries it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Go into the folder in hand, or open the file with its default app.
    Open,
    /// macOS's Quick Look.
    QuickLook,
    /// Show the targets in the file manager (Finder, Explorer…), selected.
    Reveal,
    /// Their paths, quoted for the shell, relative to the working directory where they can be
    /// ([`Viewer::copied_paths`]).
    CopyPath,
    /// Their paths, quoted for the shell, absolute.
    CopyFullPath,
    /// Their paths as they are, one a line: Finder's Copy as Pathname.
    CopyPathname,
    /// Scan the folder in hand again, or the folder holding the file in hand.
    Rescan,
    RescanAll,
    /// Move the targets to the Trash (the Recycle Bin), after asking.
    Trash,
    /// Delete the targets for good, after asking.
    Delete,
}

/// A line of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item {
        action: Action,
        label: String,
        enabled: bool,
    },
    Separator,
}

impl Entry {
    /// The item's action, if it is an item that can be chosen.
    #[must_use]
    pub fn chosen(&self) -> Option<Action> {
        match self {
            Entry::Item {
                action,
                enabled: true,
                ..
            } => Some(*action),
            _ => None,
        }
    }
}

/// What the viewer's platform can do beyond what every viewer does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    /// The file manager's name for [`Action::Reveal`]'s item: `Show in Finder`.
    pub reveal: &'static str,
    pub quick_look: bool,
    /// [`Action::CopyPathname`], Finder's plain path.
    pub pathname: bool,
    /// A Trash to move entries to; without one, deleting is for good.
    pub trash: bool,
}

impl Viewer {
    /// [`Action::Open`]: go into the folder in hand, or give the file in hand's path for the
    /// viewer to open with its default app. `None` when it went in, or nothing is in hand.
    pub fn open_in_hand(&mut self) -> Option<PathBuf> {
        if self.enter_selected() {
            return None;
        }
        self.cursor_entry().map(|row| self.row_path(&row.path))
    }

    /// The context menu for what is targeted now: nothing when nothing is, the scan still
    /// running greys out what would change the tree.
    #[must_use]
    pub fn context_menu(&self, platform: &Platform) -> Vec<Entry> {
        let marked = self.marked.len();
        let in_hand = self.cursor_entry().map(|row| &row.entry);
        if marked == 0 && in_hand.is_none() {
            return Vec::new();
        }
        let many = (marked > 1).then(|| DisplayCount(marked as u64).to_string());
        let loaded = !self.scanning;
        let item = |action, label: String, enabled| Entry::Item {
            action,
            label,
            enabled,
        };
        let mut menu = vec![
            // One thing opens; several would be a window each.
            item(Action::Open, "Open".to_string(), many.is_none()),
        ];
        if platform.quick_look {
            menu.push(item(Action::QuickLook, "Quick Look".to_string(), true));
        }
        menu.push(item(Action::Reveal, platform.reveal.to_string(), true));
        menu.push(Entry::Separator);
        let (path, full) = match &many {
            Some(count) => (
                format!("Copy {count} Paths"),
                format!("Copy {count} Full Paths"),
            ),
            None => ("Copy Path".to_string(), "Copy Full Path".to_string()),
        };
        menu.push(item(Action::CopyPath, path, true));
        menu.push(item(Action::CopyFullPath, full, true));
        if platform.pathname {
            menu.push(item(
                Action::CopyPathname,
                "Copy as Pathname".to_string(),
                true,
            ));
        }
        menu.push(Entry::Separator);
        // The folder `rescan_selected` would scan: the one in hand, or the one holding the file.
        let rescan = match in_hand {
            Some(entry) if entry.file_type == FileType::Folder => "Rescan Folder",
            _ => "Rescan Enclosing Folder",
        };
        let can_rescan = self.can_rescan();
        menu.push(item(Action::Rescan, rescan.to_string(), can_rescan));
        menu.push(item(
            Action::RescanAll,
            "Rescan Everything".to_string(),
            can_rescan,
        ));
        menu.push(Entry::Separator);
        let what = match &many {
            Some(count) => format!(" {count} Items"),
            None => String::new(),
        };
        if platform.trash {
            menu.push(item(Action::Trash, format!("Move{what} to Trash"), loaded));
            menu.push(item(
                Action::Delete,
                format!("Delete{what} Immediately…"),
                loaded,
            ));
        } else {
            menu.push(item(Action::Delete, format!("Delete{what}…"), loaded));
        }
        menu
    }
}
