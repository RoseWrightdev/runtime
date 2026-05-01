use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::os::unix::io::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::Waker;
use std::time::Instant;

use crossbeam::queue::SegQueue;
use crossbeam::utils::CachePadded;
use polling::{AsRawSource, AsSource, Event, Events, Poller};

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

/// The I/O and timer event notification system.
///
/// The `Reactor` interfaces with the operating system's event notification
/// primitives (e.g., epoll on Linux, kqueue on macOS) to provide non-blocking
/// asynchronous I/O and timer management.
///
/// ## Operational Workflow
///
/// 1.  **Registration**: Asynchronous resources (sockets, timers) register
///     their interest in specific events by submitting a [`Registration`].
/// 2.  **Event Polling**: Worker threads take turns driving the reactor's
///     wait loop. The driver thread blocks on the OS poller until events
///     occur or the next timer expiration is reached.
/// 3.  **Task Rescheduling**: Upon event notification, the Reactor identifies
///     the associated wakers and triggers task re-scheduling via the
///     [`Scheduler`][crate::executor::Scheduler].
///
/// ## Efficient Timer Management
///
/// To support high-density timer sets, Taiga implements a **Hierarchical
/// Timer Wheel**. This structure allows for $O(1)$ insertion and efficient
/// expiration detection by bucketizing timers based on their deadlines.
pub struct Reactor {
    /// The underlying OS poller.
    poller: Poller,

    /// Queue of pending registrations sent by worker threads.
    registrations: CachePadded<SegQueue<Vec<Registration>>>,

    /// Flag indicating that the poller has been notified of new work.
    pub(crate) notified: CachePadded<AtomicBool>,

    /// Global shutdown signal.
    pub(crate) shutdown: Arc<AtomicBool>,

    /// The internal mutable state of the reactor, including timer wheels and I/O slots.
    ///
    /// SAFETY: This is protected by the `is_polling` atomic flag to ensure that
    /// only one thread at a time can access it. This allows us to use `UnsafeCell`
    /// instead of a `Mutex` to avoid locking overhead in the hot path.
    state: UnsafeCell<ReactorState>,

    /// Storage for events returned by the OS poller.
    ///
    /// SAFETY: Like `state`, this is guarded by `is_polling`. It is kept as a
    /// field to allow reusing the allocated memory across poll iterations.
    events: UnsafeCell<Events>,
    is_polling: AtomicBool,
}

// SAFETY: `Reactor` is `Sync` because all mutable access to its internal
// `UnsafeCell` fields (`state` and `events`) is strictly guarded by the
// `is_polling` atomic flag, which ensures mutual exclusion.
unsafe impl Sync for Reactor {}

impl Reactor {
    pub(crate) fn new() -> Self {
        Self {
            poller: Poller::new().expect("Failed to create Poller"),
            registrations: CachePadded::new(SegQueue::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            notified: CachePadded::new(AtomicBool::new(false)),
            state: UnsafeCell::new(ReactorState {
                hot_slots: vec![None; 1024],
                cold_slots: HashMap::new(),
                timer_wheel: TimerWheel::new(Instant::now()),
            }),
            events: UnsafeCell::new(Events::new()),
            is_polling: AtomicBool::new(false),
        }
    }

    /// Register a file descriptor with the poller.
    pub(crate) fn add(&self, source: impl AsSource + AsRawSource, event: Event) {
        // SAFETY: The caller must ensure that the source's file descriptor
        // remains valid for the duration of the registration.
        unsafe {
            self.poller
                .add(source, event)
                .expect("Failed to add source to poller");
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
        // Use SeqCst (Sequential Consistency) for the notification flag.
        // This ensures a total global ordering of notifications, preventing
        // missed wake-ups when multiple threads are interacting with the poller.
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
        // Acquire ordering ensures that once we become the pollster, we see
        // any state changes made by the previous pollster (released via drop).
        if self.is_polling.swap(true, Ordering::Acquire) {
            return false;
        }

        /// RAII guard to ensure `is_polling` is reset even if a panic occurs during polling.
        ///
        /// This follows the **RAII (Resource Acquisition Is Initialization)** pattern.
        /// The "acquisition" is the successful atomic swap above, and the "release"
        /// is handled automatically by this guard's `Drop` implementation.
        struct PollGuard<'a>(&'a AtomicBool);
        impl Drop for PollGuard<'_> {
            fn drop(&mut self) {
                // Release ordering ensures all our reactor state changes (made via
                // UnsafeCell) are visible to the next thread that acquires the
                // pollster role via an Acquire swap.
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = PollGuard(&self.is_polling);

        // Check for shutdown signal. Acquire ensures we see the latest
        // shutdown state set by the Runtime.
        if self.shutdown.load(Ordering::Acquire) {
            return false;
        }

        // SAFETY: We have successfully acquired the `is_polling` guard,
        // ensuring exclusive access to the `state` and `events` fields.
        let state = unsafe { &mut *self.state.get() };
        let events = unsafe { &mut *self.events.get() };

        // 1. Process registrations
        self.handle_registrations(state);

        // 2. Wait for OS events
        events.clear();

        let timeout = state
            .timer_wheel
            .next_expiration()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));

        // Reset notification flag before waiting. SeqCst ensures this reset
        // is visible globally before we enter the blocking kernel call.
        self.notified.store(false, Ordering::SeqCst);
        let _ = self.poller.wait(events, timeout);

        // 3. Dispatch events and handle any new registrations

        // Final drain of registrations before dispatching events
        self.handle_registrations(state);

        for ev in events.iter() {
            let slot = if ev.key < 1024 {
                state.hot_slots[ev.key].as_mut()
            } else {
                state.cold_slots.get_mut(&ev.key)
            };

            if let Some(slot) = slot {
                if ev.readable {
                    if let Some(w) = slot.0.take() {
                        w.wake();
                    }
                }
                if ev.writable {
                    if let Some(w) = slot.1.take() {
                        w.wake();
                    }
                }
            }
        }

        // Tick timer wheel and dispatch
        let expired = state.timer_wheel.tick(Instant::now());
        for waker in expired {
            waker.wake();
        }

        // Last-second check for registrations before releasing the guard
        self.handle_registrations(state);

        true
    }

    fn handle_registrations(&self, state: &mut ReactorState) {
        while let Some(batch) = self.registrations.pop() {
            for reg in batch {
                match reg {
                    Registration::IO {
                        key,
                        waker,
                        read,
                        write,
                    } => {
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
                            // SAFETY: `key` is a valid file descriptor that was previously
                            // registered with the poller. `borrow_raw` is used to create
                            // a temporary reference for the duration of the `modify` call.
                            unsafe {
                                let fd = BorrowedFd::borrow_raw(key as RawFd);
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
        let reg = Registration::IO {
            key: 1,
            waker,
            read: true,
            write: false,
        };
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
