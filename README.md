# futex-queue

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/chemicstry/futex-queue)
[![Cargo](https://img.shields.io/crates/v/futex-queue.svg)](https://github.com/chemicstry/futex-queue)
[![Documentation](https://docs.rs/futex-queue/badge.svg)](https://github.com/chemicstry/futex-queue)

An efficient MPSC queue with timer capability based on Linux futex. Suitable for real-time applications.

## How it Works

Queue is based on Linux [futex syscall](https://man7.org/linux/man-pages/man2/futex.2.html) to wait for both immediate and scheduled items in a single syscall. This alleviates the need for separate timer thread, which would involve multiple context switches.

Immediate items are sent via regular futex atomic variable waking mechanism. Scheduled items use `FUTEX_WAIT_BITSET` operation with absolute timestamp of the earliest item in the queue as a timeout for the syscall.

## Example

```rust
let (tx, mut rx) = FutexQueue::<u32, 4>::new();
let now = Instant::now();

let thread = thread::spawn(move || {
    loop {
        let item = rx.recv();
        println!("{} ms: Received: {}", now.elapsed().as_millis(), item.value());
    }
});

tx.send(1).unwrap();
tx.send_scheduled(2, now + Duration::from_secs(1)).unwrap();
tx.send(3).unwrap();
tx.send_scheduled(4, now + Duration::from_secs(2)).unwrap();

thread.join().unwrap();
```

Output:

```
0 ms: Received: 3
0 ms: Received: 1
1000 ms: Received: 2
2000 ms: Received: 4
```
