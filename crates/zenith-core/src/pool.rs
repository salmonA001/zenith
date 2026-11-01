//! A worker pool with a priority queue, for generation and meshing.
//!
//! Lower priorities run first, ties in submission order. Stale work is the submitter's business:
//! a job checks whether it still matters when it starts, so a queue full of chunks the player has
//! flown away from costs a few atomic loads rather than seconds of generation.
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send>;

struct Item {
    priority: i64,
    order: u64,
    job: Job,
}
impl PartialEq for Item {
    fn eq(&self, o: &Self) -> bool {
        self.priority == o.priority && self.order == o.order
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Item {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // BinaryHeap pops the largest.
        o.priority.cmp(&self.priority).then(o.order.cmp(&self.order))
    }
}

#[derive(Default)]
struct Queue {
    heap: BinaryHeap<Item>,
    next_order: u64,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    stop: AtomicBool,
    running: AtomicUsize,
}

pub struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    pub fn new(name: &str, threads: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            running: AtomicUsize::new(0),
        });
        let threads = (0..threads.max(1))
            .map(|i| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("{name}-{i}"))
                    .spawn(move || worker(&shared))
                    .expect("spawn worker thread")
            })
            .collect();
        Self { shared, threads }
    }

    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }

    pub fn submit(&self, priority: i64, job: impl FnOnce() + Send + 'static) {
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        let order = q.next_order;
        q.next_order += 1;
        q.heap.push(Item { priority, order, job: Box::new(job) });
        drop(q);
        self.shared.wake.notify_one();
    }

    /// Jobs waiting or running.
    pub fn pending(&self) -> usize {
        let queued = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).heap.len();
        queued + self.shared.running.load(Ordering::Acquire)
    }

    /// Blocks until no job is waiting or running.
    pub fn wait_idle(&self) {
        while self.pending() > 0 {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }
}

fn worker(shared: &Shared) {
    loop {
        let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        let item = loop {
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            if let Some(item) = q.heap.pop() {
                // Counted before the lock goes so `pending` never sees it in neither place.
                shared.running.fetch_add(1, Ordering::AcqRel);
                break item;
            }
            q = shared.wake.wait(q).unwrap_or_else(|e| e.into_inner());
        };
        drop(q);
        // One panicking job must not take the worker (and every later job) with it.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(item.job));
        if result.is_err() {
            eprintln!("zenith: a worker job panicked");
        }
        shared.running.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn lowest_priority_value_runs_first() {
        let pool = Pool::new("test", 1);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        // Hold the only worker so all three jobs are queued before any runs.
        pool.submit(0, move || {
            let _ = gate_rx.recv();
        });
        let (tx, rx) = mpsc::channel();
        for (priority, name) in [(5, "last"), (1, "first"), (3, "middle")] {
            let tx = tx.clone();
            pool.submit(priority, move || tx.send(name).expect("send"));
        }
        gate_tx.send(()).expect("open gate");
        let order: Vec<_> = (0..3).map(|_| rx.recv().expect("job ran")).collect();
        assert_eq!(order, ["first", "middle", "last"]);
    }

    #[test]
    fn a_panicking_job_does_not_kill_the_worker() {
        let pool = Pool::new("test", 1);
        pool.submit(0, || panic!("expected test panic"));
        let (tx, rx) = mpsc::channel();
        pool.submit(1, move || tx.send(1).expect("send"));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(1));
    }

    #[test]
    fn wait_idle_returns_once_everything_has_run() {
        let pool = Pool::new("test", 4);
        let done = Arc::new(AtomicUsize::new(0));
        for i in 0..100 {
            let done = done.clone();
            pool.submit(i, move || {
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        pool.wait_idle();
        assert_eq!(done.load(Ordering::SeqCst), 100);
    }
}
