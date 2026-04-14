use std::io;
use std::net::SocketAddr;
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
        let stream = MioTcpStream::connect(addr)?;
        let async_stream = Self::from_mio(stream)?;

        // Wait for the connection to be established by waiting for writability
        std::future::poll_fn(|cx| {
            // In Mio/Unix, a non-blocking connect is complete when the socket becomes writable.
            // We check for errors using take_error().
            match async_stream.inner.take_error() {
                Ok(Some(e)) => Poll::Ready(Err(e)),
                Ok(None) => {
                    // Check if it's connected. If it is, peer_addr() will succeed.
                    match async_stream.inner.peer_addr() {
                        Ok(_) => Poll::Ready(Ok(())),
                        Err(ref e) if e.kind() == io::ErrorKind::NotConnected => {
                            async_stream.reactor.add_waker(async_stream.token, Interest::WRITABLE, cx.waker().clone());
                            Poll::Pending
                        }
                        Err(e) => Poll::Ready(Err(e)),
                    }
                }
                Err(e) => Poll::Ready(Err(e)),
            }
        }).await?;

        Ok(async_stream)
    }

    pub fn from_mio(mut stream: MioTcpStream) -> io::Result<Self> {
        let scheduler = RuntimeContext::current()
            .expect("AsyncTcpStream must be created within a Taiga context");
        
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
                self.reactor.clear_readiness(self.token, Interest::READABLE);
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
                self.reactor.clear_readiness(self.token, Interest::WRITABLE);
                self.reactor.add_waker(self.token, Interest::WRITABLE, cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.peer_addr()
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        std::future::poll_fn(|cx| self.poll_read(cx, buf)).await
    }

    pub async fn write(&self, buf: &[u8]) -> io::Result<usize> {
        std::future::poll_fn(|cx| self.poll_write(cx, buf)).await
    }
}

impl Drop for AsyncTcpStream {
    fn drop(&mut self) {
        let _ = self.reactor.deregister(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::AsyncTcpListener;

    #[test]
    fn test_tcp_connect_fail() {
        crate::utils::block_on_timeout(async {
            // Try connecting to a port that is unlikely to be open
            let addr = "127.0.0.1:1".parse().unwrap();
            let result = AsyncTcpStream::connect(addr).await;
            assert!(result.is_err());
        }, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_tcp_read_write() {
        let addr = "127.0.0.1:0".parse().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();

        crate::utils::block_on_timeout(async move {
            let listener = AsyncTcpListener::bind(addr).unwrap();
            let addr = listener.local_addr().unwrap();

            let server_handle = crate::spawn(async move {
                let (stream, _): (AsyncTcpStream, SocketAddr) = listener.accept().await.unwrap();
                let mut buf = [0u8; 11];
                let mut read_bytes = 0;
                while read_bytes < buf.len() {
                    let n = std::future::poll_fn(|cx| stream.poll_read(cx, &mut buf[read_bytes..])).await.unwrap();
                    if n == 0 { break; }
                    read_bytes += n;
                }
                assert_eq!(&buf, b"hello taiga");
                
                let mut written = 0;
                let reply = b"hello back";
                while written < reply.len() {
                    let n = std::future::poll_fn(|cx| stream.poll_write(cx, &reply[written..])).await.unwrap();
                    written += n;
                }
                tx.send(()).unwrap();
            });

            let stream = AsyncTcpStream::connect(addr).await.unwrap();
            
            // Write to stream
            let mut written = 0;
            let data = b"hello taiga";
            while written < data.len() {
                match std::future::poll_fn(|cx| stream.poll_write(cx, &data[written..])).await {
                    Ok(n) => written += n,
                    Err(e) => panic!("Write error: {}", e),
                }
            }

            // Read from stream
            let mut read_buf = [0u8; 10];
            let mut read_bytes = 0;
            while read_bytes < read_buf.len() {
                match std::future::poll_fn(|cx| stream.poll_read(cx, &mut read_buf[read_bytes..])).await {
                    Ok(n) => {
                        if n == 0 { break; }
                        read_bytes += n;
                    }
                    Err(e) => panic!("Read error: {}", e),
                }
            }
            assert_eq!(&read_buf, b"hello back");
            server_handle.await.unwrap();
        }, std::time::Duration::from_secs(5));

        rx.recv().unwrap();
    }
}
