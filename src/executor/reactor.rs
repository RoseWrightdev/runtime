use std::collections::HashMap;
use std::task::Waker;
use std::time::Instant;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use crossbeam::queue::SegQueue;
use crossbeam::utils::CachePadded;
use polling::{Events, Poller, Event, AsSource, AsRawSource};

use crate::time::wheel::TimerWheel;

/// A registration request sent to the reactor.
pub enum Registration {
    /// Register interest in I/O events for a file descriptor.
    IO {
        /// The file descriptor (or handle) being registered.
        key: usize,
        /// The waker to be called when the event occurs.
        waker: Waker,
        /// Interest in read readiness.
        read: bool,
        /// Interest in write readiness.
        write: bool,
    },
    /// Register a timer to expire at a specific instant.
    Timer {
        /// The instant at which the timer should fire.
        deadline: Instant,
        /// The waker to be called when the timer expires.
        waker: Waker,
    },
    /// Remove interest in a file descriptor.
    Delete {
        /// The key to be removed.
        key: usize,
    },
}

/// The internal mutable state of the reactor.
pub struct ReactorState {
    /// Fast lookup for file descriptors with values < 1024.
    hot_slots: Vec<Option<(Option<Waker>, Option<Waker>)>>,
    /// Fallback lookup for file descriptors with values >= 1024.
    cold_slots: HashMap<usize, (Option<Waker>, Option<Waker>)>,
    /// The hierarchical timer wheel used to manage timeouts efficiently.
    timer_wheel: TimerWheel,
}

/// The I/O and timer driver for the Taiga runtime.
/// 
/// The `Reactor` is responsible for monitoring file descriptors for events and 
/// managing scheduled timers. It uses an OS-specific poller (via the `polling` 
/// crate) and a hierarchical timer wheel.
/// 
/// Multiple worker threads can attempt to drive the reactor simultaneously, 
/// but only one will succeed in becoming the "pollster" at any given time.
pub struct Reactor {
    /// The underlying OS poller.
    poller: Poller,

    /// Queue of pending registrations sent by worker threads.
    registrations: CachePadded<SegQueue<Vec<Registration>>>,
    /// Flag indicating that the poller has been notified of new work.
    pub(crate) notified: CachePadded<AtomicBool>,

    /// Global shutdown signal.
    pub(crate) shutdown: Arc<AtomicBool>,
    /// The internal state, protected by a mutex for synchronization when 
    /// dispatching events or processing registrations.
    state: Mutex<ReactorState>,
    events: Mutex<Events>,
    is_polling: AtomicBool,
}

