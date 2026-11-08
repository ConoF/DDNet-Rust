//! Bounds-checked metadata reader for Teeworlds / DDNet datafiles (maps).
//!
//! Reference: upstream/ddnet/src/engine/shared/datafile.cpp, release 20.2.1.
//! Supports little-endian DATA v3/v4 structure, item table and raw block spans.
//! v4 zlib raw blocks are decoded with a strict caller-provided memory budget.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatafileError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    InvalidHeader,
    InvalidOffset,
    InvalidItem,
    InvalidItemType,
    MissingRawBlock,
    OutputTooLarge,
    DecompressionFailed,
    DecompressedSizeMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemType {
    pub type_id: u16,
    pub start: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    pub type_id: u16,
    pub id: u16,
    /// The item's 32-bit little-endian fields, without its 8-byte header.
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawBlock<'a> {
    /// Actual bytes in the datafile. Compressed with zlib for v4.
    pub encoded: &'a [u8],
    /// Expected uncompressed size, only available in v4.
    pub uncompressed_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datafile<'a> {
    pub version: u32,
    pub item_types: Vec<ItemType>,
    pub items: Vec<Item<'a>>,
    pub raw_blocks: Vec<RawBlock<'a>>,
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DatafileError> {
        let end = self.position.checked_add(length).ok_or(DatafileError::Truncated)?;
        let slice = self.data.get(self.position..end).ok_or(DatafileError::Truncated)?;
        self.position = end;
        Ok(slice)
    }

    fn int(&mut self) -> Result<i32, DatafileError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| DatafileError::Truncated)?;
        Ok(i32::from_le_bytes(bytes))
    }

    fn size(&mut self) -> Result<usize, DatafileError> {
        usize::try_from(self.int()?).map_err(|_| DatafileError::InvalidHeader)
    }
}

fn checked_bytes(count: usize, item_size: usize) -> Result<usize, DatafileError> {
    count.checked_mul(item_size).ok_or(DatafileError::InvalidHeader)
}

fn parse_offsets(
    cursor: &mut Cursor<'_>,
    count: usize,
    section_size: usize,
) -> Result<Vec<usize>, DatafileError> {
    let mut offsets = Vec::with_capacity(count);
    let mut previous = 0;
    for index in 0..count {
        let value = cursor.size()?;
        if (index == 0 && value != 0) || value < previous || value > section_size {
            return Err(DatafileError::InvalidOffset);
        }
        offsets.push(value);
        previous = value;
    }
    Ok(offsets)
}

impl<'a> Datafile<'a> {
    /// Reads item metadata and raw-block boundaries without decompressing v4 data.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DatafileError> {
        let mut cursor = Cursor::new(bytes);
        if cursor.take(4)? != b"DATA" {
            return Err(DatafileError::BadMagic);
        }
        let version = cursor.int()?;
        if version != 3 && version != 4 {
            return Err(DatafileError::UnsupportedVersion);
        }

        let _declared_size = cursor.size()?;
        let _swaplen = cursor.size()?;
        let num_types = cursor.size()?;
        let num_items = cursor.size()?;
        let num_data = cursor.size()?;
        let item_size = cursor.size()?;
        let data_size = cursor.size()?;
        if item_size % 4 != 0 {
            return Err(DatafileError::InvalidHeader);
        }

        // Validate the entire table and payload span BEFORE allocating vectors.
        let mut table_bytes = checked_bytes(num_types, 12)?;
        table_bytes = table_bytes.checked_add(checked_bytes(num_items, 4)?)
            .ok_or(DatafileError::InvalidHeader)?;
        table_bytes = table_bytes.checked_add(checked_bytes(num_data, 4)?)
            .ok_or(DatafileError::InvalidHeader)?;
        if version == 4 {
            table_bytes = table_bytes.checked_add(checked_bytes(num_data, 4)?)
                .ok_or(DatafileError::InvalidHeader)?;
        }
        let needed = table_bytes.checked_add(item_size)
            .and_then(|sum| sum.checked_add(data_size))
            .ok_or(DatafileError::InvalidHeader)?;
        if needed > bytes.len().saturating_sub(cursor.position) {
            return Err(DatafileError::Truncated);
        }

        let mut item_types = Vec::with_capacity(num_types);
        for _ in 0..num_types {
            let raw_type = cursor.size()?;
            let start = cursor.size()?;
            let count = cursor.size()?;
            if raw_type > u16::MAX as usize ||
                start.checked_add(count).is_none_or(|end| end > num_items) ||
                item_types.iter().any(|entry: &ItemType| entry.type_id == raw_type as u16)
            {
                return Err(DatafileError::InvalidItemType);
            }
            item_types.push(ItemType { type_id: raw_type as u16, start, count });
        }

