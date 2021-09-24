use std::time::{Duration, Instant};

use futex_queue::FutexQueue;

#[test]
fn simple_send_recv() {
    let (tx, rx) = FutexQueue::<u32, 4>::new();

    tx.send(1).unwrap();
    tx.send(2).unwrap();
    tx.send(3).unwrap();
    tx.send(4).unwrap();

    rx.try_recv().unwrap();
    rx.try_recv().unwrap();
    rx.try_recv().unwrap();
    rx.try_recv().unwrap();
}

#[test]
fn sorting() {
    let (tx, rx) = FutexQueue::<u32, 4>::new();

    let later = Instant::now() + Duration::from_secs(100);
    let more_later = Instant::now() + Duration::from_secs(150);

    tx.send(1).unwrap();
    tx.send_scheduled(2, more_later).unwrap();
    tx.send_scheduled(3, later).unwrap();
    tx.send(4).unwrap();

    rx.try_recv().unwrap();
    rx.try_recv().unwrap();
    assert!(rx.try_recv() == Err(Some(later)));
}

#[test]
fn timing() {
    let (tx, mut rx) = FutexQueue::<u32, 4>::new();

    let later = Instant::now() + Duration::from_secs(1);
    tx.send_scheduled(0, later).unwrap();

    let _ = rx.recv();
    assert!(Instant::now() - later < Duration::from_micros(100));
}