impl Reactor {
    pub(crate) fn new() -> Self {
        Self {
            poller: Poller::new().expect("Failed to create Poller"),
            registrations: CachePadded::new(SegQueue::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            notified: CachePadded::new(AtomicBool::new(false)),
            state: Mutex::new(ReactorState {
                hot_slots: vec![None; 1024],
                cold_slots: HashMap::new(),
                timer_wheel: TimerWheel::new(Instant::now()),
            }),
            events: Mutex::new(Events::new()),
            is_polling: AtomicBool::new(false),
        }
    }

    /// Register a file descriptor with the poller.
    pub(crate) fn add(&self, source: impl AsSource + AsRawSource, event: Event) {
        unsafe {
            self.poller.add(source, event).expect("Failed to add source to poller");
        }
    }


    /// Unregister a file descriptor from the poller.
    pub(crate) fn delete<S: AsSource + std::os::unix::io::AsRawFd>(&self, source: &S) {
        let key = source.as_raw_fd() as usize;
        let _ = self.poller.delete(source);
        // Also notify the reactor loop to clear any pending wakers for this key
        self.push_batch(vec![Registration::Delete { key }]);
    }

    /// Pushes a batch of registrations to the reactor and triggers a wake-up.
    /// 
    /// This is typically called by worker threads to submit buffered I/O or 
    /// timer requests.
    pub(crate) fn push_batch(&self, batch: Vec<Registration>) {
        if !batch.is_empty() {
            self.registrations.push(batch);
            self.notify();
        }
    }

    /// Wake up the reactor's poller wait loop.
    pub(crate) fn notify(&self) {
        if !self.notified.swap(true, Ordering::SeqCst) {
            let _ = self.poller.notify();
        }
    }

    /// Attempts to drive the reactor.
    /// 
    /// Only one thread can drive the reactor at a time. If another thread is 
    /// already polling, this method returns `false` immediately.
    /// 
    /// While driving, the reactor will:
    /// 1. Process all pending registrations.
    /// 2. Wait for OS events with a timeout based on the next timer expiration.
    /// 3. Dispatch wakers for all triggered I/O events and expired timers.
    pub(crate) fn try_poll(&self) -> bool {
        // Only one worker can be the Pollster at a time.
        if self.is_polling.swap(true, Ordering::Acquire) {
            return false;
        }

        // Use a guard to ensure is_polling is reset even on panic
        struct PollGuard<'a>(&'a AtomicBool);
        impl Drop for PollGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = PollGuard(&self.is_polling);

        if self.shutdown.load(Ordering::Acquire) {
            return false;
        }

        // 1. Process registrations
        let mut state = self.state.lock().unwrap();
        self.handle_registrations(&mut state);

        // 2. Wait for OS events - we drop the state lock during kernel wait
        let mut events = self.events.lock().unwrap();
        events.clear();

        let timeout = state.timer_wheel.next_expiration().map(|deadline| {
            deadline.saturating_duration_since(Instant::now())
        });

        // Release state lock so other workers can check or push
        drop(state);

        self.notified.store(false, Ordering::SeqCst);
        let _ = self.poller.wait(&mut events, timeout);

        // 3. Re-acquire state lock to dispatch events and handle any new registrations
        let mut state = self.state.lock().unwrap();
        
        // Final drain of registrations before dispatching events
        self.handle_registrations(&mut state);

        for ev in events.iter() {
            let slot = if ev.key < 1024 {
                state.hot_slots[ev.key].as_mut()
            } else {
                state.cold_slots.get_mut(&ev.key)
            };

            if let Some(slot) = slot {
                if ev.readable {
                    if let Some(w) = slot.0.take() { w.wake(); }
                }
                if ev.writable {
                    if let Some(w) = slot.1.take() { w.wake(); }
                }
            }
        }

        // Tick timer wheel and dispatch
        let expired = state.timer_wheel.tick(Instant::now());
        for waker in expired {
            waker.wake();
        }

        // Last-second check for registrations before releasing the lock and guard
        self.handle_registrations(&mut state);

        true
    }

    fn handle_registrations(&self, state: &mut ReactorState) {
        while let Some(batch) = self.registrations.pop() {
            for reg in batch {
                match reg {
                    Registration::IO { key, waker, read, write } => {
                        let slot = if key < 1024 {
                            state.hot_slots[key].get_or_insert((None, None))
                        } else {
                            state.cold_slots.entry(key).or_insert((None, None))
                        };
                        
                        let mut changed = false;
                        if read && slot.0.is_none() { 
                            slot.0 = Some(waker.clone());
                            changed = true;
                        }
                        if write && slot.1.is_none() { 
                            slot.1 = Some(waker.clone());
                            changed = true;
                        }

                        if changed {
                            let mut interest = Event::none(key);
                            interest.readable = slot.0.is_some();
                            interest.writable = slot.1.is_some();
                            unsafe {
                                let fd = std::os::unix::io::BorrowedFd::borrow_raw(key as std::os::unix::io::RawFd);
                                let _ = self.poller.modify(fd, interest);
                            }
                        }
                    }
                    Registration::Timer { deadline, waker } => {
                        state.timer_wheel.insert(deadline, waker);
                    }
                    Registration::Delete { key } => {
                        if key < 1024 {
                            state.hot_slots[key] = None;
                        } else {
                            state.cold_slots.remove(&key);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::task::noop_waker;

    #[test]
    fn test_reactor_new() {
        let reactor = Reactor::new();
        assert!(!reactor.notified.load(Ordering::SeqCst));
        assert!(!reactor.shutdown.load(Ordering::SeqCst));
    }

    #[test]
    fn test_push_batch() {
        let reactor = Reactor::new();
        let waker = noop_waker();
        let reg = Registration::IO { key: 1, waker, read: true, write: false };
        reactor.push_batch(vec![reg]);
        assert!(reactor.notified.load(Ordering::SeqCst));
        assert!(!reactor.registrations.is_empty());
    }

    #[test]
    fn test_notify() {
        let reactor = Reactor::new();
        reactor.notify();
        assert!(reactor.notified.load(Ordering::SeqCst));
        // Second notify should not change state but also not panic/error
        reactor.notify();
        assert!(reactor.notified.load(Ordering::SeqCst));
    }
}
