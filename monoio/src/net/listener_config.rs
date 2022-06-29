/// Custom listener config
#[derive(Debug, Clone)]
pub struct ListenerConfig {
    /// Whether to enable reuse_port.
    pub reuse_port: bool,
    /// Whether to enable reuse_addr.
    pub reuse_addr: bool,
    /// Whether to enable ip_transparent.
    pub ip_transparent: bool,
    /// Bind address for connecting sockets.
    pub bind_address: Option<std::net::SocketAddr>,
    /// Backlog size.
    pub backlog: i32,
    /// Send buffer size or None to use default.
    pub send_buf_size: Option<usize>,
    /// Recv buffer size or None to use default.
    pub recv_buf_size: Option<usize>,
}

impl Default for ListenerConfig {
    fn default() -> Self {
        Self {
            reuse_port: true,
            reuse_addr: true,
            ip_transparent: false,
            bind_address: None,
            backlog: 1024,
            send_buf_size: None,
            recv_buf_size: None,
        }
    }
}

impl ListenerConfig {
    /// Enable SO_REUSEPORT
    #[must_use]
    pub fn reuse_port(mut self, reuse_port: bool) -> Self {
        self.reuse_port = reuse_port;
        self
    }

    /// Enable SO_REUSEADDR
    #[must_use]
    pub fn reuse_addr(mut self, reuse_addr: bool) -> Self {
        self.reuse_addr = reuse_addr;
        self
    }

    /// Enable IP_TRANSPARENT
    #[must_use]
    pub fn ip_transparent(mut self, ip_transparent: bool) -> Self {
        self.ip_transparent = ip_transparent;
        self
    }

    /// Specify socket bind address
    #[must_use]
    pub fn bind_address(mut self, bind_address: std::net::SocketAddr) -> Self {
        self.bind_address = Some(bind_address);
        self
    }

    /// Specify backlog
    #[must_use]
    pub fn backlog(mut self, backlog: i32) -> Self {
        self.backlog = backlog;
        self
    }

    /// Specify SO_SNDBUF
    #[must_use]
    pub fn send_buf_size(mut self, send_buf_size: usize) -> Self {
        self.send_buf_size = Some(send_buf_size);
        self
    }

    /// Specify SO_RCVBUF
    #[must_use]
    pub fn recv_buf_size(mut self, recv_buf_size: usize) -> Self {
        self.recv_buf_size = Some(recv_buf_size);
        self
    }
}
