//! The "small files" corner, filled in: the entries too small for a tile of their own laid out
//! again inside the corner, in pixels, down to one pixel each. It is a picture of what is there,
//! no more — the motes carry no names and are no targets — so a folder of eighty thousand small
//! files shows as that many specks rather than a grey box, at the cost of one squarify over them.

use super::treemap::{Lay, Space};
use super::{Area, Grid};

/// One hidden entry's speck in the corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mote {
    pub area: Area,
    /// Which of the entries it is, by its index in what [`scatter`] was given.
    pub entry: usize,
}

/// No least tile: squarify at the specks' own proportions, and what rounds to nothing is left
/// out. With a least tile of one pixel, a row passes only if it is exactly a pixel thick, and
/// the arithmetic's last digit decides it.
const SPECKS: Grid = Grid {
    ratio: 1.0,
    min_width: 0,
    min_height: 0,
    small_files_width: 0,
    small_files_height: 0,
};

/// Entries by their `shares` of their folder — largest first, as the board lists them — laid
/// out in `corner` in one-pixel cells. As many as the corner has pixels are laid out, the largest, in proportion to each
/// other: so that as many as can be seen are, rather than all of them in proportion and most
/// under a pixel. Any that still round to nothing leave the corner's own colour showing.
///
/// The treemap's squarify, straight into motes: with no least tile none is hidden, and a
/// named [`super::Tile`] made of each first was half the time of a corner of tens of thousands.
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
    let shares: Vec<f64> = shares.iter().map(|&share| share / total).collect();
    let mut motes = Motes(Vec::with_capacity(shares.len()));
    Space::new(corner, SPECKS).squarify(&shares, &mut motes);
    motes.0
}

/// A corner's rows as motes, less those rounded to nothing.
struct Motes(Vec<Mote>);

impl Lay for Motes {
    fn tile(&mut self, entry: usize, area: Area) {
        if area.width > 0 && area.height > 0 {
            self.0.push(Mote { area, entry });
        }
    }
    // With no least tile none is too small.
    fn hidden(&mut self, _: usize, _: Area) {}
    fn row(&mut self, _: bool) {}
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsString;

    use super::{Mote, SPECKS, scatter};
    use crate::tiles::{Area, FileMetadata, FileType, TreeMap};

    /// What `scatter` was before it laid the motes straight: a treemap of stand-in entries.
    fn by_treemap(shares: &[f64], corner: &Area) -> Vec<Mote> {
        let room = usize::from(corner.width) * usize::from(corner.height);
        let sized = shares.partition_point(|&share| share > 0.0);
        let shares = &shares[..sized.min(room)];
        let total: f64 = shares.iter().sum();
        if shares.is_empty() || total <= 0.0 {
            return Vec::new();
        }
        let files: Vec<FileMetadata> = shares
            .iter()
            .map(|&share| FileMetadata {
                name: OsString::new(),
                size: 0,
                descendants: None,
                percentage: share / total,
                file_type: FileType::File,
            })
            .collect();
        let mut map = TreeMap::with_grid(corner, SPECKS);
        map.populate_tiles(files.iter().collect());
        map.tiles
            .iter()
            .zip(map.tile_entries())
            .filter(|(tile, _)| tile.width > 0 && tile.height > 0)
            .map(|(tile, &entry)| Mote {
                area: tile.area(),
                entry,
            })
            .collect()
    }

    #[test]
    fn the_motes_are_where_the_treemap_would_put_them() {
        // A small xorshift: sizes over five orders of magnitude, largest first, some empty.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..200 {
            let count = 1 + (next() % 3000) as usize;
            let mut shares: Vec<f64> = (0..count)
                .map(|_| {
                    let bits = next();
                    if bits % 17 == 0 {
                        0.0
                    } else {
                        10f64.powf(-(((bits >> 8) % 5000) as f64) / 1000.0)
                    }
                })
                .collect();
            if case % 5 == 0 {
                shares.iter_mut().for_each(|share| *share = 0.001);
            }
            shares.sort_by(|a, b| b.total_cmp(a));
            let corner = Area {
                x: (next() % 500) as u16,
                y: (next() % 500) as u16,
                width: 1 + (next() % 300) as u16,
                height: 1 + (next() % 200) as u16,
            };
            assert_eq!(
                scatter(&shares, &corner),
                by_treemap(&shares, &corner),
                "case {case}: {count} in {corner:?}"
            );
        }
    }

    #[test]
    fn every_entry_with_a_pixel_to_its_name_gets_one_inside_the_corner() {
        // A thousand equal entries, 1% of the folder between them, in 50×40 pixels: two each.
        let entries: Vec<f64> = (0..1000).map(|_| 0.00001).collect();
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
        let entries: Vec<f64> = (0..5000).map(|_| 0.0001).collect();
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
        let mut with_empty: Vec<f64> = (0..10).map(|_| 0.01).collect();
        with_empty.extend((0..10).map(|_| 0.0));
        let motes = scatter(&with_empty, &corner);
        assert!(!motes.is_empty() && motes.iter().all(|mote| mote.entry < 10));
        assert!(scatter(&[], &corner).is_empty());
    }
}
