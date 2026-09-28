use ::std::ffi::OsString;

use libduscape::tiles::{FileType, Tile};
use ratatui::style::{Color, Modifier};

use super::tile_style;

fn sample_tile(file_type: FileType, width: u16) -> Tile {
    Tile {
        x: 0,
        y: 0,
        width,
        height: 10,
        name: OsString::from(if matches!(file_type, FileType::Folder) {
            "docs"
        } else {
            "readme.txt"
        }),
        size: 2048,
        descendants: if matches!(file_type, FileType::Folder) {
            Some(3)
        } else {
            None
        },
        percentage: 0.5,
        file_type,
    }
}

#[test]
fn selected_file_uses_black_on_gray() {
    let tile = sample_tile(FileType::File, 20);
    let (_, first, second) = tile_style(&tile, true, false, true);
    assert_eq!(
        first.fg,
        Some(Color::Black),
        "not magenta, which is hard to read on gray"
    );
    assert_eq!(first.bg, Some(Color::Gray));
    assert_eq!(second.fg, Some(Color::Black));
}

#[test]
fn a_marked_tile_is_black_on_yellow_in_hand_or_not() {
    let tile = sample_tile(FileType::Folder, 20);
    let (_, marked, _) = tile_style(&tile, false, true, true);
    assert_eq!(
        (marked.fg, marked.bg),
        (Some(Color::Black), Some(Color::Yellow))
    );
    // As in the list: the mark shows, and being in hand underlines it.
    let (_, cursor, _) = tile_style(&tile, true, true, true);
    assert_eq!(cursor.bg, Some(Color::Yellow));
    assert!(cursor.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn selected_folder_uses_white_on_blue() {
    let tile = sample_tile(FileType::Folder, 30);
    let (_, first, _) = tile_style(&tile, true, false, true);
    assert_eq!(first.fg, Some(Color::White));
    assert_eq!(first.bg, Some(Color::Blue));
}

#[test]
fn in_hand_where_the_list_has_the_keyboard_is_underlined_with_no_bar() {
    for file_type in [FileType::Folder, FileType::File] {
        let tile = sample_tile(file_type, 30);
        let (fill, first, _) = tile_style(&tile, true, false, false);
        assert_eq!(fill, None, "no bar");
        assert_eq!((first.fg, first.bg), (Some(Color::White), None));
        assert!(first.add_modifier.contains(Modifier::UNDERLINED));
    }
}

#[test]
fn unselected_folder_name_is_blue_bold() {
    let tile = sample_tile(FileType::Folder, 30);
    let (_, first, _) = tile_style(&tile, false, false, true);
    assert_eq!(first.fg, Some(Color::Blue));
}

/// The tile's highlight is the list row's: the same function, with the same answers.
#[test]
fn a_tile_and_its_row_are_highlighted_alike() {
    use crate::ui::highlight::highlight;
    for file_type in [FileType::Folder, FileType::File] {
        for (cursor, marked, focused) in [
            (true, false, true),
            (true, false, false),
            (false, true, true),
            (true, true, false),
        ] {
            let tile = sample_tile(file_type, 30);
            let (_, first, _) = tile_style(&tile, cursor, marked, focused);
            assert_eq!(Some(first), highlight(file_type, cursor, marked, focused));
        }
    }
}

#[test]
fn a_highlighted_folder_is_framed_in_its_colours_and_its_inside_left_alone() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let tile = sample_tile(FileType::Folder, 20);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 15));
    super::draw_rect_on_grid(&mut buf, (0, 0), (tile.width, tile.height));
    buf[(5, 5)].set_symbol("x");
    super::frame_on_grid(&mut buf, &tile, true, false, true);
    for (x, y) in [(0, 0), (10, 0), (20, 10), (0, 5), (20, 5)] {
        let cell = &buf[(x, y)];
        assert_eq!(
            (cell.fg, cell.bg),
            (Color::White, Color::Blue),
            "border cell {x},{y}"
        );
        assert!(cell.modifier.contains(Modifier::BOLD));
    }
    assert_eq!(
        buf[(5, 5)].fg,
        Color::Reset,
        "the inside keeps its own colours"
    );
    assert_eq!(buf[(5, 5)].symbol(), "x");
    super::frame_on_grid(&mut buf, &tile, false, true, true);
    assert_eq!(
        (buf[(0, 0)].fg, buf[(0, 0)].bg),
        (Color::Black, Color::Yellow),
        "a mark frames black on yellow"
    );
}

