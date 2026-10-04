//! Where a scan's failures are, for `duscape --issues`: every one counted by its path, the paths
//! made coarser toward the root when there are too many to keep, and the counts rolled up into
//! the folders holding them for the report.
//!
//! The examples a scan keeps are its first few hundred, and the counts by folder name say what
//! the folders are called, not where: on macOS a scan of the data volume is refused some three
//! hundred folders, a third of them past the examples and all of them spread over names. What
//! the report needs is how many under each folder, every failure counted.

use ::std::collections::BTreeMap;
use ::std::path::{Path, PathBuf};

/// The failures counted by path: the path of what failed (a folder refused, a file not read).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Places {
    counts: BTreeMap<PathBuf, u64>,
}

impl Places {
    /// Paths kept apart. Past it the deepest are counted at their parent instead, a level at a
    /// time, so the totals stay whole and only the detail goes: on a NAS, a million `@eaDir`
    /// failures end as a few thousand folders' counts.
    pub const KEPT: usize = 4096;
    /// How many lines the report gives the tree.
    const LINES: usize = 60;
    /// How many folders a folder of the tree lists before counting the rest together.
    const BRANCHES: usize = 12;

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// `count` more failures at `path`.
    pub fn add(&mut self, path: &Path, count: u64) {
        *self.counts.entry(path.to_path_buf()).or_default() += count;
        // To half, not to the limit: coarsened only to it, every new path past it would cost a
        // pass over all the others.
        if self.counts.len() > Self::KEPT {
            while self.counts.len() > Self::KEPT / 2 && self.coarsen() {}
        }
    }

    /// Another's failures added to these.
    pub fn merge(&mut self, other: Places) {
        for (path, count) in other.counts {
            self.add(&path, count);
        }
    }

    /// The deepest paths counted at their parents; `false` when every path is a root's and
    /// none can be.
    fn coarsen(&mut self) -> bool {
        let deepest = self
            .counts
            .keys()
            .map(|path| path.components().count())
            .max()
            .unwrap_or(0);
        if deepest <= 1 {
            return false;
        }
        let counts = ::std::mem::take(&mut self.counts);
        for (path, count) in counts {
            let path = if path.components().count() == deepest {
                path.parent().map_or(path.clone(), Path::to_path_buf)
            } else {
                path
            };
            *self.counts.entry(path).or_default() += count;
        }
        true
    }

    /// The folder every path is under: the scan's root, near enough.
    fn common_root(&self) -> PathBuf {
        let mut paths = self.counts.keys();
        let Some(first) = paths.next() else {
            return PathBuf::new();
        };
        let mut root: Vec<_> = first.components().collect();
        for path in paths {
            let shared = root
                .iter()
                .zip(path.components())
                .take_while(|(a, b)| **a == *b)
                .count();
            root.truncate(shared);
        }
        // One path alone, or every path one: the folder holding it, so it has a line.
        if self.counts.len() == 1 || root.len() == first.components().count() {
            root.pop();
        }
        root.into_iter().collect()
    }

    /// The tree under the common root, in an arena, the root first: each folder's failures,
    /// its own and those beneath it, its folders most first.
    fn tree(&self) -> (PathBuf, Vec<Node>) {
        let root = self.common_root();
        let mut nodes = vec![Node::default()];
        for (path, count) in &self.counts {
            let below = path.strip_prefix(&root).unwrap_or(path);
            nodes[0].total += count;
            let mut at = 0;
            for part in below.components() {
                let name = part.as_os_str().to_string_lossy().into_owned();
                let next = match nodes[at]
                    .children
                    .iter()
                    .find(|&&child| nodes[child].name == name)
                {
                    Some(&child) => child,
                    None => {
                        nodes.push(Node {
                            name,
                            ..Node::default()
                        });
                        let child = nodes.len() - 1;
                        nodes[at].children.push(child);
                        child
                    }
                };
                nodes[next].total += count;
                at = next;
            }
        }
        let totals: Vec<(u64, String)> = nodes.iter().map(|n| (n.total, n.name.clone())).collect();
        for node in &mut nodes {
            node.children.sort_by(|&a, &b| {
                totals[b]
                    .0
                    .cmp(&totals[a].0)
                    .then_with(|| totals[a].1.cmp(&totals[b].1))
            });
        }
        (root, nodes)
    }

