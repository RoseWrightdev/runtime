use std::io;
use std::task::Waker;
use std::time::Duration;

use mio::{Events, Interest, Poll, Registry, Token};
use parking_lot::Mutex;
use slab::Slab;

pub struct IoSourceState {
    pub read_waker: Option<Waker>,
    pub write_waker: Option<Waker>,
    pub read_ready: bool,
    pub write_ready: bool,
}

pub struct Reactor {
    poll: Mutex<Poll>,
    events: Mutex<Events>,
    wakers: Mutex<Slab<IoSourceState>>,
    waker: mio::Waker,
}

const WAKER_TOKEN: Token = Token(usize::MAX);

impl Reactor {
    pub fn new() -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = mio::Waker::new(poll.registry(), WAKER_TOKEN)?;
        Ok(Reactor {
            poll: Mutex::new(poll),
            events: Mutex::new(Events::with_capacity(1024)),
            wakers: Mutex::new(Slab::with_capacity(1024)),
            waker,
        })
    }

    pub fn registry(&self) -> Registry {
        self.poll
            .lock()
            .registry()
            .try_clone()
            .expect("Failed to clone Mio registry")
    }

    /// Performs a poll of the OS for I/O events with an optional timeout.
    /// Returns the number of events processed.
    pub fn drive(&self, timeout: Option<Duration>) -> io::Result<usize> {
        let events = self.events.try_lock();
        let poll = self.poll.try_lock();

        // Only one worker can drive the reactor at a time
        let (mut events, mut poll) = match (events, poll) {
            (Some(e), Some(p)) => (e, p),
            _ => return Ok(0), // Someone else is driving
        };

        // Poll with the provided timeout
        poll.poll(&mut events, timeout)?;
        let n = events.iter().count();

        if !events.is_empty() {
            let mut wakers = self.wakers.lock();
            for event in events.iter() {
                let token = event.token();
                if let Some(state) = wakers.get_mut(token.0) {
                    if event.is_readable() {
                        state.read_ready = true;
                        if let Some(waker) = state.read_waker.take() {
                            waker.wake();
                        }
                    }
                    if event.is_writable() {
                        state.write_ready = true;
                        if let Some(waker) = state.write_waker.take() {
                            waker.wake();
                        }
                    }
                }
            }
        }

        Ok(n)
    }

    /// Returns true if there are any I/O sources registered with wakers.
    pub fn has_wakers(&self) -> bool {
        let wakers = self.wakers.lock();
        !wakers.is_empty()
    }

    /// Registers a new I/O source and returns a Token.
    pub fn alloc_token(&self) -> Token {
        let mut wakers = self.wakers.lock();
        let entry = wakers.vacant_entry();
        let token = Token(entry.key());
        entry.insert(IoSourceState {
            read_waker: None,
            write_waker: None,
            read_ready: false,
            write_ready: false,
        });
        token
    }

    /// Updates the waker for a specific interest (Read/Write) on a token.
    /// If the token is already ready for that interest, the waker is triggered immediately.
    pub fn add_waker(&self, token: Token, interest: Interest, waker: Waker) {
        let mut wakers = self.wakers.lock();
        if let Some(state) = wakers.get_mut(token.0) {
            if interest.is_readable() {
                if state.read_ready {
                    waker.wake();
                } else {
                    state.read_waker = Some(waker);
                }
            } else if interest.is_writable() {
                if state.write_ready {
                    waker.wake();
                } else {
                    state.write_waker = Some(waker);
                }
            }
        }
    }

    /// Clears the readiness flag for a specific interest.
    /// This should be called when an I/O operation returns WouldBlock.
    pub fn clear_readiness(&self, token: Token, interest: Interest) {
        let mut wakers = self.wakers.lock();
        if let Some(state) = wakers.get_mut(token.0) {
            if interest.is_readable() {
                state.read_ready = false;
            } else if interest.is_writable() {
                state.write_ready = false;
            }
        }
    }

    /// Wakes up the reactor if it is currently blocked in drive().
    pub fn wakeup(&self) {
        let _ = self.waker.wake();
    }

    pub fn deregister(&self, token: Token) {
        let mut wakers = self.wakers.lock();
        if wakers.contains(token.0) {
            wakers.remove(token.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reactor_token_lifecycle() {
        let reactor = Reactor::new().unwrap();
        
        let token1 = reactor.alloc_token();
        let token2 = reactor.alloc_token();
        assert_ne!(token1, token2);
        
        reactor.deregister(token1);
        let token3 = reactor.alloc_token();
        // Slab should reuse the slot for token1
        assert_eq!(token1, token3);
    }

    #[test]
    fn test_reactor_waker_trigger() {
        // We'll use a standard TCP stream to actually trigger a reactor event.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        
        let reactor = Reactor::new().unwrap();
        let mut stream = mio::net::TcpStream::connect(addr).unwrap();
        let token = reactor.alloc_token();
        
        reactor.registry().register(&mut stream, token, Interest::WRITABLE).unwrap();
        
        let (woken_tx, woken_rx) = std::sync::mpsc::channel();
        
        // A simple waker that sends to woken_tx
        struct TestWaker(std::sync::mpsc::Sender<()>);
        impl std::task::Wake for TestWaker {
            fn wake(self: std::sync::Arc<Self>) {
                let _ = self.0.send(());
            }
        }
        
        let waker = std::sync::Arc::new(TestWaker(woken_tx)).into();
        
        // Add waker to reactor
        reactor.add_waker(token, Interest::WRITABLE, waker);
        
        // Now drive the reactor. Since the stream is connecting to a local port 
        // that is already bound, it should become writable almost immediately.
        let mut events_processed = 0;
        for _ in 0..10 {
            events_processed += reactor.drive(Some(std::time::Duration::from_millis(10))).unwrap();
            if woken_rx.try_recv().is_ok() {
                break;
            }
        }
        
        assert!(events_processed > 0, "Reactor should have processed at least one event");
    }

    #[test]
    fn test_has_wakers_correctness() {
        let reactor = Reactor::new().unwrap();
        assert!(!reactor.has_wakers());
        
        let token = reactor.alloc_token();
        assert!(reactor.has_wakers());
        
        reactor.deregister(token);
        assert!(!reactor.has_wakers());
    }
}
