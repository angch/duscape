use ::std::path::Path;

use crate::model::Folder;
use crate::tiles::{Area, Board, FileType, Grid, files_in_folder};

#[test]
fn board_produces_tiles_for_folder() {
    let mut root = Folder::new(Path::new("/tmp/example"));
    root.add_file(std::path::PathBuf::from("a"), 600);
    root.add_file(std::path::PathBuf::from("b"), 400);

    let mut board = Board::new(&root);
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    });
    board.change_files(&root);

    assert_eq!(board.tiles.len(), 2);
    assert!(
        board
            .tiles
            .iter()
            .any(|t| t.file_type == FileType::File && t.size == 600)
    );
}

#[test]
fn files_in_folder_percentages_sum_to_one() {
    let mut root = Folder::default();
    root.add_file("x".into(), 75);
    root.add_file("y".into(), 25);

    let files = files_in_folder(&root, 0, crate::model::SizeKind::Disk);
    assert_eq!(files.len(), 2);
    let sum: f64 = files.iter().map(|f| f.percentage).sum();
    assert!((sum - 1.0).abs() < f64::EPSILON);
}

/// One entry holds nearly everything; the rest would round to zero cells. The board must still
/// mark that they exist, or the view looks as though the scan missed them.
#[test]
fn tiny_siblings_of_a_huge_entry_still_get_a_small_files_marker() {
    let mut root = Folder::new(Path::new("/tmp/example"));
    root.add_file(std::path::PathBuf::from("target"), 2_000_000_000);
    for name in ["a", "b", "c", "d", "e"] {
        root.add_file(std::path::PathBuf::from(name), 1_000_000);
    }

    let area = Area {
        x: 1,
        y: 1,
        width: 158,
        height: 36,
    };
    let mut board = Board::new(&root);
    board.change_area(&area);
    board.change_files(&root);

    assert_eq!(board.tiles.len(), 1, "only the huge entry is drawable");
    let (x, y) = board
        .unrenderable_tile_coordinates
        .expect("hidden entries must leave a small-files marker");
    let right = area.x + area.width;
    let bottom = area.y + area.height;
    assert!(x >= area.x && y >= area.y, "marker starts inside the board");
    assert!(
        right - x >= 4 && bottom - y >= 3,
        "marker is wide enough to draw: ({x}, {y}) in {area:?}"
    );
}

#[test]
fn small_files_marker_is_absent_when_everything_fits() {
    let mut root = Folder::new(Path::new("/tmp/example"));
    root.add_file(std::path::PathBuf::from("a"), 600);
    root.add_file(std::path::PathBuf::from("b"), 400);

    let mut board = Board::new(&root);
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    });
    board.change_files(&root);

    assert!(board.unrenderable_tile_coordinates.is_none());
}

