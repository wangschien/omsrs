//! Bounded work queue for caller-owned fee enrichment.
//!
//! This queue carries the original fill payload without interpreting it. The
//! caller performs transport, reconciliation and fee booking independently.

use std::collections::VecDeque;

pub const FEE_ENRICH_QUEUE_CAPACITY: usize = 64;
pub const FEE_ENRICH_MAX_ATTEMPTS: u8 = 3;

#[derive(Debug)]
pub struct FeeEnrichItem<T> {
    order_id: String,
    signal: T,
    attempts: u8,
}

impl<T> FeeEnrichItem<T> {
    pub fn order_id(&self) -> &str {
        &self.order_id
    }

    pub fn signal(&self) -> &T {
        &self.signal
    }

    /// Number of unsuccessful attempts already returned to the queue.
    pub fn attempts(&self) -> u8 {
        self.attempts
    }
}

#[derive(Debug)]
pub struct FeeEnrichQueue<T> {
    entries: VecDeque<FeeEnrichItem<T>>,
    high_water: u64,
    dropped: u64,
}

impl<T> Default for FeeEnrichQueue<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            high_water: 0,
            dropped: 0,
        }
    }
}

impl<T> FeeEnrichQueue<T> {
    pub fn push_new(&mut self, order_id: String, signal: T) {
        self.push(FeeEnrichItem {
            order_id,
            signal,
            attempts: 0,
        });
    }

    fn push(&mut self, item: FeeEnrichItem<T>) {
        if self.entries.len() >= FEE_ENRICH_QUEUE_CAPACITY {
            let _ = self.entries.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.entries.push_back(item);
        self.high_water = self.high_water.max(self.entries.len() as u64);
    }

    /// Return an unsuccessful attempt at the tail, or count its final drop.
    pub fn retry_or_drop(&mut self, mut item: FeeEnrichItem<T>) {
        item.attempts = item.attempts.saturating_add(1);
        if item.attempts >= FEE_ENRICH_MAX_ATTEMPTS {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.push(item);
        }
    }

    /// Count a popped item discarded after an exhaustive miss or booking error.
    pub fn drop_item(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    pub fn pop(&mut self) -> Option<FeeEnrichItem<T>> {
        self.entries.pop_front()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::{FeeEnrichQueue, FEE_ENRICH_QUEUE_CAPACITY};

    #[test]
    fn pressure_counters_saturate_without_resetting_historical_high_water() {
        let mut queue = FeeEnrichQueue {
            entries: Default::default(),
            high_water: u64::MAX,
            dropped: u64::MAX - 1,
        };
        for n in 0..=FEE_ENRICH_QUEUE_CAPACITY {
            queue.push_new(n.to_string(), n);
        }
        assert_eq!(queue.dropped(), u64::MAX);
        assert_eq!(queue.high_water(), u64::MAX);
        queue.drop_item();
        assert_eq!(queue.dropped(), u64::MAX);

        let mut item = queue.pop().unwrap();
        item.attempts = u8::MAX;
        queue.retry_or_drop(item);
        assert_eq!(queue.dropped(), u64::MAX);
        assert_eq!(queue.len(), FEE_ENRICH_QUEUE_CAPACITY - 1);
    }
}
