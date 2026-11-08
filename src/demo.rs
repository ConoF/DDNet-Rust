//! DDNet/Teeworlds TWDEMO v3-v6 metadata reader.
//! Reference: DDNet 20.2.1 engine/demo.h and engine/shared/demo.cpp.
//! Header inspection only: no demo playback, recording or editing.

use std::io::Read;
use std::path::Path;

const MARKER: &[u8; 7] = b"TWDEMO\0";
const HEADER_SIZE: usize = 176;
const TIMELINE_MARKERS: usize = 64;
const TIMELINE_SIZE: usize = 4 + TIMELINE_MARKERS * 4;
const UUID_SHA256: [u8; 16] = [
    0x6b, 0xe6, 0xda, 0x4a, 0xce, 0xbd, 0x38, 0x0c,
    0x9b, 0x5b, 0x12, 0x89, 0xc8, 0x42, 0xd7, 0x80,
];
const MAX_MAP_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PREFIX: usize = HEADER_SIZE + TIMELINE_SIZE + 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoError {
    Truncated,
    BadMarker,
    UnsupportedVersion,
    InvalidString,
    InvalidTimeline,
    InvalidMapSize,
    InvalidSha256,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoInfo {
    pub version: u8,
    pub network_version: String,
    pub map_name: String,
    pub kind: String,
    pub timestamp: String,
    pub map_size: u32,
    pub map_crc: u32,
    pub length_seconds: u32,
    pub timeline_markers: Vec<i32>,
    pub map_sha256: Option<[u8; 32]>,
    pub map_offset: usize,
    pub first_chunk_offset: u64,
}

fn string(bytes: &[u8]) -> Result<String, DemoError> {
    let end = bytes.iter().position(|&byte| byte == 0)
        .ok_or(DemoError::InvalidString)?;
    std::str::from_utf8(&bytes[..end])
        .map(str::to_owned).map_err(|_| DemoError::InvalidString)
}

fn big_endian(bytes: &[u8]) -> Result<u32, DemoError> {
    let array: [u8; 4] = bytes.get(..4).ok_or(DemoError::Truncated)?
        .try_into().map_err(|_| DemoError::Truncated)?;
    Ok(u32::from_be_bytes(array))
}

impl DemoInfo {
    /// Parse only a bounded header prefix. The full demo body is
    /// never decompressed or held in memory.
    pub fn parse_prefix(prefix: &[u8], file_size: u64) -> Result<Self, DemoError> {
        let header = prefix.get(..HEADER_SIZE).ok_or(DemoError::Truncated)?;
        if &header[..7] != MARKER { return Err(DemoError::BadMarker); }
        let version = header[7];
        if !(3..=6).contains(&version) { return Err(DemoError::UnsupportedVersion); }
        let network_version = string(&header[8..72])?;
        let map_name = string(&header[72..136])?;
        let map_size = big_endian(&header[136..140])?;
        let map_crc = big_endian(&header[140..144])?;
        let kind = string(&header[144..152])?;
        let length_seconds = big_endian(&header[152..156])?;
        let timestamp = string(&header[156..176])?;
        if u64::from(map_size) > MAX_MAP_BYTES { return Err(DemoError::InvalidMapSize); }
        let mut offset = HEADER_SIZE;
        let mut timeline_markers = Vec::new();
        if version >= 4 {
            let timeline = prefix.get(offset..offset + TIMELINE_SIZE)
                .ok_or(DemoError::Truncated)?;
            let count = big_endian(&timeline[..4])? as usize;
            if count > TIMELINE_MARKERS { return Err(DemoError::InvalidTimeline); }
            for i in 0..count {
                let tick = big_endian(&timeline[4 + i * 4..8 + i * 4])? as i32;
                if tick < 0 || timeline_markers.last().is_some_and(|prev| *prev >= tick) {
                    return Err(DemoError::InvalidTimeline);
                }
                timeline_markers.push(tick);
            }
            offset += TIMELINE_SIZE;
        }
        let mut map_sha256 = None;
        if version >= 6
            && prefix.get(offset..offset + 16)
                .is_some_and(|bytes| bytes == UUID_SHA256.as_slice())
        {
            let digest: [u8; 32] = prefix.get(offset + 16..offset + 48)
                .ok_or(DemoError::InvalidSha256)?
                .try_into().map_err(|_| DemoError::InvalidSha256)?;
            map_sha256 = Some(digest);
            offset += 48;
        }
        let first_chunk_offset = (offset as u64).checked_add(u64::from(map_size))
            .ok_or(DemoError::InvalidMapSize)?;
        if first_chunk_offset > file_size { return Err(DemoError::Truncated); }
        Ok(Self {
            version, network_version, map_name, kind, timestamp,
            map_size, map_crc, length_seconds, timeline_markers,
            map_sha256, map_offset: offset, first_chunk_offset,
        })
    }

    /// Bounded IO: at most 484 bytes, even for very large recordings.
    pub fn inspect_file(path: &Path) -> Result<Self, DemoError> {
        let mut file = std::fs::File::open(path).map_err(|_| DemoError::Io)?;
        let size = file.metadata().map_err(|_| DemoError::Io)?.len();
        let count = usize::try_from(size.min(MAX_PREFIX as u64))
            .map_err(|_| DemoError::Truncated)?;
        let mut prefix = vec![0; count];
        file.read_exact(&mut prefix).map_err(|_| DemoError::Io)?;
        Self::parse_prefix(&prefix, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(version: u8, map: &[u8], sha: bool) -> Vec<u8> {
        let mut bytes = vec![0; HEADER_SIZE];
        bytes[..7].copy_from_slice(MARKER);
        bytes[7] = version;
        bytes[8..11].copy_from_slice(b"0.6");
        bytes[72..76].copy_from_slice(b"ctf1");
        bytes[136..140].copy_from_slice(&(map.len() as u32).to_be_bytes());
        bytes[140..144].copy_from_slice(&0x12345678u32.to_be_bytes());
        bytes[144..150].copy_from_slice(b"client");
        bytes[152..156].copy_from_slice(&120u32.to_be_bytes());
        bytes[156..166].copy_from_slice(b"2026-11-08");
        if version >= 4 {
            bytes.extend_from_slice(&vec![0; TIMELINE_SIZE]);
            bytes[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&2u32.to_be_bytes());
            bytes[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&50u32.to_be_bytes());
            bytes[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&100u32.to_be_bytes());
        }
        if sha {
            bytes.extend_from_slice(&UUID_SHA256);
            bytes.extend_from_slice(&[0xab; 32]);
        }
        bytes.extend_from_slice(map);
        bytes
    }

    #[test]
    fn parses_all_known_versions_and_versioned_map_offsets() {
        for version in [3, 4, 5, 6] {
            let bytes = fixture(version, b"DATA", version == 6);
            let info = DemoInfo::parse_prefix(&bytes, bytes.len() as u64).unwrap();
            assert_eq!(info.version, version);
            assert_eq!(info.network_version, "0.6");
            assert_eq!(info.map_name, "ctf1");
            assert_eq!(info.kind, "client");
            assert_eq!(info.timestamp, "2026-11-08");
            assert_eq!(info.map_size, 4);
            assert_eq!(info.map_crc, 0x12345678);
            assert_eq!(info.length_seconds, 120);
            assert_eq!(info.timeline_markers, if version == 3 { vec![] } else { vec![50, 100] });
            assert_eq!(info.map_sha256.is_some(), version == 6);
            let offset = HEADER_SIZE + if version >= 4 { TIMELINE_SIZE } else { 0 }
                + if version == 6 { 48 } else { 0 };
            assert_eq!(info.map_offset, offset);
            assert_eq!(info.first_chunk_offset, bytes.len() as u64);
        }
    }

    #[test]
    fn rejects_corrupt_marker_truncated_header_and_invalid_strings() {
        let valid = fixture(6, b"DATA", true);
        assert_eq!(DemoInfo::parse_prefix(&valid[..80], 80), Err(DemoError::Truncated));
        let mut bad = valid.clone();
        bad[0] = b'A';
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::BadMarker));
        bad = valid.clone();
        bad[7] = 2;
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::UnsupportedVersion));
        bad = valid.clone();
        bad[72..136].fill(b'x');
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::InvalidString));
        bad = valid;
        bad[72] = 255;
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::InvalidString));
    }

    #[test]
    fn validates_embedded_map_and_timeline_bounds() {
        let original = fixture(4, b"DATA", false);
        assert_eq!(DemoInfo::parse_prefix(&original, (original.len() - 1) as u64), Err(DemoError::Truncated));
        let mut bad = original.clone();
        bad[136..140].copy_from_slice(&(65u32 * 1024 * 1024).to_be_bytes());
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::InvalidMapSize));
        bad = original.clone();
        bad[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&65u32.to_be_bytes());
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::InvalidTimeline));
        bad = original;
        bad[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&49u32.to_be_bytes());
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::InvalidTimeline));
    }

    #[test]
    fn version_six_without_sha_extension_is_allowed() {
        let bytes = fixture(6, b"DATA", false);
        let info = DemoInfo::parse_prefix(&bytes, bytes.len() as u64).unwrap();
        assert_eq!(info.map_offset, HEADER_SIZE + TIMELINE_SIZE);
        assert!(info.map_sha256.is_none());
        let mut bad = fixture(6, b"DATA", true);
        bad.truncate(HEADER_SIZE + TIMELINE_SIZE + 16 + 20);
        assert_eq!(DemoInfo::parse_prefix(&bad, bad.len() as u64), Err(DemoError::InvalidSha256));
    }

    #[test]
    fn inspect_file_is_bounded_to_header_prefix() {
        let path = std::env::temp_dir().join(format!("ddnet-rust-inspect-{}.demo", std::process::id()));
        std::fs::write(&path, fixture(6, b"DATA", true)).unwrap();
        let info = DemoInfo::inspect_file(&path).unwrap();
        assert_eq!(info.map_name, "ctf1");
        assert_eq!(info.map_sha256, Some([0xab; 32]));
        std::fs::remove_file(path).unwrap();
    }
}