/// A folder holding shared blocks — hard links, or XFS/btrfs reflinks — is *smaller* than the sum
/// of the entries inside it, because the same blocks reached twice count once. The layout must
/// still fit on the board: dividing each entry by the folder's deduplicated size gives fractions
/// summing to well over 1.0, and tiles laid out from those run off the end of the screen.
///
/// This is a rendering crash, not a cosmetic one: the UI indexes the terminal buffer directly at
/// `rect.x + rect.width`, so a tile outside the board panics the whole app.
#[test]
fn entries_larger_than_the_folder_holding_them_stay_on_the_board() {
    let mut root = Folder::new(Path::new("/tmp/example"));
    for name in ["a", "b", "c", "d"] {
        root.add_file(std::path::PathBuf::from(name), 1_000_000);
    }
    // What deduplication does: four entries of 1 MB each, all the same blocks, so the folder holds
    // 1 MB rather than 4 MB.
    root.sizes = crate::model::Sizes::new(1_000_000, 1_000_000);

    let files = files_in_folder(&root, 0, crate::model::SizeKind::Disk);
    let sum: f64 = files.iter().map(|file| file.percentage).sum();
    assert!(
        sum <= 1.0 + 1e-9,
        "percentages must not exceed the board, got {sum}"
    );

    let area = Area {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let mut board = Board::new(&root);
    board.change_area(&area);
    board.change_files(&root);

    for tile in &board.tiles {
        assert!(
            tile.x + tile.width <= area.width && tile.y + tile.height <= area.height,
            "tile {:?} at {},{} sized {}x{} escapes the {}x{} board",
            tile.name,
            tile.x,
            tile.y,
            tile.width,
            tile.height,
            area.width,
            area.height
        );
    }
}

/// Every cell inside the board belongs to exactly one tile, borders included, and a tile's own
/// corner finds that tile.
#[test]
fn tile_at_finds_the_one_tile_under_each_cell() {
    let mut root = Folder::new(Path::new("/tmp/example"));
    for (name, size) in [("a", 500), ("b", 300), ("c", 150), ("d", 50)] {
        root.add_file(std::path::PathBuf::from(name), size);
    }
    let mut board = Board::new(&root);
    board.change_area(&Area {
        x: 0,
        y: 1,
        width: 80,
        height: 22,
    });
    board.change_files(&root);
    assert_eq!(board.tiles.len(), 4);

    for (index, tile) in board.tiles.iter().enumerate() {
        assert_eq!(
            board.tile_at(tile.x, tile.y),
            Some(index),
            "top-left corner"
        );
        let (last_column, last_row) = (tile.x + tile.width - 1, tile.y + tile.height - 1);
        assert_eq!(
            board.tile_at(last_column, last_row),
            Some(index),
            "inner corner"
        );
    }
    for row in 1..23 {
        for column in 0..80 {
            let owners = board
                .tiles
                .iter()
                .filter(|tile| {
                    (tile.x..tile.x + tile.width).contains(&column)
                        && (tile.y..tile.y + tile.height).contains(&row)
                })
                .count();
            assert!(owners <= 1, "cell ({column}, {row}) has {owners} owners");
        }
    }
    assert_eq!(board.tile_at(0, 0), None, "the title row is not the board");
    assert_eq!(board.tile_at(200, 200), None, "off the board");
}

/// In a window's pixels an entry gets a tile once it is a few pixels either way, where a
/// terminal's 8×3-cell minimum sends it to the "small files" corner.
#[test]
fn a_pixel_grid_gives_small_entries_tiles_of_their_own() {
    let mut root = Folder::new(Path::new("/tmp/example"));
    // 120 files in 60×40 cells: 20 cells each, under the terminal's 8×3 and over 4×4.
    for index in 0..120 {
        root.add_file(std::path::PathBuf::from(format!("small{index}")), 100);
    }
    let area = Area {
        x: 0,
        y: 0,
        width: 60,
        height: 40,
    };
    let mut board = Board::new(&root);
    board.change_area(&area);
    board.change_files(&root);
    assert!(board.tiles.is_empty(), "none fits the terminal's cells");
    assert!(board.unrenderable_tile_coordinates.is_some());

    board.set_grid(Grid::pixels(4));
    // All but the last strip's, which the squarify leaves thinner.
    assert!(board.tiles.len() >= 100, "{} tiles", board.tiles.len());
    for tile in &board.tiles {
        assert!(tile.width >= 4 && tile.height >= 4, "{tile:?}");
        // Square cells: no tile is drawn out 2.5 times too wide.
        let ratio = f64::from(tile.width.max(tile.height)) / f64::from(tile.width.min(tile.height));
        assert!(ratio < 2.0, "{tile:?}");
    }
}

fn folder_of(files: &[(&str, u64)]) -> Folder {
    let mut root = Folder::new(Path::new("/tmp/example"));
    for &(name, size) in files {
        root.add_file(name.into(), size.into());
    }
    root
}

fn places(tiles: &[crate::tiles::Tile]) -> Vec<(std::ffi::OsString, u16, u16, u16, u16)> {
    tiles
        .iter()
        .map(|tile| (tile.name.clone(), tile.x, tile.y, tile.width, tile.height))
        .collect()
}

fn board_over(root: &Folder) -> Board {
    let mut board = Board::new(root);
    board.set_grid(Grid::pixels(4));
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 400,
        height: 300,
    });
    board.change_files(root);
    board
}

fn tile_named<'a>(board: &'a Board, name: &str) -> &'a crate::tiles::Tile {
    board
        .tiles
        .iter()
        .find(|tile| tile.name == name)
        .expect("a tile")
}

#[test]
fn a_steady_layout_of_the_same_sizes_is_the_fresh_one() {
    let root = folder_of(&[("a", 500), ("b", 300), ("c", 200), ("d", 120), ("e", 80)]);
    let mut board = board_over(&root);
    let fresh = places(&board.tiles);
    board.change_files_steady(&root);
    assert_eq!(places(&board.tiles), fresh);
}

