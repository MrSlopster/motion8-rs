//! Server integration tests: a real UDP client against a synthetic source.

use std::net::UdpSocket;
use std::time::Duration;

use crate::dsu::{self, DataRequest, MotionSample, MSG_DATA, SLOT_STATE_CONNECTED};
use crate::server::*;
use crate::source::{ControllerSnapshot, MotionSource};

const SID: u32 = 0xfeedface;

/// Synthetic source: one BT controller on slot 0, one USB on slot 1 (dying battery).
struct FakeSource {
    sample: MotionSample,
}

impl MotionSource for FakeSource {
    fn poll(&mut self) {}

    fn controllers(&self) -> Vec<ControllerSnapshot> {
        vec![
            ControllerSnapshot {
                slot: 0,
                connected: true,
                connection: dsu::CONN_BLUETOOTH,
                mac: [0xaa; 6],
                battery: 0x05,
                name: "Fake Ultimate 2".into(),
            },
            ControllerSnapshot {
                slot: 1,
                connected: true,
                connection: dsu::CONN_USB,
                mac: [0xbb; 6],
                battery: 0x01,
                name: "Fake USB pad".into(),
            },
        ]
    }

    fn motion(&mut self, _slot: u8) -> Option<MotionSample> {
        Some(self.sample)
    }
}

fn test_config() -> ServerConfig {
    ServerConfig {
        port: 0, // OS-assigned
        send_rate_hz: 100,
        bind_addr: "127.0.0.1".into(),
        client_timeout: Some(Duration::from_millis(80)),
    }
}

/// Drain one datagram and return (type, payload-after-type).
fn recv_type(socket: &UdpSocket) -> (u32, Vec<u8>) {
    let mut buf = [0u8; 2048];
    let (n, _src) = socket.recv_from(&mut buf).expect("datagram expected");
    let t = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
    (t, buf[20..n].to_vec())
}

#[test]
fn version_request_gets_version_response() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    client.send(&client_packet(dsu::MSG_VERSION, &[])).unwrap();
    server.tick();

    let (t, body) = recv_type(&client);
    assert_eq!(t, dsu::MSG_VERSION);
    assert_eq!(u16::from_le_bytes([body[0], body[1]]), dsu::PROTOCOL_VERSION);
}

#[test]
fn info_request_reports_both_slots_with_state() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    client
        .send(&client_packet(
            dsu::MSG_INFO,
            &{
                let mut b = 2i32.to_le_bytes().to_vec();
                b.push(0);
                b.push(3);
                b
            },
        ))
        .unwrap();
    server.tick();

    // Two packets, one per requested slot.
    let (t1, b1) = recv_type(&client);
    let (t2, b2) = recv_type(&client);
    assert_eq!(t1, dsu::MSG_INFO);
    assert_eq!(t2, dsu::MSG_INFO);
    // Slot 0 connected via BT
    assert_eq!(b1[0], 0);
    assert_eq!(b1[1], dsu::SLOT_STATE_CONNECTED);
    assert_eq!(b1[2], dsu::MODEL_FULL_GYRO);
    assert_eq!(b1[3], dsu::CONN_BLUETOOTH);
    assert_eq!(&b1[4..10], &[0xaa; 6]);
    assert_eq!(b1[10], 0x05, "battery");
    assert_eq!(b1[11], 0, "trailing NUL");
    // Slot 3 not managed by source → disconnected, zeroed
    assert_eq!(b2[0], 3);
    assert_eq!(b2[1], dsu::SLOT_STATE_DISCONNECTED);
    assert_eq!(&b2[2..12], &[0; 10]);
}

