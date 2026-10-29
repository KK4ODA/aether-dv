//! Compressed golden vector. For this upstream vector our deflate (miniz_oxide
//! via flate2, level 9) reproduces CPython `zlib.compress(..., 9)` byte for
//! byte. General byte parity across deflate implementations is NOT guaranteed
//! (macrats found Apple's differs); receivers must rely on decode equivalence.
use aether_ddt2::{Frame, PayloadEncoding};

#[test]
fn compressed_upstream_vector_byte_exact() {
    let f = Frame::new(3, 2, 1, "FOO", "BAR", b"This is a test".to_vec());
    let ours: String = f
        .encode(PayloadEncoding::Always)
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        ours,
        "dd00030201c3ab0014464f4f7e7e7e7e7e4241527e7e7e7e7e78da0bc9c82c5600a2448592d4e21200247304f6"
    );
}
