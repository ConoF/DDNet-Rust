//! Bounded DDNet 0.6 snapshot wire decoding and multipart reassembly.
//! Matches upstream/ddnet/src/engine/client/client.cpp snapshot unpacking,
//! upstream/ddnet/src/engine/shared/protocol.h (900-byte parts) and
//! snapshot.h (MAX_PARTS=64), pinned to release 20.2.1.
//!
//! This module assembles compressed/delta snapshot payload bytes. It does not
//! yet decode variable-integer deltas into actual game objects.

use crate::game_protocol::{
    NETMSG_SNAP, NETMSG_SNAPEMPTY, NETMSG_SNAPSINGLE,
};
use crate::protocol::{CodecError, Reader};

pub const MAX_PARTS: usize = 64;
pub const MAX_PART_BYTES: usize = 900;
pub const MAX_SNAPSHOT_BYTES: usize = MAX_PARTS * MAX_PART_BYTES;
pub const MAX_TICK: i32 = 0x6fff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    Codec(CodecError),
    UnknownMessage,
    InvalidTick,
    InvalidPart,
    InvalidSize,
    MetadataMismatch,
    ConflictingDuplicate,
    OldTick,
    MissingState,
}
impl From<CodecError> for SnapshotError {
    fn from(value: CodecError) -> Self { Self::Codec(value) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind { Multipart, Single, Empty }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPacket {
    pub kind: SnapshotKind,
    pub game_tick: i32,
    pub delta_tick: i32,
    pub crc: Option<u32>,
    pub part_count: usize,
    pub part_index: usize,
    pub bytes: Vec<u8>,
}

impl SnapshotPacket {
    pub fn parse(message_id: u16, payload: &[u8]) -> Result<Self, SnapshotError> {
        let kind = match message_id {
            NETMSG_SNAP => SnapshotKind::Multipart,
            NETMSG_SNAPSINGLE => SnapshotKind::Single,
            NETMSG_SNAPEMPTY => SnapshotKind::Empty,
            _ => return Err(SnapshotError::UnknownMessage),
        };
        let mut reader = Reader::new(payload);
        let game_tick = reader.read_int()?;
        let delta_tag = reader.read_int()?;
        let delta_tick = game_tick.checked_sub(delta_tag)
            .ok_or(SnapshotError::InvalidTick)?;
        if !(0..=MAX_TICK).contains(&game_tick) {
            return Err(SnapshotError::InvalidTick);
        }
        let (part_count, part_index) = if kind == SnapshotKind::Multipart {
            let count = reader.read_int()?;
            let index = reader.read_int()?;
            let count = usize::try_from(count).map_err(|_| SnapshotError::InvalidPart)?;
            let index = usize::try_from(index).map_err(|_| SnapshotError::InvalidPart)?;
            (count, index)
        } else {
            (1, 0)
        };
        if part_count == 0 || part_count > MAX_PARTS || part_index >= part_count {
            return Err(SnapshotError::InvalidPart);
        }
        let (crc, bytes) = if kind == SnapshotKind::Empty {
            (None, Vec::new())
        } else {
            let crc = reader.read_int()? as u32;
            let size = reader.read_int()?;
            let size = usize::try_from(size).map_err(|_| SnapshotError::InvalidSize)?;
            if size > MAX_PART_BYTES {
                return Err(SnapshotError::InvalidSize);
            }
            (Some(crc), reader.read_raw(size)?.to_vec())
        };
        if reader.remaining() != 0 {
            return Err(SnapshotError::InvalidSize);
        }
        Ok(Self {
            kind, game_tick, delta_tick, crc, part_count, part_index, bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledSnapshot {
    pub game_tick: i32,
    pub delta_tick: i32,
    /// Snapshot CRC, absent for SNAPEMPTY.
    pub crc: Option<u32>,
    /// Payload is still packed delta integers and must be decoded separately.
    pub packed_bytes: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
pub struct SnapshotAssembler {
    tick: Option<i32>,
    delta: i32,
    crc: Option<u32>,
    pieces: Vec<Option<Vec<u8>>>,
}

impl SnapshotAssembler {
    pub fn new() -> Self { Self::default() }

    pub fn last_tick(&self) -> Option<i32> { self.tick }

    pub fn push(
        &mut self, frame: SnapshotPacket,
    ) -> Result<Option<AssembledSnapshot>, SnapshotError> {
        if self.tick.is_some_and(|tick| frame.game_tick < tick) {
            return Err(SnapshotError::OldTick);
        }
        if frame.kind != SnapshotKind::Multipart {
            self.tick = Some(frame.game_tick);
            self.delta = frame.delta_tick;
            self.crc = frame.crc;
            self.pieces.clear();
            return Ok(Some(AssembledSnapshot {
                game_tick: frame.game_tick, delta_tick: frame.delta_tick,
                crc: frame.crc, packed_bytes: frame.bytes,
            }));
        }
        if frame.part_index + 1 < frame.part_count && frame.bytes.len() != MAX_PART_BYTES {
            return Err(SnapshotError::InvalidSize);
        }

        if self.tick != Some(frame.game_tick) {
            self.tick = Some(frame.game_tick);
            self.delta = frame.delta_tick;
            self.crc = frame.crc;
            self.pieces = vec![None; frame.part_count];
        } else if self.pieces.len() != frame.part_count
            || self.delta != frame.delta_tick || self.crc != frame.crc
        {
            return Err(SnapshotError::MetadataMismatch);
        }
        let old = &mut self.pieces[frame.part_index];
        if let Some(contents) = old {
            if contents != &frame.bytes {
                return Err(SnapshotError::ConflictingDuplicate);
            }
        } else {
            *old = Some(frame.bytes);
        }
        if self.pieces.iter().any(Option::is_none) {
            return Ok(None);
        }

        let size: usize = self.pieces.iter().map(|item| item.as_ref().map_or(0, Vec::len)).sum();
        if size > MAX_SNAPSHOT_BYTES {
            return Err(SnapshotError::InvalidSize);
        }
        let mut buffer = Vec::with_capacity(size);
        for piece in &self.pieces {
            buffer.extend_from_slice(piece.as_ref().ok_or(SnapshotError::MissingState)?);
        }
        self.pieces.clear();
        Ok(Some(AssembledSnapshot {
            game_tick: frame.game_tick, delta_tick: frame.delta_tick,
            crc: self.crc, packed_bytes: buffer,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::write_int;

    fn message(
        id: u16, tick: i32, delta_tag: i32, parts: Option<(i32, i32)>,
        crc: i32, bytes: &[u8],
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        write_int(tick, &mut payload);
        write_int(delta_tag, &mut payload);
        if let Some((num, part)) = parts {
            write_int(num, &mut payload);
            write_int(part, &mut payload);
        }
        if id != NETMSG_SNAPEMPTY {
            write_int(crc, &mut payload);
            write_int(bytes.len() as i32, &mut payload);
            payload.extend_from_slice(bytes);
        }
        payload
    }

    #[test]
    fn unpack_single_and_empty_snapshots() {
        let single = message(NETMSG_SNAPSINGLE, 100, 5, None, -123, b"delta");
        let frame = SnapshotPacket::parse(NETMSG_SNAPSINGLE, &single).unwrap();
        assert_eq!(frame.game_tick, 100);
        assert_eq!(frame.delta_tick, 95);
        assert_eq!(frame.crc, Some((-123i32) as u32));
        let mut assembler = SnapshotAssembler::new();
        let decoded = assembler.push(frame).unwrap().unwrap();
        assert_eq!(decoded.packed_bytes, b"delta");
        assert_eq!(assembler.last_tick(), Some(100));

        let empty = message(NETMSG_SNAPEMPTY, 101, 1, None, 0, &[]);
        let decoded = assembler.push(
            SnapshotPacket::parse(NETMSG_SNAPEMPTY, &empty).unwrap()
        ).unwrap().unwrap();
        assert_eq!(decoded.game_tick, 101);
        assert_eq!(decoded.delta_tick, 100);
        assert_eq!(decoded.crc, None);
        assert!(decoded.packed_bytes.is_empty());
    }

    #[test]
    fn reassemble_out_of_order_and_ignore_exact_duplicate() {
        let first = message(NETMSG_SNAP, 300, 2, Some((2, 0)), 123, &[1; 900]);
        let last = message(NETMSG_SNAP, 300, 2, Some((2, 1)), 123, b"trailing");
        let mut assembler = SnapshotAssembler::new();
        let part_last = SnapshotPacket::parse(NETMSG_SNAP, &last).unwrap();
        let part_first = SnapshotPacket::parse(NETMSG_SNAP, &first).unwrap();
        assert_eq!(assembler.push(part_last.clone()), Ok(None));
        assert_eq!(assembler.push(part_last), Ok(None));
        let output = assembler.push(part_first).unwrap().unwrap();
        assert_eq!(output.game_tick, 300);
        assert_eq!(output.delta_tick, 298);
        assert_eq!(output.packed_bytes.len(), 908);
        assert!(output.packed_bytes[..900].iter().all(|b| *b == 1));
        assert_eq!(&output.packed_bytes[900..], b"trailing");
    }

    #[test]
    fn reject_mismatched_parts_and_stale_ticks() {
        let first = message(NETMSG_SNAP, 100, 3, Some((2, 0)), 5, &[3; 900]);
        let other = message(NETMSG_SNAP, 100, 3, Some((2, 1)), 6, b"x");
        let mut assembler = SnapshotAssembler::new();
        assert_eq!(assembler.push(SnapshotPacket::parse(NETMSG_SNAP, &first).unwrap()), Ok(None));
        assert_eq!(
            assembler.push(SnapshotPacket::parse(NETMSG_SNAP, &other).unwrap()),
            Err(SnapshotError::MetadataMismatch)
        );
        let old = message(NETMSG_SNAPSINGLE, 99, 1, None, 0, b"x");
        assert_eq!(
            assembler.push(SnapshotPacket::parse(NETMSG_SNAPSINGLE, &old).unwrap()),
            Err(SnapshotError::OldTick)
        );
    }

    #[test]
    fn reject_truncated_and_oversized_parts() {
        assert_eq!(SnapshotPacket::parse(999, &[]), Err(SnapshotError::UnknownMessage));
        assert_eq!(SnapshotPacket::parse(NETMSG_SNAP, &[0]), Err(
            SnapshotError::Codec(CodecError::UnexpectedEof)
        ));
        let huge = message(NETMSG_SNAPSINGLE, 30, 1, None, 5, &[3; 901]);
        assert_eq!(SnapshotPacket::parse(NETMSG_SNAPSINGLE, &huge),
            Err(SnapshotError::InvalidSize));
        let invalid_count = message(NETMSG_SNAP, 30, 1, Some((65, 0)), 5, b"a");
        assert_eq!(SnapshotPacket::parse(NETMSG_SNAP, &invalid_count),
            Err(SnapshotError::InvalidPart));
        let short_not_last = message(NETMSG_SNAP, 30, 1, Some((2, 0)), 5, b"a");
        assert_eq!(
            SnapshotAssembler::new().push(
                SnapshotPacket::parse(NETMSG_SNAP, &short_not_last).unwrap()
            ),
            Err(SnapshotError::InvalidSize)
        );
    }

    #[test]
    fn fuzz_untrusted_snap_headers_without_panics() {
        let mut rng = 0x5920_4131u32;
        for length in 0..200 {
            let mut bytes = vec![0u8; length];
            for byte in &mut bytes {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = (rng >> 24) as u8;
            }
            let _ = SnapshotPacket::parse(NETMSG_SNAPSINGLE, &bytes);
            let _ = SnapshotPacket::parse(NETMSG_SNAP, &bytes);
            let _ = SnapshotPacket::parse(NETMSG_SNAPEMPTY, &bytes);
        }
    }
}
