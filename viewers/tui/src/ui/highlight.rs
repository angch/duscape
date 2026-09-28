//! How the entry in hand and the marked entries are shown: one rule for the list and the
//! treemap, so a row and its tile look alike.

use ::ratatui::style::{Color, Modifier, Style};

use libduscape::tiles::FileType;

/// The colours of an entry that is `cursor` (in hand) and/or `marked`, in a panel that has the
/// keyboard (`focused`) or not; `None` when it is neither.
///
/// Marked is black on yellow, bold and underlined when it is in hand too, so a mark always
/// shows. In hand in the panel with the keyboard is a bar: white on blue for a folder, black on
/// light gray for a file (not magenta on gray, which is hard to read). In hand in the other
/// panel is white, bold and underlined, with no bar: where the keyboard is not.
#[must_use]
pub fn highlight(file_type: FileType, cursor: bool, marked: bool, focused: bool) -> Option<Style> {
    let in_hand = Modifier::BOLD | Modifier::UNDERLINED;
    if marked {
        let marked = Style::default().fg(Color::Black).bg(Color::Yellow);
        return Some(if cursor {
            marked.add_modifier(in_hand)
        } else {
            marked
        });
    }
    if !cursor {
        return None;
    }
    if !focused {
        return Some(Style::default().fg(Color::White).add_modifier(in_hand));
    }
    let bar = match file_type {
        FileType::Folder => Style::default().fg(Color::White).bg(Color::Blue),
        FileType::File => Style::default().fg(Color::Black).bg(Color::Gray),
    };
    Some(bar.add_modifier(Modifier::BOLD))
}
