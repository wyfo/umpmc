use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

struct NodeIndex {
    value: MaybeUninit<usize>,
    is_set: AtomicBool,
}

impl NodeIndex {
    fn new() -> Self {
        NodeIndex {
            value: MaybeUninit::uninit(),
            is_set: AtomicBool::new(false),
        }
    }
    fn get(&self) -> Option<usize> {
        if self.is_set.load(Ordering::Acquire) {
            Some(unsafe { self.value.assume_init() })
        } else {
            None
        }
    }
    fn set(&mut self, value: usize) {
        debug_assert!(!self.is_set.load(Ordering::Acquire));
        self.value.write(value);
        self.is_set.store(true, Ordering::Release);
    }
    fn unset(&self) {
        debug_assert!(self.is_set.load(Ordering::Acquire));
        self.is_set.store(false, Ordering::Release)
    }
}

struct Node<T> {
    value: MaybeUninit<T>,
    index: NodeIndex,
    prev: *mut Node<T>,
    next: AtomicPtr<Node<T>>,
}

impl<T> Node<T> {
    fn new() -> Self {
        Node {
            value: MaybeUninit::uninit(),
            index: NodeIndex::new(),
            prev: std::ptr::null_mut(),
            next: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

struct Cache<T> {
    head: AtomicPtr<Node<T>>,
}

impl<T> Cache<T> {
    fn new() -> Self {
        Cache {
            head: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
    fn pop(&self) -> *mut Node<T> {
        let mut head = self.head.load(Ordering::Relaxed);
        while !head.is_null() {
            match self.head.compare_exchange_weak(
                head,
                unsafe { &*head }.prev,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return head,
                Err(n) => head = n,
            }
        }
        std::ptr::null_mut()
    }
    fn get(&self) -> NonNull<Node<T>> {
        match NonNull::new(self.pop()) {
            Some(node) => node,
            None => unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(Node::new()))) },
        }
    }
    fn put(&self, node: NonNull<Node<T>>) {
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            unsafe { &mut *node.as_ptr() }.prev = head;
            match self.head.compare_exchange_weak(
                head,
                node.as_ptr(),
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(h) => head = h,
            }
        }
    }
    fn clear(&self) {
        while let Some(node) = NonNull::new(self.pop()) {
            unsafe { Box::from_raw(node.as_ptr()) };
        }
    }
}

impl<T> Drop for Cache<T> {
    fn drop(&mut self) {
        self.clear()
    }
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Eq, Ord, Debug, Hash)]
pub enum Dequeue<T> {
    Empty,
    Spin,
    Data(T),
}

impl<T> Dequeue<T> {
    pub fn data(self) -> Option<T> {
        match self {
            Dequeue::Data(v) => Some(v),
            _ => None,
        }
    }
}

impl<T> Into<Option<T>> for Dequeue<T> {
    fn into(self) -> Option<T> {
        self.data()
    }
}

pub struct Queue<T> {
    head: AtomicPtr<Node<T>>,
    tail: AtomicPtr<Node<T>>,
    index: AtomicUsize,
    cache: Cache<T>,
}

impl<T> Queue<T> {
    pub fn new() -> Self {
        Queue {
            head: AtomicPtr::new(std::ptr::null_mut()),
            tail: AtomicPtr::new(std::ptr::null_mut()),
            index: AtomicUsize::new(0),
            cache: Cache::new(),
        }
    }