#[test]
fn a_folder_too_short_for_its_header_leaves_no_colour_on_its_entries_borders() {
    use ::std::path::Path;
    use libduscape::Folder;
    use libduscape::model::SizeKind;
    use libduscape::tiles::nest_with;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    let mut root = Folder::new(Path::new("/tmp/example"));
    root.add_file("short/a".into(), 600);
    root.add_file("short/b".into(), 400);
    // Tall enough to hold its entries under its border, too short for the header row above.
    let tile = Tile {
        height: 8,
        width: 40,
        name: OsString::from("short"),
        ..sample_tile(FileType::Folder, 40)
    };
    let nesting = crate::ui::display::NESTING;
    assert!(!nesting.labelled(&tile));
    let tiles = [tile];
    let nested = nest_with(&root, &tiles, SizeKind::Disk, &nesting, &mut |_| {});
    assert!(nested.tops[0].is_some(), "its entries are nested");
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 12));
    super::RectangleGrid::new(&tiles, None, None)
        .nested(&nested, &nesting)
        .render(Rect::new(0, 0, 50, 12), &mut buf);
    for y in 0..12 {
        for x in 0..50 {
            let cell = &buf[(x, y)];
            assert!(
                !("─│┌┐└┘├┤┬┴┼".contains(cell.symbol()) && cell.fg == Color::Blue),
                "a border at {x},{y} took a header's blue"
            );
        }
    }
}

#[test]
fn a_highlighted_file_folder_or_nesting_folder_is_framed_alike() {
    use ::std::path::Path;
    use libduscape::Folder;
    use libduscape::model::SizeKind;
    use libduscape::tiles::nest_with;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    let mut root = Folder::new(Path::new("/tmp/example"));
    root.add_file("holds/a".into(), 600);
    root.add_file("holds/b".into(), 400);
    root.add_file("empty/x".into(), 1);
    root.add_file("file.txt".into(), 500);
    // A folder with room for its entries, a folder too small for them, and a file.
    let place = |name: &str, file_type, x, width, height| Tile {
        x,
        y: 0,
        width,
        height,
        name: OsString::from(name),
        ..sample_tile(file_type, width)
    };
    let tiles = [
        place("holds", FileType::Folder, 0, 40, 14),
        place("empty", FileType::Folder, 40, 12, 5),
        place("file.txt", FileType::File, 52, 12, 5),
    ];
    let nesting = crate::ui::display::NESTING;
    let nested = nest_with(&root, &tiles, SizeKind::Disk, &nesting, &mut |_| {});
    assert!(nested.tops[0].is_some() && nested.tops[1].is_none());
    let colours = [
        (Color::White, Color::Blue),
        (Color::White, Color::Blue),
        (Color::Black, Color::Gray),
    ];
    for (index, colour) in colours.into_iter().enumerate() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 70, 16));
        super::RectangleGrid::new(&tiles, None, Some(index))
            .nested(&nested, &nesting)
            .render(Rect::new(0, 0, 70, 16), &mut buf);
        let t = &tiles[index];
        for (x, y) in [(t.x, t.y), (t.x + t.width, t.y + t.height), (t.x, t.y + 2)] {
            let cell = &buf[(x, y)];
            assert_eq!(
                (cell.fg, cell.bg),
                colour,
                "{:?}'s border at {x},{y}",
                t.name
            );
        }
    }
}

/// The corner's mark is one cell wide, as ratatui counts it and as terminals draw it: a
/// character East Asian "ambiguous" width (`□`) is two cells in many of them.
#[test]
fn the_small_files_mark_is_one_cell_wide() {
    use ::unicode_width::UnicodeWidthStr;
    assert_eq!(super::SMALL_FILE.width(), 1);
    assert_eq!(super::SMALL_FILE.width_cjk(), 1, "not ambiguous width");
    assert_eq!("□".width_cjk(), 2, "what the check catches");
}
