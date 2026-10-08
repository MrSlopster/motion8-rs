//! Cemuhook UDP (DSU) protocol v1001 wire format.
//!
//! Pure data-in/data-out: encoding, decoding, CRC. No I/O here.
//! Reference: https://v1993.github.io/cemuhook-protocol/

use crc32fast::Hasher;

pub const PROTOCOL_VERSION: u16 = 1001;
pub const DEFAULT_PORT: u16 = 26760;

pub const MSG_VERSION: u32 = 0x100000;
pub const MSG_INFO: u32 = 0x100001;
pub const MSG_DATA: u32 = 0x100002;

pub const MAGIC_CLIENT: &[u8; 4] = b"DSUC";
pub const MAGIC_SERVER: &[u8; 4] = b"DSUS";

pub const SLOT_STATE_DISCONNECTED: u8 = 0;
pub const SLOT_STATE_CONNECTED: u8 = 2;
pub const MODEL_FULL_GYRO: u8 = 2;
pub const CONN_USB: u8 = 1;
pub const CONN_BLUETOOTH: u8 = 2;

/// How a data-stream subscriber identified itself (msg 0x100002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataRequest {
    /// bitmask 0: stream every connected controller
    All,
    /// bitmask 1
    BySlot(u8),
    /// bitmask 2
    ByMac([u8; 6]),
}

/// An incoming client request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Version,
    Info { slots: Vec<u8> },
    Data(DataRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    BadMagic,
    BadVersion,
    BadCrc,
    BadLength,
    UnknownType(u32),
    MalformedBody,
}

/// The 11-byte "shared response beginning" of info/data messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedBegin {
    pub slot: u8,
    pub state: u8,
    pub model: u8,
    pub connection: u8,
    pub mac: [u8; 6],
    pub battery: u8,
}

/// One motion sample, already in DSU units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSample {
    /// Accelerometer, in g. Axes as required by DSU.
    pub accel: [f32; 3],
    /// Gyroscope, deg/s, order: pitch, yaw, roll.
    pub gyro: [f32; 3],
    /// Motion timestamp in microseconds.
    pub timestamp_us: u64,
}

fn crc32(data: &[u8]) -> u32 {
    let mut h = Hasher::new();
    h.update(data);
    h.finalize()
}

fn write_header(out: &mut Vec<u8>, msg_type: u32, body_len: u16, server_id: u32) {
    out.extend_from_slice(MAGIC_SERVER);
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&[0; 4]); // CRC placeholder, filled below
    out.extend_from_slice(&server_id.to_le_bytes());
    out.extend_from_slice(&msg_type.to_le_bytes());
}

fn finish_with_crc(out: &mut Vec<u8>) {
    let mut packet = std::mem::take(out);
    let mut zeroed = packet.clone();
    zeroed[8..12].fill(0);
    let crc = crc32(&zeroed);
    packet[8..12].copy_from_slice(&crc.to_le_bytes());
    *out = packet;
}

/// Build a full server packet (header + type + body), CRC included.
pub fn encode_server_packet(msg_type: u32, body: &[u8], server_id: u32) -> Vec<u8> {
    let body_len = (4 + body.len()) as u16; // msg type counts toward length
    let mut out = Vec::with_capacity(16 + body_len as usize);
    write_header(&mut out, msg_type, body_len, server_id);
    out.extend_from_slice(body);
    finish_with_crc(&mut out);
    out
}

/// Response to a version request.
pub fn build_version_packet(server_id: u32) -> Vec<u8> {
    encode_server_packet(MSG_VERSION, &PROTOCOL_VERSION.to_le_bytes(), server_id)
}

/// Response to an info request for one slot (11-byte begin + `\0`).
pub fn build_info_packet(begin: &SharedBegin, server_id: u32) -> Vec<u8> {
    let mut body = begin.encode();
    body.push(0);
    encode_server_packet(MSG_INFO, &body, server_id)
}

