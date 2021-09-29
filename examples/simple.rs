use futex_queue::FutexQueue;
use std::{
    thread,
    time::{Duration, Instant},
};

fn main() {
    let (tx, mut rx) = FutexQueue::<u32, 4>::new();
    let now = Instant::now();

    let thread = thread::spawn(move || loop {
        let item = rx.recv();
        println!(
            "{} ms: Received: {}",
            now.elapsed().as_millis(),
            item.value()
        );
    });

    tx.send(1).unwrap();
    tx.send_scheduled(2, now + Duration::from_secs(1)).unwrap();
    tx.send(3).unwrap();
    tx.send_scheduled(4, now + Duration::from_secs(2)).unwrap();

    thread.join().unwrap();
}
