//! Teeworlds/DDNet 0.6 game-system message encoding and map transfer.
//!
//! Reference: upstream/ddnet/src/engine/shared/protocol.h, client.cpp
//! RepackMsg/SendInfo/SendMapRequest and NETMSG_MAP_CHANGE/MAP_DATA handlers.
//! Mirrors original DDNet 20.2.1 0.6 message numbering. Does not implement
//! UUID-to-type registry, snapshots, 0.7 messages or live game state.

use crate::protocol::{decode_int, write_int, write_string, CodecError, Reader};

pub const GAME_NETVERSION_06: &str = "0.6 626fce9a778df4d4";
pub const NETMSG_INFO: u16 = 1;
pub const NETMSG_MAP_CHANGE: u16 = 2;
pub const NETMSG_MAP_DATA: u16 = 3;
pub const NETMSG_CON_READY: u16 = 4;
pub const NETMSG_SNAP: u16 = 5;
pub const NETMSG_SNAPEMPTY: u16 = 6;
pub const NETMSG_SNAPSINGLE: u16 = 7;
pub const NETMSG_INPUTTIMING: u16 = 9;
pub const NETMSG_READY: u16 = 14;
pub const NETMSG_ENTERGAME: u16 = 15;
pub const NETMSG_INPUT: u16 = 16;
pub const NETMSG_REQUEST_MAP_DATA: u16 = 19;
pub const NETMSG_PING: u16 = 22;
pub const NETMSG_PING_REPLY: u16 = 23;

pub const NETMSGTYPE_SV_READYTOENTER: u16 = 8;
pub const NETMSGTYPE_CL_STARTINFO: u16 = 20;

/// Feedback sent by the DDNet 0.6 server for a previously transmitted
/// NETMSG_INPUT (engine/server/server.cpp: input tick and milliseconds
/// remaining until TickStartTime). Negative means the input was late.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputTiming {
    pub intended_tick: i32,
    pub time_left_ms: i32,
}

impl InputTiming {
    pub fn parse(payload: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(payload);
        let intended_tick = reader.read_int()?;
        let time_left_ms = reader.read_int()?;
        if !(0..=0x6fff_ffff).contains(&intended_tick) || reader.remaining() != 0 {
            return Err(MessageError::InvalidMessage);
        }
        Ok(Self { intended_tick, time_left_ms })
    }
}