#[test]
fn a_folder_overtaking_another_keeps_its_place() {
    let before = [("a", 500), ("b", 450), ("c", 200), ("d", 120), ("e", 80)];
    let after = [("a", 500), ("b", 560), ("c", 200), ("d", 120), ("e", 80)];
    let mut board = board_over(&folder_of(&before));
    let a = tile_named(&board, "a").clone();
    let b = tile_named(&board, "b").clone();
    board.change_files_steady(&folder_of(&after));
    // Where squarify afresh puts the new largest first, the steady layout leaves `a` where it
    // was: `b` grows in its own place.
    assert_eq!(
        (tile_named(&board, "a").x, tile_named(&board, "a").y),
        (a.x, a.y)
    );
    let grown = tile_named(&board, "b");
    assert!(
        u32::from(grown.width) * u32::from(grown.height) > u32::from(b.width) * u32::from(b.height)
    );
    let fresh = board_over(&folder_of(&after));
    assert_eq!(
        (tile_named(&fresh, "b").x, tile_named(&fresh, "b").y),
        (0, 0)
    );
}

#[test]
fn a_steady_layout_places_new_entries_and_overlaps_nothing() {
    let mut board = board_over(&folder_of(&[("a", 500), ("b", 300), ("c", 200)]));
    board.change_files_steady(&folder_of(&[
        ("a", 520),
        ("b", 300),
        ("c", 210),
        ("new", 40),
    ]));
    let names: Vec<_> = board.tiles.iter().map(|tile| tile.name.clone()).collect();
    assert_eq!(names.len() + board.hidden().len(), 4, "{names:?}");
    for (i, one) in board.tiles.iter().enumerate() {
        assert!(
            one.x + one.width <= 400 && one.y + one.height <= 300,
            "{one:?}"
        );
        for other in &board.tiles[i + 1..] {
            let apart = one.x + one.width <= other.x
                || other.x + other.width <= one.x
                || one.y + one.height <= other.y
                || other.y + other.height <= one.y;
            assert!(apart, "{one:?} overlaps {other:?}");
        }
    }
}

#[test]
fn a_steady_layout_gone_thin_is_laid_out_afresh() {
    let mut board = board_over(&folder_of(&[
        ("a", 100),
        ("b", 100),
        ("c", 100),
        ("d", 100),
    ]));
    let after = folder_of(&[("a", 100), ("b", 100), ("c", 100), ("d", 5000)]);
    board.change_files_steady(&after);
    assert_eq!(places(&board.tiles), places(&board_over(&after).tiles));
}

#[test]
fn empty_files_beside_a_large_one_leave_it_its_tile() {
    // Empty files are common (lock files, `.gitkeep`): their share is 0, and a row of nothing
    // but them divided 0 by 0 — a NaN place, rounded to the board's corner, which made the
    // "small files" corner the whole board and hid every tile.
    for grid in [Grid::TERMINAL, Grid::pixels(4)] {
        let root = folder_of(&[("big", 100), ("empty1", 0), ("empty2", 0)]);
        let mut board = Board::new(&root);
        board.set_grid(grid);
        board.change_area(&Area {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        board.change_files(&root);
        assert_eq!(
            places(&board.tiles).len(),
            1,
            "{grid:?}: big keeps its tile"
        );
        let (x, y) = board
            .unrenderable_tile_coordinates
            .expect("a corner for the empty files");
        assert!(
            x > 0 || y > 0,
            "{grid:?}: the corner is not the whole board"
        );
    }
}

/// A volume's free space shown beside its root's entries: a strip at the board's far side, the
/// free space at its bottom right and the unscanned space before it, each of its share; the
/// entries in the rest. No entry of the listing, no stop for the selection, and gone when the
/// board zooms or is told to show the entries alone.
/// A 400×200 pixel board of two entries at a volume's root, half of it free and a fifth of
/// it unscanned.
fn volume_board() -> (Folder, Board) {
    use crate::tiles::FreeSpace;
    let mut root = Folder::new(Path::new("/tmp/volume"));
    root.add_file(std::path::PathBuf::from("a"), 600);
    root.add_file(std::path::PathBuf::from("b"), 400);
    let mut board = Board::new(&root);
    board.set_grid(Grid::pixels(4));
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 400,
        height: 200,
    });
    board.set_free_space(Some(FreeSpace {
        bytes: 1000,
        share: 0.5,
        unscanned: 400,
        unscanned_share: 0.2,
        scanned: false,
        purgeable: 0,
    }));
    board.change_files(&root);
    (root, board)
}

fn cells(tile: &crate::tiles::Tile) -> f64 {
    f64::from(tile.width) * f64::from(tile.height)
}

