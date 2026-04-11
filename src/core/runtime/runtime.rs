
struct Runtime {
    scheduler: &Scheduler
}

impl Runtime {
    pub fn new() -> Self {
        
    }
    
    pub fn spawn<F, T>(self: &Arc<Self>, future: F) 
    where
        F: Future<Output = T> + Send + 'static,
        T: Any + Send + 'static,
    {
        self.scheduler.spawn_internal(future);
    }

    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {

        let (tx, rx) = std::sync::mpsc::channel();
        self.scheduler.spawn_internal(async move {
            let res = future.await;
            let _ = tx.send(res);
        });
        rx.recv().expect("Runtime internal channel closed")
    }
}