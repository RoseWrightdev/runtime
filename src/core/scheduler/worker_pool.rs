use crossbeam::deque::Stealer;
use crossbeam::sync::Unparker;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::executor::context::Context as ExecutorContext;
use crate::core::executor::local_queue::LocalQueue;
use crate::core::executor::worker::Worker;
use crate::core::runtime::context::Context as RuntimeContext;
use crate::core::scheduler::scheduler::Scheduler;
use crate::core::scheduler::task::TaskRef;

pub(crate) struct Pool {
    workers: std::sync::Mutex<Vec<Worker>>,
    stealers: Arc<[Stealer<TaskRef>]>,
    unparkers: Vec<Unparker>,
    next_unparker: AtomicUsize,
}

impl Pool {
    pub fn new(num_workers: usize) -> Self {
        let mut workers = Vec::with_capacity(num_workers);
        let mut stealers = Vec::with_capacity(num_workers);
        let mut unparkers = Vec::with_capacity(num_workers);

        for index in 0..num_workers {
            let worker = Worker::new(
                index,
                Self::steal_global,
                Self::steal_local,
                Self::drive_reactor,
            );

            stealers.push(worker.get_stealer());
            unparkers.push(worker.get_unparker());
            workers.push(worker);
        }

        Self {
            workers: std::sync::Mutex::new(workers),
            stealers: Arc::from(stealers),
            unparkers,
            next_unparker: AtomicUsize::new(0),
        }
    }

    pub fn start(&self, scheduler: Arc<Scheduler>) {
        let workers = {
            let mut lock = self.workers.lock().unwrap();
            std::mem::take(&mut *lock)
        };
        let stealers = self.stealers.clone();

        for mut worker in workers {
            let scheduler = scheduler.clone();
            let stealers = stealers.clone();
            let index = worker.get_index();

            std::thread::spawn(move || {
                // Initialize Runtime context
                let _rt_guard = RuntimeContext::enter(scheduler);

                // Initialize Executor context (worker index, stealers, and local queue handle)
                let queue_ptr = worker.get_queue_ptr();
                ExecutorContext::with(|ctx| {
                    ctx.worker_index = Some(index);
                    ctx.stealers = Some(Arc::new(stealers));
                    ctx.local_queue_ptr = Some(queue_ptr);
                });

                // Start execution loop
                worker.run();
            });
        }
    }

    pub fn steal_global() -> Option<TaskRef> {
        RuntimeContext::current().and_then(|rt| rt.steal_global())
    }

    pub fn steal_local(dest: &mut LocalQueue) -> Option<TaskRef> {
        ExecutorContext::with(|ctx| {
            if let (Some(index), Some(any_stealers)) = (ctx.worker_index, &ctx.stealers) {
                if let Some(stealers) = any_stealers.downcast_ref::<Arc<[Stealer<TaskRef>]>>() {
                    let stealers: &[Stealer<TaskRef>] = &**stealers;
                    for (i, stealer) in stealers.iter().enumerate() {
                        if i == index {
                            continue;
                        }

                        if let Some(task) = dest.steal_into(stealer) {
                            return Some(task);
                        }
                    }
                }
            }
            None
        })
    }

    pub fn drive_reactor(timeout: Option<std::time::Duration>) -> usize {
        if let Some(scheduler) = RuntimeContext::current() {
            scheduler.reactor.drive(timeout).unwrap_or(0)
        } else {
            0
        }
    }

    pub(crate) fn unpark_all(&self) {
        for unparker in &self.unparkers {
            unparker.unpark();
        }
    }

    pub(crate) fn notify_many(&self, count: usize) {
        if self.unparkers.is_empty() || count == 0 {
            return;
        }
        
        let len = self.unparkers.len();
        let start = self.next_unparker.fetch_add(count, Ordering::Relaxed);
        
        for i in 0..count {
            let index = (start + i) % len;
            self.unparkers[index].unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_construction() {
        let pool = Pool::new(4);

        // Check unparkers harvested
        assert_eq!(pool.unparkers.len(), 4);

        // Check stealers harvested
        assert_eq!(pool.stealers.len(), 4);

        // Check workers held before start
        {
            let lock = pool.workers.lock().unwrap();
            assert_eq!(lock.len(), 4);
        }
    }

    #[test]
    fn test_pool_unparking_round_robin() {
        let pool = Pool::new(4);
        
        // Initial next_unparker is 0
        assert_eq!(pool.next_unparker.load(Ordering::Relaxed), 0);
        
        // Notify 2 should unpark 0 and 1, next should be 2
        pool.notify_many(2);
        assert_eq!(pool.next_unparker.load(Ordering::Relaxed), 2);
        
        // Notify 3 should unpark 2, 3, and wrap to 0, next should be 5
        pool.notify_many(3);
        assert_eq!(pool.next_unparker.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_empty_pool() {
        let pool = Pool::new(0);
        assert_eq!(pool.unparkers.len(), 0);
        assert_eq!(pool.stealers.len(), 0);
        
        // Should not panic
        pool.notify_many(1);
        pool.unpark_all();
    }
}
