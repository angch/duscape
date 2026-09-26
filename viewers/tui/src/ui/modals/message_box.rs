use ::ratatui::buffer::Buffer;
use ::ratatui::layout::Rect;
use ::ratatui::style::{Color, Modifier, Style};
use ::ratatui::widgets::Widget;
use ::unicode_width::UnicodeWidthStr;

use crate::ui::grid::draw_filled_rect;
use libduscape::FileToDelete;
use libduscape::format::{DisplayCount, DisplaySize, truncate_middle};
use libduscape::tiles::FileType;

/// Text as it may be drawn: lossy, since a name need not be UTF-8, and with control characters
/// replaced, since one would reach the terminal as a command.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn file_name(file_to_delete: &FileToDelete) -> String {
    printable(
        &file_to_delete
            .path_to_file
            .last()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default(),
    )
}

/// The full path if it fits, else the name, cut in the middle.
fn truncated_file_name_line(file_to_delete: &FileToDelete, max_len: u16) -> String {
    let full_path = printable(&file_to_delete.full_path().to_string_lossy());
    if usize::from(max_len) > UnicodeWidthStr::width(full_path.as_str()) {
        full_path
    } else {
        truncate_middle(&file_name(file_to_delete), max_len)
    }
}

/// What is about to go: one entry's path, or several names — as many as fit, then how many more.
fn names_line(files: &[FileToDelete], max_len: u16) -> String {
    if let [file] = files {
        return truncated_file_name_line(file, max_len);
    }
    let fits = |line: &String| UnicodeWidthStr::width(line.as_str()) <= usize::from(max_len);
    let names: Vec<String> = files.iter().map(file_name).collect();
    let all = names.join(", ");
    if fits(&all) {
        return all;
    }
    (1..names.len())
        .rev()
        .map(|shown| {
            format!(
                "{} and {} more",
                names[..shown].join(", "),
                DisplayCount((names.len() - shown) as u64)
            )
        })
        .find(fits)
        .unwrap_or_else(|| {
            truncate_middle(
                &format!(
                    "{} and {} more",
                    names[0],
                    DisplayCount(names.len() as u64 - 1)
                ),
                max_len,
            )
        })
}

/// The first of `options` that fits in `max_len` columns.
fn first_that_fits(options: Vec<String>, max_len: u16) -> String {
    let last = options.last().cloned().unwrap_or_default();
    options
        .into_iter()
        .find(|option| UnicodeWidthStr::width(option.as_str()) <= usize::from(max_len))
        .unwrap_or(last)
}

fn question_line(files: &[FileToDelete], max_len: u16) -> String {
    match files {
        [file] => match file.file_type {
            FileType::File => first_that_fits(
                vec!["Delete this file?".to_string(), "Delete?".to_string()],
                max_len,
            ),
            FileType::Folder => {
                let children = DisplayCount(file.num_descendants.unwrap_or_default());
                first_that_fits(
                    vec![
                        format!("Delete folder with {children} children?"),
                        "Delete folder?".to_string(),
                        "Delete?".to_string(),
                    ],
                    max_len,
                )
            }
        },
        files => {
            let count = DisplayCount(files.len() as u64);
            // Sizes are not additive where hard links are involved; this is what each held.
            let size = DisplaySize(files.iter().map(|file| file.size).sum::<u128>() as f64);
            first_that_fits(
                vec![
                    format!("Delete these {count} items ({size})?"),
                    format!("Delete {count} items?"),
                    "Delete?".to_string(),
                ],
                max_len,
            )
        }
    }
}

/// Draw `text` centred across `rect` on row `y`.
fn centred(buf: &mut Buffer, rect: &Rect, y: u16, text: &str, style: Style) {
    let width = UnicodeWidthStr::width(text) as u16;
    let x = rect.x + (rect.width.saturating_sub(width) as f64 / 2.0).ceil() as u16;
    buf.set_string(x, y, text, style);
}

fn text_style() -> Style {
    Style::default()
        .bg(Color::Black)
        .fg(Color::Red)
        .add_modifier(Modifier::BOLD)
}

