use std::io;
use std::task::Waker;
use std::time::Duration;

use mio::{Events, Interest, Poll, Registry, Token};
use parking_lot::Mutex;
use slab::Slab;

pub struct IoSourceState {
    pub read_waker: Option<Waker>,
    pub write_waker: Option<Waker>,
}

pub struct Reactor {
    poll: Mutex<Poll>,
    events: Mutex<Events>,
    wakers: Mutex<Slab<IoSourceState>>,
}

impl Reactor {
    pub fn new() -> io::Result<Self> {
        Ok(Reactor {
            poll: Mutex::new(Poll::new()?),
            events: Mutex::new(Events::with_capacity(1024)),
            wakers: Mutex::new(Slab::with_capacity(1024)),
        })
    }

    pub fn registry(&self) -> Registry {
        self.poll
            .lock()
            .registry()
            .try_clone()
            .expect("Failed to clone Mio registry")
    }

    /// Performs a non-blocking poll of the OS for I/O events.
    pub fn drive(&self) -> io::Result<()> {
        let events = self.events.try_lock();
        let poll = self.poll.try_lock();

        // Only one worker can drive the reactor at a time
        let (mut events, mut poll) = match (events, poll) {
            (Some(e), Some(p)) => (e, p),
            _ => return Ok(()), // Someone else is driving
        };

        // Non-blocking poll
        poll.poll(&mut events, Some(Duration::ZERO))?;

        if !events.is_empty() {
            let mut wakers = self.wakers.lock();
            for event in events.iter() {
                let token = event.token();
                if let Some(state) = wakers.get_mut(token.0) {
                    if event.is_readable() {
                        if let Some(waker) = state.read_waker.take() {
                            waker.wake();
                        }
                    }
                    if event.is_writable() {
                        if let Some(waker) = state.write_waker.take() {
                            waker.wake();
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Registers a new I/O source and returns a Token.
    pub fn alloc_token(&self) -> Token {
        let mut wakers = self.wakers.lock();
        let entry = wakers.vacant_entry();
        let token = Token(entry.key());
        entry.insert(IoSourceState {
            read_waker: None,
            write_waker: None,
        });
        token
    }

    /// Updates the waker for a specific interest (Read/Write) on a token.
    pub fn add_waker(&self, token: Token, interest: Interest, waker: Waker) {
        let mut wakers = self.wakers.lock();
        if let Some(state) = wakers.get_mut(token.0) {
            if interest.is_readable() {
                state.read_waker = Some(waker);
            } else if interest.is_writable() {
                state.write_waker = Some(waker);
            }
        }
    }

    pub fn deregister(&self, token: Token) {
        let mut wakers = self.wakers.lock();
        if wakers.contains(token.0) {
            wakers.remove(token.0);
        }
    }
}
