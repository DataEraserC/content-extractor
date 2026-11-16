//! Small helpers with no home of their own.
//!
//! Deliberately dependency-free: both of these are a handful of lines, and
//! pulling in `base64`/`rand` for them would add two crates to the build graph
//! for something we can state outright.
//!
//! ## Why the generators take a seed
//!
//! Reading a clock or an entropy source is an effect, so this crate does not do
//! it. The token and id generators are pure functions of a caller-supplied
//! seed; [`crate::ambient::Ambient`] is the type that carries one, and
//! `vlogin-abi` is where a real clock and a real seed enter the process.

/// Standard base64 (RFC 4648, with padding).
///
/// Used for turning a site-supplied QR PNG into a data URL, and for the
/// RSA-encrypted password blob bilibili expects.
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b =
            [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// One round of splitmix64, so a low-entropy seed still starts a stream
/// somewhere unpredictable.
///
/// Exposed because [`crate::ambient::Ambient::derive`] needs to turn one seed
/// into an independent one without holding any state.
#[must_use]
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x | 1
}

/// Random token drawn from `[a-zA-Z0-9]`, for device fingerprints and OAuth
/// `state` values.
///
/// NOT CRYPTOGRAPHIC. It is a pure function of the seed and passed through
/// xorshift. That is fine for an opaque correlation value, and would be
/// indefensible for a secret. `state` only needs to be unguessable to the
/// extent that it defeats a drive-by CSRF on a loopback listener — use real
/// `OsRng` if that threat model ever tightens.
#[must_use]
pub fn token_from(seed: u64, len: usize) -> String {
    const TABLE: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut x = mix(seed);
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push(TABLE[(x % 62) as usize] as char);
    }
    out
}

/// Random 16-hex-char id, used for account records and session ids.
#[must_use]
pub fn id_from(seed: u64) -> String {
    let mut x = mix(seed);
    let mut out = String::with_capacity(16);
    for _ in 0..16 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push_str(&format!("{:x}", (x >> 60) & 0xf));
    }
    out
}

/// Extract the lower-cased host from an absolute URL.
///
/// Hand-rolled rather than pulled from a crate: the only thing URL routing here
/// needs is a hostname to suffix-match against a channel's host list, and taking
/// `url` as a dependency would drag in the whole ICU/Unicode-IDNA tree for one
/// string operation.
#[must_use]
pub fn host_of(input: &str) -> Option<String> {
    let rest = input.split_once("://").map(|(_, rest)| rest).unwrap_or(input);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit_once('@').map(|(_, host)| host).unwrap_or(authority);
    // Bare IPv6 literals keep their brackets; strip them so suffix matching
    // and comparisons behave the same as for names.
    let host = authority
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(authority)
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Percent-encode a byte slice for use in a query string or form body.
#[must_use]
pub fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Append `key=value` to a query string, choosing `?` or `&` as needed.
pub fn push_query(query: &mut String, key: &str, value: &str) {
    query.push_str(if query.is_empty() { "?" } else { "&" });
    query.push_str(&format!("{}={}", urlencode(key), urlencode(value)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_tokens_are_deterministic_per_seed_and_differ_across_seeds() {
        assert_eq!(id_from(7), id_from(7));
        assert_ne!(id_from(7), id_from(8));
        assert_eq!(id_from(7).len(), 16);
        assert!(id_from(7).bytes().all(|b| b.is_ascii_hexdigit()));

        assert_eq!(token_from(7, 32).len(), 32);
        assert_ne!(token_from(7, 32), token_from(8, 32));
        assert!(token_from(7, 32).bytes().all(|b| b.is_ascii_alphanumeric()));
    }

    #[test]
    fn host_of_reads_the_authority_not_the_path() {
        assert_eq!(host_of("https://www.bilibili.com/video/BV1"), Some("www.bilibili.com".into()));
        assert_eq!(host_of("https://user:pw@Host.Example:8443/x"), Some("host.example".into()));
        assert_eq!(host_of("https://[::1]:80/"), Some("::1".into()));
        assert_eq!(host_of(""), None);
    }
}
