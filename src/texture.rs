//! Bounded, safe PNG texture loader for DDNet mapres assets.
//! Supports non-interlaced 8-bit PNG grayscale/RGB/indexed/GA/RGBA,
//! with all five PNG row filters and explicit CRC checking.
//! APNG, interlacing and 1/2/4/16-bit PNGs are not yet supported.

use std::io::Read;

pub const MAX_TEXTURE_DIMENSION: u32 = 4096;
const MAX_PNG_INPUT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureError {
    Malformed,
    Unsupported,
    Corrupt,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// Premultiplication is NOT applied. RGBA byte order.
    pub rgba: Vec<u8>,
}

fn read_be_u32(data: &[u8]) -> Result<u32, TextureError> {
    let value = data.get(..4).ok_or(TextureError::Malformed)?;
    Ok(u32::from_be_bytes(value.try_into().map_err(|_| TextureError::Malformed)?))
}

fn valid_dimensions(w: u32, h: u32) -> Result<usize, TextureError> {
    if w == 0 || h == 0 || w > MAX_TEXTURE_DIMENSION || h > MAX_TEXTURE_DIMENSION {
        return Err(TextureError::TooLarge);
    }
    (w as usize).checked_mul(h as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or(TextureError::TooLarge)
}

fn paeth(left: u8, up: u8, corner: u8) -> u8 {
    let a = i32::from(left);
    let b = i32::from(up);
    let c = i32::from(corner);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc { left }
    else if pb <= pc { up }
    else { corner }
}

impl Texture {
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, TextureError> {
        if rgba.len() != valid_dimensions(width, height)? {
            return Err(TextureError::Malformed);
        }
        Ok(Self { width, height, rgba })
    }

    /// Verify PNG chunk checksums and reconstruct filtered scanlines using
    /// the existing Rust-only zlib backend. No libpng, GPU or unsafe calls.
    pub fn from_png(bytes: &[u8]) -> Result<Self, TextureError> {
        if bytes.len() > MAX_PNG_INPUT {
            return Err(TextureError::TooLarge);
        }
        if !bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
            return Err(TextureError::Malformed);
        }

        let mut cursor = 8usize;
        let mut header: Option<(u32, u32, u8)> = None;
        let mut compressed = Vec::new();
        let mut palette = Vec::new();
        let mut transparency = Vec::new();
        let mut seen_idat = false;
        let mut seen_iend = false;
        while cursor < bytes.len() {
            let length = read_be_u32(bytes.get(cursor..).ok_or(TextureError::Malformed)?)?
                as usize;
            let start = cursor.checked_add(4).ok_or(TextureError::TooLarge)?;
            let body = start.checked_add(4).ok_or(TextureError::TooLarge)?;
            let after_data = body.checked_add(length).ok_or(TextureError::TooLarge)?;
            let after_crc = after_data.checked_add(4).ok_or(TextureError::TooLarge)?;
            if after_crc > bytes.len() { return Err(TextureError::Malformed); }
            let kind = &bytes[start..body];
            let payload = &bytes[body..after_data];
            let expected_crc = read_be_u32(&bytes[after_data..after_crc])?;
            if crate::game_protocol::crc32_update(0, &bytes[start..after_data]) != expected_crc {
                return Err(TextureError::Corrupt);
            }
            if header.is_none() && kind != b"IHDR" {
                return Err(TextureError::Malformed);
            }
            match kind {
                b"IHDR" => {
                    if header.is_some() || length != 13 { return Err(TextureError::Malformed); }
                    let width = read_be_u32(payload)?;
                    let height = read_be_u32(&payload[4..])?;
                    valid_dimensions(width, height)?;
                    let bit_depth = payload[8];
                    let kind = payload[9];
                    if bit_depth != 8 || !matches!(kind, 0 | 2 | 3 | 4 | 6)
                        || payload[10..13] != [0, 0, 0]
                    {
                        return Err(TextureError::Unsupported);
                    }
                    header = Some((width, height, kind));
                }
                b"PLTE" => {
                    if seen_idat || !palette.is_empty() || payload.is_empty()
                        || payload.len() % 3 != 0 || payload.len() > 768
                    {
                        return Err(TextureError::Malformed);
                    }
                    palette.extend_from_slice(payload);
                }
                b"tRNS" => {
                    if seen_idat || !transparency.is_empty() {
                        return Err(TextureError::Malformed);
                    }
                    transparency.extend_from_slice(payload);
                }
                b"IDAT" => {
                    seen_idat = true;
                    if compressed.len().checked_add(length).is_none_or(|n| n > MAX_PNG_INPUT) {
                        return Err(TextureError::TooLarge);
                    }
                    compressed.extend_from_slice(payload);
                }
                b"IEND" => {
                    if length != 0 || !seen_idat || after_crc != bytes.len() {
                        return Err(TextureError::Malformed);
                    }
                    seen_iend = true;
                    break;
                }
                _ => {
                    // PNG critical chunks have the uppercase first byte.
                    if kind[0].is_ascii_uppercase() { return Err(TextureError::Unsupported); }
                }
            }
            cursor = after_crc;
        }
        if !seen_iend { return Err(TextureError::Malformed); }
        let (width, height, color_type) = header.ok_or(TextureError::Malformed)?;
        if (color_type == 3 && palette.is_empty())
            || (color_type == 3 && transparency.len() > palette.len() / 3)
        {
            return Err(TextureError::Malformed);
        }
        let channels: usize = match color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            6 => 4,
            _ => return Err(TextureError::Unsupported),
        };
        if (color_type == 0 && !matches!(transparency.len(), 0 | 2))
            || (color_type == 2 && !matches!(transparency.len(), 0 | 6))
            || (matches!(color_type, 4 | 6) && !transparency.is_empty())
        {
            return Err(TextureError::Malformed);
        }

        let stride = (width as usize).checked_mul(channels)
            .ok_or(TextureError::TooLarge)?;
        let expected = stride.checked_add(1).and_then(|n| n.checked_mul(height as usize))
            .ok_or(TextureError::TooLarge)?;
        let mut decoded = Vec::new();
        let limit = u64::try_from(expected).map_err(|_| TextureError::TooLarge)?
            .checked_add(1).ok_or(TextureError::TooLarge)?;
        flate2::read::ZlibDecoder::new(&compressed[..]).take(limit)
            .read_to_end(&mut decoded).map_err(|_| TextureError::Corrupt)?;
        if decoded.len() != expected { return Err(TextureError::Corrupt); }

        let mut rgba = Vec::with_capacity(valid_dimensions(width, height)?);
        let mut previous = vec![0u8; stride];
        let mut current = vec![0u8; stride];
        for row in decoded.chunks_exact(stride + 1) {
            let filter = row[0];
            if filter > 4 { return Err(TextureError::Unsupported); }
            for x in 0..stride {
                let left = if x >= channels { current[x - channels] } else { 0 };
                let above = previous[x];
                let corner = if x >= channels { previous[x - channels] } else { 0 };
                let predictor = match filter {
                    0 => 0,
                    1 => left,
                    2 => above,
                    3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
                    4 => paeth(left, above, corner),
                    _ => unreachable!(),
                };
                current[x] = row[x + 1].wrapping_add(predictor);
            }
            for texel in current.chunks_exact(channels) {
                match color_type {
                    0 => {
                        let a = if transparency.len() == 2 && texel[0] == transparency[1] {
                            0
                        } else { 255 };
                        rgba.extend_from_slice(&[texel[0], texel[0], texel[0], a]);
                    }
                    2 => {
                        let a = if transparency.len() == 6 && transparency[1] == texel[0]
                            && transparency[3] == texel[1]
                            && transparency[5] == texel[2]
                        { 0 } else { 255 };
                        rgba.extend_from_slice(&[texel[0], texel[1], texel[2], a]);
                    }
                    3 => {
                        let index = texel[0] as usize;
                        let at = index * 3;
                        let colors = palette.get(at..at + 3).ok_or(TextureError::Malformed)?;
                        let a = transparency.get(index).copied().unwrap_or(255);
                        rgba.extend_from_slice(&[colors[0], colors[1], colors[2], a]);
                    }
                    4 => rgba.extend_from_slice(&[texel[0], texel[0], texel[0], texel[1]]),
                    6 => rgba.extend_from_slice(texel),
                    _ => unreachable!(),
                }
            }
            std::mem::swap(&mut previous, &mut current);
        }
        Self::from_rgba(width, height, rgba)
    }

    pub fn pixel(&self, x: usize, y: usize) -> Option<[u8; 4]> {
        if x >= self.width as usize || y >= self.height as usize { return None; }
        let at = (y * self.width as usize + x) * 4;
        Some(self.rgba.get(at..at + 4)?.try_into().ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn chunk(out: &mut Vec<u8>, name: &[u8; 4], bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(bytes);
        let start = out.len() - bytes.len() - 4;
        out.extend_from_slice(&crate::game_protocol::crc32_update(0, &out[start..]).to_be_bytes());
    }

    fn png(width: u32, height: u32, color: u8, scanlines: &[u8]) -> Vec<u8> {
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, color, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        let mut writer = flate2::write::ZlibEncoder::new(
            Vec::new(), flate2::Compression::default(),
        );
        writer.write_all(scanlines).unwrap();
        chunk(&mut png, b"IDAT", &writer.finish().unwrap());
        chunk(&mut png, b"IEND", &[]);
        png
    }

    #[test]
    fn decodes_existing_rgb_framebuffer_png_losslessly() {
        let mut frame = crate::graphics::Framebuffer::new(2, 2).unwrap();
        frame.pixels = vec![0x00ff00, 0x123456, 0xabcdef, 0x0000ff];
        let texture = Texture::from_png(&frame.png_bytes().unwrap()).unwrap();
        assert_eq!(texture.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(texture.pixel(1, 0), Some([0x12, 0x34, 0x56, 255]));
        assert_eq!(texture.pixel(0, 1), Some([0xab, 0xcd, 0xef, 255]));
        assert_eq!(texture.pixel(1, 1), Some([0, 0, 255, 255]));
    }

    #[test]
    fn reconstructs_all_filters_and_alpha() {
        // Two RGBA pixels, five rows. Each filter reconstructs the same
        // 8-byte row, including left, upper and Paeth predictors.
        let wanted = [20, 60, 100, 255, 80, 120, 140, 128];
        let mut input = Vec::new();
        let mut previous = [0u8; 8];
        for filter in 0..=4u8 {
            input.push(filter);
            for x in 0..8 {
                let left = if x < 4 { 0 } else { wanted[x - 4] };
                let up = previous[x];
                let corner = if x < 4 { 0 } else { previous[x - 4] };
                let predictor = match filter {
                    0 => 0,
                    1 => left,
                    2 => up,
                    3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                    _ => paeth(left, up, corner),
                };
                input.push(wanted[x].wrapping_sub(predictor));
            }
            previous = wanted;
        }
        let tex = Texture::from_png(&png(2, 5, 6, &input)).unwrap();
        for row in 0..5 {
            assert_eq!(&tex.rgba[row * 8..(row + 1) * 8], wanted.as_slice());
        }
    }

    #[test]
    fn rejects_corruption_unsupported_and_oversized_frames() {
        let good = png(1, 1, 2, &[0, 255, 0, 0]);
        assert_eq!(Texture::from_png(&good).unwrap().pixel(0, 0),
            Some([255, 0, 0, 255]));
        let mut bad = good.clone();
        bad[20] ^= 1;
        assert_eq!(Texture::from_png(&bad), Err(TextureError::Corrupt));
        let mut unsupported = png(1, 1, 2, &[0, 255, 0, 0]);
        unsupported[24] = 1; // PNG interlace flag, CRC must be updated.
        assert!(Texture::from_png(&unsupported).is_err());
        assert_eq!(Texture::from_png(&good[..15]), Err(TextureError::Malformed));
        assert_eq!(Texture::from_rgba(4097, 1, vec![]), Err(TextureError::TooLarge));
    }

    #[test]
    fn palette_png_alpha_and_zero_length_data_are_validated() {
        let mut bytes = vec![137, 80, 78, 71, 13, 10, 26, 10];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        chunk(&mut bytes, b"IHDR", &ihdr);
        chunk(&mut bytes, b"PLTE", &[0, 255, 0, 255, 0, 0]);
        chunk(&mut bytes, b"tRNS", &[255, 17]);
        let mut compressor = flate2::write::ZlibEncoder::new(
            Vec::new(), flate2::Compression::default(),
        );
        compressor.write_all(&[0, 0, 1]).unwrap();
        chunk(&mut bytes, b"IDAT", &compressor.finish().unwrap());
        chunk(&mut bytes, b"IEND", &[]);
        let tex = Texture::from_png(&bytes).unwrap();
        assert_eq!(tex.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(tex.pixel(1, 0), Some([255, 0, 0, 17]));
        bytes.push(0);
        assert_eq!(Texture::from_png(&bytes), Err(TextureError::Malformed));
    }
}
