//! A pane's raw output tail: the last 256 KiB, for `GET /v1/panes/{id}/tail` (spec §4).

use std::collections::VecDeque;

/// How much raw output a pane keeps.
pub const TAIL_CAPACITY: usize = 256 * 1024;

/// A byte ring holding the most recent output, up to its capacity.
#[derive(Debug)]
pub struct Tail {
    bytes: VecDeque<u8>,
    capacity: usize,
}

impl Tail {
    /// An empty tail keeping at most `capacity` bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Appends output, dropping the oldest bytes past the capacity.
    pub fn push(&mut self, bytes: &[u8]) {
        let keep = &bytes[bytes.len().saturating_sub(self.capacity)..];
        let overflow = (self.bytes.len() + keep.len()).saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(keep);
    }

    /// The last `n` bytes, or all of them when fewer are held.
    pub fn last(&self, n: usize) -> Vec<u8> {
        let skip = self.bytes.len().saturating_sub(n);
        self.bytes.range(skip..).copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(chunks: &[&[u8]]) -> Tail {
        let mut tail = Tail::new(8);
        for chunk in chunks {
            tail.push(chunk);
        }
        tail
    }

    mod tail {
        use super::*;

        #[test]
        fn should_keep_everything_under_the_capacity() {
            assert_eq!(tail(&[b"ab", b"cd"]).last(100), b"abcd");
        }

        #[test]
        fn should_drop_the_oldest_bytes_past_the_capacity() {
            assert_eq!(tail(&[b"012345", b"6789"]).last(100), b"23456789");
        }

        #[test]
        fn should_keep_the_end_of_a_chunk_bigger_than_the_capacity() {
            assert_eq!(tail(&[b"xy", b"0123456789"]).last(100), b"23456789");
        }

        #[test]
        fn should_give_only_the_last_n_bytes() {
            assert_eq!(tail(&[b"012345"]).last(2), b"45");
        }

        #[test]
        fn should_hold_256_kib_for_a_pane() {
            let mut tail = Tail::new(TAIL_CAPACITY);
            tail.push(&vec![b'x'; TAIL_CAPACITY + 10]);
            assert_eq!(tail.last(usize::MAX).len(), 256 * 1024);
        }
    }
}