impl SharedBegin {
    /// 11 bytes: slot, state, model, connection, mac[6], battery.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(11);
        v.push(self.slot);
        v.push(self.state);
        v.push(self.model);
        v.push(self.connection);
        v.extend_from_slice(&self.mac);
        v.push(self.battery);
        v
    }
}

/// Build a 100-byte data packet (header + type + 80-byte payload).
pub fn build_data_packet(
    begin: &SharedBegin,
    packet_no: u32,
    sample: &MotionSample,
    server_id: u32,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(80);
    payload.extend_from_slice(&begin.encode()); // 11
    payload.push(1); // connected
    payload.extend_from_slice(&packet_no.to_le_bytes()); // +4 = 16
    payload.extend_from_slice(&[0, 0]); // button bitmasks
    payload.push(0); // home
    payload.push(0); // touch button
    payload.extend_from_slice(&[128, 128, 128, 128]); // sticks neutral
    payload.extend_from_slice(&[0u8; 12]); // analog buttons
    payload.extend_from_slice(&[0u8; 12]); // two touch records
    payload.extend_from_slice(&sample.timestamp_us.to_le_bytes()); // +8 = 48
    for v in sample.accel {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    for v in sample.gyro {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    debug_assert_eq!(payload.len(), 80);
    encode_server_packet(MSG_DATA, &payload, server_id)
}

/// Parse and validate a client packet.
pub fn parse_request(packet: &[u8]) -> Result<Request, ParseError> {
    if packet.len() < 20 {
        return Err(ParseError::TooShort);
    }
    if &packet[0..4] != MAGIC_CLIENT {
        return Err(ParseError::BadMagic);
    }
    let version = u16::from_le_bytes([packet[4], packet[5]]);
    if version != PROTOCOL_VERSION {
        return Err(ParseError::BadVersion);
    }
    let body_len = u16::from_le_bytes([packet[6], packet[7]]) as usize;
    if body_len != packet.len() - 16 {
        return Err(ParseError::BadLength);
    }
    let mut zeroed = packet.to_vec();
    zeroed[8..12].fill(0);
    if crc32(&zeroed) != u32::from_le_bytes([packet[8], packet[9], packet[10], packet[11]]) {
        return Err(ParseError::BadCrc);
    }
    let msg_type = u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]);
    let body = &packet[20..];
    match msg_type {
        MSG_VERSION => Ok(Request::Version),
        MSG_INFO => {
            if body.len() < 4 {
                return Err(ParseError::MalformedBody);
            }
            let count = i32::from_le_bytes([body[0], body[1], body[2], body[3]]);
            if count < 0 || count > 4 || body.len() < 4 + count as usize {
                return Err(ParseError::MalformedBody);
            }
            Ok(Request::Info {
                slots: body[4..4 + count as usize].to_vec(),
            })
        }
        MSG_DATA => {
            if body.is_empty() {
                return Err(ParseError::MalformedBody);
            }
            let flags = body[0];
            let req = match flags {
                1 => {
                    if body.len() < 2 {
                        return Err(ParseError::MalformedBody);
                    }
                    DataRequest::BySlot(body[1])
                }
                2 => {
                    if body.len() < 7 {
                        return Err(ParseError::MalformedBody);
                    }
                    let mut mac = [0u8; 6];
                    mac.copy_from_slice(&body[1..7]);
                    DataRequest::ByMac(mac)
                }
                0 => DataRequest::All,
                _ => return Err(ParseError::MalformedBody),
            };
            Ok(Request::Data(req))
        }
        other => Err(ParseError::UnknownType(other)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SID: u32 = 0xdeadbeef;

    #[test]
    fn version_response_header_layout() {
        let p = build_version_packet(SID);
        assert_eq!(&p[0..4], MAGIC_SERVER);
        assert_eq!(u16::from_le_bytes([p[4], p[5]]), PROTOCOL_VERSION);
        // body = msg type (4) + u16 version (2)
        assert_eq!(u16::from_le_bytes([p[6], p[7]]), 6);
        assert_ne!([p[8], p[9], p[10], p[11]], [0; 4], "CRC must be set");
        assert_eq!(u32::from_le_bytes([p[12], p[13], p[14], p[15]]), SID);
        assert_eq!(u32::from_le_bytes([p[16], p[17], p[18], p[19]]), MSG_VERSION);
        assert_eq!(u16::from_le_bytes([p[20], p[21]]), PROTOCOL_VERSION);
        assert_eq!(p.len(), 22);
    }

    #[test]
    fn version_response_crc_covers_full_packet() {
        // Re-verify the embedded CRC over the whole packet with the field
        // zeroed (independent of the builder's internal path). Catches the
        // builder forgetting to include any region in the CRC input.
        let p = build_version_packet(SID);
        let mut zeroed = p.clone();
        zeroed[8..12].fill(0);
        let mut h = Hasher::new();
        h.update(&zeroed);
        assert_eq!(h.finalize(), u32::from_le_bytes([p[8], p[9], p[10], p[11]]));
    }

    #[test]
    fn corrupt_crc_is_rejected() {
        let p = build_version_packet(SID);
        let mut as_client = p;
        as_client[0..4].copy_from_slice(MAGIC_CLIENT);
        as_client[8] ^= 0xff;
        assert_eq!(parse_request(&as_client), Err(ParseError::BadCrc));
    }

    #[test]
    fn parse_info_request() {
        let mut body = Vec::new();
        body.extend_from_slice(&2i32.to_le_bytes());
        body.push(0);
        body.push(3);
        let mut p = Vec::new();
        p.extend_from_slice(MAGIC_CLIENT);
        p.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        p.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.extend_from_slice(&0x1111u32.to_le_bytes());
        p.extend_from_slice(&MSG_INFO.to_le_bytes());
        p.extend_from_slice(&body);
        let mut zeroed = p.clone();
        zeroed[8..12].fill(0);
        let crc = crc32(&zeroed);
        p[8..12].copy_from_slice(&crc.to_le_bytes());

        assert_eq!(
            parse_request(&p),
            Ok(Request::Info { slots: vec![0, 3] })
        );
    }

    #[test]
    fn parse_data_request_by_slot() {
        let packet = client_packet(MSG_DATA, &[1, 2]);
        assert_eq!(parse_request(&packet), Ok(Request::Data(DataRequest::BySlot(2))));
    }

    #[test]
    fn parse_data_request_by_mac() {
        let packet = client_packet(MSG_DATA, &[2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(
            parse_request(&packet),
            Ok(Request::Data(DataRequest::ByMac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])))
        );
    }

    #[test]
    fn parse_data_request_all() {
        let packet = client_packet(MSG_DATA, &[0]);
        assert_eq!(parse_request(&packet), Ok(Request::Data(DataRequest::All)));
    }

    #[test]
    fn reject_unknown_type_and_bad_magic() {
        assert_eq!(
            parse_request(&client_packet(0x999999, &[])),
            Err(ParseError::UnknownType(0x999999))
        );
        assert_eq!(parse_request(&[]), Err(ParseError::TooShort));
        assert_eq!(parse_request(&[0u8; 20]), Err(ParseError::BadMagic));
    }

    /// Build a CRC-valid client packet for tests.
    fn client_packet(msg_type: u32, body: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(MAGIC_CLIENT);
        p.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        p.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.extend_from_slice(&0x2222u32.to_le_bytes());
        p.extend_from_slice(&msg_type.to_le_bytes());
        p.extend_from_slice(body);
        let mut zeroed = p.clone();
        zeroed[8..12].fill(0);
        let crc = crc32(&zeroed);
        p[8..12].copy_from_slice(&crc.to_le_bytes());
        p
    }

    #[test]
    fn info_packet_layout() {
        let begin = SharedBegin {
            slot: 1,
            state: SLOT_STATE_CONNECTED,
            model: MODEL_FULL_GYRO,
            connection: CONN_BLUETOOTH,
            mac: [1, 2, 3, 4, 5, 6],
            battery: 0x05,
        };
        let p = build_info_packet(&begin, SID);
        assert_eq!(u32::from_le_bytes([p[16], p[17], p[18], p[19]]), MSG_INFO);
        assert_eq!(p.len(), 32); // 16 header + 4 type + 11 begin + \0
        assert_eq!(p[20], 1); // slot
        assert_eq!(p[21], SLOT_STATE_CONNECTED);
        assert_eq!(p[22], MODEL_FULL_GYRO);
        assert_eq!(p[23], CONN_BLUETOOTH);
        assert_eq!(&p[24..30], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(p[30], 0x05);
        assert_eq!(p[31], 0);
    }

    #[test]
    fn data_packet_layout() {
        let begin = SharedBegin {
            slot: 0,
            state: SLOT_STATE_CONNECTED,
            model: MODEL_FULL_GYRO,
            connection: CONN_USB,
            mac: [9, 8, 7, 6, 5, 4],
            battery: 0x04,
        };
        let sample = MotionSample {
            accel: [0.5, -1.0, 0.25],
            gyro: [10.0, -20.0, 30.5],
            timestamp_us: 0x0123456789abcdef,
        };
        let p = build_data_packet(&begin, 42, &sample, SID);
        assert_eq!(p.len(), 100, "DSU data packets are exactly 100 bytes");
        assert_eq!(u32::from_le_bytes([p[16], p[17], p[18], p[19]]), MSG_DATA);
        assert_eq!(p[31], 1, "connected flag");
        assert_eq!(u32::from_le_bytes([p[32], p[33], p[34], p[35]]), 42, "packet number");
        assert_eq!(p[40], 128, "stick Lx neutral");
        assert_eq!(p[43], 128, "stick Ry neutral");
        assert_eq!(u64::from_le_bytes(p[68..76].try_into().unwrap()), sample.timestamp_us);
        assert_eq!(f32::from_le_bytes(p[76..80].try_into().unwrap()), 0.5, "accel x");
        assert_eq!(f32::from_le_bytes(p[80..84].try_into().unwrap()), -1.0, "accel y");
        assert_eq!(f32::from_le_bytes(p[84..88].try_into().unwrap()), 0.25, "accel z");
        assert_eq!(f32::from_le_bytes(p[88..92].try_into().unwrap()), 10.0, "gyro pitch");
        assert_eq!(f32::from_le_bytes(p[92..96].try_into().unwrap()), -20.0, "gyro yaw");
        assert_eq!(f32::from_le_bytes(p[96..100].try_into().unwrap()), 30.5, "gyro roll");
        // begin echoed
        assert_eq!(&p[20..24], &[0, 2, 2, 1]); // slot, state, model, connection
        assert_eq!(&p[24..30], &[9, 8, 7, 6, 5, 4]);
        assert_eq!(p[30], 0x04, "battery");
    }

    /// Helper used by server integration tests.
    pub(crate) fn public_crc(data: &[u8]) -> u32 {
        crc32(data)
    }

    #[test]
    fn info_request_malformed_bodies() {
        // count > 4
        assert_eq!(
            parse_request(&client_packet(MSG_INFO, &5i32.to_le_bytes())),
            Err(ParseError::MalformedBody)
        );
        // truncated slot array
        assert_eq!(
            parse_request(&client_packet(MSG_INFO, &2i32.to_le_bytes())),
            Err(ParseError::MalformedBody)
        );
        // data request with unknown flag
        assert_eq!(
            parse_request(&client_packet(MSG_DATA, &[7])),
            Err(ParseError::MalformedBody)
        );
    }
}
