use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(pub(crate) u64);

/// Native handles are shared across async tasks in one execution tree.
/// A call keeps a lease so close cannot finalize a handle while that call uses it.
#[derive(Clone, Default)]
pub struct SharedResourceTable {
    inner: Arc<Mutex<SharedResourceState>>,
}

#[derive(Default)]
struct SharedResourceState {
    next_id: u64,
    resources: HashMap<ResourceId, Arc<dyn Any + Send + Sync>>,
}

impl SharedResourceTable {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert<T: Any + Send + Sync>(&self, value: T) -> ResourceId {
        let mut state = self.inner.lock().unwrap();
        let id = ResourceId(state.next_id | (1u64 << 63));
        state.next_id = state
            .next_id
            .checked_add(1)
            .filter(|n| *n < (1u64 << 63))
            .expect("native resource ID overflow");
        state.resources.insert(id, Arc::new(value));
        id
    }
    pub fn get<T: Any + Send + Sync>(&self, id: ResourceId) -> Option<Arc<T>> {
        self.inner
            .lock()
            .unwrap()
            .resources
            .get(&id)?
            .clone()
            .downcast()
            .ok()
    }
    pub fn remove<T: Any + Send + Sync>(&self, id: ResourceId) -> Option<Arc<T>> {
        let mut state = self.inner.lock().unwrap();
        if !state.resources.get(&id)?.is::<T>() {
            return None;
        }
        state.resources.remove(&id)?.downcast().ok()
    }
}

#[derive(Default)]
pub struct ResourceTable {
    next_id: u64,
    resources: HashMap<ResourceId, Box<dyn Any + Send>>,
}

impl ResourceTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<T: Any + Send>(&mut self, value: T) -> ResourceId {
        let id = ResourceId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.resources.insert(id, Box::new(value));
        id
    }

    pub fn get<T: Any + Send>(&self, id: ResourceId) -> Option<&T> {
        self.resources.get(&id)?.downcast_ref::<T>()
    }

    pub fn get_mut<T: Any + Send>(&mut self, id: ResourceId) -> Option<&mut T> {
        self.resources.get_mut(&id)?.downcast_mut::<T>()
    }

    pub fn remove<T: Any + Send>(&mut self, id: ResourceId) -> Option<T> {
        if !self.resources.get(&id)?.is::<T>() {
            return None;
        }
        self.resources
            .remove(&id)?
            .downcast::<T>()
            .ok()
            .map(|boxed| *boxed)
    }

    pub fn contains(&self, id: ResourceId) -> bool {
        self.resources.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.resources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    pub fn clear(&mut self) {
        self.resources.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_ids_are_stable_and_typed() {
        let mut table = ResourceTable::new();
        let id = table.insert(String::from("hello"));
        assert_eq!(table.get::<String>(id).map(String::as_str), Some("hello"));
        assert!(table.get::<u64>(id).is_none());
        assert!(table.contains(id));
        assert_eq!(table.remove::<String>(id).as_deref(), Some("hello"));
        assert!(!table.contains(id));
    }

    #[test]
    fn wrong_typed_remove_preserves_resource() {
        let mut table = ResourceTable::new();
        let id = table.insert(String::from("hello"));

        assert_eq!(table.remove::<u64>(id), None);
        assert!(table.contains(id));
        assert_eq!(table.get::<String>(id).map(String::as_str), Some("hello"));
    }
}