    /// The report's lines: under the common root, its folders by how many failed in each, most
    /// first, in at most [`Self::LINES`]. The lines go to the folders holding the most, wherever
    /// they are — opened largest first, not depth first, so one deep corner cannot take them
    /// all — and a folder holding one folder alone shares its line (`private/var/`).
    #[must_use]
    pub fn report(&self) -> String {
        use ::std::fmt::Write;
        let mut out = String::new();
        if self.is_empty() {
            return out;
        }
        let (root, nodes) = self.tree();
        let mut open = vec![false; nodes.len()];
        let mut lines = 1;
        // Opening a folder costs its lines: one a folder shown, one for the rest; nothing for a
        // folder of one, which is drawn on its parent's line.
        let cost = |node: &Node| match node.children.len() {
            0 => 0,
            1 => 0,
            n if n > Self::BRANCHES => Self::BRANCHES + 1,
            n => n,
        };
        let mut waiting = ::std::collections::BinaryHeap::new();
        waiting.push((nodes[0].total, ::std::cmp::Reverse(0usize)));
        while let Some((_, ::std::cmp::Reverse(at))) = waiting.pop() {
            let node = &nodes[at];
            // One failure is its own line already: nothing below it to tell.
            if node.children.is_empty() || (at != 0 && node.total < 2) {
                continue;
            }
            if lines + cost(node) > Self::LINES {
                continue;
            }
            lines += cost(node);
            open[at] = true;
            for &child in node.children.iter().take(Self::BRANCHES) {
                waiting.push((nodes[child].total, ::std::cmp::Reverse(child)));
            }
        }
        let _ = writeln!(out, "  {:>10}  {}", nodes[0].total, root.display());
        write(&nodes, &open, 0, 1, &mut out);
        out
    }
}

/// A folder of the report's tree.
#[derive(Debug, Default)]
struct Node {
    name: String,
    total: u64,
    /// By index in the arena, most failures first.
    children: Vec<usize>,
}

/// The lines for `at`'s folders, opened ones followed down.
fn write(nodes: &[Node], open: &[bool], at: usize, depth: usize, out: &mut String) {
    use ::std::fmt::Write;
    if !open[at] {
        return;
    }
    let indent = "  ".repeat(depth);
    let children = &nodes[at].children;
    for &child in children.iter().take(Places::BRANCHES) {
        // A chain of folders of one each, on one line, joined and ended (a folder with more
        // under it) by the platform's separator, as the root line's path is written: on
        // Windows `\Data` then `private/var/db/` read as two kinds of path.
        let mut name = nodes[child].name.clone();
        let mut end = child;
        while open[end] && nodes[end].children.len() == 1 {
            end = nodes[end].children[0];
            name.push(::std::path::MAIN_SEPARATOR);
            name.push_str(&nodes[end].name);
        }
        let slash = if nodes[end].children.is_empty() {
            ""
        } else {
            ::std::path::MAIN_SEPARATOR_STR
        };
        let _ = writeln!(out, "  {:>10}  {indent}{name}{slash}", nodes[child].total);
        write(nodes, open, end, depth + 1, out);
    }
    if children.len() > Places::BRANCHES {
        let rest = &children[Places::BRANCHES..];
        let count: u64 = rest.iter().map(|&child| nodes[child].total).sum();
        let folders = if rest.len() == 1 { "other" } else { "others" };
        let _ = writeln!(out, "  {count:>10}  {indent}… in {} {folders}", rest.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `report`'s text with the platform's separator, as `report` writes it.
    fn native(text: &str) -> String {
        text.replace('/', ::std::path::MAIN_SEPARATOR_STR)
    }

    #[test]
    fn failures_are_rolled_up_into_the_folders_holding_them() {
        let mut places = Places::default();
        for name in ["Mail", "Messages", "Safari"] {
            places.add(&Path::new("/Data/Users/me/Library").join(name), 1);
        }
        for index in 0..20 {
            places.add(
                &Path::new("/Data/private/var/db").join(format!("d{index}")),
                1,
            );
        }
        places.add(Path::new("/Data/.Trash"), 1);
        let report = places.report();
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines[0], native("          24  /Data"), "{report}");
        // A folder of one folder alone shares its line.
        assert_eq!(
            lines[1],
            native("          20    private/var/db/"),
            "{report}"
        );
        assert!(
            report.contains(&native(
                "           3    Users/me/Library/\n           1      Mail\n"
            )),
            "{report}"
        );
        assert!(report.contains("           1    .Trash\n"), "{report}");
        // Twenty under db: twelve named, the rest counted together.
        assert!(report.contains("… in 8 others"), "{report}");
    }

    #[test]
    fn too_many_places_are_counted_at_their_parents() {
        let mut places = Places::default();
        for share in 0..3 {
            for index in 0..2000 {
                places.add(
                    &Path::new("/volume1")
                        .join(format!("share{share}"))
                        .join(format!("dir{index}"))
                        .join("@eaDir"),
                    1,
                );
            }
        }
        assert!(places.counts.len() <= Places::KEPT);
        assert_eq!(
            places.counts.values().sum::<u64>(),
            6000,
            "every one counted"
        );
        let report = places.report();
        assert!(
            report.starts_with(&native("        6000  /volume1\n")),
            "{report}"
        );
        assert!(report.contains("        2000    share0\n"), "{report}");
    }

    #[test]
    fn one_place_is_shown_under_its_folder() {
        let mut places = Places::default();
        places.add(Path::new("/Data/.Trash"), 1);
        assert_eq!(
            places.report(),
            native("           1  /Data\n           1    .Trash\n")
        );
    }
}
