use lru::LruCache;
use std::num::NonZeroUsize;

pub struct PageCache<T> {
    cache: LruCache<usize, T>,
}

impl<T> PageCache<T> {
    pub fn new(capacity: usize) -> Self {
        let safe_capacity = NonZeroUsize::new(capacity.max(1)).expect("non-zero cache capacity");
        Self {
            cache: LruCache::new(safe_capacity),
        }
    }

    pub fn capacity(&self) -> usize {
        self.cache.cap().get()
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    pub fn contains(&self, key: usize) -> bool {
        self.cache.contains(&key)
    }

    pub fn get(&mut self, key: usize) -> Option<&T> {
        self.cache.get(&key)
    }

    pub fn put(&mut self, key: usize, value: T) {
        self.cache.put(key, value);
    }

    pub fn resize(&mut self, capacity: usize) {
        let safe_capacity = NonZeroUsize::new(capacity.max(1)).expect("non-zero cache capacity");
        self.cache.resize(safe_capacity);
    }
}

#[cfg(test)]
mod tests {
    use super::PageCache;

    #[test]
    fn evicts_least_recently_used_pages() {
        let mut cache = PageCache::new(2);
        cache.put(1, "one");
        cache.put(2, "two");
        assert!(cache.contains(1));

        let _ = cache.get(1);
        cache.put(3, "three");

        assert!(cache.contains(1));
        assert!(!cache.contains(2));
        assert!(cache.contains(3));
    }
}
