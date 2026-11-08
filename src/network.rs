//! Bounds-checked DDNet/Teeworlds 0.6 wire packet and chunk headers.
//!
//! Reference: upstream/ddnet/src/engine/shared/network.h and network.cpp,
//! v20.2.1. This does NOT implement socket IO, reliability, Huffman, tokens,
//! the 0.7 packet format, snapshots or handshake state.

pub const MAX_PACKET_SIZE: usize = 1400;
pub const MAX_CHUNK_SIZE: usize = 1023;
pub const MAX_SEQUENCE: u16 = 1024;
pub const FLAG_CONTROL: u8 = 1 << 2;
pub const FLAG_CONNLESS: u8 = 1 << 3;
pub const FLAG_RESEND: u8 = 1 << 4;
pub const FLAG_COMPRESSION: u8 = 1 << 5;
pub const CHUNK_VITAL: u8 = 1;
pub const CHUNK_RESEND: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    TooShort,
    TooLarge,
    InvalidFlags,
    InvalidAck,
    InvalidChunkCount,
    InvalidChunkHeader,
    InvalidChunkLength,
    TrailingBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub flags: u8,
    pub ack: u16,
    pub num_chunks: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    Connless {
        extended: bool,
        extra_data: Option<[u8; 4]>,
        payload: &'a [u8],
    },
    Connected {
        header: PacketHeader,
        /// Encoded chunk bytes. Huffman decompression is not implemented.
        payload: &'a [u8],
    },
}

fn validate_connected(header: PacketHeader, payload_len: usize) -> Result<(), NetworkError> {
    if header.ack >= MAX_SEQUENCE {
        return Err(NetworkError::InvalidAck);
    }
    if header.flags & !(FLAG_CONTROL | FLAG_RESEND | FLAG_COMPRESSION) != 0 {
        return Err(NetworkError::InvalidFlags);
    }
    if header.flags & FLAG_CONTROL != 0 {
        if header.num_chunks != 0 || payload_len == 0 || header.flags & FLAG_COMPRESSION != 0 {
            return Err(NetworkError::InvalidChunkCount);
        }
    } else if header.num_chunks == 0 && header.flags & FLAG_RESEND == 0 {
        return Err(NetworkError::InvalidChunkCount);
    }
    Ok(())
}

/// Parses a 0.6 packet; compressed chunks are returned encoded, not decompressed.
pub fn parse_packet_06(bytes: &[u8]) -> Result<Packet<'_>, NetworkError> {
    if bytes.len() > MAX_PACKET_SIZE {
        return Err(NetworkError::TooLarge);
    }
    if bytes.len() < 3 {
        return Err(NetworkError::TooShort);
    }
    let flags = bytes[0] >> 2;
    if flags & FLAG_CONNLESS != 0 {
        if bytes.len() < 6 {
            return Err(NetworkError::TooShort);
        }
        let extended = bytes.starts_with(b"xe");
        let extra_data = if extended {
            Some([bytes[2], bytes[3], bytes[4], bytes[5]])
        } else {
            None
        };
        return Ok(Packet::Connless {
            extended,
            extra_data,
            payload: &bytes[6..],
        });
    }
    let header = PacketHeader {
        flags,
        ack: (u16::from(bytes[0] & 3) << 8) | u16::from(bytes[1]),
        num_chunks: bytes[2],
    };
    validate_connected(header, bytes.len() - 3)?;
    Ok(Packet::Connected { header, payload: &bytes[3..] })
}

/// Encodes the connection-oriented 0.6 header (without automatic compression).
pub fn encode_packet_06(
    header: PacketHeader,
    payload: &[u8],
) -> Result<Vec<u8>, NetworkError> {
    if payload.len() > MAX_PACKET_SIZE - 3 {
        return Err(NetworkError::TooLarge);
    }
    validate_connected(header, payload.len())?;
    let mut bytes = Vec::with_capacity(3 + payload.len());
    bytes.push((header.flags << 2) | ((header.ack >> 8) as u8 & 3));
    bytes.push(header.ack as u8);
    bytes.push(header.num_chunks);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    pub flags: u8,
    pub size: u16,
    /// Sequence is present for VITAL chunks only.
    pub sequence: Option<u16>,
}

impl ChunkHeader {
    pub fn encode_06(self) -> Result<Vec<u8>, NetworkError> {
        if self.flags > 3 || self.size as usize > MAX_CHUNK_SIZE {
            return Err(NetworkError::InvalidChunkHeader);
        }
        let vital = self.flags & CHUNK_VITAL != 0;
        if vital != self.sequence.is_some() {
            return Err(NetworkError::InvalidChunkHeader);
        }
        let mut first = ((self.flags & 3) << 6) | ((self.size >> 4) as u8 & 0x3f);
        let mut second = (self.size & 0x0f) as u8;
        if vital {
            let sequence = self.sequence.ok_or(NetworkError::InvalidChunkHeader)?;
            if sequence >= MAX_SEQUENCE {
                return Err(NetworkError::InvalidChunkHeader);
            }
            second |= ((sequence >> 2) as u8) & 0xf0;
            let mut bytes = Vec::with_capacity(3);
            bytes.push(first);
            bytes.push(second);
            bytes.push(sequence as u8);
            Ok(bytes)
        } else {
            Ok(vec![first, second])
        }
    }

