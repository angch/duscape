use ::ratatui::buffer::Buffer;
use ::ratatui::layout::Rect;
use ::ratatui::style::{Color, Modifier, Style};

use super::{CellSizeOpt, TitleTelescope};

#[test]
fn telescope_renders_total_on_wide_terminal() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
    let mut telescope = TitleTelescope::new(Style::default().fg(Color::Yellow));
    telescope.append_to_left_side(vec![
        CellSizeOpt::new(" Total: 1.0K (2 files), freed: 0 ".into()),
        CellSizeOpt::new(" Total: 1.0K (2 files) ".into()),
    ]);
    telescope.append_to_right_side(vec![CellSizeOpt::new(" /tmp/scan ".into())]);
    telescope.render(Rect::new(0, 0, 120, 1), &mut buf);

    let line: String = (0..120)
        .filter_map(|x| buf[(x, 0)].symbol().chars().next())
        .collect();
    assert!(line.contains("Total: 1.0K"));
    assert!(line.contains("/tmp/scan"));
}

#[test]
fn telescope_shows_loading_indicator_when_loading() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
    let mut telescope = TitleTelescope::new(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    telescope.append_to_left_side(vec![CellSizeOpt::new(" Total: 1.0K (1 files) ".into())]);
    telescope.append_to_right_side(vec![CellSizeOpt::new(" /tmp ".into())]);
    let telescope = telescope.loading(true, 3);
    telescope.render(Rect::new(0, 0, 80, 1), &mut buf);

    let line: String = (0..80)
        .filter_map(|x| buf[(x, 0)].symbol().chars().next())
        .collect();
    assert!(
        !line.trim().is_empty(),
        "loading title should render visible content"
    );
}

fn title_text(title: super::TitleLine<'_>, width: u16) -> String {
    use ::ratatui::widgets::Widget;
    let area = Rect::new(0, 0, width, 1);
    let mut buf = Buffer::empty(area);
    title.render(area, &mut buf);
    (0..width)
        .filter_map(|x| buf[(x, 0)].symbol().chars().next())
        .collect()
}

fn scanned(path: &::std::path::PathBuf) -> crate::ui::FolderInfo<'_> {
    crate::ui::FolderInfo {
        path,
        size: 300 * 1024 * 1024 * 1024,
        num_descendants: 10,
    }
}

/// A whole-volume scan says how much of the volume's used space it did not find, and the disk
/// used figure is the scan's total plus that.
#[test]
fn title_shows_space_outside_the_scan() {
    let path = ::std::path::PathBuf::from("C:\\");
    let outside = 100u128 * 1024 * 1024 * 1024;
    let title =
        super::TitleLine::new(scanned(&path), scanned(&path), 0).outside_scan(Some(outside));
    let line = title_text(title, 160);
    assert!(
        line.contains("disk used: 400.0G, 100.0G outside the scan"),
        "{line}"
    );
}

/// The title says whether the sizes are on-disk usage (the default) or the logical `-a` length,
/// so the reader is never left guessing which they are looking at.
#[test]
fn title_labels_the_size_mode() {
    let path = ::std::path::PathBuf::from("C:\\");
    let on_disk = super::TitleLine::new(scanned(&path), scanned(&path), 0).apparent_size(false);
    assert!(
        title_text(on_disk, 160).contains("Total on disk:"),
        "the default is on-disk usage and should say so"
    );
    let apparent = super::TitleLine::new(scanned(&path), scanned(&path), 0).apparent_size(true);
    assert!(
        title_text(apparent, 160).contains("Total (apparent):"),
        "-a shows logical lengths and should say so"
    );
}

/// Once the scan is done the title reports how long it took; while scanning it does not.
#[test]
fn title_shows_scan_time_when_complete() {
    let path = ::std::path::PathBuf::from("C:\\");
    let done = super::TitleLine::new(scanned(&path), scanned(&path), 0)
        .scan_duration(Some(::std::time::Duration::from_millis(1350)));
    assert!(
        title_text(done, 160).contains("scanned in 1.4s"),
        "a completed scan shows its elapsed time"
    );
    let scanning = super::TitleLine::new(scanned(&path), scanned(&path), 0)
        .scan_duration(Some(::std::time::Duration::from_millis(1350)))
        .show_loading();
    assert!(
        !title_text(scanning, 160).contains("scanned in"),
        "while still scanning, no elapsed time is shown"
    );
}

/// While scanning, most of the volume has simply not been reached yet.
#[test]
fn title_hides_space_outside_the_scan_while_scanning() {
    let path = ::std::path::PathBuf::from("C:\\");
    let title = super::TitleLine::new(scanned(&path), scanned(&path), 0)
        .outside_scan(Some(1 << 30))
        .show_loading();
    assert!(!title_text(title, 160).contains("outside"));
}

#[test]
fn title_hides_space_outside_the_scan_when_there_is_none() {
    let path = ::std::path::PathBuf::from("C:\\");
    let title = super::TitleLine::new(scanned(&path), scanned(&path), 0).outside_scan(Some(0));
    assert!(!title_text(title, 160).contains("outside"));
}

/// Counts in the title are grouped by thousands, so a whole-disk file count is readable at a
/// glance: 11,341,063 files, not 11341063.
#[test]
fn title_separates_thousands_in_counts() {
    let path = ::std::path::PathBuf::from("/");
    let info = || crate::ui::FolderInfo {
        path: &path,
        size: 766 * 1024 * 1024 * 1024,
        num_descendants: 11_341_063,
    };
    let title = super::TitleLine::new(info(), info(), 0).read_errors(1_296);
    let line = title_text(title, 160);
    assert!(line.contains("(11,341,063 files)"), "{line}");
    assert!(line.contains("failed to read 1,296 files"), "{line}");
}

/// While a copy is flashing the title is only that, label and all; on a narrow line the label
/// goes first and then the middle of the path.
#[test]
fn title_flashes_what_was_copied() {
    let path = ::std::path::PathBuf::from("/");
    let title = super::TitleLine::new(scanned(&path), scanned(&path), 0)
        .clipboard_flash(Some("relative path: 'my dir/file'"));
    let line = title_text(title, 80);
    assert!(
        line.contains("Copied relative path: 'my dir/file'"),
        "{line}"
    );
    assert!(
        !line.contains("Total"),
        "the flash replaces the rest: {line}"
    );

    let long = format!("absolute path: /{}", "x".repeat(200));
    let title =
        super::TitleLine::new(scanned(&path), scanned(&path), 0).clipboard_flash(Some(&long));
    let line = title_text(title, 60);
    assert!(!line.contains("Copied"), "{line}");
    assert!(line.trim_start().starts_with("/x"), "{line}");
}
