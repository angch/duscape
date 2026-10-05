//! The marked rows, in the order marked, each its path from the listed folder — with a set
//! beside the order, so asking whether a row or tile is marked costs a lookup and not a pass
//! over the marks. Every painter asks it of every row and tile on screen, and after Ctrl+A on
//! a folder of 87k files a pass each was billions of comparisons a frame.

use ::std::collections::HashSet;
use ::std::ffi::{OsStr, OsString};

#[derive(Debug, Default, Clone)]
pub struct Marks {
    order: Vec<Vec<OsString>>,
    set: HashSet<Vec<OsString>>,
    /// The folder's own entries marked, by name: a tile asks by its name, with no path made.
    top: HashSet<OsString>,
    /// How many marks are deeper than the folder's own entries: with none, no mark can be
    /// inside another, and [`Marks::outermost`] is all of them.
    deep: usize,
    /// Counts changes, for what is worked out from the marks and kept (the size they weigh).
    generation: u64,
}

impl Marks {
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// In the order marked.
    pub fn iter(&self) -> ::std::slice::Iter<'_, Vec<OsString>> {
        self.order.iter()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Vec<OsString>] {
        &self.order
    }

    #[must_use]
    pub fn contains(&self, path: &[OsString]) -> bool {
        self.set.contains(path)
    }

    /// Whether the folder's own entry `name` is marked.
    #[must_use]
    pub fn contains_name(&self, name: &OsStr) -> bool {
        self.top.contains(name)
    }

    /// Whether any mark is deeper than the folder's own entries: a row of a folder opened in
    /// place, or a nested tile.
    #[must_use]
    pub fn has_deep(&self) -> bool {
        self.deep > 0
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The marks with none of their folders marked too, in the order marked: a folder marked
    /// with rows inside it is the folder, for a delete (its rows would be gone before their
    /// turn), a count and a size.
    pub fn outermost(&self) -> impl Iterator<Item = &Vec<OsString>> {
        let deep = self.deep;
        self.order.iter().filter(move |path| {
            deep == 0 || !(1..path.len()).any(|len| self.set.contains(&path[..len]))
        })
    }

    /// Whether `path` is marked and no folder above it is.
    #[must_use]
    pub fn is_outermost(&self, path: &[OsString]) -> bool {
        self.contains(path)
            && (self.deep == 0 || !(1..path.len()).any(|len| self.set.contains(&path[..len])))
    }

    /// Mark `path`, after the others; nothing if it is marked already.
    pub(crate) fn push(&mut self, path: Vec<OsString>) {
        if self.set.insert(path.clone()) {
            self.deep += usize::from(path.len() > 1);
            if let [name] = path.as_slice() {
                self.top.insert(name.clone());
            }
            self.order.push(path);
            self.generation += 1;
        }
    }

    /// Take the mark off `path`. Whether it was marked.
    pub(crate) fn remove(&mut self, path: &[OsString]) -> bool {
        if !self.set.remove(path) {
            return false;
        }
        self.order.retain(|marked| marked.as_slice() != path);
        self.deep -= usize::from(path.len() > 1);
        if let [name] = path {
            self.top.remove(name);
        }
        self.generation += 1;
        true
    }

    pub(crate) fn clear(&mut self) {
        if !self.order.is_empty() {
            self.order.clear();
            self.set.clear();
            self.top.clear();
            self.deep = 0;
            self.generation += 1;
        }
    }

    /// Keep the marks `keep` says to.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&[OsString]) -> bool) {
        let before = self.order.len();
        let (set, top) = (&mut self.set, &mut self.top);
        self.order.retain(|path| {
            let kept = keep(path);
            if !kept {
                set.remove(path);
                if let [name] = path.as_slice() {
                    top.remove(name);
                }
            }
            kept
        });
        if self.order.len() != before {
            self.deep = self.order.iter().filter(|path| path.len() > 1).count();
            self.generation += 1;
        }
    }

    /// The marks `paths` says, in its order, each once.
    pub(crate) fn replace(&mut self, paths: impl IntoIterator<Item = Vec<OsString>>) {
        self.clear();
        for path in paths {
            self.push(path);
        }
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::Marks;
    use ::std::ffi::OsString;

    fn path(names: &[&str]) -> Vec<OsString> {
        names.iter().map(OsString::from).collect()
    }

    #[test]
    fn marks_keep_their_order_once_each_and_know_what_is_inside_what() {
        let mut marks = Marks::default();
        marks.push(path(&["a", "x"]));
        marks.push(path(&["b"]));
        marks.push(path(&["a"]));
        marks.push(path(&["b"]));
        assert_eq!(marks.len(), 3, "b once");
        assert!(marks.contains(&path(&["a", "x"])));
        assert!(marks.contains_name(OsString::from("b").as_os_str()));
        assert!(
            !marks.contains_name(OsString::from("x").as_os_str()),
            "only top-level names"
        );
        let outer: Vec<_> = marks.outermost().cloned().collect();
        assert_eq!(outer, [path(&["b"]), path(&["a"])], "a/x is inside a");
        assert!(!marks.is_outermost(&path(&["a", "x"])));
        assert!(marks.remove(&path(&["a"])));
        assert_eq!(marks.outermost().count(), 2, "a/x stands alone again");
        marks.retain(|marked| marked.len() == 1);
        assert_eq!(marks.as_slice(), [path(&["b"])]);
        let generation = marks.generation();
        marks.clear();
        assert!(marks.is_empty() && marks.generation() > generation);
    }
}
