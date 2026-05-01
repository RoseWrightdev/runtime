use std::time::Instant;

/// Per-worker statistics used to dynamically tune the scheduler.
/// 
/// This is a streamlined version of the Tokio "Self-Tuning" heuristic. It uses 
/// an Exponentially-Weighted Moving Average (EWMA) of task poll times to 
/// determine how often a worker should check the global injector queue.
pub(crate) struct Stats {
    /// Moving average of task poll time in nanoseconds.
    avg_poll_time_ns: f64,
    /// Timestamp when the current batch of tasks started processing.
    batch_start: Instant,
    /// Number of tasks polled in the current batch.
    tasks_in_batch: u32,
}

/// Smoothing factor for the EWMA. Lower = more stable, higher = faster response.
const ALPHA: f64 = 0.1; 
/// The target time (in nanoseconds) we want to spend processing local work 
/// before checking the global queue. Default is 200 microseconds.
const TARGET_GLOBAL_INTERVAL_NS: f64 = 200_000.0; 
/// Minimum tasks to poll before checking global (prevents excessive lock fighting).
const MIN_INTERVAL: u32 = 2;
/// Maximum tasks to poll before checking global (prevents starvation of global tasks).
const MAX_INTERVAL: u32 = 127;

impl Stats {
    pub fn new() -> Self {
        Self {
            // Seed with a value that results in an interval of ~61 (our old default).
            avg_poll_time_ns: TARGET_GLOBAL_INTERVAL_NS / 61.0,
            batch_start: Instant::now(),
            tasks_in_batch: 0,
        }
    }

    /// Mark the start of a local processing burst.
    pub fn start_batch(&mut self) {
        self.batch_start = Instant::now();
        self.tasks_in_batch = 0;
    }

    /// Record that a task was polled.
    pub fn record_poll(&mut self) {
        self.tasks_in_batch += 1;
    }

    /// End the current batch and update the moving average.
    pub fn end_batch(&mut self) {
        if self.tasks_in_batch == 0 {
            return;
        }

        let elapsed = self.batch_start.elapsed().as_nanos() as f64;
        let mean_poll_time = elapsed / self.tasks_in_batch as f64;

        // Weight the alpha by the number of tasks in this batch.
        // This makes large batches have a larger impact on the average.
        let weighted_alpha = 1.0 - (1.0 - ALPHA).powf(self.tasks_in_batch as f64);
        
        self.avg_poll_time_ns = (weighted_alpha * mean_poll_time) + ((1.0 - weighted_alpha) * self.avg_poll_time_ns);
    }

    /// Calculate the ideal number of tasks to poll before checking the global queue.
    pub fn tuned_interval(&self) -> u32 {
        let interval = (TARGET_GLOBAL_INTERVAL_NS / self.avg_poll_time_ns) as u32;
        interval.clamp(MIN_INTERVAL, MAX_INTERVAL)
    }
}