//! Inputs this crate cannot produce for itself.
//!
//! Reading a clock and drawing entropy are effects, so a protocol layer must not
//! do either. A few wire formats still need them: Tencent wants a cache-busting
//! `t=` parameter, iQiyi and Douyin mint a per-session device fingerprint, the
//! account pool stamps `created_at`. Those values are part of the request or the
//! document, not machine-local decisions, so the *values* belong here while
//! *producing* them stays with the caller.
//!
//! Every provider therefore takes an [`Ambient`] argument and never asks the
//! operating system anything. `extract-abi` is where a real clock and a real
//! entropy source are wired in.

use crate::util;

/// One operation's clock and entropy, supplied by the caller.
///
/// Not `Copy`-randomised per call on purpose: a caller that performs one
/// operation constructs one `Ambient`, so a single numeric seed keeps every
/// derived value inside that operation distinct from the next operation's
/// without this crate owning a counter.
#[derive(Debug, Clone, Copy)]
pub struct Ambient {
    now_ms: i64,
    seed: u64,
}

impl Ambient {
    /// Build one operation's inputs: `now_ms` is milliseconds since the epoch,
    /// `seed` is a caller-supplied pseudo-entropy seed.
    #[must_use]
    pub fn new(now_ms: i64, seed: u64) -> Self {
        Self { now_ms, seed }
    }

    /// Milliseconds since the epoch, as handed in.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// Seconds since the epoch, for the cookie codec's `Max-Age` arithmetic.
    #[must_use]
    pub fn now_unix(&self) -> i64 {
        self.now_ms.div_euclid(1000)
    }

    /// The seed this operation was given.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// A 16-hex-character id, used for account records.
    #[must_use]
    pub fn id(&self) -> String {
        util::id_from(self.seed)
    }

    /// An opaque random-looking token drawn from `[a-zA-Z0-9]`.
    ///
    /// NOT CRYPTOGRAPHIC — see [`util::token_from`]. Fine for a device
    /// fingerprint, indefensible for a secret.
    #[must_use]
    pub fn token(&self, len: usize) -> String {
        util::token_from(self.seed, len)
    }

    /// A sibling `Ambient` for a second independent value in one operation.
    ///
    /// Without this, two tokens drawn from one `Ambient` would be identical;
    /// the salt makes them independent while keeping this crate stateless.
    #[must_use]
    pub fn derive(&self, salt: u64) -> Self {
        Self { now_ms: self.now_ms, seed: util::mix(self.seed ^ salt) }
    }
}
