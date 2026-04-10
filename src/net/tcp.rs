use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::future::Future;
use std::pin::Pin;

use crate::executor::Handle;
use polling::Event;
use socket2::{Domain, Type, Socket};
use libc;

pub struct AsyncTcpListener {
    inner: TcpListener,
    reactor: Arc<crate::executor::Reactor>,
}

impl AsyncTcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let inner = TcpListener::bind(addr)?;
        inner.set_nonblocking(true)?;
        
        let handle = Handle::current();
        handle.reactor.add(&inner, Event::none(inner.as_raw_fd() as usize));
        
        // Set SO_LINGER(0) for benchmark efficiency
        let socket = unsafe { socket2::Socket::from_raw_fd(inner.as_raw_fd()) };
        let _ = socket.set_linger(Some(std::time::Duration::ZERO));
        std::mem::forget(socket);
        
        Ok(Self { inner, reactor: handle.reactor.clone() })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.inner.local_addr()
    }

    pub async fn accept(&self) -> io::Result<(AsyncTcpStream, std::net::SocketAddr)> {
        AcceptFuture { listener: self }.await
    }
}

struct AcceptFuture<'a> {
    listener: &'a AsyncTcpListener,
}

impl Future for AcceptFuture<'_> {
    type Output = io::Result<(AsyncTcpStream, std::net::SocketAddr)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.listener.inner.accept() {
            Ok((stream, addr)) => {
                stream.set_nonblocking(true)?;
                let async_stream = AsyncTcpStream::new(stream, self.listener.reactor.clone())?;
                Poll::Ready(Ok((async_stream, addr)))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                Handle::current().register_io(self.listener.inner.as_raw_fd() as usize, cx.waker().clone(), true, false);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

pub struct AsyncTcpStream {
    inner: TcpStream,
    reactor: Arc<crate::executor::Reactor>,
}

impl AsyncTcpStream {
    pub(crate) fn new(inner: TcpStream, reactor: Arc<crate::executor::Reactor>) -> io::Result<Self> {
        inner.set_nonblocking(true)?;
        reactor.add(&inner, Event::none(inner.as_raw_fd() as usize));
        Ok(Self { inner, reactor })
    }

    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addrs = addr.to_socket_addrs()?;
        let mut last_err = None;

        for address in addrs {
            let domain = if address.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
            let socket = Socket::new(domain, Type::STREAM, None)?;
            socket.set_nonblocking(true)?;
            
            // Set SO_LINGER(0) for benchmark efficiency
            let _ = socket.set_linger(Some(std::time::Duration::ZERO));
            
            match socket.connect(&address.into()) {
                Ok(_) => {
                    let stream = TcpStream::from(socket);
                    let reactor = Handle::current().reactor.clone();
                    return Self::new(stream, reactor);
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock || 
                              e.raw_os_error() == Some(libc::EINPROGRESS) => {
                    let stream = TcpStream::from(socket);
                    let reactor = Handle::current().reactor.clone();
                    let async_stream = Self::new(stream, reactor)?;
                    
                    // Wait for the Reactor to signal 'writable' (connection complete)
                    ConnectFuture { stream: &async_stream }.await?;
                    return Ok(async_stream);
                }
                Err(e) => last_err = Some(e),
            }
        }
        
        Err(last_err.unwrap_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "could not resolve address")))
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        ReadFuture { stream: self, buf }.await
    }

    pub async fn write(&self, buf: &[u8]) -> io::Result<usize> {
        WriteFuture { stream: self, buf }.await
    }

    pub async fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let n = self.write(buf).await?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "failed to write whole buffer"));
            }
            buf = &buf[n..];
        }
        Ok(())
    }
}

struct ReadFuture<'a, 'b> {
    stream: &'a AsyncTcpStream,
    buf: &'b mut [u8],
}

impl Future for ReadFuture<'_, '_> {
    type Output = io::Result<usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Use the Read trait implementation for &TcpStream
        match (&this.stream.inner).read(this.buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                Handle::current().register_io(this.stream.inner.as_raw_fd() as usize, cx.waker().clone(), true, false);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

struct WriteFuture<'a, 'b> {
    stream: &'a AsyncTcpStream,
    buf: &'b [u8],
}

impl Future for WriteFuture<'_, '_> {
    type Output = io::Result<usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Use the Write trait implementation for &TcpStream
        match (&this.stream.inner).write(this.buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                Handle::current().register_io(this.stream.inner.as_raw_fd() as usize, cx.waker().clone(), false, true);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Drop for AsyncTcpListener {
    fn drop(&mut self) {
        self.reactor.delete(&self.inner);
    }
}

impl Drop for AsyncTcpStream {
    fn drop(&mut self) {
        self.reactor.delete(&self.inner);
    }
}

impl AsRawFd for AsyncTcpStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.inner.as_raw_fd()
    }
}

struct ConnectFuture<'a> {
    stream: &'a AsyncTcpStream,
}

impl Future for ConnectFuture<'_> {
    type Output = io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.stream.inner.take_error() {
            Ok(None) => {
                // No error. Now check if we are actually connected by trying to get the peer address.
                // On some platforms, a writable socket might still not be connected.
                if self.stream.inner.peer_addr().is_ok() {
                    return Poll::Ready(Ok(()));
                }
                
                // Still connecting... register for writable event.
                Handle::current().register_io(self.stream.inner.as_raw_fd() as usize, cx.waker().clone(), false, true);
                Poll::Pending
            }
            Ok(Some(err)) => {
                // Connection failed
                Poll::Ready(Err(err))
            }
            Err(e) => {
                Poll::Ready(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Runtime;

    #[test]
    fn test_async_tcp_bind_and_local_addr() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let listener = AsyncTcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            assert_eq!(addr.ip(), std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
            assert!(addr.port() > 0);
        });
    }

    #[test]
    fn test_async_tcp_connect_and_transmit() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let listener = AsyncTcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let handle = crate::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 11];
                stream.read(&mut buf).await.unwrap();
                assert_eq!(&buf, b"hello world");
                stream.write_all(b"foobar").await.unwrap();
            });

            let client = AsyncTcpStream::connect(addr).await.unwrap();
            client.write_all(b"hello world").await.unwrap();
            let mut buf = [0u8; 6];
            client.read(&mut buf).await.unwrap();
            assert_eq!(&buf, b"foobar");

            handle.await.unwrap();
        });
    }
}
