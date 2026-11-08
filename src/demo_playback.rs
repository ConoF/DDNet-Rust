//! DDNet TWDEMO v3-v6 chunk index, bounded decompression and frame seeking.
//!
//! Reference: upstream/ddnet/src/engine/shared/demo.cpp (20.2.1).
//! The original chunk body is Huffman-compressed packed i32s. Full
//! snapshots use CSnapshot layout; deltas use CSnapshotDelta::UnpackDelta.
//! This module is an offline decoder, not the native demo-player UI.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::demo::{DemoError, DemoInfo};
use crate::huffman::{Huffman, HuffmanError};
use crate::snapshot_delta::{DeltaDecoder, DeltaError, SnapshotObject, SnapshotState};

pub const MAX_DEMO_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_DEMO_CHUNKS: usize = 2_000_000;
const MAX_DEMO_CHUNK_BYTES: usize = 65_535;
const MAX_SNAPSHOT_BYTES: usize = 65_536;
const MAX_ITEMS: usize = 1024;
const MAX_TICK: i32 = 0x6fff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    Io,
    Demo(DemoError),
    Huffman(HuffmanError),
    Delta(DeltaError),
    Truncated,
    InvalidChunk,
    InvalidTick,
    InvalidSnapshot,
    TooLarge,
    NoKeyframe,
}
impl From<DemoError> for ReplayError {
    fn from(e: DemoError) -> Self { Self::Demo(e) }
}
impl From<HuffmanError> for ReplayError {
    fn from(e: HuffmanError) -> Self { Self::Huffman(e) }
}
impl From<DeltaError> for ReplayError {
    fn from(e: DeltaError) -> Self { Self::Delta(e) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Snapshot,
    Message,
    Delta,
    Reserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoChunk {
    pub tick: i32,
    pub kind: ChunkKind,
    pub payload_offset: u64,
    pub compressed_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoFrame {
    pub tick: i32,
    pub keyframe: bool,
    pub marker_offset: u64,
    pub snapshots: u32,
    pub deltas: u32,
    pub messages: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoIndex {
    pub info: DemoInfo,
    pub frames: Vec<DemoFrame>,
    pub chunks: Vec<DemoChunk>,
}

fn byte(file: &mut File) -> Result<u8, ReplayError> {
    let mut b = [0u8; 1];
    file.read_exact(&mut b).map_err(|_| ReplayError::Truncated)?;
    Ok(b[0])
}
fn tick_be(file: &mut File) -> Result<i32, ReplayError> {
    let mut data = [0u8; 4];
    file.read_exact(&mut data).map_err(|_| ReplayError::Truncated)?;
    Ok(i32::from_be_bytes(data))
}

impl DemoIndex {
    /// Scans chunk headers and skips bodies by seek; this does not copy
    /// recorded map bytes or decompress snapshots into memory.
    pub fn scan_file(path: &Path) -> Result<Self, ReplayError> {
        let info = DemoInfo::inspect_file(path)?;
        let mut file = File::open(path).map_err(|_| ReplayError::Io)?;
        let file_size = file.metadata().map_err(|_| ReplayError::Io)?.len();
        if file_size > MAX_DEMO_FILE_BYTES { return Err(ReplayError::TooLarge); }
        file.seek(SeekFrom::Start(info.first_chunk_offset))
            .map_err(|_| ReplayError::Io)?;
        let mut frames: Vec<DemoFrame> = Vec::new();
        let mut chunks = Vec::new();
        let mut current_tick: Option<i32> = None;

        while file.stream_position().map_err(|_| ReplayError::Io)? < file_size {
            if frames.len().saturating_add(chunks.len()) >= MAX_DEMO_CHUNKS {
                return Err(ReplayError::TooLarge);
            }
            let offset = file.stream_position().map_err(|_| ReplayError::Io)?;
            let marker = byte(&mut file)?;
            if marker & 0x80 != 0 {
                let keyframe = marker & 0x40 != 0;
                let legacy = i32::from(marker & 0x3f);
                let next_tick = if info.version < 5 && legacy != 0 {
                    current_tick.and_then(|old| old.checked_add(legacy))
                        .ok_or(ReplayError::InvalidTick)?
                } else if info.version >= 5 && marker & 0x20 != 0 {
                    current_tick.and_then(|old| old.checked_add(i32::from(marker & 0x1f)))
                        .ok_or(ReplayError::InvalidTick)?
                } else {
                    tick_be(&mut file)?
                };
                if !(0..MAX_TICK).contains(&next_tick)
                    || current_tick.is_some_and(|previous| next_tick <= previous)
                {
                    return Err(ReplayError::InvalidTick);
                }
                current_tick = Some(next_tick);
                frames.push(DemoFrame {
                    tick: next_tick, keyframe, marker_offset: offset,
                    snapshots: 0, deltas: 0, messages: 0,
                });
                continue;
            }

            let tick = current_tick.ok_or(ReplayError::InvalidChunk)?;
            let kind = match (marker >> 5) & 3 {
                1 => ChunkKind::Snapshot,
                2 => ChunkKind::Message,
                3 => ChunkKind::Delta,
                _ => ChunkKind::Reserved,
            };
            let length = match marker & 0x1f {
                30 => usize::from(byte(&mut file)?),
                31 => {
                    let low = usize::from(byte(&mut file)?);
                    let high = usize::from(byte(&mut file)?);
                    low | high << 8
                }
                size => usize::from(size),
            };
            if length > MAX_DEMO_CHUNK_BYTES {
                return Err(ReplayError::TooLarge);
            }
            let payload_offset = file.stream_position().map_err(|_| ReplayError::Io)?;
            let end = payload_offset.checked_add(length as u64)
                .ok_or(ReplayError::TooLarge)?;
            if end > file_size { return Err(ReplayError::Truncated); }
            file.seek(SeekFrom::Start(end)).map_err(|_| ReplayError::Io)?;

            let frame = frames.last_mut().ok_or(ReplayError::InvalidChunk)?;
            match kind {
                ChunkKind::Snapshot => frame.snapshots += 1,
                ChunkKind::Delta => frame.deltas += 1,
                ChunkKind::Message => frame.messages += 1,
                ChunkKind::Reserved => {}
            }
            chunks.push(DemoChunk { tick, kind, payload_offset, compressed_bytes: length });
        }
        if !frames.iter().any(|frame| frame.keyframe && frame.snapshots > 0) {
            return Err(ReplayError::NoKeyframe);
        }
        for &marker in &info.timeline_markers {
            if !frames.iter().any(|f| f.tick == marker) {
                return Err(ReplayError::InvalidTick);
            }
        }
        Ok(Self { info, frames, chunks })
    }

    /// Decodes one compressed chunk into the original little-endian
    /// CVariableInt::Decompress output, represented as 32-bit integers.
    pub fn decode_chunk(
        &self, file: &mut File, chunk: &DemoChunk, codec: &Huffman,
    ) -> Result<Vec<i32>, ReplayError> {
        if chunk.compressed_bytes == 0 { return Ok(Vec::new()); }
        if chunk.compressed_bytes > MAX_DEMO_CHUNK_BYTES {
            return Err(ReplayError::TooLarge);
        }
        file.seek(SeekFrom::Start(chunk.payload_offset))
            .map_err(|_| ReplayError::Io)?;
        let mut compressed = vec![0u8; chunk.compressed_bytes];
        file.read_exact(&mut compressed).map_err(|_| ReplayError::Truncated)?;
        let packed = codec.decompress(&compressed, MAX_SNAPSHOT_BYTES)?;
        let words = DeltaDecoder::unpack_integer_stream(&packed)?;
        if words.len() > MAX_SNAPSHOT_BYTES / 4 {
            return Err(ReplayError::TooLarge);
        }
        Ok(words)
    }

    /// Reconstructs the last snapshot at or before the requested tick by
    /// seeking to its nearest preceding keyframe and applying only the
    /// following snapshots/deltas. A message chunk does not change state.
    pub fn snapshot_at_tick(
        &self, path: &Path, wanted_tick: i32,
    ) -> Result<(i32, SnapshotState), ReplayError> {
        let start = self.frames.iter().rposition(|frame| {
            frame.tick <= wanted_tick && frame.keyframe && frame.snapshots > 0
        }).ok_or(ReplayError::NoKeyframe)?;
        let start_tick = self.frames[start].tick;
        let mut file = File::open(path).map_err(|_| ReplayError::Io)?;
        let codec = Huffman::new();
        let decoder = DeltaDecoder::for_vanilla_06();
        let mut state: Option<SnapshotState> = None;
        let mut displayed_tick = start_tick;

        for chunk in &self.chunks {
            if chunk.tick < start_tick { continue; }
            if chunk.tick > wanted_tick { break; }
            match chunk.kind {
                ChunkKind::Snapshot => {
                    let words = self.decode_chunk(&mut file, chunk, &codec)?;
                    state = Some(parse_full_snapshot(&words)?);
                    displayed_tick = chunk.tick;
                }
                ChunkKind::Delta => {
                    let previous = state.as_ref().ok_or(ReplayError::NoKeyframe)?;
                    let words = self.decode_chunk(&mut file, chunk, &codec)?;
                    let next = if words.is_empty() {
                        previous.clone()
                    } else {
                        decoder.apply_words(&words, previous)?
                    };
                    state = Some(next);
                    displayed_tick = chunk.tick;
                }
                ChunkKind::Message | ChunkKind::Reserved => {}
            }
        }
        Ok((displayed_tick, state.ok_or(ReplayError::NoKeyframe)?))
    }

    /// Bounded extraction of the recording's embedded map without writing
    /// or resolving any server-controlled path on the local filesystem.
    pub fn embedded_map(&self, path: &Path) -> Result<Option<Vec<u8>>, ReplayError> {
        if self.info.map_size == 0 { return Ok(None); }
        let mut file = File::open(path).map_err(|_| ReplayError::Io)?;
        file.seek(SeekFrom::Start(self.info.map_offset as u64))
            .map_err(|_| ReplayError::Io)?;
        let mut map = vec![0u8; self.info.map_size as usize];
        file.read_exact(&mut map).map_err(|_| ReplayError::Truncated)?;
        Ok(Some(map))
    }
}

/// Parses CSnapshot (data_size, num_items, offsets, item headers and data)
/// from CVariableInt::Decompress output. UUID-extended item type IDs are
/// kept in their internal form until full UUID mapping is implemented.
pub fn parse_full_snapshot(words: &[i32]) -> Result<SnapshotState, ReplayError> {
    if words.len() < 2 || words.len() > MAX_SNAPSHOT_BYTES / 4 {
        return Err(ReplayError::InvalidSnapshot);
    }
    let data_size = usize::try_from(words[0]).map_err(|_| ReplayError::InvalidSnapshot)?;
    let num_items = usize::try_from(words[1]).map_err(|_| ReplayError::InvalidSnapshot)?;
    if num_items > MAX_ITEMS || !data_size.is_multiple_of(4)
        || 2usize.checked_add(num_items).and_then(|n| n.checked_add(data_size / 4))
            != Some(words.len())
    {
        return Err(ReplayError::InvalidSnapshot);
    }
    let data = &words[2 + num_items..];
    let mut last_end = 0usize;
    let mut snapshot = Vec::with_capacity(num_items);
    let mut seen = std::collections::HashSet::new();
    for i in 0..num_items {
        let start = usize::try_from(words[2 + i]).map_err(|_| ReplayError::InvalidSnapshot)?;
        let end = if i + 1 < num_items {
            usize::try_from(words[3 + i]).map_err(|_| ReplayError::InvalidSnapshot)?
        } else { data_size };
        if start % 4 != 0 || end % 4 != 0 || start != last_end
            || end < start + 4 || end > data_size
        {
            return Err(ReplayError::InvalidSnapshot);
        }
        let payload = &data[start / 4..end / 4];
        let key = payload[0] as u32;
        let type_id = (key >> 16) as u16;
        let id = key as u16;
        if type_id > 0x7fff || !seen.insert((type_id, id)) {
            return Err(ReplayError::InvalidSnapshot);
        }
        snapshot.push(SnapshotObject {
            type_id, id, words: payload[1..].to_vec(),
        });
        last_end = end;
    }
    if (num_items == 0 && data_size != 0) || last_end != data_size {
        return Err(ReplayError::InvalidSnapshot);
    }
    Ok(SnapshotState { items: snapshot })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::write_int;

    fn append_chunk(bytes: &mut Vec<u8>, kind: u8, words: &[i32]) {
        let mut packed = Vec::new();
        for &word in words {
            write_int(word, &mut packed);
        }
        let compressed = Huffman::new().compress(&packed, 65_535).unwrap();
        let len = compressed.len();
        assert!(len < 65_535);
        if len < 30 {
            bytes.push((kind << 5) | len as u8);
        } else if len < 256 {
            bytes.extend_from_slice(&[(kind << 5) | 30, len as u8]);
        } else {
            bytes.extend_from_slice(&[(kind << 5) | 31, len as u8, (len >> 8) as u8]);
        }
        bytes.extend_from_slice(&compressed);
    }

    fn test_demo(version: u8) -> Vec<u8> {
        let mut bytes = vec![0; 176];
        bytes[..7].copy_from_slice(b"TWDEMO\0");
        bytes[7] = version;
        bytes[8..11].copy_from_slice(b"0.6");
        bytes[72..76].copy_from_slice(b"ctf1");
        bytes[144..150].copy_from_slice(b"client");
        if version >= 4 {
            bytes.extend_from_slice(&[0; 260]);
        }
        bytes.push(0xc0);
        bytes.extend_from_slice(&50u32.to_be_bytes());
        let mut character = vec![0i32; 22];
        character[1] = 100;
        character[2] = 200;
        let mut snap = vec![92, 1, 0, 9 << 16];
        snap.extend_from_slice(&character);
        append_chunk(&mut bytes, 1, &snap);

        // Historical v3/v4 can encode delta using the lower six bits
        // even without the 0x20 explicit compressed-tick marker.
        if version >= 5 { bytes.push(0xa1); }
        else { bytes.push(0x81); }
        let mut difference = vec![0, 1, 0, 9, 0];
        let mut values = vec![0i32; 22];
        values[1] = 10;
        difference.extend_from_slice(&values);
        append_chunk(&mut bytes, 3, &difference);
        append_chunk(&mut bytes, 2, &[42]);
        bytes
    }

    fn fixture_file(version: u8) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ddnet-rust-demo-timeline-{}-v{}.demo", std::process::id(), version,
        ));
        std::fs::write(&path, test_demo(version)).unwrap();
        path
    }

    #[test]
    fn scans_all_versions_and_reconstructs_snapshot_delta_by_tick() {
        for version in [3, 4, 5, 6] {
            let path = fixture_file(version);
            let index = DemoIndex::scan_file(&path).unwrap();
            assert_eq!(index.frames.len(), 2);
            assert_eq!(index.frames[0].tick, 50);
            assert!(index.frames[0].keyframe);
            assert_eq!(index.frames[0].snapshots, 1);
            assert_eq!(index.frames[1].tick, 51);
            assert_eq!(index.frames[1].deltas, 1);
            assert_eq!(index.frames[1].messages, 1);
            assert_eq!(index.chunks.len(), 3);
            let (tick, first) = index.snapshot_at_tick(&path, 50).unwrap();
            assert_eq!(tick, 50);
            assert_eq!(first.find(9, 0).unwrap().words[1], 100);
            let (tick, after) = index.snapshot_at_tick(&path, 51).unwrap();
            assert_eq!(tick, 51);
            assert_eq!(after.find(9, 0).unwrap().words[1], 110);
            assert_eq!(index.snapshot_at_tick(&path, 40), Err(ReplayError::NoKeyframe));
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn snapshot_parser_rejects_invalid_lengths_offsets_and_duplicate_keys() {
        assert_eq!(parse_full_snapshot(&[0, 0]), Ok(SnapshotState::default()));
        assert_eq!(parse_full_snapshot(&[4, 1, 0, (9 << 16) | 7]).unwrap()
            .find(9, 7).unwrap().words.len(), 0);
        assert_eq!(parse_full_snapshot(&[8, 1, 4, (9 << 16) | 7, 5]),
            Err(ReplayError::InvalidSnapshot));
        assert_eq!(parse_full_snapshot(&[4, 1, 0, (9 << 16) | 7, 0]),
            Err(ReplayError::InvalidSnapshot));
        assert_eq!(parse_full_snapshot(&[-1, 0]), Err(ReplayError::InvalidSnapshot));
        assert_eq!(parse_full_snapshot(&[0, -1]), Err(ReplayError::InvalidSnapshot));
    }

    #[test]
    fn scanner_rejects_truncated_payloads_and_nonmonotonic_or_missing_ticks() {
        let path = fixture_file(6);
        let good = std::fs::read(&path).unwrap();
        for length in [good.len() - 1, good.len() - 2] {
            std::fs::write(&path, &good[..length]).unwrap();
            assert!(DemoIndex::scan_file(&path).is_err());
        }
        let mut bad = good.clone();
        let marker = 176 + 260;
        bad[marker + 1..marker + 5].copy_from_slice(&i32::MAX.to_be_bytes());
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(DemoIndex::scan_file(&path), Err(ReplayError::InvalidTick));
        bad = good;
        bad[marker] = 0xa1; // delta tick without a base
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(DemoIndex::scan_file(&path), Err(ReplayError::InvalidTick));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn huffman_output_and_snapshot_body_are_bounded() {
        let path = fixture_file(5);
        let index = DemoIndex::scan_file(&path).unwrap();
        let mut file = File::open(&path).unwrap();
        let empty = DemoChunk {
            tick: 50, kind: ChunkKind::Snapshot,
            payload_offset: 0, compressed_bytes: 0,
        };
        assert!(index.decode_chunk(&mut file, &empty, &Huffman::new()).unwrap().is_empty());
        assert_eq!(parse_full_snapshot(&[0, 1, 0]), Err(ReplayError::InvalidSnapshot));
        assert_eq!(parse_full_snapshot(&vec![0; 20_000]), Err(ReplayError::InvalidSnapshot));
        std::fs::remove_file(path).unwrap();
    }
}
