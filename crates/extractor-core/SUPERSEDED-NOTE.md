# Crate rename map

The workspace was `vlogin-*` (video login). It is now content-extractorion:

| old | new |
|---|---|
| `vlogin-core` | `extractor-core` (kernel; names no platform) |
| `vlogin-core/src/channels/bilibili.rs` | `platform-bilibili` crate |
| `vlogin-core/src/channels/youtube.rs` | `platform-youtube` crate |
| `vlogin-core/src/channels/douyin.rs` | `platform-douyin` crate |
| `vlogin-core/src/channels/tencent.rs` | `platform-tencent` crate |
| `vlogin-core/src/channels/iqiyi.rs` | `platform-iqiyi` crate |
| `vlogin-core/src/channels/youku.rs` | `platform-youku` crate |
| `vlogin-core/src/channels/generic.rs` | `platform-generic` crate |
| `vlogin-core/src/channels/mod.rs` | `extractor-core/src/provider.rs` |
| `vlogin-abi` | `extractor-abi` (`libextractor.so`, composition root) |
| `vlogin-cli` (`vlogin`) | `extractor-cli` (`cx`) |

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

## Noun rename (final): content-extract → content-extractor

The project name became the noun `content-extractor`, and every identifier was
moved with it so the repo name and the code agree:

| old | new |
|---|---|
| `extract-core` / `extract_core` | `extractor-core` / `extractor_core` |
| `extract-abi` (rlib `extract`) | `extractor-abi` (rlib `extractor`) |
| `extract-cli` | `extractor-cli` (binary stays `cx`) |
| `libextract.so` | `libextractor.so` |
| `extract_call` / `extract_version` | `extractor_call` / `extractor_version` |
| `$EXTRACT_HOME` / `$EXTRACTOR_NATIVE_LIB` | `$EXTRACTOR_HOME` / `$EXTRACTOR_NATIVE_LIB` |
| plugin `dsh-content-extract` | `dsh-content-extractor` |
| tool `content_extract` | `content_extractor` |
| routes `/content-extract/*` | `/content-extractor/*` |
| state dir `content-extract` | `content-extractor` |

The verb survives where it is a verb: `Extractor.extract()`, `ContentKind`,
`candidates()`. Only the *names of things* became nouns.
