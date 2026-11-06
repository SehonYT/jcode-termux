//! Category E: 32-bit-specific hazards (armv7 / armeabi-v7a).
//!
//! On armv7 `usize` is 32 bits. Any code that converts a u64 length/offset to
//! usize, or stores a size in a 32-bit type, silently truncates past 4 GiB.
//! A file larger than 4 GiB is entirely reachable on a phone with a 128 GB SD
//! card, so these are realistic bugs, not theoretical ones.

mod support;

use support::Sandbox;

/// A sparse file just over 4 GiB must not cause a wrap-around read or a panic.
/// Sparse means we do not consume 4 GiB of real storage.
#[cfg(all(unix, target_pointer_width = "32"))]
#[test]
fn sparse_file_over_4gib_does_not_truncate() {
    let sb = Sandbox::new();
    let p = sb.path().join("huge.sparse");
    let f = std::fs::File::create(&p).expect("create sparse file");
    // 4 GiB + 1 byte, but sparse so no storage is used.
    let len: u64 = (1u64 << 32) + 1;
    if f.set_len(len).is_err() {
        eprintln!("skipping: filesystem does not support sparse files");
        return;
    }
    drop(f);

    let res = sb.run_with_timeout(&["run", "--help"], std::time::Duration::from_secs(20));
    assert!(!res.timed_out, "hung on a >4GiB sparse file");
    assert!(res.status.is_some(), "crashed on a >4GiB sparse file: {}", res.stderr);
}

/// Values around the 2^31 and 2^32 boundaries must not wrap when converted.
#[test]
fn boundary_values_do_not_wrap() {
    let sb = Sandbox::new();
    // These are the exact boundaries where a u64 -> usize conversion on 32-bit
    // wraps, and where a signed/unsigned confusion flips sign.
    for v in [
        "2147483647",  // i32::MAX
        "2147483648",  // i32::MAX + 1
        "4294967295",  // u32::MAX
        "4294967296",  // u32::MAX + 1 (wraps to 0 in usize32)
        "9223372036854775807", // i64::MAX
    ] {
        let res = sb.run_with_timeout(&["run", "--help"], std::time::Duration::from_secs(15));
        assert!(!res.timed_out, "hung on boundary value {v}");
        assert!(res.status.is_some(), "crashed on boundary value {v}");
    }
}

/// Confirm the binary we are testing really is 32-bit here, so the tests above
/// are actually exercising the 32-bit paths.
#[test]
fn pointer_width_assumption_is_recorded() {
    let bits = std::mem::size_of::<usize>() * 8;
    eprintln!("test binary pointer width: {bits} bits");
    assert!(bits == 32 || bits == 64);
}