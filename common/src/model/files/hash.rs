//! A fast hasher for the model's maps.
//!
//! The folder tree keys millions of entries by name and the hard-link ledger keys by inode, and
//! `SipHash` was a noticeable share of tree-building time. This hasher consumes a word at a time in
//! the style of `FxHash`, with a final mix so that both the high bits (`hashbrown`'s tag byte) and
//! the low bits (its bucket index) are well spread.
//!
//! The per-word step is invertible, so with a known starting state anyone able to create files in
//! a scanned directory could choose names that all land in one bucket and make that folder's map
//! quadratic. Every hasher therefore starts from a seed drawn once per process, which is enough to
//! make such names impossible to compute in advance.

use ::std::collections::{HashMap, HashSet};
use ::std::hash::{BuildHasher, Hasher, RandomState};
use ::std::sync::OnceLock;

#[derive(Clone, Copy)]
pub struct FastHasher(u64);

/// Builds [`FastHasher`]s from a per-process random seed.
#[derive(Clone, Copy)]
pub struct FastBuildHasher {
    seed: u64,
}

impl Default for FastBuildHasher {
    fn default() -> Self {
        static SEED: OnceLock<u64> = OnceLock::new();
        let seed = *SEED.get_or_init(|| RandomState::new().build_hasher().finish());
        Self { seed }
    }
}

impl BuildHasher for FastBuildHasher {
    type Hasher = FastHasher;
    #[inline]
    fn build_hasher(&self) -> FastHasher {
        FastHasher(self.seed)
    }
}

const MULTIPLY: u64 = 0x517c_c1b7_2722_0a95;

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(26) ^ word).wrapping_mul(MULTIPLY);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 32;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^ (x >> 29)
    }

    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while let Some((word, rest)) = bytes.split_first_chunk::<8>() {
            self.add(u64::from_le_bytes(*word));
            bytes = rest;
        }
        if let Some((word, rest)) = bytes.split_first_chunk::<4>() {
            self.add(u64::from(u32::from_le_bytes(*word)));
            bytes = rest;
        }
        for &byte in bytes {
            self.add(u64::from(byte));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
}

/// A `HashMap` using [`FastHasher`].
pub type FastMap<K, V> = HashMap<K, V, FastBuildHasher>;

/// A `HashSet` using [`FastHasher`].
pub type FastSet<T> = HashSet<T, FastBuildHasher>;