#[test]
fn free_space_takes_its_share_and_is_no_entry() {
    use crate::tiles::{FREE_SPACE_NAME, UNSCANNED_NAME};
    let (root, mut board) = volume_board();

    assert_eq!(board.tiles.len(), 4);
    let free = board.free_tile().expect("the free space has a tile");
    let unscanned = board
        .unscanned_tile()
        .expect("the unscanned space has a tile");
    let (f, u) = (&board.tiles[free], &board.tiles[unscanned]);
    assert_eq!(f.name, FREE_SPACE_NAME);
    assert_eq!(u.name, UNSCANNED_NAME);
    assert_eq!(f.size, 1000);
    assert_eq!(u.size, 400);
    assert_eq!(
        (f.x, f.width, f.y + f.height),
        (0, 400, 200),
        "the free space is a band along the bottom, the board's width"
    );
    let whole = 400.0 * 200.0;
    assert!((cells(f) / whole - 0.5).abs() < 0.01, "{f:?}");
    assert!((cells(u) / whole - 0.2).abs() < 0.01, "{u:?}");
    assert_eq!((u.x + u.width, u.y, u.y + u.height), (400, 0, f.y));
    assert!(
        board.tiles[..unscanned]
            .iter()
            .all(|tile| tile.x + tile.width <= u.x && tile.y + tile.height <= f.y),
        "the entries' tiles come first, left of the unscanned strip and above the free space"
    );
    assert!(
        board
            .listing()
            .iter()
            .all(|entry| entry.name != FREE_SPACE_NAME),
        "the listing holds entries alone"
    );
    assert_eq!(
        board.tile_at(f.x, f.y),
        Some(free),
        "the tile is where it says, for a viewer to make nothing of"
    );

    // The selection never lands on it: from nothing, nor by moving.
    let free = board.free_tile().expect("the free space has a tile");
    board.reset_selected_index();
    board.move_selected_right();
    assert_ne!(board.get_selected_index(), Some(free));
    for _ in 0..8 {
        board.move_selected_right();
        assert_ne!(board.get_selected_index(), Some(free));
        board.move_selected_down();
        assert_ne!(board.get_selected_index(), Some(free));
        board.move_selected_left();
        assert_ne!(board.get_selected_index(), Some(free));
        board.move_selected_up();
        assert_ne!(board.get_selected_index(), Some(free));
    }

    board.zoom_in(&root);
    assert!(board.free_tile().is_none(), "zoomed, the entries alone");
    board.reset_zoom(&root);
    assert!(board.free_tile().is_some());
    board.set_free_space(None);
    board.change_files(&root);
    assert!(board.free_tile().is_none());
    assert_eq!(board.tiles.len(), 2);
    assert!((board.tiles.iter().map(|tile| tile.percentage).sum::<f64>() - 1.0).abs() < 1e-9);
}

/// As the scan finds more, the free space keeps its place and its area; the entries grow into
/// the unscanned space.
#[test]
fn free_space_keeps_its_place_as_the_scan_goes() {
    use crate::tiles::FreeSpace;
    let (mut root, mut board) = volume_board();
    let before = cells(&board.tiles[board.free_tile().expect("a free space")]);
    root.add_file(std::path::PathBuf::from("c"), 400);
    board.set_free_space(Some(FreeSpace {
        bytes: 1000,
        share: 0.5,
        unscanned: 0,
        unscanned_share: 0.0,
        scanned: true,
        purgeable: 0,
    }));
    board.change_files_steady(&root);
    assert!(board.unscanned_tile().is_none());
    let f = &board.tiles[board.free_tile().expect("still a free space")];
    assert_eq!((f.x + f.width, f.y + f.height), (400, 200));
    assert!((cells(f) - before).abs() / (400.0 * 200.0) < 0.01);
}

/// Before the scan has found anything the entries have no room: the free and unscanned space
/// fill the board, and no corner opens over them.
#[test]
fn free_space_before_anything_is_found() {
    use crate::tiles::FreeSpace;
    let root = Folder::new(Path::new("/tmp/volume"));
    let mut board = Board::new(&root);
    board.set_grid(Grid::pixels(4));
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 300,
        height: 400,
    });
    board.set_free_space(Some(FreeSpace {
        bytes: 300,
        share: 0.3,
        unscanned: 700,
        unscanned_share: 0.7,
        scanned: false,
        purgeable: 0,
    }));
    board.change_files(&root);
    assert_eq!(board.tiles.len(), 2);
    assert!(board.corner().is_none());
    let f = &board.tiles[board.free_tile().expect("a free space")];
    let u = &board.tiles[board.unscanned_tile().expect("an unscanned space")];
    // The free space along the bottom; the unscanned space all of the rest.
    assert_eq!((f.x, f.width, f.y + f.height), (0, 300, 400));
    assert_eq!((u.x, u.y, u.width, u.y + u.height), (0, 0, 300, f.y));
}

