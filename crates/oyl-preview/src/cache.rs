//! A memory budget for page bitmaps: least recently seen goes first.
//!
//! The cache holds at most one bitmap per page index. Pages on screen (and
//! their one-page margin) are protected from eviction, so the budget can be
//! exceeded while a very large zoom needs more than it allows; it is
//! restored as soon as those pages scroll away.

use std::collections::HashMap;
use std::ops::Range;

/// The default budget: about 300 MB of pixels.
pub const DEFAULT_BUDGET_BYTES: usize = 300 * 1024 * 1024;

#[derive(Debug)]
struct Entry<T> {
    value: T,
    bytes: usize,
    last_used: u64,
}

/// Page bitmaps by page index, with a byte budget and LRU eviction.
#[derive(Debug)]
pub struct BitmapCache<T> {
    entries: HashMap<usize, Entry<T>>,
    used: usize,
    budget: usize,
    clock: u64,
}

impl<T> BitmapCache<T> {
    /// An empty cache that keeps at most `budget` bytes (outside protected pages).
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            used: 0,
            budget,
            clock: 0,
        }
    }

    /// The byte budget.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Changes the byte budget; takes effect at the next [`Self::evict`].
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    /// Bytes held right now.
    pub fn used_bytes(&self) -> usize {
        self.used
    }

    /// Number of pages with a bitmap.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no page has a bitmap.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bitmap of a page.
    pub fn get(&self, page: usize) -> Option<&T> {
        self.entries.get(&page).map(|e| &e.value)
    }

    /// Marks the bitmaps of `pages` as just seen.
    pub fn touch(&mut self, pages: Range<usize>) {
        self.clock += 1;
        for page in pages {
            if let Some(entry) = self.entries.get_mut(&page) {
                entry.last_used = self.clock;
            }
        }
    }

    /// Stores a page's bitmap and returns the one it replaces.
    pub fn insert(&mut self, page: usize, value: T, bytes: usize) -> Option<T> {
        self.clock += 1;
        let replaced = self.remove(page);
        self.used += bytes;
        self.entries.insert(
            page,
            Entry {
                value,
                bytes,
                last_used: self.clock,
            },
        );
        replaced
    }

    /// Takes a page's bitmap out.
    pub fn remove(&mut self, page: usize) -> Option<T> {
        let entry = self.entries.remove(&page)?;
        self.used -= entry.bytes;
        Some(entry.value)
    }

    /// Takes out the bitmaps of every page at index `len` and above (the
    /// document got shorter).
    pub fn truncate(&mut self, len: usize) -> Vec<T> {
        let gone: Vec<usize> = self.entries.keys().copied().filter(|&p| p >= len).collect();
        gone.into_iter().filter_map(|p| self.remove(p)).collect()
    }

    /// Evicts least recently seen bitmaps outside `protected` until the
    /// cache is within budget, and returns them.
    pub fn evict(&mut self, protected: Range<usize>) -> Vec<T> {
        if self.used <= self.budget {
            return Vec::new();
        }
        let mut candidates: Vec<(u64, usize)> = self
            .entries
            .iter()
            .filter(|(page, _)| !protected.contains(page))
            .map(|(&page, entry)| (entry.last_used, page))
            .collect();
        candidates.sort_unstable();
        let mut evicted = Vec::new();
        for (_, page) in candidates {
            if self.used <= self.budget {
                break;
            }
            if let Some(value) = self.remove(page) {
                evicted.push(value);
            }
        }
        evicted
    }

    /// Takes every bitmap out.
    pub fn drain(&mut self) -> Vec<T> {
        self.used = 0;
        self.entries.drain().map(|(_, e)| e.value).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_replace_and_remove_track_bytes() {
        let mut cache = BitmapCache::new(1000);
        assert_eq!(cache.insert(0, "a", 100), None);
        assert_eq!(cache.insert(1, "b", 200), None);
        assert_eq!(cache.used_bytes(), 300);
        assert_eq!(cache.insert(0, "a2", 50), Some("a"));
        assert_eq!(cache.used_bytes(), 250);
        assert_eq!(cache.get(0), Some(&"a2"));
        assert_eq!(cache.remove(1), Some("b"));
        assert_eq!(cache.used_bytes(), 50);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn eviction_removes_least_recently_seen_first() {
        let mut cache = BitmapCache::new(300);
        for page in 0..5 {
            cache.insert(page, page, 100);
        }
        // Page 0 was seen again; 1 and 2 are now the oldest.
        cache.touch(0..1);
        let mut evicted = cache.evict(10..10);
        evicted.sort();
        assert_eq!(evicted, vec![1, 2]);
        assert_eq!(cache.used_bytes(), 300);
        assert!(cache.get(0).is_some());
    }

    #[test]
    fn protected_pages_survive_even_over_budget() {
        let mut cache = BitmapCache::new(100);
        cache.insert(3, "big", 500);
        cache.insert(4, "big too", 500);
        cache.insert(9, "far", 100);
        let evicted = cache.evict(3..5);
        assert_eq!(evicted, vec!["far"]);
        assert_eq!(cache.used_bytes(), 1000);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn within_budget_nothing_is_evicted() {
        let mut cache = BitmapCache::new(1000);
        cache.insert(0, (), 400);
        cache.insert(1, (), 400);
        assert!(cache.evict(0..0).is_empty());
        cache.set_budget(500);
        assert_eq!(cache.evict(1..2).len(), 1);
        assert_eq!(cache.budget(), 500);
    }

    #[test]
    fn truncate_and_drain_release_everything_they_take() {
        let mut cache = BitmapCache::new(1000);
        for page in 0..6 {
            cache.insert(page, page, 10);
        }
        let mut gone = cache.truncate(4);
        gone.sort();
        assert_eq!(gone, vec![4, 5]);
        assert_eq!(cache.used_bytes(), 40);
        assert_eq!(cache.drain().len(), 4);
        assert_eq!(cache.used_bytes(), 0);
        assert!(cache.is_empty());
    }
}
