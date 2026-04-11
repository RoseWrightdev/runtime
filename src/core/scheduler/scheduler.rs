pub(crate) struct Scheduler {}

impl Scheduler {
    pub fn new() -> Self {}

    pub fn spawn_internal<F, T>(future: F) 
    where 
        F: Future<Output = T> + Send + 'static 
    {

    }
}