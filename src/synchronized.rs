use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker as TaskWaker};
use std::thread::Thread;
use std::time::{Duration, Instant};

use crate::{Dequeue, Queue};

enum InnerWaker {
    Sync(Thread),
    Async(TaskWaker),
}

struct Waker {
    inner: InnerWaker,
    notified: AtomicBool,
}

impl Waker {
    fn new_sync() -> Self {
        Waker {
            inner: InnerWaker::Sync(std::thread::current()),
            notified: AtomicBool::new(false),
        }
    }

    fn new_async(waker: TaskWaker) -> Self {
        Waker {
            inner: InnerWaker::Async(waker),
            notified: AtomicBool::new(false),
        }
    }

    pub fn abort(&self) {
        if self.notified.swap(true, Ordering::Release) {
            if let InnerWaker::Sync(_) = self.inner {
                std::thread::park()
            }
        }
    }
    pub fn wake(&self) -> bool {
        if !self.notified.swap(true, Ordering::Release) {
            match &self.inner {
                InnerWaker::Async(waker) => waker.wake_by_ref(),
                InnerWaker::Sync(thread) => thread.unpark(),
            }
            true
        } else {
            false
        }
    }
}

pub struct SynchronizedQueue<T> {
    inner: Queue<T>,
    wake_queue: Queue<Arc<Waker>>,
}

impl<T> SynchronizedQueue<T> {
    pub fn new() -> Self {
        SynchronizedQueue {
            inner: Queue::new(),
            wake_queue: Queue::new(),
        }
    }

    pub fn enqueue_notify_spin(&self, value: T, spin: usize) {
        self.inner.enqueue(value);
        while let Dequeue::Data(waker) = self.wake_queue.dequeue_spin(spin) {
            if waker.wake() {
                break;
            }
        }
    }

    pub fn enqueue(&self, value: T) {
        self.enqueue_notify_spin(value, 0)
    }

    pub fn try_dequeue_spin(&self, spin: usize) -> Dequeue<T> {
        self.inner.dequeue_spin(spin)
    }

    pub fn try_dequeue(&self) -> Dequeue<T> {
        self.try_dequeue_spin(0)
    }

    fn dequeue_sync(&self, spin: usize, timeout: Option<Duration>) -> Dequeue<T> {
        let end = timeout.map(|t| Instant::now() + t);
        loop {
            if let res @ Dequeue::Data(_) = self.try_dequeue_spin(spin) {
                return res;
            }
            let waker = Arc::new(Waker::new_sync());
            self.wake_queue.enqueue(waker.clone());
            if let res @ Dequeue::Data(_) = self.try_dequeue_spin(spin) {
                waker.abort();
                return res;
            }
            if let Some(end) = end {
                std::thread::park_timeout(end - Instant::now());
                if Instant::now() >= end {
                    return self.try_dequeue_spin(spin);
                }
            } else {
                std::thread::park();
            }
        }
    }

    pub fn dequeue_spin(&self, spin: usize) -> T {
        self.dequeue_sync(spin, None).data().unwrap()
    }

    pub fn dequeue(&self) -> T {
        self.dequeue_spin(0)
    }

    pub fn dequeue_timeout_spin(&self, timeout: Duration, spin: usize) -> Dequeue<T> {
        self.dequeue_sync(spin, Some(timeout))
    }

    pub fn dequeue_timeout(&self, timeout: Duration) -> Dequeue<T> {
        self.dequeue_timeout_spin(timeout, 0)
    }

    pub fn dequeue_async_spin(&self, spin: usize) -> impl Future<Output = T> + '_ {
        DequeueFuture { queue: self, spin }
    }

    pub fn dequeue_async(&self) -> impl Future<Output = T> + '_ {
        self.dequeue_async_spin(0)
    }
}

struct DequeueFuture<'a, T> {
    queue: &'a SynchronizedQueue<T>,
    spin: usize,
}

impl<'a, T> Future for DequeueFuture<'a, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Dequeue::Data(res) = self.queue.try_dequeue_spin(self.spin) {
            Poll::Ready(res)
        } else {
            let waker = Arc::new(Waker::new_async(cx.waker().clone()));
            self.queue.wake_queue.enqueue(waker.clone());
            if let Dequeue::Data(res) = self.queue.try_dequeue_spin(self.spin) {
                waker.abort();
                Poll::Ready(res)
            } else {
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::synchronized::SynchronizedQueue;

    #[test]
    fn synchronized() {
        let queue = Arc::new(SynchronizedQueue::new());
        {
            let queue = queue.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_micros(10));
                queue.enqueue(0)
            });
        }
        assert_eq!(queue.dequeue(), 0);
    }
    #[test]
    fn synchronized_async() {
        let queue = Arc::new(SynchronizedQueue::new());
        {
            let queue = queue.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_micros(10));
                queue.enqueue(0)
            });
        }
        assert_eq!(futures::executor::block_on(queue.dequeue_async()), 0);
    }
}
