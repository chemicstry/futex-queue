use std::{
    cmp,
    sync::{atomic, Arc, Mutex},
    time::Instant,
};

use heapless::{binary_heap::Min, BinaryHeap};
use linux_futex::{Futex, Private};

const FUTEX_PARKED: i32 = -1;
const FUTEX_EMPTY: i32 = 0;
const FUTEX_NOTIFIED: i32 = 1;

/// A fixed size MPSC queue with timer capability based on Linux futex. Suitable for real-time applications.
/// Size N must be a power of 2.
pub struct FutexQueue<T, const N: usize> {
    // Ideally this would be lock-free priority queue, but it's a complicated beast
    // All critical sections are as short as possible so hopefully this mutex spins for a few cycles without syscall
    queue: Mutex<BinaryHeap<Item<T>, Min, N>>,
    // Futex used to notify receiver
    reader_state: Futex<Private>,
}

impl<T, const N: usize> FutexQueue<T, N> {
    pub fn new() -> (Sender<T, N>, Receiver<T, N>) {
        let inner = Arc::new(FutexQueue {
            queue: Mutex::new(BinaryHeap::default()),
            reader_state: Futex::new(FUTEX_EMPTY),
        });

        (
            Sender {
                inner: inner.clone(),
            },
            Receiver { inner },
        )
    }
}

/// Sender half of the queue. Safe to share between threads.
pub struct Sender<T, const N: usize> {
    inner: Arc<FutexQueue<T, N>>,
}

/// Receiver half of the queue.
pub struct Receiver<T, const N: usize> {
    inner: Arc<FutexQueue<T, N>>,
}

impl<T, const N: usize> Sender<T, N> {
    /// Sends an item into the queue.
    /// The receive order of sent items is not guaranteed.
    pub fn send(&self, item: T) -> Result<(), T> {
        let res = self.inner.queue.lock().unwrap().push(Item::Immediate(item));

        match res {
            Ok(()) => {
                if self
                    .inner
                    .reader_state
                    .value
                    .swap(FUTEX_NOTIFIED, atomic::Ordering::Release)
                    == FUTEX_PARKED
                {
                    // Wake up receiver thread because it was parked
                    self.inner.reader_state.wake(1);
                }

                Ok(())
            }
            Err(item) => Err(item.into_value()),
        }
    }

    /// Puts item into a queue to be received at a specified instant.
    /// Receive order is earliest deadline first (after all immediate items).
    pub fn send_scheduled(&self, item: T, instant: Instant) -> Result<(), T> {
        // Keep critical section small
        let (res, reload_timer) = {
            let mut queue = self.inner.queue.lock().unwrap();
            // Reload timer if new instant is the earliest in the queue or if there are no scheduled items.
            // Note that this also evaluates to true if there is an immediate item at the front of the queue,
            // but this edge case is rare and should not cause major performance issues.
            let reload_timer = queue
                .peek()
                .map(|i| i.instant())
                .flatten()
                .map(|i| instant < i)
                .unwrap_or(true);
            let res = queue.push(Item::Scheduled(item, instant));
            (res, reload_timer)
        };

        match res {
            Ok(()) => {
                if reload_timer
                    && self
                        .inner
                        .reader_state
                        .value
                        .swap(FUTEX_NOTIFIED, atomic::Ordering::Release)
                        == FUTEX_PARKED
                {
                    self.inner.reader_state.wake(1);
                }

                Ok(())
            }
            Err(item) => Err(item.into_value()),
        }
    }
}

impl<T, const N: usize> Receiver<T, N> {
    /// Tries to receive from the queue without blocking.
    /// Immediate items are returned first, then scheduled items in the order of earliest deadline first.
    /// Error contains an optional Instant of the earliest (not ready) deadline in the queue.
    pub fn try_recv(&mut self) -> Result<Item<T>, Option<Instant>> {
        let mut queue = self.inner.queue.lock().unwrap();

        match queue.peek() {
            Some(item) => {
                match item {
                    // Immediate items are sorted at the beginning of the queue
                    Item::Immediate(_) => Ok(queue.pop().unwrap()),
                    Item::Scheduled(_, instant) => {
                        if *instant <= Instant::now() {
                            // Scheduled item is ready
                            Ok(queue.pop().unwrap())
                        } else {
                            // Queue is not empty, but none of the scheduled items are ready
                            Err(Some(*instant))
                        }
                    }
                }
            }
            // Queue is empty and there are no scheduled items
            None => Err(None),
        }
    }

    /// Tries to receive from the queue and blocks the current thread if queue is empty.
    /// Immediate items are returned first, then scheduled items in the order of earliest deadline first.
    pub fn recv(&mut self) -> Item<T> {
        loop {
            let next_instant = match self.try_recv() {
                Ok(item) => return item,
                Err(next_instant) => next_instant,
            };

            // Check if anything new was queued while running to prevent expensive futex syscall
            // Change NOTIFIED=>EMPTY or EMPTY=>PARKED, and continue in the first case
            if self
                .inner
                .reader_state
                .value
                .fetch_sub(1, atomic::Ordering::Acquire)
                == FUTEX_NOTIFIED
            {
                continue;
            }

            if let Some(instant) = next_instant {
                self.inner
                    .reader_state
                    .wait_bitset_until(FUTEX_PARKED, u32::MAX, instant)
                    .ok();
            } else {
                self.inner.reader_state.wait(FUTEX_PARKED).ok();
            }

            // Reset state
            self.inner
                .reader_state
                .value
                .store(FUTEX_EMPTY, atomic::Ordering::Release);
        }
    }
}

/// Represents queued item.
pub enum Item<T> {
    /// Item queued to be received immediately.
    Immediate(T),
    /// Item queued to be received at a specified instant.
    Scheduled(T, Instant),
}

impl<T> Item<T> {
    /// Returns reference to the item value.
    pub fn value(&self) -> &T {
        match self {
            Item::Immediate(i) | Item::Scheduled(i, _) => i,
        }
    }

    /// Consumes item to unwrap the contained value.
    pub fn into_value(self) -> T {
        match self {
            Item::Immediate(i) | Item::Scheduled(i, _) => i,
        }
    }

    /// Returns the scheduled instant of the item.
    /// Returns None if the item was immediate.
    pub fn instant(&self) -> Option<Instant> {
        match self {
            Item::Immediate(_) => None,
            Item::Scheduled(_, instant) => Some(*instant),
        }
    }
}

impl<T> Eq for Item<T> {}

impl<T> PartialEq for Item<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Item::Immediate(_), Item::Immediate(_)) => true,
            (Item::Immediate(_), Item::Scheduled(_, _)) => false,
            (Item::Scheduled(_, _), Item::Immediate(_)) => false,
            (Item::Scheduled(_, i1), Item::Scheduled(_, i2)) => i1 == i2,
        }
    }
}

impl<T> Ord for Item<T> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.partial_cmp(other).unwrap_or(cmp::Ordering::Less)
    }
}

// Item::Immediate first, then Item::Scheduled sorted by instant
impl<T> PartialOrd for Item<T> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        match (self, other) {
            (Item::Immediate(_), Item::Immediate(_)) => None,
            (Item::Immediate(_), Item::Scheduled(_, _)) => Some(cmp::Ordering::Less),
            (Item::Scheduled(_, _), Item::Immediate(_)) => Some(cmp::Ordering::Greater),
            (Item::Scheduled(_, i1), Item::Scheduled(_, i2)) => Some(i1.cmp(i2)),
        }
    }
}
