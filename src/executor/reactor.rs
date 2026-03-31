use std::collections::HashMap;
use std::task::Waker;
use std::time::Instant;
use std::sync::{Arc, Mutex};
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

pub struct ReactorState {
    hot_slots: Vec<Option<(Option<Waker>, Option<Waker>)>>,
    cold_slots: HashMap<usize, (Option<Waker>, Option<Waker>)>,
    timer_wheel: TimerWheel,
}

pub struct Reactor {
    poller: Poller,
    registrations: SegQueue<Registration>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) notified: AtomicBool,
    state: Mutex<ReactorState>,
    events: Mutex<Events>,
}

impl Reactor {
    pub(crate) fn new() -> Self {
        Self {
            poller: Poller::new().expect("Failed to create Poller"),
            registrations: SegQueue::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            notified: AtomicBool::new(false),
            state: Mutex::new(ReactorState {
                hot_slots: vec![None; 1024],
                cold_slots: HashMap::new(),
                timer_wheel: TimerWheel::new(Instant::now()),
            }),
            events: Mutex::new(Events::new()),
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

    /// Attempts to poll the reactor if no other worker is currently polling.
    /// Returns true if it successfully acquired the lock and polled, false otherwise.
    pub(crate) fn try_poll(&self) -> bool {
        if let Ok(mut state) = self.state.try_lock() {
            if self.shutdown.load(Ordering::Acquire) {
                return false;
            }

            // 1. Drain registration queue into local state
            while let Some(reg) = self.registrations.pop() {
                match reg {
                    Registration::IO { key, waker, read, write } => {
                        let slot = if key < 1024 {
                            state.hot_slots[key].get_or_insert((None, None))
                        } else {
                            state.cold_slots.entry(key).or_insert((None, None))
                        };
                        
                        if read { slot.0 = Some(waker.clone()); }
                        if write { slot.1 = Some(waker.clone()); }

                        let mut interest = Event::none(key);
                        interest.readable = slot.0.is_some();
                        interest.writable = slot.1.is_some();

                        unsafe {
                            let fd = std::os::unix::io::BorrowedFd::borrow_raw(key as std::os::unix::io::RawFd);
                            let _ = self.poller.modify(fd, interest);
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

            // 2. Wait for OS events or Timer expiration
            let mut events = self.events.lock().unwrap();
            events.clear();

            let timeout = state.timer_wheel.next_expiration().map(|deadline| {
                deadline.saturating_duration_since(Instant::now())
            });

            // Clear notified flag BEFORE waiting to ensure we catch any notifications during wait
            self.notified.store(false, Ordering::SeqCst);
            // Block inside epoll_wait while holding the Mutex lock
            let _ = self.poller.wait(&mut events, timeout);

            // 3. Dispatch I/O events
            for ev in events.iter() {
                let slot = if ev.key < 1024 {
                    state.hot_slots[ev.key].as_mut()
                } else {
                    state.cold_slots.get_mut(&ev.key)
                };

                if let Some(slot) = slot {
                    let mut r_waker = None;
                    let mut w_waker = None;
                    
                    if ev.readable { r_waker = slot.0.take(); }
                    if ev.writable { w_waker = slot.1.take(); }

                    // Direct wake
                    if let Some(w) = r_waker { w.wake(); }
                    if let Some(w) = w_waker { w.wake(); }

                    // Clear triggered interest in poller
                    let mut interest = Event::none(ev.key);
                    interest.readable = slot.0.is_some();
                    interest.writable = slot.1.is_some();

                    unsafe {
                        let fd = std::os::unix::io::BorrowedFd::borrow_raw(ev.key as std::os::unix::io::RawFd);
                        let _ = self.poller.modify(fd, interest);
                    }

                    if slot.0.is_none() && slot.1.is_none() {
                        if ev.key < 1024 {
                            state.hot_slots[ev.key] = None;
                        } else {
                            state.cold_slots.remove(&ev.key);
                        }
                    }
                }
            }

            // 4. Tick the timer wheel and dispatch expired timers
            let expired = state.timer_wheel.tick(Instant::now());
            for waker in expired {
                waker.wake();
            }

            return true;
        }
        
        false
    }
}
