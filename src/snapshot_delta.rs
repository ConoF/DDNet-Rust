//! Bounded DDNet snapshot variable-integer delta decoding and item rebuilding.
//!
//! Ported from pinned DDNet 20.2.1 engine/shared/snapshot.cpp:
//! CSnapshotDelta::UnpackDelta and CSnapshot::Crc.
//! This module requires the correct static netobject size table to decode
//! actual server snapshots. Until all generated protocol sizes are provided,
//! unknown objects must carry their size in each update.

use std::collections::{BTreeMap, BTreeSet};
use crate::protocol::{decode_int, CodecError};

pub const MAX_ITEMS: usize = 1024;
pub const MAX_WORDS: usize = 65536 / 4;
pub const MAX_TYPES: u16 = 32767;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaError {
    Codec(CodecError),
    InvalidHeader,
    InvalidType,
    InvalidId,
    InvalidSize,
    Truncated,
    TrailingData,
    TooManyItems,
    SnapshotTooLarge,
    DuplicateKey,
    ItemSizeMismatch,
}
impl From<CodecError> for DeltaError {
    fn from(value: CodecError) -> Self { Self::Codec(value) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotObject {
    pub type_id: u16,
    pub id: u16,
    pub words: Vec<i32>,
}
impl SnapshotObject {
    fn key(&self) -> (u16, u16) { (self.type_id, self.id) }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotState {
    pub items: Vec<SnapshotObject>,
}
impl SnapshotState {
    /// Original DDNet snapshot checksum: sum of all object words, u32 wrapping.
    pub fn checksum(&self) -> u32 {
        self.items.iter().flat_map(|item| item.words.iter())
            .fold(0u32, |sum, value| sum.wrapping_add(*value as u32))
    }

    pub fn find(&self, type_id: u16, id: u16) -> Option<&SnapshotObject> {
        self.items.iter().find(|x| x.type_id == type_id && x.id == id)
    }
}

#[derive(Debug, Clone)]
pub struct DeltaDecoder {
    /// Sizes in 32-bit words, zero means an explicit size field is expected.
    sizes: [u16; 64],
}

impl Default for DeltaDecoder {
    fn default() -> Self { Self { sizes: [0; 64] } }
}

impl DeltaDecoder {
    pub fn new() -> Self { Self::default() }

    /// Complete fixed-size metadata for the vanilla 0.6 DDNet netobjects.
    /// Extended UUID netobjects continue to carry dynamic sizes.
    pub fn for_vanilla_06() -> Self {
        let mut decoder = Self::new();
        for (kind, &words) in crate::netobjects::VANILLA_OBJECT_WORDS.iter().enumerate().skip(1) {
            decoder.sizes[kind] = words;
        }
        decoder
    }


    pub fn set_static_words(
        &mut self, type_id: u8, words: u16,
    ) -> Result<(), DeltaError> {
        let index = usize::from(type_id);
        if index >= self.sizes.len() || words == 0 || usize::from(words) > MAX_WORDS {
            return Err(DeltaError::InvalidSize);
        }
        self.sizes[index] = words;
        Ok(())
    }

    /// Decode network varints to the exact C++ input form, limited to
    /// twice the maximum snapshot byte size.
    pub fn unpack_integer_stream(
        packed: &[u8],
    ) -> Result<Vec<i32>, DeltaError> {
        if packed.len() > MAX_WORDS * 8 {
            return Err(DeltaError::SnapshotTooLarge);
        }
        let mut output = Vec::new();
        let mut pos = 0;
        while pos < packed.len() {
            if output.len() >= MAX_WORDS * 2 {
                return Err(DeltaError::SnapshotTooLarge);
            }
            let (number, size) = decode_int(&packed[pos..])?;
            pos += size;
            output.push(number);
        }
        Ok(output)
    }

    pub fn unpack(
        &self,
        packed: &[u8],
        from: &SnapshotState,
    ) -> Result<SnapshotState, DeltaError> {
        if packed.is_empty() {
            return self.apply_words(&[0, 0, 0], from);
        }
        let integers = Self::unpack_integer_stream(packed)?;
        self.apply_words(&integers, from)
    }

    /// Reconstruct object fields from a decoded DDNet snapshot delta.
    pub fn apply_words(
        &self, words: &[i32], from: &SnapshotState,
    ) -> Result<SnapshotState, DeltaError> {
        if words.len() < 3 {
            return Err(DeltaError::InvalidHeader);
        }
        let deleted = usize::try_from(words[0]).map_err(|_| DeltaError::InvalidHeader)?;
        let updated = usize::try_from(words[1]).map_err(|_| DeltaError::InvalidHeader)?;
        let temp = usize::try_from(words[2]).map_err(|_| DeltaError::InvalidHeader)?;
        if deleted > MAX_ITEMS || updated > MAX_ITEMS || temp > MAX_WORDS
            || 3usize.checked_add(deleted).is_none_or(|n| n > words.len())
        {
            return Err(DeltaError::InvalidHeader);
        }

        let mut gone = BTreeSet::new();
        for &key in &words[3..3+deleted] {
            let key = key as u32;
            let ty = (key >> 16) as u16;
            let id = key as u16;
            if ty > MAX_TYPES || !gone.insert((ty,id)) {
                return Err(DeltaError::DuplicateKey);
            }
        }

        let mut items: BTreeMap<(u16,u16),Vec<i32>> = BTreeMap::new();
        for item in &from.items {
            if item.type_id > MAX_TYPES || items.insert(
                item.key(), item.words.clone()
            ).is_some() {
                return Err(DeltaError::DuplicateKey);
            }
        }
        items.retain(|key,_| !gone.contains(key));

        let mut pos = 3 + deleted;
        let mut seen = BTreeSet::new();
        for _ in 0..updated {
            if words.len().saturating_sub(pos) < 2 {
                return Err(DeltaError::Truncated);
            }
            let ty = words[pos];
            let id = words[pos+1];
            pos += 2;
            if !(0..=MAX_TYPES as i32).contains(&ty) {
                return Err(DeltaError::InvalidType);
            }
            if !(0..=65535).contains(&id) {
                return Err(DeltaError::InvalidId);
            }
            let key = (ty as u16, id as u16);
            if !seen.insert(key) {
                return Err(DeltaError::DuplicateKey);
            }
            let known = self.sizes.get(ty as usize).copied().unwrap_or(0);
            let count = if known > 0 {
                usize::from(known)
            } else {
                let &raw = words.get(pos).ok_or(DeltaError::Truncated)?;
                pos += 1;
                usize::try_from(raw).map_err(|_| DeltaError::InvalidSize)?
            };
            if count > MAX_WORDS || count > words.len().saturating_sub(pos) {
                return Err(DeltaError::InvalidSize);
            }
            let values = &words[pos..pos+count];
            pos += count;
            let output = if let Some(prior) = from.find(key.0, key.1) {
                if prior.words.len() != count {
                    return Err(DeltaError::ItemSizeMismatch);
                }
                prior.words.iter().zip(values).map(|(a,b)|a.wrapping_add(*b)).collect()
            } else {
                values.to_vec()
            };
            items.insert(key, output);
        }

        // The original delta format has a temp counter but no temp payload.
        if pos != words.len() {
            return Err(DeltaError::TrailingData);
        }
        if items.len() > MAX_ITEMS {
            return Err(DeltaError::TooManyItems);
        }
        let total_data_words = items.values().map(|v|v.len()+1).sum::<usize>();
        let total_snapshot_words = total_data_words + items.len() + 2;
        if total_snapshot_words > MAX_WORDS {
            return Err(DeltaError::SnapshotTooLarge);
        }
        Ok(SnapshotState {
            items: items.into_iter().map(|((type_id,id),words)| {
                SnapshotObject { type_id, id, words }
            }).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::write_int;

    fn packed(data: &[i32]) -> Vec<u8> {
        let mut result = Vec::new();
        for &word in data {
            write_int(word, &mut result);
        }
        result
    }

    #[test]
    fn reconstruct_empty_delta_and_crc() {
        let decoder = DeltaDecoder::new();
        let previous = SnapshotState {
            items: vec![SnapshotObject { type_id: 65, id: 1, words: vec![5, -1] }]
        };
        assert_eq!(decoder.unpack(&[], &previous).unwrap(), previous);
        assert_eq!(decoder.unpack(&packed(&[0,0,0]), &previous).unwrap(), previous);
        assert_eq!(previous.checksum(), 4);
    }

    #[test]
    fn insert_and_update_delta_with_wrapping_add() {
        let d = DeltaDecoder::new();
        let initial = d.unpack(&packed(&[0, 1, 0, 65, 2, 2, i32::MAX, -3]),
            &SnapshotState::default()).unwrap();
        assert_eq!(initial.find(65, 2).unwrap().words, [i32::MAX, -3]);
        assert_eq!(initial.checksum(), i32::MAX as u32 - 3);
        let next = d.unpack(&packed(&[0, 1, 0, 65, 2, 2, 1, 6]), &initial).unwrap();
        assert_eq!(next.find(65,2).unwrap().words, [i32::MIN, 3]);
        assert_eq!(next.checksum(), (i32::MIN as u32).wrapping_add(3));
    }

    #[test]
    fn apply_static_size_and_deletes() {
        let mut d = DeltaDecoder::new();
        d.set_static_words(4, 3).unwrap();
        let first = d.unpack(&packed(&[0, 2, 0, 4, 0, 1,2,3, 65, 1, 1, 9]),
            &SnapshotState::default()).unwrap();
        assert_eq!(first.find(4,0).unwrap().words, [1,2,3]);
        assert_eq!(first.find(65,1).unwrap().words, [9]);
        let key = 65i32 << 16 | 1;
        let after = d.unpack(&packed(&[1, 1, 0, key, 4, 0, 2,3,4]), &first).unwrap();
        assert!(after.find(65,1).is_none());
        assert_eq!(after.find(4,0).unwrap().words, [3,5,7]);
    }

    #[test]
    fn malformed_delta_does_not_overwrite_prior_state() {
        let d = DeltaDecoder::new();
        let old = SnapshotState { items: vec![SnapshotObject {
            type_id: 65, id: 0, words: vec![7],
        }] };
        let original = old.clone();
        assert_eq!(d.unpack(&packed(&[0,1,0,65,0,2,3]), &old),
            Err(DeltaError::InvalidSize));
        assert_eq!(old, original);
        assert_eq!(d.unpack(&packed(&[-1,0,0]), &old), Err(DeltaError::InvalidHeader));
        assert_eq!(d.unpack(&packed(&[0,1,0,65]), &old), Err(DeltaError::Truncated));
        assert_eq!(d.unpack(&packed(&[0,1,0,65,0,1,1,99]), &old),
            Err(DeltaError::TrailingData));
        assert_eq!(d.unpack(&packed(&[0,2,0,65,0,1,2,65,0,1,3]), &old),
            Err(DeltaError::DuplicateKey));
    }

    #[test]
    fn official_06_static_schema_decodes_character_without_size_prefix() {
        let d = DeltaDecoder::for_vanilla_06();
        let mut words = vec![0, 1, 0, 9, 0];
        words.extend(0..22);
        let state = d.unpack(&packed(&words), &SnapshotState::default()).unwrap();
        let item = state.find(9, 0).unwrap();
        assert_eq!(item.words.len(), 22);
        assert_eq!(crate::netobjects::CharacterCoreNet::from_object(item).unwrap().x, 1);
        assert_eq!(crate::netobjects::CharacterCoreNet::from_object(item).unwrap().hook_state, 9);
        assert_eq!(state.checksum(), (0..22u32).sum::<u32>());
    }

    #[test]
    fn reject_truncated_variable_ints_and_random_bad_snapshots() {
        assert_eq!(DeltaDecoder::unpack_integer_stream(&[0x80]),
            Err(DeltaError::Codec(CodecError::UnexpectedEof)));
        let d = DeltaDecoder::new();
        let mut x = 0x1561_0042u32;
        for size in 0..256 {
            let mut bytes = vec![0u8; size];
            for b in &mut bytes {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                *b = (x >> 24) as u8;
            }
            let _ = d.unpack(&bytes, &SnapshotState::default());
        }
    }
}
