//! Insertion-ordered string-keyed record, the payload of `Value::Object`.
//!
//! Small records (the overwhelmingly common case: structs, JSON objects with a
//! handful of fields) are a flat vector searched linearly, with no hashing and
//! no per-key allocation: keys are `Arc<str>`, so cloning a record only bumps
//! reference counts, and records built from the same struct declaration share
//! their key allocations (pointer-equality makes the common lookup a pointer
//! compare). Records that grow past `INDEX_THRESHOLD` fields get a hash index.
//!
//! Semantics match the `IndexMap<String, Value>` this replaces: insertion
//! order is preserved, re-inserting an existing key keeps its position, removal
//! preserves the order of the rest, and equality ignores order.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use super::value::Value;

const INDEX_THRESHOLD: usize = 16;

#[derive(Clone, Default)]
pub struct Record {
    entries: Vec<(Arc<str>, Value)>,
    /// Key → position, built only for large records.
    index: Option<Box<HashMap<Arc<str>, usize>>>,
}

impl Record {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            index: None,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    fn position(&self, key: &str) -> Option<usize> {
        if let Some(index) = &self.index {
            return index.get(key).copied();
        }
        let key_ptr = key.as_ptr();
        self.entries.iter().position(|(existing, _)| {
            // Same allocation (shared struct keys) or equal text.
            std::ptr::eq(existing.as_ptr(), key_ptr) || **existing == *key
        })
    }

    #[inline]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|at| &self.entries[at].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let at = self.position(key)?;
        Some(&mut self.entries[at].1)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        self.position(key)
    }

    /// Inserts or replaces; a replaced key keeps its original position. The
    /// key is only converted to an `Arc<str>` when it is actually new.
    pub fn insert<K: AsRef<str> + Into<Arc<str>>>(&mut self, key: K, value: Value) -> Option<Value> {
        if let Some(at) = self.position(key.as_ref()) {
            return Some(std::mem::replace(&mut self.entries[at].1, value));
        }
        let key: Arc<str> = key.into();
        let at = self.entries.len();
        if let Some(index) = &mut self.index {
            index.insert(Arc::clone(&key), at);
        }
        self.entries.push((key, value));
        if self.index.is_none() && self.entries.len() > INDEX_THRESHOLD {
            self.rebuild_index();
        }
        None
    }

    fn rebuild_index(&mut self) {
        let mut index = HashMap::with_capacity(self.entries.len());
        for (at, (key, _)) in self.entries.iter().enumerate() {
            index.insert(Arc::clone(key), at);
        }
        self.index = Some(Box::new(index));
    }

    /// Removes a key, keeping the order of the remaining entries.
    pub fn shift_remove(&mut self, key: &str) -> Option<Value> {
        let at = self.position(key)?;
        let (_, value) = self.entries.remove(at);
        if self.index.is_some() {
            if self.entries.len() > INDEX_THRESHOLD {
                self.rebuild_index();
            } else {
                self.index = None;
            }
        }
        Some(value)
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.shift_remove(key)
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter {
            inner: self.entries.iter(),
        }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Arc<str>, &mut Value)> {
        self.entries.iter_mut().map(|(key, value)| (&*key, value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &Arc<str>> + '_ {
        self.entries.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> + '_ {
        self.entries.iter().map(|(_, value)| value)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> + '_ {
        self.entries.iter_mut().map(|(_, value)| value)
    }

    pub fn first(&self) -> Option<(&Arc<str>, &Value)> {
        self.entries.first().map(|(key, value)| (key, value))
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Arc<str>, &mut Value) -> bool) {
        self.entries.retain_mut(|(key, value)| keep(key, value));
        if self.index.is_some() {
            if self.entries.len() > INDEX_THRESHOLD {
                self.rebuild_index();
            } else {
                self.index = None;
            }
        }
    }
}

pub struct Iter<'a> {
    inner: std::slice::Iter<'a, (Arc<str>, Value)>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Arc<str>, &'a Value);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key, value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(key, value)| (key, value))
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a Record {
    type Item = (&'a Arc<str>, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl IntoIterator for Record {
    type Item = (Arc<str>, Value);
    type IntoIter = std::vec::IntoIter<(Arc<str>, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K: AsRef<str> + Into<Arc<str>>> FromIterator<(K, Value)> for Record {
    fn from_iter<I: IntoIterator<Item = (K, Value)>>(iter: I) -> Self {
        let mut record = Record::new();
        record.extend(iter);
        record
    }
}

impl<K: AsRef<str> + Into<Arc<str>>> Extend<(K, Value)> for Record {
    fn extend<I: IntoIterator<Item = (K, Value)>>(&mut self, iter: I) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<K: AsRef<str> + Into<Arc<str>>, const N: usize> From<[(K, Value); N]> for Record {
    fn from(entries: [(K, Value); N]) -> Self {
        entries.into_iter().collect()
    }
}

impl From<IndexMap<String, Value>> for Record {
    fn from(map: IndexMap<String, Value>) -> Self {
        map.into_iter().collect()
    }
}

impl From<Record> for IndexMap<String, Value> {
    fn from(record: Record) -> Self {
        record
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }
}

impl PartialEq for Record {
    /// Order-insensitive, like the `IndexMap` it replaces.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|theirs| theirs == value))
    }
}

impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl std::ops::Index<&str> for Record {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get(key).expect("no entry found for key")
    }
}
