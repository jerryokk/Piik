//! An insertion-ordered map with live iteration.
//!
//! Wire-visible participant order, candidate ordering, round-robin state and
//! deletion during iteration depend on these semantics, which mirror a
//! JavaScript `Map`.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::ControlFlow;

#[derive(Debug, Clone)]
struct Entry<K, V> {
    key: K,
    /// `None` marks a tombstone; the value is dropped on deletion.
    value: Option<V>,
}

/// Keeps keys in first-insertion order. [`Map::set`] on an existing key keeps
/// its position; [`Map::move_to_back`] removes and reinserts it at the end.
/// [`Map::visit`] is live: an entry deleted before it is visited is skipped
/// and an entry appended during the visit is visited.
#[derive(Debug, Clone)]
pub struct Map<K, V> {
    entries: Vec<Entry<K, V>>,
    index: HashMap<K, usize>,
    deleted: usize,
    iterating: usize,
}

impl<K, V> Default for Map<K, V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            deleted: 0,
            iterating: 0,
        }
    }
}

impl<K: Eq + Hash + Clone, V> Map<K, V> {
    /// Creates an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports the number of live entries.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Reports whether the map holds no live entry.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Returns the value for `key`.
    pub fn get(&self, key: &K) -> Option<&V> {
        let position = *self.index.get(key)?;
        self.entries[position].value.as_ref()
    }

    /// Returns the value for `key` mutably, keeping its position.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let position = *self.index.get(key)?;
        self.entries[position].value.as_mut()
    }

    /// Reports whether `key` is present.
    pub fn has(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    /// Stores `value` under `key`, keeping the existing position when the key
    /// is already present and appending otherwise.
    pub fn set(&mut self, key: K, value: V) {
        if let Some(&position) = self.index.get(&key) {
            self.entries[position].value = Some(value);
            return;
        }
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push(Entry {
            key,
            value: Some(value),
        });
    }

    /// Re-inserts `key` at the end, as JavaScript `map.delete(k); map.set(k, v)`
    /// does. It is a plain [`Map::set`] when the key is absent.
    pub fn move_to_back(&mut self, key: K, value: V) {
        self.delete(&key);
        self.set(key, value);
    }

    /// Removes `key` and reports whether it was present.
    pub fn delete(&mut self, key: &K) -> bool {
        let Some(position) = self.index.remove(key) else {
            return false;
        };
        self.entries[position].value = None;
        self.deleted += 1;
        self.compact();
        true
    }

    /// Removes every entry. Dropping the index before compacting is safe
    /// because every entry is now a tombstone, so compaction writes no index
    /// entry.
    pub fn clear(&mut self) {
        for entry in &mut self.entries {
            if entry.value.take().is_some() {
                self.deleted += 1;
            }
        }
        self.index.clear();
        self.compact();
    }

    /// Returns a snapshot of the live keys in order.
    pub fn keys(&self) -> Vec<K> {
        self.iter().map(|(key, _)| key.clone()).collect()
    }

    /// Returns a snapshot of the live values in order.
    pub fn values(&self) -> Vec<V>
    where
        V: Clone,
    {
        self.iter().map(|(_, value)| value.clone()).collect()
    }

    /// Iterates live entries in order without allowing mutation.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries
            .iter()
            .filter_map(|entry| entry.value.as_ref().map(|value| (&entry.key, value)))
    }

    /// Visits live entries in order with JavaScript `Map` semantics. The
    /// callback receives the map itself, so it may set, delete or move entries
    /// (including nested visits); returning [`ControlFlow::Break`] stops early.
    /// Key and value are cloned so the callback may mutate the map freely.
    pub fn visit<F>(&mut self, mut callback: F)
    where
        V: Clone,
        F: FnMut(&mut Self, K, V) -> ControlFlow<()>,
    {
        self.iterating += 1;
        let mut position = 0;
        while position < self.entries.len() {
            let entry = &self.entries[position];
            position += 1;
            let Some(value) = entry.value.clone() else {
                continue;
            };
            let key = entry.key.clone();
            if callback(self, key, value).is_break() {
                break;
            }
        }
        self.iterating -= 1;
        self.compact();
    }

    /// Drops tombstones once they dominate, but never while a visit is walking
    /// the entry vector.
    fn compact(&mut self) {
        if self.iterating > 0 || self.deleted == 0 || self.deleted * 2 < self.entries.len() {
            return;
        }
        let entries = std::mem::take(&mut self.entries);
        self.entries.reserve(self.index.len());
        for entry in entries {
            if entry.value.is_some() {
                self.index.insert(entry.key.clone(), self.entries.len());
                self.entries.push(entry);
            }
        }
        self.deleted = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_insertion_order_and_position_on_set() {
        let mut map = Map::new();
        map.set("a", 1);
        map.set("b", 2);
        map.set("c", 3);
        map.set("a", 10);
        assert_eq!(map.keys(), ["a", "b", "c"]);
        assert_eq!(map.values(), [10, 2, 3]);
        map.move_to_back("a", 11);
        assert_eq!(map.keys(), ["b", "c", "a"]);
        assert_eq!(map.get(&"a"), Some(&11));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn live_iteration_skips_deleted_and_visits_appended() {
        let mut map = Map::new();
        for key in ["a", "b", "c", "d"] {
            map.set(key, key.len());
        }
        let mut visited = Vec::new();
        map.visit(|map, key, _| {
            visited.push(key);
            match key {
                // Deleted before its turn: skipped, like a JS Map.
                "a" => {
                    map.delete(&"c");
                }
                // Appended during iteration: visited.
                "b" => map.set("e", 1),
                _ => {}
            }
            ControlFlow::Continue(())
        });
        assert_eq!(visited, ["a", "b", "d", "e"]);
        assert_eq!(map.keys(), ["a", "b", "d", "e"]);
    }

    #[test]
    fn compaction_preserves_order_and_index() {
        let mut map = Map::new();
        for index in 0..100 {
            map.set(index, index);
        }
        for index in (0..100).step_by(2) {
            map.delete(&index);
        }
        assert!(map.len() == 50 && map.entries.len() <= 60);
        for index in (1..100).step_by(2) {
            assert_eq!(map.get(&index), Some(&index));
        }
        let keys = map.keys();
        assert_eq!(keys[0], 1);
        assert_eq!(keys[49], 99);
        assert!(keys.is_sorted());
        assert!(
            !map.delete(&1000),
            "deleting an absent key must report false"
        );
        map.clear();
        assert!(map.is_empty() && map.keys().is_empty());
    }

    #[test]
    fn visit_defers_compaction_and_stops_on_break() {
        let mut map = Map::new();
        for index in 0..10 {
            map.set(index, index);
        }
        let mut visited = Vec::new();
        map.visit(|map, key, _| {
            visited.push(key);
            for other in 0..10 {
                if other != key {
                    map.delete(&other);
                }
            }
            ControlFlow::Break(())
        });
        assert_eq!(visited, [0]);
        assert_eq!(map.keys(), [0]);
        assert_eq!(map.entries.len(), 1, "compaction runs after the visit");
    }
}