        let item_offsets = parse_offsets(&mut cursor, num_items, item_size)?;
        let data_offsets = parse_offsets(&mut cursor, num_data, data_size)?;
        let mut uncompressed_sizes = Vec::new();
        if version == 4 {
            for _ in 0..num_data {
                uncompressed_sizes.push(cursor.size()?);
            }
        }

        let item_section = cursor.take(item_size)?;
        let data_section = cursor.take(data_size)?;

        let mut items = Vec::with_capacity(num_items);
        for index in 0..num_items {
            let start = item_offsets[index];
            let end = item_offsets.get(index + 1).copied().unwrap_or(item_size);
            let span = item_section.get(start..end).ok_or(DatafileError::InvalidOffset)?;
            if span.len() < 8 {
                return Err(DatafileError::InvalidItem);
            }
            let packed_id = u32::from_le_bytes(
                span[..4].try_into().map_err(|_| DatafileError::InvalidItem)?
            );
            let payload_length = i32::from_le_bytes(
                span[4..8].try_into().map_err(|_| DatafileError::InvalidItem)?
            );
            let payload_length = usize::try_from(payload_length)
                .map_err(|_| DatafileError::InvalidItem)?;
            if payload_length % 4 != 0 {
                return Err(DatafileError::InvalidItem);
            }
            let end_of_payload = 8usize.checked_add(payload_length)
                .ok_or(DatafileError::InvalidItem)?;
            let payload = span.get(8..end_of_payload).ok_or(DatafileError::InvalidItem)?;
            items.push(Item {
                type_id: (packed_id >> 16) as u16,
                id: packed_id as u16,
                payload,
            });
        }

        for item_type in &item_types {
            let end = item_type.start + item_type.count; // validated earlier
            if items[item_type.start..end].iter()
                .any(|item| item.type_id != item_type.type_id)
            {
                return Err(DatafileError::InvalidItemType);
            }
        }

        let mut raw_blocks = Vec::with_capacity(num_data);
        for index in 0..num_data {
            let start = data_offsets[index];
            let end = data_offsets.get(index + 1).copied().unwrap_or(data_size);
            let encoded = data_section.get(start..end).ok_or(DatafileError::InvalidOffset)?;
            raw_blocks.push(RawBlock {
                encoded,
                uncompressed_size: if version == 4 {
                    Some(uncompressed_sizes[index])
                } else {
                    None
                },
            });
        }

