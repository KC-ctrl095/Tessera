//! A fixed-size ring buffer for keeping the most recent log lines.
//!
//! ```
//! let mut ring = ringlog::RingBuffer::new(2);
//! ring.push("a");
//! ring.push("b");
//! assert_eq!(ring.push("c"), Some("a"));
//! assert_eq!(ring.iter().copied().collect::<Vec<_>>(), ["b", "c"]);
//! ```

/// Keeps the last `capacity` items pushed. Pushing into a full buffer drops
/// the oldest item.
pub struct RingBuffer<T> {
    slots: Vec<Option<T>>,
    /// Index of the oldest item.
    head: usize,
    len: usize,
}

impl<T> RingBuffer<T> {
    /// An empty buffer for up to `capacity` items (at least 1).
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be at least 1");
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || None);
        RingBuffer { slots, head: 0, len: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Adds an item as the newest. When the buffer is full, the oldest item
    /// is dropped to make room and returned.
    pub fn push(&mut self, item: T) -> Option<T> {
        let cap = self.capacity();
        let tail = (self.head + self.len) % cap;
        if self.len == cap {
            // Full: the tail slot is the oldest item's slot.
            let old = self.slots[tail].replace(item);
            self.head = (self.head + 1) % cap;
            old
        } else {
            self.slots[tail] = Some(item);
            self.len += 1;
            None
        }
    }

    /// Removes and returns the oldest item (`None` when empty).
    pub fn pop_front(&mut self) -> Option<T> {
        let item = self.slots[self.head].take();
        self.head += 1;
        self.len -= 1;
        item
    }

    /// The item at position `i`, counting from the oldest (0). `None` when
    /// `i` is not less than `len()`.
    pub fn get(&self, i: usize) -> Option<&T> {
        let cap = self.capacity();
        self.slots[(self.head + i) % cap].as_ref()
    }

    /// Iterates from the oldest item to the newest.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        (0..self.len).map(move |i| self.slots[i].as_ref().expect("live slot"))
    }

    /// The newest `n` items, oldest first (all of them when `n >= len()`).
    pub fn last_n(&self, n: usize) -> Vec<&T> {
        self.iter().skip(n).collect()
    }

    /// Removes every item.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.head = 0;
        self.len = 0;
    }
}
