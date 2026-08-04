//! Small leaf helpers shared by the whole subsystem.
//!
//! Everything here is pure and dependency-free so that `registry.rs`, `state.rs`,
//! `jobs.rs` and `ops.rs` can all use it without any of them naming each other.

use crate::git::error::GitError;

/// A clock, injectable so `jobs.rs` can test retention and throttling with zero
/// `sleep` calls.
pub type NowFn = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;

/// Milliseconds since the unix epoch.
///
/// Saturating rather than panicking: a machine whose clock is set before 1970 is
/// misconfigured, not a reason to take the app down.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub fn system_now() -> NowFn {
    std::sync::Arc::new(now_ms)
}

/// `1785766867000` -> `"2026-08-03 14:21:07 UTC"`.
///
/// Hand-rolled instead of pulling in `chrono`/`time`: the host needs exactly one
/// format, in one timezone, for log lines and commit messages.
pub fn utc_stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month, day).
///
/// Proleptic Gregorian with no leap seconds — the same calendar `date -u` prints.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard-alphabet base64 with the `=` padding stripped.
///
/// Twenty lines instead of a dependency, and the only caller is the SSH host-key
/// fingerprint, whose format (`SHA256:<43 chars>`) is defined as unpadded.
pub fn b64_nopad(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// git2 hands out index and conflict paths as raw bytes.
///
/// Non-UTF-8 paths are legal in git on unix, and `String::from_utf8_lossy` would
/// produce a path that no longer addresses the entry — so convert losslessly there,
/// and refuse on Windows, where such a path cannot be represented at all.
#[cfg(unix)]
pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b)))
}

#[cfg(windows)]
pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError> {
    std::str::from_utf8(b)
        .map(std::path::PathBuf::from)
        .map_err(|_| GitError::io("non-UTF-8 path in index is not representable on Windows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_after_this_was_written() {
        // 2026-01-01T00:00:00Z. Catches a clock read that silently returns 0.
        assert!(now_ms() > 1_767_225_600_000);
        assert!(system_now()() > 1_767_225_600_000);
    }

    #[test]
    fn utc_stamp_matches_date_u() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_stamp(999), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_stamp(1_000), "1970-01-01 00:00:01 UTC");
        assert_eq!(utc_stamp(1_785_766_867_000), "2026-08-03 14:21:07 UTC");
        assert_eq!(utc_stamp(1_785_000_000_205), "2026-07-25 17:20:00 UTC");
        // Leap day, and the last second before the next one.
        assert_eq!(utc_stamp(1_709_164_800_000), "2024-02-29 00:00:00 UTC");
        assert_eq!(utc_stamp(1_709_251_199_000), "2024-02-29 23:59:59 UTC");
        // 2000 is a leap year, 1900 and 2100 are not; a naive %4 rule breaks here.
        assert_eq!(utc_stamp(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_stamp(4_107_542_400_000), "2100-03-01 00:00:00 UTC");
    }

    #[test]
    fn b64_nopad_matches_rfc4648_without_padding() {
        assert_eq!(b64_nopad(b""), "");
        assert_eq!(b64_nopad(b"f"), "Zg");
        assert_eq!(b64_nopad(b"fo"), "Zm8");
        assert_eq!(b64_nopad(b"foo"), "Zm9v");
        assert_eq!(b64_nopad(b"foob"), "Zm9vYg");
        assert_eq!(b64_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(b64_nopad(b"foobar"), "Zm9vYmFy");
        // Both non-alphanumeric characters of the standard alphabet.
        assert_eq!(b64_nopad(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(b64_nopad(&[0xfb, 0xff, 0xbf]), "+/+/");
        // A sha256 digest is 32 bytes -> 43 unpadded characters.
        assert_eq!(b64_nopad(&[0u8; 32]).len(), 43);
    }

    #[cfg(unix)]
    #[test]
    fn bytes_to_path_keeps_non_utf8_bytes_addressable() {
        use std::os::unix::ffi::OsStrExt;
        let raw = b"caf\xe9/notes.md";
        let p = bytes_to_path(raw).expect("unix paths are bytes");
        assert_eq!(p.as_os_str().as_bytes(), raw);
    }
}
