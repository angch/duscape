use ::ratatui::buffer::Buffer;
use ::ratatui::layout::Rect;
use ::ratatui::style::{Color, Modifier, Style};
use ::ratatui::widgets::Widget;
use ::std::path::{Path, PathBuf};

use libdiskonaut::format::{DisplayCount, DisplaySize, truncate_middle};
use libdiskonaut::tiles::{FileMetadata, FileType};

use crate::config::Keybinds;

fn render_currently_selected(
    buf: &mut Buffer,
    currently_selected: &FileMetadata,
    max_len: u16,
    y: u16,
) {
    let file_name = currently_selected.name.to_string_lossy();
    let size = DisplaySize(currently_selected.size as f64);
    let descendants = currently_selected.descendants;
    let (style, lines) = match currently_selected.file_type {
        FileType::File => (
            Style::default().add_modifier(Modifier::BOLD),
            vec![
                format!("SELECTED: {} ({})", file_name, size),
                format!("SELECTED: {}", file_name),
                format!("{}", file_name),
            ],
        ),
        FileType::Folder => (
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            vec![
                format!(
                    "SELECTED: {} ({}, {} files)",
                    file_name,
                    size,
                    DisplayCount(descendants.expect("a folder should have descendants"))
                ),
                format!("SELECTED: {} ({})", file_name, size),
                format!("SELECTED: {}", file_name),
                format!("{}", file_name),
            ],
        ),
    };
    for line in lines {
        if (line.chars().count() as u16) < max_len {
            buf.set_string(1, y, line, style);
            break;
        }
    }
}

fn render_last_read_path(buf: &mut Buffer, last_read_path: &Path, max_len: u16, y: u16) {
    let last_read_path = last_read_path.to_string_lossy();
    if (last_read_path.chars().count() as u16) < max_len {
        buf.set_string(1, y, last_read_path, Style::default());
    } else {
        buf.set_string(
            1,
            y,
            truncate_middle(&last_read_path, max_len),
            Style::default(),
        );
    }
}

/// The help line, spelled with the keys actually bound so that it cannot disagree with them.
fn controls_legend(kb: &Keybinds, hide_delete: bool, switch_panel: bool) -> (String, String) {
    let delete_long = if hide_delete {
        String::new()
    } else {
        format!("<{}> - delete, ", kb.delete)
    };
    let delete_short = if hide_delete {
        String::new()
    } else {
        format!(", <{}>: del", kb.delete)
    };
    let switch_long = if switch_panel {
        format!("<{}> - list/map, ", kb.switch_panel)
    } else {
        String::new()
    };
    (
        format!(
            "<arrows> - move around, {switch_long}<{enter}> - enter folder, <{parent}> - parent \
             folder, {delete_long}<{zoom_in}/{zoom_out}/{reset_zoom}> - zoom in/out/reset, \
             <{quit}> - quit",
            enter = kb.enter,
            parent = kb.parent,
            zoom_in = kb.zoom_in,
            zoom_out = kb.zoom_out,
            reset_zoom = kb.reset_zoom,
            quit = kb.quit,
        ),
        format!(
            "←↓↑→/<{}>/<{}>{}: navigate{delete_short}",
            kb.enter,
            kb.parent,
            if switch_panel {
                format!("/<{}>", kb.switch_panel)
            } else {
                String::new()
            }
        ),
    )
}

fn render_controls_legend(
    buf: &mut Buffer,
    keybinds: &Keybinds,
    hide_delete: bool,
    switch_panel: bool,
    max_len: u16,
    y: u16,
) {
    let (long_controls_line, short_controls_line) =
        controls_legend(keybinds, hide_delete, switch_panel);
    let too_small_line = "(...)";
    if max_len >= long_controls_line.chars().count() as u16 {
        buf.set_string(
            1,
            y,
            long_controls_line,
            Style::default().add_modifier(Modifier::BOLD),
        );
    } else if max_len >= short_controls_line.chars().count() as u16 {
        buf.set_string(
            1,
            y,
            short_controls_line,
            Style::default().add_modifier(Modifier::BOLD),
        );
    } else {
        buf.set_string(
            1,
            y,
            too_small_line,
            Style::default().add_modifier(Modifier::BOLD),
        );
    }
}

