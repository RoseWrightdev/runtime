use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll};

use mio::net::TcpListener as MioTcpListener;
use mio::{Interest, Token};

use crate::core::runtime::context::Context as RuntimeContext;
use crate::core::reactor::Reactor;
use crate::net::AsyncTcpStream;

pub struct AsyncTcpListener {
    inner: MioTcpListener,
    token: Token,
    reactor: std::sync::Arc<Reactor>,
}

impl AsyncTcpListener {
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let mut listener = MioTcpListener::bind(addr)?;
        
        let scheduler = RuntimeContext::current()
            .expect("AsyncTcpListener::bind must be called within a Taiga context");
        
        let reactor = scheduler.reactor.clone();
        let token = reactor.alloc_token();
        
        reactor.registry().register(&mut listener, token, Interest::READABLE)?;

        Ok(AsyncTcpListener {
            inner: listener,
            token,
            reactor,
        })
    }

    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<(AsyncTcpStream, SocketAddr)>> {
        match self.inner.accept() {
            Ok((stream, addr)) => {
                let async_stream = AsyncTcpStream::from_mio(stream)?;
                Poll::Ready(Ok((async_stream, addr)))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.reactor.clear_readiness(self.token, Interest::READABLE);
                self.reactor.add_waker(self.token, Interest::READABLE, cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    pub async fn accept(&self) -> io::Result<(AsyncTcpStream, SocketAddr)> {
        std::future::poll_fn(|cx| self.poll_accept(cx)).await
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

impl Drop for AsyncTcpListener {
    fn drop(&mut self) {
        let _ = self.reactor.deregister(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_listener_bind_success() {
        crate::utils::block_on_timeout(async {
            let addr = "127.0.0.1:0".parse().unwrap();
            let listener = AsyncTcpListener::bind(addr).unwrap();
            assert!(listener.local_addr().is_ok());
        }, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_listener_bind_fail() {
        crate::utils::block_on_timeout(async {
            // Bind two listeners to the same port
            let addr = "127.0.0.1:0".parse().unwrap();
            let listener1 = AsyncTcpListener::bind(addr).unwrap();
            let actual_addr = listener1.local_addr().unwrap();
            
            let result = AsyncTcpListener::bind(actual_addr);
            assert!(result.is_err());
        }, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_listener_accept_multiple() {
        let addr = "127.0.0.1:0".parse().unwrap();
        
        crate::utils::block_on_timeout(async move {
            let listener = AsyncTcpListener::bind(addr).unwrap();
            let actual_addr = listener.local_addr().unwrap();
            
            let num_clients = 5;
            let (tx, _rx) = std::sync::mpsc::channel();
            
            let mut client_handles = Vec::new();
            for i in 0..num_clients {
                let tx = tx.clone();
                client_handles.push(crate::spawn(async move {
                    let _stream = AsyncTcpStream::connect(actual_addr).await.expect("Client connect failed");
                    tx.send(i).unwrap();
                }));
            }
            
            for _ in 0..num_clients {
                let _ = listener.accept().await.expect("Accept failed");
            }
            
            for h in client_handles {
                h.await.unwrap();
            }
        }, std::time::Duration::from_secs(30));
    }
}
