use std::any::Any;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(pub(crate) u64);

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
}
