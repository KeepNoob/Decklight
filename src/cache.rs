use lru::LruCache;
use std::num::NonZeroUsize;

pub trait CacheWeight {
    fn cache_weight_bytes(&self) -> usize;
}

impl CacheWeight for &'static str {
    fn cache_weight_bytes(&self) -> usize {
        self.len()
    }
}

pub struct PageCache<T: CacheWeight> {
    cache: LruCache<usize, T>,
    current_weight_bytes: usize,
    max_weight_bytes: usize,
}

impl<T: CacheWeight> PageCache<T> {
    pub fn new(capacity: usize, max_weight_bytes: usize) -> Self {
        let safe_capacity = NonZeroUsize::new(capacity.max(1)).expect("non-zero cache capacity");
        let safe_max_weight = max_weight_bytes.max(1);
        Self {
            cache: LruCache::new(safe_capacity),
            current_weight_bytes: 0,
            max_weight_bytes: safe_max_weight,
        }
    }

    pub fn capacity(&self) -> usize {
        self.cache.cap().get()
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn total_weight_bytes(&self) -> usize {
        self.current_weight_bytes
    }

    pub fn max_weight_bytes(&self) -> usize {
        self.max_weight_bytes
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
        let incoming_weight = value.cache_weight_bytes();

        if let Some(old_value) = self.cache.pop(&key) {
            self.current_weight_bytes = self
                .current_weight_bytes
                .saturating_sub(old_value.cache_weight_bytes());
        }

        // If one bitmap is larger than budget, keep cache empty and skip insertion.
        if incoming_weight > self.max_weight_bytes {
            return;
        }

        while self.current_weight_bytes.saturating_add(incoming_weight) > self.max_weight_bytes {
            let Some((_evicted_key, evicted_value)) = self.cache.pop_lru() else {
                break;
            };
            self.current_weight_bytes = self
                .current_weight_bytes
                .saturating_sub(evicted_value.cache_weight_bytes());
        }

        if let Some((_old_key, old_value)) = self.cache.push(key, value) {
            self.current_weight_bytes = self
                .current_weight_bytes
                .saturating_sub(old_value.cache_weight_bytes());
        }
        self.current_weight_bytes = self.current_weight_bytes.saturating_add(incoming_weight);
    }

    pub fn resize(&mut self, capacity: usize) {
        let safe_capacity = NonZeroUsize::new(capacity.max(1)).expect("non-zero cache capacity");
        self.cache.resize(safe_capacity);
        self.current_weight_bytes = self
            .cache
            .iter()
            .map(|(_key, value)| value.cache_weight_bytes())
            .sum();
    }

    pub fn set_max_weight_bytes(&mut self, max_weight_bytes: usize) {
        self.max_weight_bytes = max_weight_bytes.max(1);
        while self.current_weight_bytes > self.max_weight_bytes {
            let Some((_evicted_key, evicted_value)) = self.cache.pop_lru() else {
                self.current_weight_bytes = 0;
                break;
            };
            self.current_weight_bytes = self
                .current_weight_bytes
                .saturating_sub(evicted_value.cache_weight_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PageCache;

    #[test]
    fn evicts_least_recently_used_pages() {
        let mut cache = PageCache::new(2, 1024);
        cache.put(1, "one");
        cache.put(2, "two");
        assert!(cache.contains(1));

        let _ = cache.get(1);
        cache.put(3, "three");

        assert!(cache.contains(1));
        assert!(!cache.contains(2));
        assert!(cache.contains(3));
    }

    #[test]
    fn respects_weight_budget_even_when_capacity_allows_more() {
        let mut cache = PageCache::new(10, 5);
        cache.put(1, "abc");
        cache.put(2, "de");
        assert_eq!(cache.total_weight_bytes(), 5);
        cache.put(3, "fg");
        assert!(cache.total_weight_bytes() <= 5);
        assert!(!cache.contains(1));
        assert!(cache.contains(2) || cache.contains(3));
    }
}
