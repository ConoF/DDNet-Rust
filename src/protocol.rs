//! Signed integer encoding and NUL-terminated strings from DDNet 20.2.1.
//! Reference: upstream/ddnet/src/engine/shared/compression.h/.cpp.
//! Strict bounds checks reject malformed data that legacy C++ might tolerate.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    UnexpectedEof,
    IntegerOverflow,
    InvalidUtf8,
    EmbeddedNul,
}

/// Teeworlds sign-and-extension variable-length integer encoding.
pub fn write_int(value: i32, output: &mut Vec<u8>) {
    let negative = value < 0;
    let mut magnitude = if negative { (!value) as u32 } else { value as u32 };
    let mut byte = (magnitude & 0x3f) as u8 | if negative { 0x40 } else { 0 };
    magnitude >>= 6;

    while magnitude != 0 {
        output.push(byte | 0x80);
        byte = (magnitude & 0x7f) as u8;
        magnitude >>= 7;
    }
    output.push(byte);
}

/// Return (value, bytes consumed); valid integers fit into five bytes.
pub fn decode_int(bytes: &[u8]) -> Result<(i32, usize), CodecError> {
    let first = *bytes.first().ok_or(CodecError::UnexpectedEof)?;
    let negative = first & 0x40 != 0;
    let mut value = u32::from(first & 0x3f);
    let mut byte = first;
    let mut consumed = 1usize;
    let mut shift = 6;

    while byte & 0x80 != 0 {
        if consumed >= 5 {
            return Err(CodecError::IntegerOverflow);
        }
        byte = *bytes.get(consumed).ok_or(CodecError::UnexpectedEof)?;
        let chunk = u32::from(byte & 0x7f);
        if consumed == 4 && chunk > 0x0f {
            return Err(CodecError::IntegerOverflow);
        }
        value |= chunk << shift;
        consumed += 1;
        shift += 7;
    }

    if value > i32::MAX as u32 {
        return Err(CodecError::IntegerOverflow);
    }
    let magnitude = value as i32;
    Ok((if negative { !magnitude } else { magnitude }, consumed))
}

/// Write a UTF-8 string terminated by the single NUL byte used on the wire.
pub fn write_string(input: &str, output: &mut Vec<u8>) -> Result<(), CodecError> {
    if input.as_bytes().contains(&0) {
        return Err(CodecError::EmbeddedNul);
    }
    output.extend_from_slice(input.as_bytes());
    output.push(0);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.input.len() - self.offset
    }

    pub fn read_int(&mut self) -> Result<i32, CodecError> {
        let (value, size) = decode_int(&self.input[self.offset..])?;
        self.offset += size;
        Ok(value)
    }

    pub fn read_string(&mut self) -> Result<&'a str, CodecError> {
        let remaining = &self.input[self.offset..];
        let end = remaining
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(CodecError::UnexpectedEof)?;
        let result = std::str::from_utf8(&remaining[..end])
            .map_err(|_| CodecError::InvalidUtf8)?;
        self.offset += end + 1;
        Ok(result)
    }

    pub fn read_raw(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        let end = self.offset.checked_add(len).ok_or(CodecError::UnexpectedEof)?;
        let bytes = self.input.get(self.offset..end).ok_or(CodecError::UnexpectedEof)?;
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_integer_layout_vectors() {
        let vectors: &[(i32, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (-1, &[0x40]),
            (63, &[0x3f]),
            (-64, &[0x7f]),
            (64, &[0x80, 0x01]),
            (-65, &[0xc0, 0x01]),
            (255, &[0xbf, 0x03]),
            (i32::MAX, &[0xbf, 0xff, 0xff, 0xff, 0x0f]),
            (i32::MIN, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        ];
        for &(value, bytes) in vectors {
            let mut output = Vec::new();
            write_int(value, &mut output);
            assert_eq!(output, bytes, "encoding {value}");
            assert_eq!(decode_int(bytes), Ok((value, bytes.len())));
        }
    }

    #[test]
    fn roundtrip_short_range_and_random_i32() {
        for value in -32768..=32767 {
            let mut bytes = Vec::new();
            write_int(value, &mut bytes);
            assert_eq!(decode_int(&bytes), Ok((value, bytes.len())));
        }
        let mut state = 0x1234_5678u32;
        for _ in 0..100_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let value = state as i32;
            let mut bytes = Vec::new();
            write_int(value, &mut bytes);
            assert_eq!(decode_int(&bytes), Ok((value, bytes.len())));
        }
    }

    #[test]
    fn rejected_malformed_varints() {
        assert_eq!(decode_int(&[]), Err(CodecError::UnexpectedEof));
        assert_eq!(decode_int(&[0x80]), Err(CodecError::UnexpectedEof));
        assert_eq!(decode_int(&[0x80; 5]), Err(CodecError::IntegerOverflow));
        assert_eq!(
            decode_int(&[0xbf, 0xff, 0xff, 0xff, 0x1f]),
            Err(CodecError::IntegerOverflow)
        );
    }

    #[test]
    fn strings_and_cursor_bounds() {
        let mut bytes = Vec::new();
        write_int(-200, &mut bytes);
        write_string("Ти", &mut bytes).unwrap();
        bytes.extend_from_slice(&[7, 8]);
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.read_int(), Ok(-200));
        assert_eq!(reader.read_string(), Ok("Ти"));
        assert_eq!(reader.read_raw(2), Ok(&[7u8, 8][..]));
        assert_eq!(reader.remaining(), 0);
        assert_eq!(reader.read_int(), Err(CodecError::UnexpectedEof));
        assert_eq!(reader.read_raw(usize::MAX), Err(CodecError::UnexpectedEof));
    }

    #[test]
    fn malformed_strings_do_not_advance() {
        let mut reader = Reader::new(&[0xff, 0]);
        assert_eq!(reader.read_string(), Err(CodecError::InvalidUtf8));
        assert_eq!(reader.remaining(), 2);
        assert_eq!(write_string("a\0b", &mut Vec::new()), Err(CodecError::EmbeddedNul));
        assert_eq!(
            Reader::new(b"unterminated").read_string(),
            Err(CodecError::UnexpectedEof)
        );
    }
}