        Ok(Self { version: version as u32, item_types, items, raw_blocks })
    }

    /// Extract a raw block with a strict memory budget. v3 blocks are uncompressed;
    /// v4 blocks use zlib and must match the expected uncompressed length.
    /// No partial output is returned on an error.
    pub fn raw_data(&self, index: usize, max_output: usize) -> Result<Vec<u8>, DatafileError> {
        use std::io::Read;

        let block = self.raw_blocks.get(index).ok_or(DatafileError::MissingRawBlock)?;
        let Some(expected) = block.uncompressed_size else {
            if block.encoded.len() > max_output {
                return Err(DatafileError::OutputTooLarge);
            }
            return Ok(block.encoded.to_vec());
        };
        if expected > max_output {
            return Err(DatafileError::OutputTooLarge);
        }
        if expected == 0 {
            return Err(DatafileError::DecompressedSizeMismatch);
        }
        let max_read = u64::try_from(expected)
            .map_err(|_| DatafileError::OutputTooLarge)?
            .checked_add(1)
            .ok_or(DatafileError::OutputTooLarge)?;
        let decoder = flate2::read::ZlibDecoder::new(block.encoded);
        let mut output = Vec::new();
        decoder
            .take(max_read)
            .read_to_end(&mut output)
            .map_err(|_| DatafileError::DecompressionFailed)?;
        if output.len() != expected {
            return Err(DatafileError::DecompressedSizeMismatch);
        }
        Ok(output)
    }

    pub fn find_item(&self, type_id: u16, id: u16) -> Option<Item<'a>> {
        self.items.iter().copied().find(|item| item.type_id == type_id && item.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_i32(dst: &mut Vec<u8>, value: i32) {
        dst.extend_from_slice(&value.to_le_bytes());
    }

    fn sample(version: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"DATA");
        add_i32(&mut bytes, version);
        add_i32(&mut bytes, 0); // file size, filled below
        add_i32(&mut bytes, 0); // swap length
        add_i32(&mut bytes, 1); // item type records
        add_i32(&mut bytes, 1); // item count
        add_i32(&mut bytes, 1); // raw data count
        add_i32(&mut bytes, 12); // bytes of items
        add_i32(&mut bytes, 3); // bytes of raw data
        add_i32(&mut bytes, 1); // type ID
        add_i32(&mut bytes, 0); // first index
        add_i32(&mut bytes, 1); // number in this type
        add_i32(&mut bytes, 0); // item offset
        add_i32(&mut bytes, 0); // data offset
        if version == 4 {
            add_i32(&mut bytes, 3); // uncompressed raw size
        }
        add_i32(&mut bytes, (1 << 16) | 7); // packed type/id
        add_i32(&mut bytes, 4); // item payload size
        add_i32(&mut bytes, 42); // item payload
        bytes.extend_from_slice(&[1, 2, 3]); // v4 opaque compressed placeholder
        let size = i32::try_from(bytes.len() - 16).unwrap();
        bytes[8..12].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[test]
    fn parse_v3_item_and_raw() {
        let bytes = sample(3);
        let parsed = Datafile::parse(&bytes).unwrap();
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.item_types, vec![ItemType { type_id: 1, start: 0, count: 1 }]);
        assert_eq!(parsed.find_item(1, 7).unwrap().payload, 42i32.to_le_bytes());
        assert_eq!(parsed.find_item(1, 8), None);
        assert_eq!(parsed.raw_blocks[0].encoded, &[1, 2, 3]);
        assert_eq!(parsed.raw_blocks[0].uncompressed_size, None);
    }

    #[test]
    fn parse_v4_metadata_without_decompression() {
        let bytes = sample(4);
        let parsed = Datafile::parse(&bytes).unwrap();
        assert_eq!(parsed.version, 4);
        assert_eq!(parsed.raw_blocks[0].encoded, &[1, 2, 3]);
        assert_eq!(parsed.raw_blocks[0].uncompressed_size, Some(3));
    }

    #[test]
    fn malformed_headers_and_offsets_are_rejected() {
        let mut bad = sample(3);
        bad[0] = b'X';
        assert_eq!(Datafile::parse(&bad), Err(DatafileError::BadMagic));
        let mut bad = sample(3);
        bad[4..8].copy_from_slice(&5i32.to_le_bytes());
        assert_eq!(Datafile::parse(&bad), Err(DatafileError::UnsupportedVersion));
        let mut bad = sample(3);
        bad[48..52].copy_from_slice(&1i32.to_le_bytes()); // first item offset
        assert_eq!(Datafile::parse(&bad), Err(DatafileError::InvalidOffset));
        let mut bad = sample(3);
        bad[60..64].copy_from_slice(&(-1i32).to_le_bytes()); // item payload size
        assert_eq!(Datafile::parse(&bad), Err(DatafileError::InvalidItem));
    }

    #[test]
    fn decompress_v4_raw_data_and_bound_memory() {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;

        let mut compressor = ZlibEncoder::new(Vec::new(), Compression::default());
        compressor.write_all(b"abc").unwrap();
        let compressed = compressor.finish().unwrap();

        let mut bytes = sample(4);
        bytes.truncate(bytes.len() - 3);
        bytes.extend_from_slice(&compressed);
        let data_size = i32::try_from(compressed.len()).unwrap();
        bytes[32..36].copy_from_slice(&data_size.to_le_bytes());
        let file_size = i32::try_from(bytes.len() - 16).unwrap();
        bytes[8..12].copy_from_slice(&file_size.to_le_bytes());

        let map = Datafile::parse(&bytes).unwrap();
        assert_eq!(map.raw_data(0, 3).unwrap(), b"abc");
        assert_eq!(map.raw_data(0, 2), Err(DatafileError::OutputTooLarge));
        assert_eq!(map.raw_data(1, 100), Err(DatafileError::MissingRawBlock));

        bytes[56..60].copy_from_slice(&4i32.to_le_bytes());
        let changed = Datafile::parse(&bytes).unwrap();
        assert_eq!(changed.raw_data(0, 100), Err(DatafileError::DecompressedSizeMismatch));
    }

    #[test]
    fn reject_invalid_zlib_stream() {
        let baseline = sample(4);
        let file = Datafile::parse(&baseline).unwrap();
        assert!(matches!(
            file.raw_data(0, 128),
            Err(DatafileError::DecompressionFailed | DatafileError::DecompressedSizeMismatch)
        ));
        let version3 = sample(3);
        let file = Datafile::parse(&version3).unwrap();
        assert_eq!(file.raw_data(0, 3).unwrap(), [1, 2, 3]);
        assert_eq!(file.raw_data(0, 2), Err(DatafileError::OutputTooLarge));
    }

    #[test]
    fn truncate_every_byte_boundary_and_fuzz_headers() {
        for version in [3, 4] {
            let baseline = sample(version);
            for length in 0..baseline.len() {
                assert!(Datafile::parse(&baseline[..length]).is_err());
            }
            for seed in 0..5000u32 {
                let mut mutated = baseline.clone();
                let index = (seed as usize * 17 + 3) % mutated.len();
                mutated[index] ^= ((seed >> 3) as u8) | 1;
                let _ = Datafile::parse(&mutated);
            }
        }
    }
}
