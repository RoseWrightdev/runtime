use crate::core::executor::worker::Worker;
use crate::core::scheduler::task::Task;
use crossbeam::deque::Stealer;
use crossbeam::sync::{Parker, Unparker};
use std::sync::Arc;

pub(crate) struct Pool {
    workers: Vec<Worker>,
    stealers: Vec<Stealer<Arc<Task>>>,
    unparkers: Vec<Unparker>,
}

impl Pool {
    pub fn new(num_workers: usize) -> Self {
        let mut workers = vec![];
        let mut stealers = vec![];
        let mut unparkers = vec![];

        for index in 0..num_workers {
            let worker = Worker::new(
                index,
                Self::steal_global,
                Self::steal_local,
                Self::drive_reactor, // We can stub this for now
            );

            stealers.push(worker.get_stealer());
            unparkers.push(worker.get_unparker());
            workers.push(worker);
        }

        Self {
            workers,
            stealers,
            unparkers,
        }
    }

    fn steal_global() -> Option<Arc<Task>> {
        None
    }

    fn steal_local() -> Option<Arc<Task>> {
        None
    }

    fn drive_reactor() {
        unimplemented!()
    }
}