pub const MAX_MAP_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageId {
    Regular { system: bool, number: u16 },
    Extended { system: bool, uuid: [u8; 16] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message<'a> {
    pub id: MessageId,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageError {
    Codec(CodecError),
    InvalidId,
    TooShort,
    InvalidMessage,
    InvalidMap,
    InvalidChunk,
    MapSizeExceeded,
    MapCrcMismatch,
    Incomplete,
}

impl From<CodecError> for MessageError {
    fn from(value: CodecError) -> Self {
        Self::Codec(value)
    }
}

pub fn encode_message(id: MessageId, payload: &[u8]) -> Result<Vec<u8>, MessageError> {
    let mut bytes = Vec::with_capacity(5 + 16 + payload.len());
    match id {
        MessageId::Regular { system, number } => {
            if number == 0 || number > 0x7fff {
                return Err(MessageError::InvalidId);
            }
            write_int((i32::from(number) << 1) | i32::from(system), &mut bytes);
        }
        MessageId::Extended { system, uuid } => {
            write_int(i32::from(system), &mut bytes);
            bytes.extend_from_slice(&uuid);
        }
    }
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

/// Decode the legacy message header (one variable-int or variable-int+UUID).
pub fn decode_message(bytes: &[u8]) -> Result<Message<'_>, MessageError> {
    let (packed, consumed) = decode_int(bytes)?;
    if packed < 0 {
        return Err(MessageError::InvalidId);
    }
    let system = packed & 1 != 0;
    let number = packed >> 1;
    if number == 0 {
        let start = consumed;
        let end = start.checked_add(16).ok_or(MessageError::TooShort)?;
        let uuid: [u8; 16] = bytes.get(start..end)
            .ok_or(MessageError::TooShort)?
            .try_into()
            .map_err(|_| MessageError::TooShort)?;
        Ok(Message {
            id: MessageId::Extended { system, uuid },
            payload: &bytes[end..],
        })
    } else if number <= 0x7fff {
        Ok(Message {
            id: MessageId::Regular { system, number: number as u16 },
            payload: &bytes[consumed..],
        })
    } else {
        Err(MessageError::InvalidId)
    }
}

fn system(id: u16, payload: &[u8]) -> Result<Vec<u8>, MessageError> {
    encode_message(MessageId::Regular { system: true, number: id }, payload)
}

/// Vanilla client NETMSG_INFO, called only AFTER transport CONNECTACCEPT.
pub fn client_info(password: &str) -> Result<Vec<u8>, MessageError> {
    let mut data = Vec::new();
    write_string(GAME_NETVERSION_06, &mut data)?;
    write_string(password, &mut data)?;
    system(NETMSG_INFO, &data)
}

/// Original Teeworlds 0.6 Cl_StartInfo game message. Must be sent after
/// the system CON_READY and before the server's Sv_ReadyToEnter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartInfo {
    pub name: String,
    pub clan: String,
    pub country: i32,
    pub skin: String,
    pub use_custom_color: bool,
    pub color_body: i32,
    pub color_feet: i32,
}

impl Default for StartInfo {
    fn default() -> Self {
        Self {
            name: "DDNet-Rust".to_owned(),
            clan: String::new(),
            country: -1,
            skin: "default".to_owned(),
            use_custom_color: false,
            color_body: 0,
            color_feet: 0,
        }
    }
}

impl StartInfo {
    /// Strict validation prevents malformed names/skins from reaching
    /// the remote server. Limits are measured in UTF-8 bytes.
    pub fn pack(&self) -> Result<Vec<u8>, MessageError> {
        if self.name.is_empty() || self.name.len() >= 16
            || self.clan.len() >= 12 || self.skin.is_empty()
            || self.skin.len() >= 24 {
            return Err(MessageError::InvalidMessage);
        }
        let mut data = Vec::new();
        write_string(&self.name, &mut data)?;
        write_string(&self.clan, &mut data)?;
        write_int(self.country, &mut data);
        write_string(&self.skin, &mut data)?;
        write_int(i32::from(self.use_custom_color), &mut data);
        write_int(self.color_body, &mut data);
        write_int(self.color_feet, &mut data);
        encode_message(MessageId::Regular {
            system: false,
            number: NETMSGTYPE_CL_STARTINFO,
        }, &data)
    }
}

pub fn client_ready() -> Vec<u8> {
    // NETMSG_READY=14, system bit=1.
    vec![(NETMSG_READY * 2 + 1) as u8]
}

pub fn client_enter_game() -> Vec<u8> {
    // NETMSG_ENTERGAME=15, system bit=1.
    vec![(NETMSG_ENTERGAME * 2 + 1) as u8]
}

pub fn request_map_chunk(index: i32) -> Result<Vec<u8>, MessageError> {
    if index < 0 {
        return Err(MessageError::InvalidChunk);
    }
    let mut data = Vec::new();
    write_int(index, &mut data);
    system(NETMSG_REQUEST_MAP_DATA, &data)
}

pub fn client_ping_reply() -> Vec<u8> {
    vec![(NETMSG_PING_REPLY * 2 + 1) as u8]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapChange {
    pub name: String,
    pub crc: u32,
    pub size: usize,
}

impl MapChange {
    /// Parse a vanilla server's NETMSG_MAP_CHANGE payload.
    pub fn parse(payload: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(payload);
        let name = reader.read_string()?;
        let crc = reader.read_int()? as u32;
        let size = reader.read_int()?;
        let size = usize::try_from(size).map_err(|_| MessageError::InvalidMap)?;
        if size > MAX_MAP_BYTES || !valid_map_filename(name) || reader.remaining() != 0 {
            return Err(MessageError::InvalidMap);
        }
        Ok(Self { name: name.to_owned(), crc, size })
    }
}

/// A map name is a single filename, never an arbitrary path.
fn valid_map_filename(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".."
        && !s.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '|' | '*' | '?' | '"')
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapData<'a> {
    pub last: bool,
    pub crc: u32,
    pub chunk: i32,
    pub bytes: &'a [u8],
}

impl<'a> MapData<'a> {
    /// DDNet 0.6 NETMSG_MAP_DATA: last, crc, chunk, size, bytes.
    pub fn parse(payload: &'a [u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(payload);
        let last = reader.read_int()?;
        let crc = reader.read_int()? as u32;
        let chunk = reader.read_int()?;
        let size = reader.read_int()?;
        if !matches!(last, 0 | 1) || chunk < 0 || !(1..=1024).contains(&size) {
            return Err(MessageError::InvalidChunk);
        }
        let bytes = reader.read_raw(size as usize)?;
        if reader.remaining() != 0 {
            return Err(MessageError::InvalidChunk);
        }
        Ok(Self { last: last != 0, crc, chunk, bytes })
    }
}

/// Standard CRC-32 used by original DDNet maps, without platform dependencies.
/// \`previous\` is the CRC-32 of all earlier bytes, initially zero.
pub fn crc32_update(previous: u32, bytes: &[u8]) -> u32 {
    let mut crc = !previous;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

/// Bounded, sequential map downloader model. Does not write untrusted map paths
/// or assume server-supplied chunks are valid.
#[derive(Debug, Clone)]
pub struct MapDownload {
    change: MapChange,
    bytes: Vec<u8>,
    next_chunk: i32,
    crc: u32,
    complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapProgress {
    pub received: usize,
    pub total: usize,
    pub next_chunk: i32,
    pub complete: bool,
}

impl MapDownload {
    pub fn new(change: MapChange, max_bytes: usize) -> Result<Self, MessageError> {
        if change.size > max_bytes || change.size > MAX_MAP_BYTES
            || !valid_map_filename(&change.name)
        {
            return Err(MessageError::MapSizeExceeded);
        }
        Ok(Self {
            change, bytes: Vec::new(), next_chunk: 0, crc: 0, complete: false,
        })
    }

    pub fn ingest(&mut self, chunk: MapData<'_>) -> Result<MapProgress, MessageError> {
        if self.complete || chunk.chunk != self.next_chunk || chunk.crc != self.change.crc {
            return Err(MessageError::InvalidChunk);
        }
        let next_size = self.bytes.len().checked_add(chunk.bytes.len())
            .ok_or(MessageError::MapSizeExceeded)?;
        if next_size > self.change.size {
            return Err(MessageError::MapSizeExceeded);
        }
        let next_crc = crc32_update(self.crc, chunk.bytes);
        if chunk.last {
            if next_size != self.change.size {
                return Err(MessageError::Incomplete);
            }
            if next_crc != self.change.crc {
                return Err(MessageError::MapCrcMismatch);
            }
        } else if next_size == self.change.size {
            return Err(MessageError::InvalidChunk);
        }

        self.bytes.extend_from_slice(chunk.bytes);
        self.crc = next_crc;
        self.next_chunk += 1;
        self.complete = chunk.last;
        Ok(MapProgress {
            received: self.bytes.len(),
            total: self.change.size,
            next_chunk: self.next_chunk,
            complete: self.complete,
        })
    }

    pub fn into_map_data(self) -> Result<Vec<u8>, MessageError> {
        if !self.complete {
            return Err(MessageError::Incomplete);
        }
        Ok(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_timing_payload_matches_server_and_rejects_garbage() {
        let mut payload = Vec::new();
        write_int(12345, &mut payload);
        write_int(-37, &mut payload);
        assert_eq!(InputTiming::parse(&payload), Ok(InputTiming {
            intended_tick: 12345, time_left_ms: -37,
        }));
        let mut extra = payload.clone();
        extra.push(0);
        assert_eq!(InputTiming::parse(&extra), Err(MessageError::InvalidMessage));
        assert!(InputTiming::parse(&payload[..1]).is_err());
        let mut negative = Vec::new();
        write_int(-1, &mut negative);
        write_int(0, &mut negative);
        assert_eq!(InputTiming::parse(&negative), Err(MessageError::InvalidMessage));
        let mut extreme = Vec::new();
        write_int(12345, &mut extreme);
        write_int(i32::MIN, &mut extreme);
        assert_eq!(InputTiming::parse(&extreme).unwrap().time_left_ms, i32::MIN);
    }

    #[test]
    fn original_game_startinfo_wire_fields() {
        let info = StartInfo {
            name: "Rust".into(), clan: "DDNet".into(), country: -1,
            skin: "default".into(), use_custom_color: false,
            color_body: 0, color_feet: 0,
        };
        let data = info.pack().unwrap();
        let msg = decode_message(&data).unwrap();
        assert_eq!(msg.id, MessageId::Regular {
            system: false, number: NETMSGTYPE_CL_STARTINFO,
        });
        let mut fields = Reader::new(msg.payload);
        assert_eq!(fields.read_string(), Ok("Rust"));
        assert_eq!(fields.read_string(), Ok("DDNet"));
        assert_eq!(fields.read_int(), Ok(-1));
        assert_eq!(fields.read_string(), Ok("default"));
        for _ in 0..3 { assert_eq!(fields.read_int(), Ok(0)); }
        assert_eq!(fields.remaining(), 0);
        assert_eq!(NETMSGTYPE_SV_READYTOENTER, 8);
    }

    #[test]
    fn start_info_validates_wire_lengths() {
        let mut info = StartInfo {
            name: "abcdefghijklmnop".into(),
            ..StartInfo::default()
        };
        assert_eq!(info.pack(), Err(MessageError::InvalidMessage));
        info.name = "Rust".into();
        info.skin = String::new();
        assert_eq!(info.pack(), Err(MessageError::InvalidMessage));
        info.skin = "default".into();
        info.clan = "null\0clan".into();
        assert_eq!(info.pack(), Err(MessageError::Codec(CodecError::EmbeddedNul)));
    }

    #[test]
    fn client_06_message_golden_vectors() {
        assert_eq!(client_ready(), vec![29]);
        assert_eq!(client_enter_game(), vec![31]);
        assert_eq!(client_ping_reply(), vec![47]);
        assert_eq!(request_map_chunk(0).unwrap(), vec![39, 0]);
        assert_eq!(request_map_chunk(1).unwrap(), vec![39, 1]);
        assert_eq!(request_map_chunk(-1), Err(MessageError::InvalidChunk));
        assert_eq!(client_info("").unwrap(),
            [vec![3], b"0.6 626fce9a778df4d4\0".to_vec(), vec![0]].concat());
    }

    #[test]
    fn message_id_roundtrips_regular_and_extended_uuid() {
        let uuid = [0x13u8; 16];
        let ids = [
            MessageId::Regular { system: true, number: 1 },
            MessageId::Regular { system: false, number: 13 },
            MessageId::Extended { system: true, uuid },
            MessageId::Extended { system: false, uuid },
        ];
        for id in ids {
            let raw = encode_message(id, b"payload").unwrap();
            let decoded = decode_message(&raw).unwrap();
            assert_eq!(decoded.id, id);
            assert_eq!(decoded.payload, b"payload");
        }
    }

    #[test]
    fn malformed_message_headers_rejected() {
        assert_eq!(decode_message(&[]), Err(MessageError::Codec(CodecError::UnexpectedEof)));
        assert_eq!(decode_message(&[1]), Err(MessageError::TooShort));
        assert_eq!(decode_message(&[0x40]), Err(MessageError::InvalidId));
        assert_eq!(encode_message(MessageId::Regular { system: true, number: 0 }, &[]),
            Err(MessageError::InvalidId));
    }

    #[test]
    fn vanilla_map_change_and_valid_file_names() {
        let mut data = Vec::new();
        write_string("ctf1", &mut data).unwrap();
        write_int(-1, &mut data);
        write_int(4664, &mut data);
        assert_eq!(MapChange::parse(&data), Ok(MapChange {
            name: "ctf1".into(), crc: u32::MAX, size: 4664,
        }));
        for invalid in ["/etc/passwd", "..", "../map", "maps\\invalid", "", "a:b"] {
            let mut encoded = Vec::new();
            write_string(invalid, &mut encoded).unwrap();
            write_int(0, &mut encoded);
            write_int(9, &mut encoded);
            assert_eq!(MapChange::parse(&encoded), Err(MessageError::InvalidMap));
        }
    }

    fn map_data_packet(last: i32, crc: u32, chunk: i32, data: &[u8]) -> Vec<u8> {
        let mut packet = Vec::new();
        for value in [last, crc as i32, chunk, data.len() as i32] {
            write_int(value, &mut packet);
        }
        packet.extend_from_slice(data);
        packet
    }

    #[test]
    fn real_map_chunk_format_and_crc() {
        assert_eq!(crc32_update(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(
            crc32_update(crc32_update(0, b"1234"), b"56789"),
            0xcbf4_3926
        );
        let data = map_data_packet(1, 0x1111_2222, 2, b"DATA");
        let msg = MapData::parse(&data).unwrap();
        assert!(msg.last);
        assert_eq!(msg.chunk, 2);
        assert_eq!(msg.crc, 0x1111_2222);
        assert_eq!(msg.bytes, b"DATA");
    }

    #[test]
    fn chunked_download_reassembles_and_validates_crc() {
        let actual = b"DATA-hello-world";
        let crc = crc32_update(0, actual);
        let change = MapChange { name: "sample".into(), crc, size: actual.len() };
        let mut download = MapDownload::new(change.clone(), 100).unwrap();
        let first_bytes = map_data_packet(0, crc, 0, &actual[..5]);
        let first = MapData::parse(&first_bytes).unwrap();
        assert_eq!(download.ingest(first).unwrap(), MapProgress {
            received: 5, total: actual.len(), next_chunk: 1, complete: false,
        });
        assert_eq!(download.ingest(first), Err(MessageError::InvalidChunk));
        let second_bytes = map_data_packet(1, crc, 1, &actual[5..]);
        let second = MapData::parse(&second_bytes).unwrap();
        assert!(download.ingest(second).unwrap().complete);
        assert_eq!(download.into_map_data().unwrap(), actual);
        assert_eq!(MapDownload::new(change, 5).err(), Some(MessageError::MapSizeExceeded));
    }

    #[test]
    fn damaged_final_chunk_is_rejected_without_progress() {
        let expected = b"DATA-hello";
        let crc = crc32_update(0, expected);
        let mut downloader = MapDownload::new(MapChange {
            name: "test".into(), crc, size: expected.len(),
        }, 100).unwrap();
        let corrupted_bytes = map_data_packet(1, crc, 0, b"DATA-hallo");
        let corrupted = MapData::parse(&corrupted_bytes).unwrap();
        assert_eq!(downloader.ingest(corrupted), Err(MessageError::MapCrcMismatch));
        let valid_bytes = map_data_packet(1, crc, 0, expected);
        let valid = MapData::parse(&valid_bytes).unwrap();
        assert!(downloader.ingest(valid).unwrap().complete);
    }

    #[test]
    fn truncated_and_malicious_map_chunks_are_rejected() {
        assert_eq!(MapData::parse(&[1]), Err(MessageError::Codec(CodecError::UnexpectedEof)));
        let zero_size = map_data_packet(1, 0, 0, b"");
        assert_eq!(MapData::parse(&zero_size), Err(MessageError::InvalidChunk));
        let mut packet = map_data_packet(1, 0, 0, b"ok");
        packet.pop();
        assert_eq!(MapData::parse(&packet),
            Err(MessageError::Codec(CodecError::UnexpectedEof)));
        let mut packet = map_data_packet(1, 0, 0, b"ok");
        packet.push(1);
        assert_eq!(MapData::parse(&packet), Err(MessageError::InvalidChunk));
    }
}
