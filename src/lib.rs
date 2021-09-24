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

pub struct FutexQueue<T, const N: usize> {
    // Ideally this would be lock-free priority queue, but it's a complicated beast
    // All critical sections are as short as possible so hopefully this mutex spins for a few cycles without syscall
    queue: Mutex<BinaryHeap<Item<T>, Min, N>>,
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

pub struct Sender<T, const N: usize> {
    inner: Arc<FutexQueue<T, N>>,
}

pub struct Receiver<T, const N: usize> {
    inner: Arc<FutexQueue<T, N>>,
}

impl<T, const N: usize> Sender<T, N> {
    pub fn send(&self, item: T) -> Result<(), T> {
        match self.inner.queue.lock().unwrap().push(Item::Immediate(item)) {
            Ok(()) => {
                if self
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

    pub fn send_scheduled(&self, item: T, instant: Instant) -> Result<(), T> {
        match self
            .inner
            .queue
            .lock()
            .unwrap()
            .push(Item::Scheduled(item, instant))
        {
            Ok(()) => {
                if self
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
    pub fn try_recv(&self) -> Result<Item<T>, Option<Instant>> {
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

    pub fn recv(&mut self) -> Item<T> {
        loop {
            let next_instant = match self.try_recv() {
                Ok(item) => return item,
                Err(next_instant) => next_instant,
            };

            // Check if anything new was queued while running to prevent expensive futex syscall
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
        }
    }
}

pub enum Item<T> {
    Immediate(T),
    Scheduled(T, Instant),
}

impl<T> Item<T> {
    pub fn value(&self) -> &T {
        match self {
            Item::Immediate(i) | Item::Scheduled(i, _) => i,
        }
    }

    pub fn into_value(self) -> T {
        match self {
            Item::Immediate(i) | Item::Scheduled(i, _) => i,
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
