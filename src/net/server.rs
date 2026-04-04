use crate::net::tcp;

struct Server {}

impl Server {
    pub fn new(tcp: tcp::AsyncTcpListener) {
                crate::spawn(async move {
            loop {
                while let Ok((stream, _)) = tcp.accept().await {
                    
            }
        }
        });
    }
}