    pub fn decode_06(bytes: &[u8]) -> Result<(Self, usize), NetworkError> {
        let first = *bytes.first().ok_or(NetworkError::TooShort)?;
        let second = *bytes.get(1).ok_or(NetworkError::TooShort)?;
        let flags = (first >> 6) & 3;
        let size = (u16::from(first & 0x3f) << 4) | u16::from(second & 0x0f);
        if flags & CHUNK_VITAL != 0 {
            let third = *bytes.get(2).ok_or(NetworkError::TooShort)?;
            let sequence = (u16::from(second & 0xf0) << 2) | u16::from(third);
            Ok((Self { flags, size, sequence: Some(sequence) }, 3))
        } else {
            Ok((Self { flags, size, sequence: None }, 2))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub header: ChunkHeader,
    pub payload: &'a [u8],
}

/// The caller must first remove any negotiated security token and decompress
/// Huffman content. Strictly checks that chunks consume the entire slice.
pub fn parse_chunks_06(
    mut bytes: &[u8],
    count: u8,
) -> Result<Vec<Chunk<'_>>, NetworkError> {
    let mut result = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (header, header_len) = ChunkHeader::decode_06(bytes)?;
        let length = header_len + header.size as usize;
        if length > bytes.len() {
            return Err(NetworkError::InvalidChunkLength);
        }
        result.push(Chunk { header, payload: &bytes[header_len..length] });
        bytes = &bytes[length..];
    }
    if !bytes.is_empty() {
        return Err(NetworkError::TrailingBytes);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_and_connless_from_upstream_layout() {
        let control = [0x10, 0x00, 0x00, 0x01];
        assert_eq!(
            parse_packet_06(&control),
            Ok(Packet::Connected {
                header: PacketHeader { flags: FLAG_CONTROL, ack: 0, num_chunks: 0 },
                payload: &[1]
            })
        );

        let normal = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, b'o', b'k'];
        assert_eq!(
            parse_packet_06(&normal),
            Ok(Packet::Connless { extended: false, extra_data: None, payload: b"ok" })
        );
        let extended = [b'x', b'e', 1, 2, 3, 4, b'o', b'k'];
        assert_eq!(
            parse_packet_06(&extended),
            Ok(Packet::Connless {
                extended: true, extra_data: Some([1, 2, 3, 4]), payload: b"ok"
            })
        );
    }

    #[test]
    fn vital_chunk_header_matches_cpp_bit_layout() {
        let header = ChunkHeader { flags: CHUNK_VITAL, size: 512, sequence: Some(0x3ab) };
        let bytes = header.encode_06().unwrap();
        assert_eq!(bytes, [0x60, 0xe0, 0xab]);
        assert_eq!(ChunkHeader::decode_06(&bytes), Ok((header, 3)));
        assert_eq!(
            ChunkHeader::decode_06(&[0x3f, 0x0f]),
            Ok((ChunkHeader { flags: 0, size: 1023, sequence: None }, 2))
        );
    }

    #[test]
    fn packet_and_chunk_roundtrip() {
        let chunk = ChunkHeader { flags: CHUNK_VITAL, size: 3, sequence: Some(1023) };
        let mut chunk_bytes = chunk.encode_06().unwrap();
        chunk_bytes.extend_from_slice(b"tee");
        let header = PacketHeader { flags: 0, ack: 1023, num_chunks: 1 };
        let wire = encode_packet_06(header, &chunk_bytes).unwrap();
        assert_eq!(&wire[..3], &[0x03, 0xff, 0x01]);
        let packet = parse_packet_06(&wire).unwrap();
        match packet {
            Packet::Connected { header: decoded, payload } => {
                assert_eq!(decoded, header);
                let chunks = parse_chunks_06(payload, decoded.num_chunks).unwrap();
                assert_eq!(chunks.len(), 1);
                assert_eq!(chunks[0].header, chunk);
                assert_eq!(chunks[0].payload, b"tee");
            }
            Packet::Connless { .. } => panic!("wrong packet variant"),
        }
    }

    #[test]
    fn rejects_corrupt_input_and_lengths() {
        assert_eq!(parse_packet_06(&[]), Err(NetworkError::TooShort));
        assert_eq!(parse_packet_06(&[0u8; 1401]), Err(NetworkError::TooLarge));
        assert_eq!(parse_packet_06(&[0x04, 0, 1, 0]), Err(NetworkError::InvalidFlags));
        assert_eq!(parse_packet_06(&[0x10, 0, 1, 1]), Err(NetworkError::InvalidChunkCount));
        assert_eq!(ChunkHeader::decode_06(&[0x40, 0x00]), Err(NetworkError::TooShort));
        assert_eq!(parse_chunks_06(&[0x00, 0x03, b'a'], 1), Err(NetworkError::InvalidChunkLength));
        assert_eq!(parse_chunks_06(&[1], 0), Err(NetworkError::TrailingBytes));
        assert_eq!(
            ChunkHeader { flags: CHUNK_VITAL, size: 0, sequence: Some(1024) }.encode_06(),
            Err(NetworkError::InvalidChunkHeader)
        );
    }

    #[test]
    fn chunk_header_roundtrips_full_length_and_sequence_ranges() {
        for size in 0..=MAX_CHUNK_SIZE as u16 {
            for seq in [0, 1, 255, 256, 512, 1023] {
                let original = ChunkHeader {
                    flags: CHUNK_VITAL,
                    size,
                    sequence: Some(seq),
                };
                let encoded = original.encode_06().unwrap();
                assert_eq!(ChunkHeader::decode_06(&encoded), Ok((original, 3)));
            }
        }
    }
}
