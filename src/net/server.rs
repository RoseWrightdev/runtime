use crate::{AsyncTcpStream, net::tcp, net::status};
use std::io;

pub struct Server {
    tcp: tcp::AsyncTcpListener,
}

impl Server {
    pub fn new(tcp: tcp::AsyncTcpListener) -> Self {
        Self { tcp }
    }

    pub fn run(self) {
        crate::spawn(async move {
            loop {
                match self.tcp.accept().await {
                    Ok((stream, _)) => {
                        crate::spawn(async move {
                            let resp = Self::handle_connection(stream).await; {
                                
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
        });
    }

    async fn handle_connection<Code>(mut _stream: AsyncTcpStream) -> io::Result<>
    {
        // Implementation for HTTP parsing would go here
        Ok(())
    }
}
