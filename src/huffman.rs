//! Bit-for-bit Rust port of official DDNet 20.2.1 static Huffman compression.
//! Original: upstream/ddnet/src/engine/shared/huffman.cpp.
//!
//! Tree construction uses stable descending frequency ordering and preserves
//! 0/1 leaf order. Bit output is little-endian within each byte, as upstream.
//! C++ differential tests are part of Linux CI. No FFI or unsafe Rust code.

const EOF_SYMBOL: usize = 256;
const MAX_SYMBOLS: usize = EOF_SYMBOL + 1;
const MAX_NODES: usize = MAX_SYMBOLS * 2 - 1;

/// Fixed table copied from pinned DDNet 20.2.1 huffman.cpp, not trained on
/// dynamic input (same bitstream on all operating systems).
const FREQUENCIES: [u32; MAX_SYMBOLS] = [
    1 << 30, 4545, 2657, 431, 1950, 919, 444, 482, 2244, 617, 838, 542,
    715, 1814, 304, 240, 754, 212, 647, 186, 283, 131, 146, 166,
    543, 164, 167, 136, 179, 859, 363, 113, 157, 154, 204, 108,
    137, 180, 202, 176, 872, 404, 168, 134, 151, 111, 113, 109,
    120, 126, 129, 100, 41, 20, 16, 22, 18, 18, 17, 19,
    16, 37, 13, 21, 362, 166, 99, 78, 95, 88, 81, 70,
    83, 284, 91, 187, 77, 68, 52, 68, 59, 66, 61, 638,
    71, 157, 50, 46, 69, 43, 11, 24, 13, 19, 10, 12,
    12, 20, 14, 9, 20, 20, 10, 10, 15, 15, 12, 12,
    7, 19, 15, 14, 13, 18, 35, 19, 17, 14, 8, 5,
    15, 17, 9, 15, 14, 18, 8, 10, 2173, 134, 157, 68,
    188, 60, 170, 60, 194, 62, 175, 71, 148, 67, 167, 78,
    211, 67, 156, 69, 1674, 90, 174, 53, 147, 89, 181, 51,
    174, 63, 163, 80, 167, 94, 128, 122, 223, 153, 218, 77,
    200, 110, 190, 73, 174, 69, 145, 66, 277, 143, 141, 60,
    136, 53, 180, 57, 142, 57, 158, 61, 166, 112, 152, 92,
    26, 22, 21, 28, 20, 26, 30, 21, 32, 27, 20, 17,
    23, 21, 30, 22, 22, 21, 27, 25, 17, 27, 23, 18,
    39, 26, 15, 21, 12, 18, 18, 27, 20, 18, 15, 19,
    11, 17, 33, 12, 18, 15, 19, 18, 16, 26, 17, 18,
    9, 10, 25, 22, 22, 17, 20, 16, 6, 16, 15, 20,
    14, 18, 24, 335, 1,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuffmanError {
    InputTruncated,
    OutputLimit,
    InvalidTree,
}

#[derive(Debug, Clone)]
struct Node {
    leaves: Option<[usize; 2]>,
    symbol: usize,
    bits: u64,
    count: u8,
}

impl Node {
    fn leaf(symbol: usize) -> Self {
        Self { leaves: None, symbol, bits: 0, count: 0 }
    }

    fn branch(a: usize, b: usize) -> Self {
        Self { leaves: Some([a, b]), symbol: 0, bits: 0, count: 0 }
    }
}

/// A deterministic codec; instantiate once and reuse per connection.
#[derive(Debug, Clone)]
pub struct Huffman {
    nodes: Vec<Node>,
    root: usize,
}

impl Default for Huffman {
    fn default() -> Self {
        Self::new()
    }
}

impl Huffman {
    pub fn new() -> Self {
        let mut nodes: Vec<Node> = (0..MAX_SYMBOLS).map(Node::leaf).collect();
        let mut remaining: Vec<(u64, usize)> = FREQUENCIES.iter()
            .enumerate()
            .map(|(symbol, &freq)| (u64::from(freq), symbol))
            .collect();

        while remaining.len() > 1 {
            // Rust sort_by is stable, preserving C++ std::stable_sort tie
            // ordering even after a merged node changes its frequency.
            remaining.sort_by_key(|left| std::cmp::Reverse(left.0));
            let lowest = remaining.pop().expect("nonempty tree");
            let next = remaining.last_mut().expect("two or more nodes");
            let index = nodes.len();
            nodes.push(Node::branch(lowest.1, next.1));
            next.0 += lowest.0;
            next.1 = index;
        }
        assert_eq!(nodes.len(), MAX_NODES);
        let root = nodes.len() - 1;
        let mut result = Self { nodes, root };
        result.assign_codes(root, 0, 0);
        result
    }

    fn assign_codes(&mut self, node: usize, bits: u64, depth: u8) {
        if let Some([left, right]) = self.nodes[node].leaves {
            self.assign_codes(right, bits | (1u64 << depth), depth + 1);
            self.assign_codes(left, bits, depth + 1);
        } else {
            self.nodes[node].bits = bits;
            self.nodes[node].count = depth;
        }
    }

    pub fn compress(&self, input: &[u8], max_output: usize) -> Result<Vec<u8>, HuffmanError> {
        let mut output = Vec::new();
        let mut bit_buffer = 0u64;
        let mut bit_count = 0u32;

        for symbol in input.iter().map(|&x| x as usize)
            .chain(std::iter::once(EOF_SYMBOL))
        {
            let node = &self.nodes[symbol];
            bit_buffer |= node.bits << bit_count;
            bit_count += u32::from(node.count);
            while bit_count >= 8 {
                if output.len() >= max_output {
                    return Err(HuffmanError::OutputLimit);
                }
                output.push(bit_buffer as u8);
                bit_buffer >>= 8;
                bit_count -= 8;
            }
        }
        if bit_count != 0 {
            if output.len() >= max_output {
                return Err(HuffmanError::OutputLimit);
            }
            output.push(bit_buffer as u8);
        }
        Ok(output)
    }

    /// Decode until the EOF symbol. Each input byte is consumed LSB first.
    /// Output bounds are checked before every byte is appended.
    pub fn decompress(&self, input: &[u8], max_output: usize) -> Result<Vec<u8>, HuffmanError> {
        let mut output = Vec::new();
        let mut current = self.root;
        for &byte in input {
            for shift in 0..8 {
                let bit = usize::from((byte >> shift) & 1);
                let leaves = self.nodes[current].leaves.ok_or(HuffmanError::InvalidTree)?;
                let index = leaves[bit];
                let node = &self.nodes[index];
                if node.leaves.is_none() {
                    if node.symbol == EOF_SYMBOL {
                        return Ok(output);
                    }
                    if output.len() >= max_output {
                        return Err(HuffmanError::OutputLimit);
                    }
                    output.push(node.symbol as u8);
                    current = self.root;
                } else {
                    current = index;
                }
            }
        }
        Err(HuffmanError::InputTruncated)
    }
}

/// Fixed corpus used by both the Rust CLI and the upstream C++ differential
/// test. Keep in sync with tools/reference_huffman.cpp.
pub fn reference_inputs() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        vec![0],
        b"DDNet packet".to_vec(),
        (0..=255u8).collect(),
        vec![0; 512],
        (0..512usize).map(|i| (i & 0xff) as u8).collect(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_encode_reference_corpus() {
        let codec = Huffman::new();
        for input in reference_inputs() {
            let encoded = codec.compress(&input, 10_000).unwrap();
            assert_eq!(codec.decompress(&encoded, 10_000).unwrap(), input);
        }
    }

    #[test]
    fn deterministic_randomized_network_payloads() {
        let codec = Huffman::new();
        let mut state = 0x1865_1032u32;
        for length in [1usize, 2, 3, 17, 31, 64, 128, 512, 1024, 1397] {
            let mut data = vec![0u8; length];
            for b in &mut data {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                *b = (state >> 24) as u8;
            }
            let packed = codec.compress(&data, 10_000).unwrap();
            assert_eq!(codec.decompress(&packed, length).unwrap(), data);
        }
    }

    #[test]
    fn decoding_is_bounded_and_rejects_empty_input() {
        let codec = Huffman::new();
        let bytes = codec.compress(&vec![0u8; 1024], 10_000).unwrap();
        assert_eq!(codec.decompress(&bytes, 100), Err(HuffmanError::OutputLimit));
        assert_eq!(codec.compress(&vec![0u8; 1024], 1), Err(HuffmanError::OutputLimit));
        assert_eq!(codec.decompress(&[], 100), Err(HuffmanError::InputTruncated));
    }

    #[test]
    fn decoder_fuzzes_malformed_bitstrings_without_panicking() {
        let codec = Huffman::new();
        let mut rng = 0x1020_3040u32;
        for length in 0..150 {
            let mut input = vec![0u8; length];
            for byte in &mut input {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = rng as u8;
            }
            let result = codec.decompress(&input, 1400);
            if let Ok(data) = result {
                assert!(data.len() <= 1400);
            }
        }
    }
}
