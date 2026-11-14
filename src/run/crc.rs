pub const CRC32_POLYNOMIAL_REFLECTED: u32 = 0xedb8_8320;
const BYTE_VALUES: usize = 256;
const BITS_PER_BYTE: u32 = 8;

const TABLE: [u32; BYTE_VALUES] = table();

const fn table() -> [u32; BYTE_VALUES] {
    let mut table = [0_u32; BYTE_VALUES];
    let mut index = 0;
    while index < BYTE_VALUES {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < BITS_PER_BYTE {
            value = if value & 1 == 1 {
                (value >> 1) ^ CRC32_POLYNOMIAL_REFLECTED
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xff) as usize;
        crc = (crc >> BITS_PER_BYTE) ^ TABLE[index];
    }
    !crc
}
