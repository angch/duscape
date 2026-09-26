//! `dos-tiles X Y W H size...`: the tiles `libduscape::tiles::TreeMap` lays out in that area
//! for entries of those sizes (largest first, as the caller gives them), one per line as
//! `index x y w h`, then `small x y` or `small none`.

use libduscape::tiles::{Area, FileMetadata, FileType, TreeMap};
use std::ffi::OsString;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |i: usize| -> u16 { args[i].parse().expect("a number") };
    let area = Area {
        x: number(0),
        y: number(1),
        width: number(2),
        height: number(3),
    };
    let sizes: Vec<u128> = args[4..].iter().map(|s| s.parse().expect("a size")).collect();
    let total: u128 = sizes.iter().sum();
    let files: Vec<FileMetadata> = sizes
        .iter()
        .enumerate()
        .map(|(index, &size)| FileMetadata {
            name: OsString::from(index.to_string()),
            size,
            descendants: None,
            // files_in_folder's rule: equal shares when nothing has a size
            percentage: if total == 0 {
                1.0 / sizes.len() as f64
            } else {
                size as f64 / total as f64
            },
            file_type: FileType::File,
        })
        .collect();
    let mut treemap = TreeMap::new(&area);
    treemap.populate_tiles(files.iter().collect());
    for tile in &treemap.tiles {
        println!(
            "{} {} {} {} {}",
            tile.name.to_string_lossy(),
            tile.x,
            tile.y,
            tile.width,
            tile.height
        );
    }
    match treemap.unrenderable_tile_coordinates {
        Some((x, y)) => println!("small {x} {y}"),
        None => println!("small none"),
    }
}
