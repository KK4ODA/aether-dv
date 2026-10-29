//! DDT2 checksum: CRC-16/XMODEM (poly 0x1021, init 0, no reflection).
//!
//! D-RATS computes it bitwise with two zero bytes of message augmentation
//! (`crc_checksum.py`), which is equivalent to the standard non-augmented
//! CRC-16/XMODEM. Check value for `b"123456789"` is `0x31C3`.

/// Compute the DDT2 / CRC-16/XMODEM checksum of `data`.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn empty() {
        assert_eq!(crc16_xmodem(b""), 0);
    }
}
