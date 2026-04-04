use std::collections::HashMap;
use std::task::Waker;
use std::time::Instant;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use crossbeam::queue::SegQueue;
use crossbeam::utils::CachePadded;
use polling::{Events, Poller, Event, AsSource, AsRawSource};

use crate::time::wheel::TimerWheel;

pub enum Registration {
    IO {
        key: usize,
        waker: Waker,
        read: bool,
        write: bool,
    },
    Timer {
        deadline: Instant,
        waker: Waker,
    },
    Delete {
        key: usize,
    },
}

pub struct ReactorState {
    hot_slots: Vec<Option<(Option<Waker>, Option<Waker>)>>,
    cold_slots: HashMap<usize, (Option<Waker>, Option<Waker>)>,
    timer_wheel: TimerWheel,
}

pub struct Reactor {
    // setup
    poller: Poller,

    //worker -> reactor
    registrations: CachePadded<SegQueue<Vec<Registration>>>,
    pub(crate) notified: CachePadded<AtomicBool>,

    // poller only
    pub(crate) shutdown: Arc<AtomicBool>,
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

    /// Pushes a batch of registrations and notifies the reactor.
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

    /// Attempts to poll the reactor if no other worker is currently polling.
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
