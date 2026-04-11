use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use mio::net::TcpStream as MioTcpStream;
use mio::{Interest, Token};

use crate::core::runtime::context::Context as RuntimeContext;
use crate::core::reactor::Reactor;

pub struct AsyncTcpStream {
    inner: MioTcpStream,
    token: Token,
    reactor: std::sync::Arc<Reactor>,
}

impl AsyncTcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let mut stream = MioTcpStream::connect(addr)?;
        
        let scheduler = RuntimeContext::current()
            .expect("AsyncTcpStream::connect must be called within a Taiga context");
        
        let reactor = scheduler.reactor.clone();
        let token = reactor.alloc_token();
        
        reactor.registry().register(&mut stream, token, Interest::READABLE | Interest::WRITABLE)?;

        Ok(AsyncTcpStream {
            inner: stream,
            token,
            reactor,
        })
    }

    pub fn poll_read(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        match io::Read::read(&mut &self.inner, buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.reactor.add_waker(self.token, Interest::READABLE, cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    pub fn poll_write(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match io::Write::write(&mut &self.inner, buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.reactor.add_waker(self.token, Interest::WRITABLE, cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Drop for AsyncTcpStream {
    fn drop(&mut self) {
        let _ = self.reactor.deregister(self.token);
    }
}
