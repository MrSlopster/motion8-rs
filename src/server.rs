//! Cemuhook UDP server: request handling + data streaming.
//!
//! Single-threaded: the owner calls `tick()` at the send rate.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::dsu::{
    self, build_data_packet, build_info_packet, build_version_packet, parse_request, MotionSample,
    Request, SharedBegin,
};
use crate::source::{ControllerSnapshot, MotionSource};

pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_SLOTS: usize = 4;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub send_rate_hz: u32,
    /// IP to bind, e.g. "127.0.0.1" or "0.0.0.0".
    pub bind_addr: String,
    /// Overridable for tests; production uses CLIENT_TIMEOUT.
    pub client_timeout: Option<Duration>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: dsu::DEFAULT_PORT,
            send_rate_hz: 100,
            bind_addr: "127.0.0.1".to_string(),
            client_timeout: None,
        }
    }
}

struct Subscriber {
    request: dsu::DataRequest,
    last_request: Instant,
}

pub struct Server<S: MotionSource> {
    socket: UdpSocket,
    source: S,
    subscribers: HashMap<SocketAddr, Subscriber>,
    /// Per (client, slot) data-packet counters.
    packet_no: HashMap<(SocketAddr, u8), u32>,
    server_id: u32,
    timeout: Duration,
    recv_buf: Vec<u8>,
}

impl<S: MotionSource> Server<S> {
    /// Bind to the configured port (or an OS-assigned port when
    /// `config.port == 0`, used by tests).
    pub fn bind(config: &ServerConfig, source: S, server_id: u32) -> std::io::Result<Self> {
        let socket = UdpSocket::bind((config.bind_addr.as_str(), config.port))?;
        Ok(Self {
            socket,
            source,
            subscribers: HashMap::new(),
            packet_no: HashMap::new(),
            server_id,
            timeout: config.client_timeout.unwrap_or(CLIENT_TIMEOUT),
            recv_buf: vec![0u8; 2048],
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().expect("bound socket")
    }

    fn controllers_by_slot(&mut self) -> Vec<ControllerSnapshot> {
        self.source.poll();
        let mut v = self.source.controllers();
        v.truncate(MAX_SLOTS);
        v
    }

    fn handle_request(
        &mut self,
        n: usize,
        src: SocketAddr,
        controllers: &[ControllerSnapshot],
    ) -> bool {
        let Ok(request) = parse_request(&self.recv_buf[..n]) else {
            return false; // malformed input: drop silently
        };
        match request {
            Request::Version => {
                let packet = build_version_packet(self.server_id);
                self.socket.send_to(&packet, src).is_ok()
            }
            Request::Info { slots } => {
                let mut sent = false;
                for &slot in &slots {
                    let snap = controllers.iter().find(|c| c.slot == slot);
                    let begin = match snap {
                        Some(c) => SharedBegin {
                            slot,
                            state: if c.connected {
                                dsu::SLOT_STATE_CONNECTED
                            } else {
                                dsu::SLOT_STATE_DISCONNECTED
                            },
                            model: dsu::MODEL_FULL_GYRO,
                            connection: c.connection,
                            mac: c.mac,
                            battery: c.battery,
                        },
                        None => SharedBegin {
                            slot,
                            state: dsu::SLOT_STATE_DISCONNECTED,
                            model: 0,
                            connection: 0,
                            mac: [0; 6],
                            battery: 0,
                        },
                    };
                    let packet = build_info_packet(&begin, self.server_id);
                    sent |= self.socket.send_to(&packet, src).is_ok();
                }
                sent
            }
            Request::Data(req) => {
                self.subscribers.insert(
                    src,
                    Subscriber {
                        request: req,
                        last_request: Instant::now(),
                    },
                );
                false // no immediate response; streaming starts next tick
            }
        }
    }

    fn send_data_for(&mut self, req: &dsu::DataRequest, src: SocketAddr, controllers: &[ControllerSnapshot]) -> bool {
        let mut sent = false;
        for c in controllers {
            if !c.connected {
                continue;
            }
            let matches = match req {
                dsu::DataRequest::All => true,
                dsu::DataRequest::BySlot(s) => *s == c.slot,
                dsu::DataRequest::ByMac(m) => *m == c.mac,
            };
            if !matches {
                continue;
            }
            if let Some(sample) = self.source.motion(c.slot) {
                let no = {
                    let e = self.packet_no.entry((src, c.slot)).or_insert(0);
                    let v = *e;
                    *e = v.wrapping_add(1);
                    v
                };
                let begin = SharedBegin {
                    slot: c.slot,
                    state: dsu::SLOT_STATE_CONNECTED,
                    model: dsu::MODEL_FULL_GYRO,
                    connection: c.connection,
                    mac: c.mac,
                    battery: c.battery,
                };
                let packet = build_data_packet(&begin, no, &sample, self.server_id);
                sent |= self.socket.send_to(&packet, src).is_ok();
            }
        }
        sent
    }

    /// One server tick: pump the source, handle incoming requests, stream
    /// data packets to active subscribers. Returns true if any packet was
    /// sent (useful for tests).
    pub fn tick(&mut self) -> bool {
        self.socket.set_nonblocking(true).ok();
        let mut controllers: Option<Vec<ControllerSnapshot>> = None;
        let mut sent = false;

        // Drain incoming requests.
        loop {
            match self.socket.recv_from(&mut self.recv_buf) {
                Ok((n, src)) => {
                    if controllers.is_none() {
                        controllers = Some(self.controllers_by_slot());
                    }
                    sent |= self.handle_request(n, src, controllers.as_ref().unwrap());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                Err(_) => break,
            }
        }

        // Refresh source state if no request forced it already.
        if controllers.is_none() {
            controllers = Some(self.controllers_by_slot());
        }

        // Stream data to active subscribers.
        let now = Instant::now();
        let expired: Vec<SocketAddr> = self
            .subscribers
            .iter()
            .filter(|(_, s)| now.duration_since(s.last_request) > self.timeout)
            .map(|(a, _)| *a)
            .collect();
        for a in expired {
            self.subscribers.remove(&a);
        }
        let requests: Vec<(SocketAddr, dsu::DataRequest)> = self
            .subscribers
            .iter()
            .map(|(a, s)| (*a, s.request.clone()))
            .collect();
        let controllers = controllers.unwrap();
        for (src, req) in requests {
            sent |= self.send_data_for(&req, src, &controllers);
        }
        sent
    }

    /// Run forever at the configured rate. (Production entry point.)
    pub fn run(mut self, rate_hz: u32) -> ! {
        let period = Duration::from_nanos(1_000_000_000 / rate_hz.max(1) as u64);
        loop {
            self.tick();
            std::thread::sleep(period);
        }
    }
}
