use std::collections::HashMap;
use std::task::Waker;
use std::time::Instant;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use crossbeam::queue::SegQueue;
use polling::{Events, Poller, Event, AsSource, AsRawSource};

use crate::time::wheel::TimerWheel;

pub(crate) enum Registration {
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

pub struct Reactor {
    poller: Poller,
    registrations: SegQueue<Registration>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) notified: AtomicBool,
}

impl Reactor {
    pub(crate) fn new() -> Self {
        Self {
            poller: Poller::new().expect("Failed to create Poller"),
            registrations: SegQueue::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            notified: AtomicBool::new(false),
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
        self.registrations.push(Registration::Delete { key });
        self.notify();
    }

    /// Submits a waker to be called when the specified key is ready for reading/writing.
    pub(crate) fn register(&self, key: usize, waker: Waker, read: bool, write: bool) {
        self.registrations.push(Registration::IO {
            key,
            waker,
            read,
            write,
        });
        self.notify();
    }

    /// Submit a new timer interest to the reactor from any thread.
    pub(crate) fn register_timer(&self, deadline: Instant, waker: Waker) {
        self.registrations.push(Registration::Timer { deadline, waker });
        self.notify();
    }

    /// Wake up the reactor's poller wait loop.
    pub(crate) fn notify(&self) {
        if !self.notified.swap(true, Ordering::SeqCst) {
            let _ = self.poller.notify();
        }
    }

    /// The main event loop for the reactor. This should be spawned on a dedicated thread.
    pub(crate) fn run(&self) {
        // Hybrid Registry: 1024-slot array for the hot-path (Fds are usually small) 
        // fallback to HashMap for extremely high FDs.
        let mut hot_slots: Vec<Option<(Option<Waker>, Option<Waker>)>> = vec![None; 1024];
        let mut cold_slots: HashMap<usize, (Option<Waker>, Option<Waker>)> = HashMap::new();
        let mut timer_wheel = TimerWheel::new(Instant::now());
        let mut events = Events::new();

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }

            // 1. Drain registration queue into local state
            while let Some(reg) = self.registrations.pop() {
                match reg {
                    Registration::IO {
                        key,
                        waker,
                        read,
                        write,
                    } => {
                        let state = if key < 1024 {
                            hot_slots[key].get_or_insert((None, None))
                        } else {
                            cold_slots.entry(key).or_insert((None, None))
                        };
                        
                        if read {
                            state.0 = Some(waker);
                        } else if write {
                            state.1 = Some(waker);
                        }

                        // Update poller interest
                        let mut interest = Event::none(key);
                        interest.readable = state.0.is_some();
                        interest.writable = state.1.is_some();

                        unsafe {
                            let fd = std::os::unix::io::BorrowedFd::borrow_raw(
                                key as std::os::unix::io::RawFd,
                            );
                            let _ = self.poller.modify(fd, interest);
                        }
                    }
                    Registration::Timer { deadline, waker } => {
                        timer_wheel.insert(deadline, waker);
                    }
                    Registration::Delete { key } => {
                        if key < 1024 {
                            hot_slots[key] = None;
                        } else {
                            cold_slots.remove(&key);
                        }
                    }
                }
            }

            // 2. Wait for OS events or Timer expiration
            events.clear();

            let timeout = timer_wheel.next_expiration().map(|deadline| {
                deadline.saturating_duration_since(Instant::now())
            });

            // Clear notified flag BEFORE waiting to ensure we catch any notifications during wait
            self.notified.store(false, Ordering::SeqCst);
            let _ = self.poller.wait(&mut events, timeout);

            // 3. Dispatch I/O events
            for ev in events.iter() {
                let state = if ev.key < 1024 {
                    hot_slots[ev.key].as_mut()
                } else {
                    cold_slots.get_mut(&ev.key)
                };

                if let Some(state) = state {
                    let mut r_waker = None;
                    let mut w_waker = None;
                    
                    if ev.readable {
                        r_waker = state.0.take();
                    }
                    if ev.writable {
                        w_waker = state.1.take();
                    }

                    // Direct wake: Zero allocations here. 
                    // Batching is implicitly handled by Scheduler's searching_workers check
                    if let Some(w) = r_waker { w.wake(); }
                    if let Some(w) = w_waker { w.wake(); }

                    // Clear triggered interest in poller
                    let mut interest = Event::none(ev.key);
                    interest.readable = state.0.is_some();
                    interest.writable = state.1.is_some();

                    unsafe {
                        let fd = std::os::unix::io::BorrowedFd::borrow_raw(
                            ev.key as std::os::unix::io::RawFd,
                        );
                        let _ = self.poller.modify(fd, interest);
                    }

                    if state.0.is_none() && state.1.is_none() {
                        if ev.key < 1024 {
                            hot_slots[ev.key] = None;
                        } else {
                            cold_slots.remove(&ev.key);
                        }
                    }
                }
            }

            // 4. Tick the timer wheel and dispatch expired timers
            let expired = timer_wheel.tick(Instant::now());
            for waker in expired {
                waker.wake();
            }
        }
    }
}
