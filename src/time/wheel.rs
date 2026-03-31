use std::task::Waker;
use std::time::Instant;

const WHEEL_SIZE: usize = 64;
const NUM_LEVELS: usize = 6;

/// A single entry in the timer wheel.
struct TimerEntry {
    deadline: Instant,
    waker: Waker,
}

/// A Hierarchical Hashed Wheel Timer
/// Provides O(1) amortized timer management by categorizing
/// timeouts into buckets of different resolutions.
pub(crate) struct TimerWheel {
    /// 6 levels, each with 64 slots. 
    /// Level 0: 1ms resolution
    /// Level 1: 64ms resolution
    /// Level 2: 4.096s resolution
    /// Level 3: ~262s resolution
    /// Level 4: ~4.6h resolution
    /// Level 5: ~12 days resolution
    levels: [Vec<Vec<TimerEntry>>; NUM_LEVELS],
    current_tick: u64,
    start_time: Instant,
}

impl TimerWheel {
    pub fn new(start_time: Instant) -> Self {
        let levels = std::array::from_fn(|_| {
            let mut slots = Vec::with_capacity(WHEEL_SIZE);
            for _ in 0..WHEEL_SIZE {
                slots.push(Vec::new());
            }
            slots
        });

        Self {
            levels,
            current_tick: 0,
            start_time,
        }
    }

    /// Insert a waker to be triggered at the specified deadline.
    pub fn insert(&mut self, deadline: Instant, waker: Waker) {
        let delta_ms = deadline.saturating_duration_since(self.start_time).as_millis() as u64;
        
        // Find the appropriate level for this duration
        let relative_tick = delta_ms.saturating_sub(self.current_tick);

        for level in 0..NUM_LEVELS {
            let level_range = 1u64 << (6 * (level + 1));
            // Check if it fits in this level's resolution (64 slots * level resolution)
            if relative_tick < level_range || level == NUM_LEVELS - 1 {
                let slot = ((self.current_tick + relative_tick) >> (6 * level)) as usize % WHEEL_SIZE;
                self.levels[level][slot].push(TimerEntry { deadline, waker });
                return;
            }
        }
    }

    /// Advance the timer wheel by the specified number of ticks (1ms each).
    /// Returns a list of wakers that have expired.
    pub fn tick(&mut self, now: Instant) -> Vec<Waker> {
        let target_tick = now.saturating_duration_since(self.start_time).as_millis() as u64;
        let mut expired = Vec::new();

        while self.current_tick < target_tick {
            self.current_tick += 1;
            
            // 1. Process Level 0 (1ms resolution)
            let slot = (self.current_tick % WHEEL_SIZE as u64) as usize;
            let wakers = std::mem::take(&mut self.levels[0][slot]);
            for entry in wakers {
                expired.push(entry.waker);
            }

            // 2. Cascade higher levels if they wrap around
            for level in 1..NUM_LEVELS {
                let level_tick_mask = (1u64 << (6 * level)) - 1;
                if self.current_tick & level_tick_mask == 0 {
                    let slot = (self.current_tick >> (6 * level)) as usize % WHEEL_SIZE;
                    let to_cascade = std::mem::take(&mut self.levels[level][slot]);
                    for entry in to_cascade {
                        self.insert(entry.deadline, entry.waker); 
                    }
                } else {
                    break;
                }
            }
        }

        expired
    }

    /// Returns the deadline of the next expiring timer, if any.
    pub fn next_expiration(&self) -> Option<Instant> {
        // 1. Check Level 0 first (highest resolution)
        for i in 0..WHEEL_SIZE {
            let slot = (self.current_tick + i as u64) as usize % WHEEL_SIZE;
            if !self.levels[0][slot].is_empty() {
                return self.levels[0][slot].iter().map(|e| e.deadline).min();
            }
        }

        // 2. Check higher levels
        for level in 1..NUM_LEVELS {
            for i in 0..WHEEL_SIZE {
                let slot = ((self.current_tick >> (6 * level)) + i as u64) as usize % WHEEL_SIZE;
                if !self.levels[level][slot].is_empty() {
                    return self.levels[level][slot].iter().map(|e| e.deadline).min();
                }
            }
        }

        None
    }
}
