# Crate rename map

The workspace was `vlogin-*` (video login). It is now content-extraction:

| old | new |
|---|---|
| `vlogin-core` | `extract-core` (kernel; names no platform) |
| `vlogin-core/src/channels/bilibili.rs` | `platform-bilibili` crate |
| `vlogin-core/src/channels/youtube.rs` | `platform-youtube` crate |
| `vlogin-core/src/channels/douyin.rs` | `platform-douyin` crate |
| `vlogin-core/src/channels/tencent.rs` | `platform-tencent` crate |
| `vlogin-core/src/channels/iqiyi.rs` | `platform-iqiyi` crate |
| `vlogin-core/src/channels/youku.rs` | `platform-youku` crate |
| `vlogin-core/src/channels/generic.rs` | `platform-generic` crate |
| `vlogin-core/src/channels/mod.rs` | `extract-core/src/provider.rs` |
| `vlogin-abi` | `extract-abi` (`libextract.so`, composition root) |
| `vlogin-cli` (`vlogin`) | `extract-cli` (`cx`) |

New platforms added with the widening: `platform-wmpvp` (完美世界电竞,
news.wmpvp.com) and `platform-astralparty` (吉星派对).

The old `crates/vlogin-*` directories have been removed.

## Host-based naming (follow-up rename)

`platform-pwp` → **`platform-wmpvp`**, because `pwp` did not identify the site.
The rule is now: name a platform after its **host**, so the directory answers
"which site is this?" on its own.

| old | new |
|---|---|
| `platform-pwp` crate / `Pwp` struct | `platform-wmpvp` / `WmPvp` |
| platform id `pwp` | `wmpvp` |
| extractors `pwp.video` / `pwp.note` | `wmpvp.video` / `wmpvp.note` |
| `hosts/dsh/lib/content/pwp/` | `hosts/dsh/lib/content/wmpvp/` |
| `test/live-pwp.mjs` | `test/live-wmpvp.mjs` |

The superseded `crates/platform-pwp`, `hosts/dsh/lib/fetch/` and
`hosts/dsh/lib/content/pwp/` trees have been removed, along with the duplicate
`test/fetch.test.js` and `test/live-pwp.mjs`.
