//! Thumbnails kept in memory, least recently used out first.

use std::collections::BTreeMap;

/// A key of the cache: the identity of the source and the bucket time.
pub type TileKey = (u32, i64);

/// How many entries on each side a search for a stand-in looks at.
const NEAREST_LOOK: usize = 16;

/// A bounded map of tiles. A value is dropped on eviction, which frees an egui texture.
pub struct TileCache<T> {
    entries: BTreeMap<TileKey, (u64, T)>,
    capacity: usize,
    clock: u64,
}

impl<T> TileCache<T> {
    pub fn new(capacity: usize) -> Self {
        Self { entries: BTreeMap::new(), capacity: capacity.max(1), clock: 0 }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Marks `key` as just used and returns its value.
    pub fn touch(&mut self, key: TileKey) -> Option<&mut T> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(&key).map(|(used, value)| {
            *used = clock;
            value
        })
    }

    /// The value of `key`, without counting as a use.
    pub fn peek_mut(&mut self, key: TileKey) -> Option<&mut T> {
        self.entries.get_mut(&key).map(|(_, value)| value)
    }

    /// Adds or replaces a value, evicting the least recently used entry when full.
    pub fn insert(&mut self, key: TileKey, value: T) {
        self.clock += 1;
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            if let Some(oldest) = self.entries.iter().min_by_key(|(_, (used, _))| *used).map(|(key, _)| *key) {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (self.clock, value));
    }

    /// The key nearest to `key` of the same source within `reach` ticks whose value `usable` accepts.
    /// Looks at no more than [`NEAREST_LOOK`] entries each way, so a tile costs the same at every zoom.
    pub fn nearest(&self, key: TileKey, reach: i64, usable: impl Fn(&T) -> bool) -> Option<TileKey> {
        let (source, time) = key;
        let below = self.entries.range((source, time.saturating_sub(reach))..=key).rev().take(NEAREST_LOOK).find(|(_, (_, value))| usable(value));
        let above = self.entries.range(key..=(source, time.saturating_add(reach))).take(NEAREST_LOOK).find(|(_, (_, value))| usable(value));
        [below, above].into_iter().flatten().min_by_key(|(candidate, _)| (candidate.1 - time).abs()).map(|(candidate, _)| *candidate)
    }

    /// Forgets the tiles of `source` that `doomed` picks.
    pub fn remove_where(&mut self, source: u32, doomed: impl Fn(&T) -> bool) {
        self.entries.retain(|(entry, _), (_, value)| *entry != source || !doomed(value));
    }

    /// Forgets every tile of `source`.
    pub fn remove_source(&mut self, source: u32) {
        self.entries.retain(|(entry, _), _| *entry != source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_used_goes_first() {
        let mut cache = TileCache::new(2);
        cache.insert((1, 0), "a");
        cache.insert((1, 10), "b");
        cache.touch((1, 0));
        cache.insert((1, 20), "c");
        assert!(cache.contains((1, 0)), "it was used last");
        assert!(!cache.contains((1, 10)));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn replacing_a_key_does_not_evict() {
        let mut cache = TileCache::new(1);
        cache.insert((1, 0), "a");
        cache.insert((1, 0), "b");
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.touch((1, 0)).copied(), Some("b"));
    }

    #[test]
    fn an_evicted_value_is_dropped() {
        let token = std::sync::Arc::new(());
        let mut cache = TileCache::new(1);
        cache.insert((1, 0), token.clone());
        assert_eq!(std::sync::Arc::strong_count(&token), 2);
        cache.insert((1, 5), std::sync::Arc::new(()));
        assert_eq!(std::sync::Arc::strong_count(&token), 1);
    }

    #[test]
    fn the_nearest_tile_is_of_the_same_source_and_within_reach() {
        let mut cache = TileCache::new(9);
        cache.insert((1, 100), "a");
        cache.insert((1, 300), "b");
        cache.insert((2, 150), "other");
        assert_eq!(cache.nearest((1, 140), 50, |_| true), Some((1, 100)));
        assert_eq!(cache.nearest((1, 180), 50, |_| true), None);
        assert_eq!(cache.nearest((1, 260), 1000, |_| true), Some((1, 300)));
        assert_eq!(cache.nearest((1, 260), 1000, |value| *value == "a"), Some((1, 100)), "only usable values");
    }

    #[test]
    fn looking_for_a_neighbour_stays_cheap_among_many_tiles() {
        let mut cache = TileCache::new(1024);
        for time in 0..1000 {
            cache.insert((1, time * 10), ());
        }
        let looked_at = std::cell::Cell::new(0);
        let found = cache.nearest((1, 5000), 100_000, |_| {
            looked_at.set(looked_at.get() + 1);
            false
        });
        assert_eq!(found, None);
        assert!(looked_at.get() <= 2 * NEAREST_LOOK, "{}", looked_at.get());
    }

    #[test]
    fn a_source_can_be_forgotten() {
        let mut cache = TileCache::new(9);
        cache.insert((1, 0), "a");
        cache.insert((2, 0), "b");
        cache.remove_source(1);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains((2, 0)));
    }
}