/// Once the scan is over, the part of the unseen space that may be snapshots and purgeable is a
/// tile of its own, beside the free space, the rest of the unseen before it; neither is an
/// entry. While the scan goes there is none.
#[test]
fn the_purgeable_part_of_the_unseen_space_is_a_tile_beside_the_free_space() {
    use crate::tiles::{FreeSpace, PURGEABLE_NAME, UNSEEN_NAME};
    let root = Folder::new(Path::new("/tmp/volume"));
    let mut board = Board::new(&root);
    board.set_grid(Grid::pixels(4));
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 300,
        height: 400,
    });
    let space = |scanned: bool| FreeSpace {
        bytes: 300,
        share: 0.3,
        unscanned: 700,
        unscanned_share: 0.7,
        scanned,
        purgeable: 200,
    };
    board.set_free_space(Some(space(true)));
    board.change_files(&root);
    let f = &board.tiles[board.free_tile().expect("a free space")];
    let u = &board.tiles[board.unscanned_tile().expect("the unseen")];
    let p = &board.tiles[board.purgeable_tile().expect("the purgeable")];
    assert_eq!((u.name.to_str(), u.size), (Some(UNSEEN_NAME), 500));
    assert_eq!((p.name.to_str(), p.size), (Some(PURGEABLE_NAME), 200));
    // Down the unscanned strip: the unseen, then the purgeable on the free space below.
    assert_eq!((u.x, u.width), (p.x, p.width));
    assert_eq!((u.y + u.height, p.y + p.height), (p.y, f.y));
    assert!((f64::from(p.height) / f64::from(u.height + p.height) - 200.0 / 700.0).abs() < 0.01);
    for index in 0..board.tiles.len() {
        assert!(!board.selectable(index), "none is an entry");
    }

    board.set_free_space(Some(space(false)));
    board.change_files(&root);
    assert!(board.purgeable_tile().is_none(), "not while the scan goes");
    assert_eq!(
        board.tiles[board.unscanned_tile().expect("unscanned")].size,
        700
    );
}

/// The free space stands still: as the scan finds more and the unscanned space shrinks, its
/// tile keeps its place, its width and its height, not only its area.
#[test]
fn the_free_space_does_not_move_or_change_shape_as_the_scan_goes() {
    use crate::tiles::FreeSpace;
    let mut root = Folder::new(Path::new("/tmp/volume"));
    let mut board = Board::new(&root);
    board.set_grid(Grid::pixels(4));
    board.change_area(&Area {
        x: 0,
        y: 0,
        width: 400,
        height: 300,
    });
    let mut seen = None;
    for (found, unscanned) in [(0u64, 700u64), (300, 400), (650, 50), (700, 0)] {
        if found > 0 {
            root.add_file(std::path::PathBuf::from(format!("f{found}")), found as u128);
        }
        board.set_free_space(Some(FreeSpace {
            bytes: 300,
            share: 0.3,
            unscanned,
            unscanned_share: unscanned as f64 / 1000.0,
            scanned: false,
            purgeable: 0,
        }));
        board.change_files_steady(&root);
        let f = &board.tiles[board.free_tile().expect("a free space")];
        let place = (f.x, f.y, f.width, f.height);
        assert_eq!(*seen.get_or_insert(place), place, "at {found} found");
    }
}

/// A board with no room — before it is given an area, or a window minimised to nothing — lays
/// out no strip and does not underflow cutting one.
#[test]
fn free_space_on_a_board_with_no_room_is_nothing() {
    use crate::tiles::FreeSpace;
    let (root, mut board) = volume_board();
    for (width, height) in [(0, 0), (400, 0), (0, 200), (1, 1)] {
        board.change_area(&Area {
            x: 0,
            y: 0,
            width,
            height,
        });
        board.set_free_space(Some(FreeSpace {
            bytes: 1000,
            share: 0.5,
            unscanned: 400,
            unscanned_share: 0.2,
            scanned: true,
            purgeable: 100,
        }));
        board.change_files(&root);
        for tile in &board.tiles {
            assert!(tile.x + tile.width <= width.max(1) && tile.y + tile.height <= height.max(1));
        }
    }
}
