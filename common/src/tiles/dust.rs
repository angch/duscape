//! The "small files" corner, filled in: the entries too small for a tile of their own laid out
//! again inside the corner, in pixels, down to one pixel each. It is a picture of what is there,
//! no more — the motes carry no names and are no targets — so a folder of eighty thousand small
//! files shows as that many specks rather than a grey box, at the cost of one squarify over them.

use ::std::ffi::OsString;

use super::{Area, FileMetadata, FileType, Grid, TreeMap};

/// One hidden entry's speck in the corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mote {
    pub area: Area,
    /// Which of the entries it is, by its index in what [`scatter`] was given.
    pub entry: usize,
}

/// Entries by their `shares` of their folder — largest first, as the board lists them — laid
/// out in `corner` in one-pixel cells. As many as the corner has pixels are laid out, the largest, in proportion to each
/// other: so that as many as can be seen are, rather than all of them in proportion and most
/// under a pixel. Any that still round to nothing leave the corner's own colour showing.
#[must_use]
pub fn scatter(shares: &[f64], corner: &Area) -> Vec<Mote> {
    let room = usize::from(corner.width) * usize::from(corner.height);
    // Empty files come last and would take no room: a row of them measures 0/0.
    let sized = shares.partition_point(|&share| share > 0.0);
    let shares = &shares[..sized.min(room)];
    let total: f64 = shares.iter().sum();
    if shares.is_empty() || total <= 0.0 {
        return Vec::new();
    }
    // Stand-ins with no name to copy, and their index where the size would be: the layout
    // needs only the shares, and gives the index back on each tile.
    let shares: Vec<FileMetadata> = shares
        .iter()
        .enumerate()
        .map(|(index, &share)| FileMetadata {
            name: OsString::new(),
            size: index as u128,
            descendants: None,
            percentage: share / total,
            file_type: FileType::File,
        })
        .collect();
    // No least tile: squarify at its own proportions, and what rounds to nothing is left out.
    // With a least tile of one pixel, a row passes only if it is exactly a pixel thick, and
    // the arithmetic's last digit decides it.
    let grid = Grid {
        ratio: 1.0,
        min_width: 0,
        min_height: 0,
        small_files_width: 0,
        small_files_height: 0,
    };
    let mut map = TreeMap::with_grid(corner, grid);
    map.populate_tiles(shares.iter().collect());
    map.tiles
        .iter()
        .filter(|tile| tile.width > 0 && tile.height > 0)
        .map(|tile| Mote {
            area: Area {
                x: tile.x,
                y: tile.y,
                width: tile.width,
                height: tile.height,
            },
            entry: tile.size as usize,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Mote, scatter};
    use crate::tiles::Area;

    fn entry(percentage: f64) -> f64 {
        percentage
    }

    #[test]
    fn every_entry_with_a_pixel_to_its_name_gets_one_inside_the_corner() {
        // A thousand equal entries, 1% of the folder between them, in 50×40 pixels: two each.
        let entries: Vec<f64> = (0..1000).map(|_| entry(0.00001)).collect();
        let refs = &entries;
        let corner = Area {
            x: 100,
            y: 60,
            width: 50,
            height: 40,
        };
        let motes = scatter(refs, &corner);
        assert!(motes.len() > 900, "{} motes", motes.len());
        let mut seen = vec![false; entries.len()];
        for Mote { area, entry } in &motes {
            assert!(area.x >= 100 && area.x + area.width <= 150, "{area:?}");
            assert!(area.y >= 60 && area.y + area.height <= 100, "{area:?}");
            assert!(area.width >= 1 && area.height >= 1);
            assert!(!seen[*entry], "entry {entry} twice");
            seen[*entry] = true;
        }
    }

    #[test]
    fn more_entries_than_pixels_lays_out_no_more_than_the_pixels() {
        let entries: Vec<f64> = (0..5000).map(|_| entry(0.0001)).collect();
        let refs = &entries;
        let corner = Area {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        };
        let motes = scatter(refs, &corner);
        // The first 200 share the 200 pixels: most of them get one.
        assert!(motes.len() > 150 && motes.len() <= 200, "{}", motes.len());
        assert!(motes.iter().all(|mote| mote.entry < 200));
        assert!(scatter(refs, &Area::default()).is_empty());
        // Empty files take no room and are left out, whatever is around them.
        let mut with_empty: Vec<f64> = (0..10).map(|_| entry(0.01)).collect();
        with_empty.extend((0..10).map(|_| entry(0.0)));
        let motes = scatter(&with_empty, &corner);
        assert!(!motes.is_empty() && motes.iter().all(|mote| mote.entry < 10));
        assert!(scatter(&[], &corner).is_empty());
    }
}
