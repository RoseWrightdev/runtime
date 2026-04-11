use crate::core::executor::worker::Worker;
use crate::core::task::Task;
use crossbeam::deque::Stealer;
use crossbeam::sync::{Parker, Unparker};
use std::sync::Arc;

pub(crate) struct Pool {
    workers: Vec<Worker>,
    stealers: Vec<Stealer<Arc<Task>>>,
    parkers: Vec<Parker>,
    unparkers: Vec<Unparker>,
}

impl Pool {
    pub fn new(num_workers: usize) -> Self {
        let mut workers = vec![];
        let mut stealers = vec![];
        let mut parkers = vec![];
        let mut unparkers = vec![];

        for index in 0..num_workers {
            let worker = Worker::new(
                index,
                Self::steal_global,
                Self::steal_local,
                unimplemented!(),
                unimplemented!(),
                unimplemented!(),
            );
            let stealer = worker.get_stealer();
            let parker = worker.get_parker();
            let unparker = worker.get_unparker();

            workers.push(worker);
            stealers.push(stealer);
            parkers.push(parker);
            unparkers.push(unparker);
        }

        Self {
            workers,
            stealers,
            parkers,
            unparkers,
        }
    }

    fn steal_global() -> Option<Arc<Task>> {
        unimplemented!()
    }

    fn steal_local() -> Option<Arc<Task>> {
        unimplemented!()
    }
}
