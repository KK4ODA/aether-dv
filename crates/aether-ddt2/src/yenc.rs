//! D-RATS yEnc variant: escape selected byte values as `'=' , (b + 64) mod 256`.
//!
//! Decoding accepts any escaped byte, so a sender may escape more values than
//! the legacy set without breaking legacy receivers (D-Rats PR #302 relied on
//! this when it added `0x84`/`0xE7`).

/// Escape character.
pub const ESCAPE: u8 = b'=';
const OFFSET: u8 = 64;

/// Bytes D-RATS 0.4.x escapes (`yencode.py` `DEFAULT_BANNED`, post-#302), plus `=`.
pub const LEGACY_BANNED: &[u8] = &[
    0x11, 0x13, 0x1A, 0x00, 0x84, 0xE7, 0xFD, 0xFE, 0xFF, 0xC0, 0xDB, ESCAPE,
];

/// JARL STD6 §6.2 also reserves `0x76` in simple data. D-RATS does not escape
/// it; Aether may (hardware test HW-2 decides). Decode-compatible either way.
pub const JARL_EXTRA: &[u8] = &[0x76];

/// A set of byte values to escape. Always includes the escape character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapeSet([bool; 256]);

impl EscapeSet {
    /// Build from a list of byte values; `=` is always added.
    pub fn new(bytes: &[u8]) -> Self {
        let mut set = [false; 256];
        for &b in bytes {
            set[b as usize] = true;
        }
        set[ESCAPE as usize] = true;
        EscapeSet(set)
    }

    /// The D-RATS 0.4.x set.
    pub fn legacy() -> Self {
        Self::new(LEGACY_BANNED)
    }

    /// Legacy set plus the JARL-reserved `0x76`.
    pub fn legacy_plus_jarl() -> Self {
        let mut v = LEGACY_BANNED.to_vec();
        v.extend_from_slice(JARL_EXTRA);
        Self::new(&v)
    }

    #[inline]
    pub fn contains(&self, b: u8) -> bool {
        self.0[b as usize]
    }
}

impl Default for EscapeSet {
    fn default() -> Self {
        Self::legacy()
    }
}

/// Encode `input`, escaping every byte in `set`.
pub fn encode(input: &[u8], set: &EscapeSet) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + input.len() / 16 + 4);
    for &b in input {
        if set.contains(b) {
            out.push(ESCAPE);
            out.push(b.wrapping_add(OFFSET));
        } else {
            out.push(b);
        }
    }
    out
}

/// Error from [`decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YencError {
    /// Input ended right after an escape character.
    TrailingEscape,
}

/// Decode, accepting an escape before any byte value.
///
/// Legacy D-RATS indexes past the end on a trailing `=` (IndexError); we
/// report it instead.
pub fn decode(input: &[u8]) -> Result<Vec<u8>, YencError> {
    let mut out = Vec::with_capacity(input.len());
    let mut it = input.iter();
    while let Some(&b) = it.next() {
        if b == ESCAPE {
            let &next = it.next().ok_or(YencError::TrailingEscape)?;
            out.push(next.wrapping_sub(OFFSET));
        } else {
            out.push(b);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_bytes_both_sets() {
        let all: Vec<u8> = (0..=255u8).collect();
        for set in [EscapeSet::legacy(), EscapeSet::legacy_plus_jarl()] {
            let enc = encode(&all, &set);
            for (i, &b) in enc.iter().enumerate() {
                // no banned value may appear unescaped
                if set.contains(b) && b != ESCAPE {
                    assert_eq!(enc[i - 1], ESCAPE, "unescaped banned byte {b:#04x}");
                }
            }
            assert_eq!(decode(&enc).unwrap(), all);
        }
    }

    #[test]
    fn legacy_decoder_semantics_accept_extra_escapes() {
        // A legacy receiver decodes 0x76 escaped by an Aether sender.
        let enc = encode(&[0x76, b'A'], &EscapeSet::legacy_plus_jarl());
        assert_eq!(enc, vec![b'=', 0xB6, b'A']);
        assert_eq!(decode(&enc).unwrap(), vec![0x76, b'A']);
    }

    #[test]
    fn trailing_escape_is_an_error() {
        assert_eq!(decode(b"abc="), Err(YencError::TrailingEscape));
    }
}
