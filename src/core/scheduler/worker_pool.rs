use std::sync::Arc;
use crossbeam::deque::Steal;
use crossbeam::deque::Stealer;
use crossbeam::sync::Unparker;

use crate::core::executor::context::Context as ExecutorContext;
use crate::core::executor::worker::Worker;
use crate::core::runtime::context::Context as RuntimeContext;
use crate::core::scheduler::task::Task;

pub(crate) struct Pool {
    workers: std::sync::Mutex<Vec<Worker>>,
    stealers: Arc<[Stealer<Arc<Task>>]>,
    unparkers: Vec<Unparker>,
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
        }
    }

    pub fn start(&self, scheduler: Arc<crate::core::scheduler::scheduler::Scheduler>) {
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

                // Initialize Executor context (worker index and stealers)
                ExecutorContext::with(|ctx| {
                    ctx.worker_index = Some(index);
                    ctx.stealers = Some(Arc::new(stealers));
                });

                // Start execution loop
                worker.run();
            });
        }
    }

    pub fn drive_reactor() {
        todo!("Stub for now, will be implemented with Mio")
    }

    pub fn steal_global() -> Option<Arc<Task>> {
        RuntimeContext::current().and_then(|rt| rt.steal_global())
    }

    pub fn steal_local() -> Option<Arc<Task>> {
        ExecutorContext::with(|ctx| {
            if let (Some(index), Some(any_stealers)) = (ctx.worker_index, &ctx.stealers) {
                if let Some(stealers) = any_stealers
                    .downcast_ref::<Arc<[Stealer<Arc<Task>>]>>() {
                        for (i, stealer) in stealers.iter().enumerate() {
                            if i == index {
                                continue;
                            }

                            loop {
                                match stealer.steal() {
                                    Steal::Success(task) => return Some(task),
                                    Steal::Retry => continue,
                                    Steal::Empty => break,
                                }
                            }
                        }
                }
            }
            None
        })
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
}
