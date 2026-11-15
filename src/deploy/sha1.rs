pub const SHA1_CHECK_INPUT: &[u8] = b"abc";
pub const SHA1_CHECK: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
const SHA1_BLOCK_BYTES: usize = 64;
const SHA1_LENGTH_BYTES: usize = 8;
const SHA1_SCHEDULE_WORDS: usize = 80;
const SHA1_INITIAL: [u32; 5] = [
    0x6745_2301,
    0xefcd_ab89,
    0x98ba_dcfe,
    0x1032_5476,
    0xc3d2_e1f0,
];
const SHA1_ROUND_CONSTANTS: [u32; 4] = [0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xca62_c1d6];
const SHA1_ROUNDS_PER_CONSTANT: usize = 20;

pub fn sha1(bytes: &[u8]) -> [u8; 20] {
    let mut state = SHA1_INITIAL;
    let bit_length = u64::try_from(bytes.len()).expect("a length fits u64") * 8;
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % SHA1_BLOCK_BYTES != SHA1_BLOCK_BYTES - SHA1_LENGTH_BYTES {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());
    assert_eq!(
        padded.len() % SHA1_BLOCK_BYTES,
        0,
        "padding fills whole blocks"
    );
    for block in padded.chunks_exact(SHA1_BLOCK_BYTES) {
        compress(&mut state, block);
    }
    let mut digest = [0_u8; 20];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

fn compress(state: &mut [u32; 5], block: &[u8]) {
    let mut schedule = [0_u32; SHA1_SCHEDULE_WORDS];
    for (index, word) in block.chunks_exact(4).enumerate() {
        schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for index in 16..SHA1_SCHEDULE_WORDS {
        schedule[index] = (schedule[index - 3]
            ^ schedule[index - 8]
            ^ schedule[index - 14]
            ^ schedule[index - 16])
            .rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (index, word) in schedule.iter().enumerate() {
        let round = index / SHA1_ROUNDS_PER_CONSTANT;
        let mixed = match round {
            0 => (b & c) | (!b & d),
            2 => (b & c) | (b & d) | (c & d),
            _ => b ^ c ^ d,
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(mixed)
            .wrapping_add(e)
            .wrapping_add(SHA1_ROUND_CONSTANTS[round])
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(value);
    }
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    sha1(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