#[test]
fn data_request_by_slot_streams_motion_sample() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    client
        .send(&client_packet(dsu::MSG_DATA, &[1, 0]))
        .unwrap();
    assert!(server.tick(), "expected at least one data packet");
    assert!(server.tick(), "streaming continues while subscribed");

    let (t1, b1) = recv_type(&client);
    let (t2, b2) = recv_type(&client);
    assert_eq!(t1, MSG_DATA);
    assert_eq!(t2, MSG_DATA);
    assert_eq!(b1[0], 0, "slot");
    assert_eq!(b1[1], SLOT_STATE_CONNECTED);
    // packet numbers increment per client
    let n1 = u32::from_le_bytes(b1[12..16].try_into().unwrap());
    let n2 = u32::from_le_bytes(b2[12..16].try_into().unwrap());
    assert_eq!(n2, n1 + 1);
    // sample values round-trip
    assert_eq!(
        f32::from_le_bytes(b1[56..60].try_into().unwrap()),
        0.25,
        "accel x in g"
    );
    assert_eq!(
        f32::from_le_bytes(b1[68..72].try_into().unwrap()),
        45.0,
        "gyro pitch deg/s"
    );
}

#[test]
fn data_request_by_mac_matches_controller() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    let mut body = vec![2u8];
    body.extend_from_slice(&[0xaa; 6]);
    client.send(&client_packet(dsu::MSG_DATA, &body)).unwrap();
    assert!(server.tick());

    let (_, b) = recv_type(&client);
    assert_eq!(b[0], 0, "slot of matched controller");
    assert_eq!(&b[4..10], &[0xaa; 6], "mac echoed");
}

#[test]
fn data_request_all_streams_both_controllers() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    client.send(&client_packet(dsu::MSG_DATA, &[0])).unwrap();
    assert!(server.tick());

    let mut slots = vec![];
    for _ in 0..2 {
        let (_, b) = recv_type(&client);
        slots.push(b[0]);
    }
    slots.sort();
    assert_eq!(slots, vec![0, 1], "All streams every connected controller");
}

#[test]
fn subscriber_times_out_without_re_requests() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    client.send(&client_packet(dsu::MSG_DATA, &[1, 0])).unwrap();
    assert!(server.tick());

    // Don't re-request; wait past the (shortened) timeout.
    std::thread::sleep(Duration::from_millis(120));
    assert!(!server.tick(), "no packets after subscriber timeout");

    // Re-request resumes streaming.
    client.send(&client_packet(dsu::MSG_DATA, &[1, 0])).unwrap();
    assert!(server.tick());
}

#[test]
fn mac_registered_but_disconnected_controller_sends_nothing() {
    // slot 1 is connected in FakeSource; ask for a mac nobody has
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    let mut body = vec![2u8];
    body.extend_from_slice(&[0x77; 6]);
    client.send(&client_packet(dsu::MSG_DATA, &body)).unwrap();
    assert!(!server.tick(), "no data for unknown MAC");
}

#[test]
fn garbage_datagrams_do_not_kill_the_server() {
    let mut server = Server::bind(&test_config(), FakeSource { sample: sample() }, SID).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client.connect(server.local_addr()).unwrap();

    for junk in [&[0u8; 4][..], &[0xffu8; 300][..], b"DSUC".to_vec().as_slice()] {
        client.send(junk).unwrap();
    }
    server.tick(); // must not panic

    // server still functional
    client.send(&client_packet(dsu::MSG_VERSION, &[])).unwrap();
    server.tick();
    let (t, _) = recv_type(&client);
    assert_eq!(t, dsu::MSG_VERSION);
}

fn sample() -> MotionSample {
    MotionSample {
        accel: [0.25, -1.0, 0.0],
        gyro: [45.0, -10.0, 5.0],
        timestamp_us: 12345,
    }
}

fn client_packet(msg_type: u32, body: &[u8]) -> Vec<u8> {
    use crate::dsu::{MAGIC_CLIENT, PROTOCOL_VERSION};
    let mut p = Vec::new();
    p.extend_from_slice(MAGIC_CLIENT);
    p.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    p.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
    p.extend_from_slice(&[0; 4]);
    p.extend_from_slice(&0x31337u32.to_le_bytes());
    p.extend_from_slice(&msg_type.to_le_bytes());
    p.extend_from_slice(body);
    let mut zeroed = p.clone();
    zeroed[8..12].fill(0);
    let crc = crate::dsu::tests::public_crc(&zeroed);
    p[8..12].copy_from_slice(&crc.to_le_bytes());
    p
}