    pub fn enqueue(&self, value: T) {
        let node = unsafe { self.cache.get().as_mut() };
        node.value.write(value);
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            node.prev = head;
            match self
                .head
                .compare_exchange_weak(head, node, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(h) => head = h,
            }
        }
        if !head.is_null() {
            let mut prev = head;
            let mut offset = 1;
            loop {
                match unsafe { &*prev }.index.get() {
                    Some(i) => {
                        node.index.set(i.wrapping_add(offset));
                        break;
                    }
                    None => {
                        if unsafe { &*prev }.prev.is_null() {
                            let index = self.index.load(Ordering::Acquire);
                            match unsafe { &*prev }.index.get() {
                                Some(i) => node.index.set(i.wrapping_add(offset)),
                                None => node.index.set(index.wrapping_add(offset)),
                            }
                            break;
                        }
                        prev = unsafe { &*prev }.prev;
                        offset += 1;
                    }
                }
            }
            unsafe { &*head }.next.store(node, Ordering::Release);
        } else {
            node.index.set(self.index.load(Ordering::Relaxed));
            self.tail.store(node, Ordering::SeqCst);
        }
    }

    fn set_tail(
        &self,
        node: &mut Node<T>,
        mut tail: *mut Node<T>,
        next: *mut Node<T>,
        index: usize,
    ) -> T {
        debug_assert!(unsafe { &*tail }.index.get().is_some());
        while let Err(t) =
            self.tail
                .compare_exchange_weak(tail, next, Ordering::SeqCst, Ordering::Relaxed)
        {
            let current_index = self.index.load(Ordering::Relaxed);
            if index != current_index - 1
                || (!t.is_null()
                    && unsafe { &*t }.prev.is_null()
                    && unsafe { &*t }.index.get() == Some(current_index))
            {
                break;
            }
            tail = t
        }
        let value = unsafe { node.value.assume_init_read() };
        node.index.unset();
        node.next.store(std::ptr::null_mut(), Ordering::Release);
        self.cache.put(node.into());
        value
    }

    pub fn dequeue_spin(&self, spin: usize) -> Dequeue<T> {
        let mut index = self.index.load(Ordering::Relaxed);
        let mut tail = self.tail.load(Ordering::Relaxed);
        while !tail.is_null() {
            let node = unsafe { &mut *tail };
            for _ in 0..spin {
                if node.index.get().is_some() {
                    break;
                }
                std::hint::spin_loop()
            }
            let tail_index = match node.index.get() {
                Some(i) => i,
                None => return Dequeue::Spin,
            };
            for _ in 0..spin {
                if !node.next.load(Ordering::Relaxed).is_null()
                    || tail == self.head.load(Ordering::Relaxed)
                {
                    break;
                }
                std::hint::spin_loop()
            }
            let head = self.head.load(Ordering::Relaxed);
            let mut next = node.next.load(Ordering::Relaxed);
            if next.is_null() && tail != head {
                return Dequeue::Spin;
            }
            let next_index = index.wrapping_add(1);
            if index == tail_index
                && match self.index.compare_exchange(
                    index,
                    next_index,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => true,
                    Err(i) => {
                        index = i;
                        false
                    }
                }
            {
                if tail == head {
                    if self
                        .head
                        .compare_exchange(
                            head,
                            std::ptr::null_mut(),
                            Ordering::SeqCst,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        return Dequeue::Data(self.set_tail(node, tail, next, index));
                    } else {
                        for _ in 0..spin {
                            if !node.next.load(Ordering::Acquire).is_null() {
                                break;
                            }
                            std::hint::spin_loop()
                        }
                        next = node.next.load(Ordering::Acquire);
                        if next.is_null()
                            && self
                                .index
                                .compare_exchange(
                                    next_index,
                                    index,
                                    Ordering::SeqCst,
                                    Ordering::Relaxed,
                                )
                                .is_ok()
                        {
                            return Dequeue::Spin;
                        } else {
                            next = node.next.load(Ordering::Acquire);
                        }
                    }
                }
                debug_assert!(!next.is_null());
                return Dequeue::Data(self.set_tail(node, tail, next, index));
            } else {
                tail = next;
            }
        }
        Dequeue::Empty
    }

    pub fn dequeue(&self) -> Dequeue<T> {
        self.dequeue_spin(0)
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        while let Dequeue::Data(_) = self.dequeue() {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::queue::{Dequeue, Queue};

    #[test]
    fn synchronous() {
        let queue = Queue::new();
        assert_eq!(queue.dequeue(), Dequeue::Empty);
        queue.enqueue(0);
        assert_eq!(queue.dequeue(), Dequeue::Data(0));
        queue.enqueue(1);
        assert_eq!(queue.dequeue(), Dequeue::Data(1));
        queue.enqueue(2);
        queue.enqueue(3);
        assert_eq!(queue.dequeue(), Dequeue::Data(2));
        queue.enqueue(4);
        queue.enqueue(5);
        assert_eq!(queue.dequeue(), Dequeue::Data(3));
        assert_eq!(queue.dequeue(), Dequeue::Data(4));
        assert_eq!(queue.dequeue(), Dequeue::Data(5));
        assert_eq!(queue.dequeue(), Dequeue::Empty);
    }

    fn test_asynchronous(nb_values: usize) {
        let start = Instant::now();
        let queue = Arc::new(Queue::new());
        let vec = Arc::new(Mutex::new(Vec::new()));
        let mut threads = vec![];
        for _ in 0..nb_values {
            let queue = queue.clone();
            let vec = vec.clone();
            threads.push(std::thread::spawn(move || loop {
                if let Dequeue::Data(n) = queue.dequeue() {
                    vec.lock().unwrap().push(n);
                    break;
                } else if Instant::now().duration_since(start).as_secs() > 10 {
                    break;
                }
            }));
        }
        std::thread::sleep(Duration::from_micros(10));
        for i in 0..nb_values {
            let queue = queue.clone();
            threads.push(std::thread::spawn(move || {
                queue.enqueue(i);
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            Arc::try_unwrap(vec)
                .unwrap()
                .into_inner()
                .unwrap()
                .into_iter()
                .collect::<HashSet<_>>()
                .len(),
            nb_values
        );
    }

    #[test]
    fn asynchronous() {
        let range = 0..100;
        let tests = [2, 8, 32];
        for nb_values in tests {
            for _ in range.clone() {
                test_asynchronous(nb_values);
            }
        }
    }
}