fn render_deletion_prompt(buf: &mut Buffer, message_rect: &Rect, files: &[FileToDelete]) {
    let max_text_len = message_rect.width - 4;
    let middle = message_rect.y + message_rect.height / 2;
    centred(
        buf,
        message_rect,
        middle - 3,
        &question_line(files, max_text_len),
        text_style(),
    );
    centred(
        buf,
        message_rect,
        middle,
        &names_line(files, max_text_len),
        text_style(),
    );
    centred(buf, message_rect, middle + 3, "(y/n)", text_style());
}

fn render_deletion_in_progress(buf: &mut Buffer, message_rect: &Rect, files: &[FileToDelete]) {
    let max_text_len = message_rect.width - 4;
    let middle = message_rect.y + message_rect.height / 2;
    centred(buf, message_rect, middle - 1, "Deleting", text_style());
    centred(
        buf,
        message_rect,
        middle + 1,
        &names_line(files, max_text_len),
        text_style(),
    );
}

pub struct MessageBox<'a> {
    files: &'a [FileToDelete],
    deletion_in_progress: bool,
}

impl<'a> MessageBox<'a> {
    pub fn new(files: &'a [FileToDelete], deletion_in_progress: bool) -> Self {
        Self {
            files,
            deletion_in_progress,
        }
    }
}

impl<'a> Widget for MessageBox<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (width, height) = if area.width > 150 {
            (150, 10)
        } else if area.width >= 50 {
            (area.width / 2, 10)
        } else {
            unreachable!("app should not be rendered if window is so small")
        };

        // position self in the middle of the rect
        let x = ((area.x + area.width) / 2) - width / 2;
        let y = ((area.y + area.height) / 2) - height / 2;

        let message_rect = Rect {
            x,
            y,
            width,
            height,
        };
        let fill_style = Style::default()
            .bg(Color::Black)
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD);

        draw_filled_rect(buf, fill_style, &message_rect);
        if self.deletion_in_progress {
            render_deletion_in_progress(buf, &message_rect, self.files);
        } else {
            render_deletion_prompt(buf, &message_rect, self.files);
        }
    }
}

#[cfg(test)]
mod tests {
    use ::std::path::PathBuf;

    use ::ratatui::buffer::Buffer;
    use ::ratatui::layout::Rect;
    use ::ratatui::widgets::Widget;
    use libduscape::FileToDelete;
    use libduscape::tiles::FileType;

    use super::{MessageBox, names_line, question_line};

    fn file(name: &str, size: u128) -> FileToDelete {
        FileToDelete {
            path_in_filesystem: PathBuf::from("/data"),
            path_to_file: vec![name.into()],
            file_type: FileType::File,
            num_descendants: None,
            size,
            sizes: libduscape::model::Sizes::new(size, size),
        }
    }

    #[test]
    fn one_entry_reads_as_it_always_did() {
        let files = [file("notes.txt", 10)];
        assert_eq!(question_line(&files, 60), "Delete this file?");
        assert_eq!(
            names_line(&files, 60),
            PathBuf::from("/data")
                .join("notes.txt")
                .display()
                .to_string()
        );
    }

    #[test]
    fn several_entries_are_counted_and_named_as_far_as_they_fit() {
        let files = [file("alpha", 1024), file("beta", 1024), file("gamma", 2048)];
        assert_eq!(question_line(&files, 60), "Delete these 3 items (4.0K)?");
        assert_eq!(question_line(&files, 16), "Delete 3 items?");
        assert_eq!(names_line(&files, 60), "alpha, beta, gamma");
        assert_eq!(names_line(&files, 18), "alpha, beta, gamma", "exactly fits");
        assert_eq!(names_line(&files, 17), "alpha and 2 more");
    }

    #[test]
    fn the_prompt_renders_for_several_entries() {
        let files = [file("alpha", 1024), file("beta", 1024)];
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        MessageBox::new(&files, false).render(area, &mut buf);
        let text: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("Delete these 2 items (2.0K)?"), "{text}");
        assert!(text.contains("alpha, beta"), "{text}");
        assert!(text.contains("(y/n)"), "{text}");
    }
}
