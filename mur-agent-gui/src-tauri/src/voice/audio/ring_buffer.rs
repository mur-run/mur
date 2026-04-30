//! Bounded interior-mutable ring buffer for streaming audio samples.
//! Used for both capture (mic → STT) and playback (TTS → speaker)
//! paths. Thread-safe via `parking_lot::Mutex`; oldest-sample-eviction
//! on overflow (we prefer dropping latency over memory growth).

use parking_lot::Mutex;
use std::collections::VecDeque;

pub struct PlaybackRing {
    inner: Mutex<VecDeque<f32>>,
    capacity: usize,
}

impl PlaybackRing {
    pub fn new(capacity_samples: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity_samples)),
            capacity: capacity_samples,
        }
    }

    pub fn push(&self, samples: &[f32]) {
        let mut q = self.inner.lock();
        for &s in samples {
            if q.len() >= self.capacity {
                q.pop_front();
            }
            q.push_back(s);
        }
    }

    pub fn pop(&self, n: usize) -> Vec<f32> {
        let mut q = self.inner.lock();
        let take = n.min(q.len());
        q.drain(..take).collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_pop_round_trips() {
        let r = PlaybackRing::new(8);
        r.push(&[1.0, 2.0, 3.0]);
        assert_eq!(r.len(), 3);
        let got = r.pop(2);
        assert_eq!(got, vec![1.0, 2.0]);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn overflow_drops_oldest_samples() {
        let r = PlaybackRing::new(3);
        r.push(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(r.len(), 3);
        let got = r.pop(3);
        assert_eq!(got, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn pop_more_than_available_returns_all() {
        let r = PlaybackRing::new(8);
        r.push(&[1.0, 2.0]);
        let got = r.pop(100);
        assert_eq!(got, vec![1.0, 2.0]);
        assert!(r.is_empty());
    }

    #[test]
    fn pop_empty_returns_empty() {
        let r = PlaybackRing::new(8);
        let got = r.pop(5);
        assert!(got.is_empty());
    }
}