fn render_small_files_legend(buf: &mut Buffer, x: u16, y: u16, small_files_legend: &str) {
    buf.set_string(
        x,
        y,
        small_files_legend,
        Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .remove_modifier(Modifier::all()),
    );
    let small_files_legend_character = &mut buf[(x + 1, y)];
    small_files_legend_character.set_style(Style::default().bg(Color::White).fg(Color::Black));
}

pub struct BottomLine<'a> {
    keybinds: &'a Keybinds,
    hide_delete: bool,
    hide_small_files_legend: bool,
    currently_selected: Option<&'a FileMetadata>,
    /// Name the key that moves between the list and the treemap, while there is a list.
    switch_panel_hint: bool,
    last_read_path: Option<&'a PathBuf>,
}

impl<'a> BottomLine<'a> {
    pub fn new(keybinds: &'a Keybinds) -> Self {
        Self {
            keybinds,
            hide_delete: false,
            hide_small_files_legend: false,
            currently_selected: None,
            switch_panel_hint: false,
            last_read_path: None,
        }
    }
    pub fn hide_delete(mut self) -> Self {
        self.hide_delete = true;
        self
    }
    pub fn hide_small_files_legend(mut self, should_hide_small_files_legend: bool) -> Self {
        self.hide_small_files_legend = should_hide_small_files_legend;
        self
    }
    pub fn currently_selected(mut self, currently_selected: Option<&'a FileMetadata>) -> Self {
        self.currently_selected = currently_selected;
        self
    }
    pub fn switch_panel_hint(mut self, show: bool) -> Self {
        self.switch_panel_hint = show;
        self
    }
    pub fn last_read_path(mut self, last_read_path: Option<&'a PathBuf>) -> Self {
        self.last_read_path = last_read_path;
        self
    }
}

impl<'a> Widget for BottomLine<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let small_files_legend = "(x = Small files)";
        let small_files_len = if self.hide_small_files_legend {
            0
        } else {
            small_files_legend.chars().count() as u16
        };
        let max_status_len = area.width - small_files_len - 1;
        let max_controls_len = area.width - 1;
        let status_line_y = area.y + area.height - 2;
        let controls_line_y = status_line_y + 1;
        if let Some(currently_selected) = self.currently_selected {
            render_currently_selected(buf, currently_selected, max_status_len, status_line_y);
        } else if let Some(last_read_path) = self.last_read_path {
            render_last_read_path(buf, last_read_path, max_status_len, status_line_y);
        }

        if !self.hide_small_files_legend {
            render_small_files_legend(
                buf,
                area.width - small_files_len - 1,
                status_line_y,
                small_files_legend,
            );
        }

        render_controls_legend(
            buf,
            self.keybinds,
            self.hide_delete,
            self.switch_panel_hint,
            max_controls_len,
            controls_line_y,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeyBinding;
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn legend_names_the_bound_delete_key() {
        let (long, short) = controls_legend(&Keybinds::default(), false, false);
        assert!(long.contains("<d> - delete"), "{long}");
        assert!(short.contains("<d>: del"), "{short}");
        assert!(!long.contains("BACKSPACE"), "{long}");
    }

    #[test]
    fn legend_follows_a_rebound_delete_key() {
        let kb = Keybinds {
            delete: KeyBinding::key(KeyCode::Backspace),
            ..Keybinds::default()
        };
        let (long, short) = controls_legend(&kb, false, false);
        assert!(long.contains("<BACKSPACE> - delete"), "{long}");
        assert!(short.contains("<BACKSPACE>: del"), "{short}");
    }

    #[test]
    fn legend_omits_delete_when_hidden() {
        let (long, short) = controls_legend(&Keybinds::default(), true, false);
        assert!(!long.contains("delete"), "{long}");
        assert!(!short.contains("del"), "{short}");
    }
}

#[cfg(test)]
mod switch_panel_tests {
    use super::controls_legend;
    use crate::config::Keybinds;

    #[test]
    fn legend_names_the_switch_panel_key_while_there_is_a_list() {
        let (long, short) = controls_legend(&Keybinds::default(), false, true);
        assert!(long.contains("<TAB> - list/map"), "{long}");
        assert!(short.contains("/<TAB>: navigate"), "{short}");
        let (long, short) = controls_legend(&Keybinds::default(), false, false);
        assert!(!long.contains("TAB"), "{long}");
        assert!(!short.contains("TAB"), "{short}");
    }
}
