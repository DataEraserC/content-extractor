# content-extractor

Harness-independent **content extraction** with per-platform login.

Not a video downloader: a kernel that answers *what content could be extracted
from this input, and by which extractor* — then lets the caller choose. Video is
one content kind among articles, image-text posts, audio, live rooms, game
replays and plain files.

```
cx platforms                          # which sites, and how to log in
cx extractors                         # every content extractor
cx candidates <url>                   # who COULD handle this (plural, ranked)
cx resolve bilibili.article           # the descriptor of a chosen one
cx login start bilibili qr            # -> {"sessionId":…,"challenge":{…}}
cx login poll <sessionId>             # keep calling until result is done
cx import bilibili ./cookies.txt      # store a finished credential
cx export bilibili > jar.txt          # hand cookies to an extractor
```

## Selection is explicit, never inferred

This is the central design rule. One host serves many content kinds:
`bilibili.com` has videos (`/video/BV…`), column articles (`/read/cv…`), opus
posts (`/opus/…`), audio (`/audio/au…`) and live rooms. A first-match-wins
registry would hand an article to the video extractor and fail as "site
unsupported" — a wrong answer dressed as a missing feature.

So there are two steps and no shortcut:

1. **`candidates(url)`** returns **every** extractor whose patterns suggest it
   could handle the input, ranked by pattern specificity. It is a suggestion
   list. When two extractors genuinely tie (a news.wmpvp.com post may be a video or an
   image-text note, and the URL cannot say which), both are returned with equal
   scores.
2. **`extractor(id)`** resolves the id the caller picked, or refuses with the
   known ids listed. Nothing is guessed.

A host that wants one-shot behaviour shows the candidate list and asks. What it
must not do is pretend the ambiguity was not there.

## Layout

```
crates/
  extractor-core/          kernel: provider contract, registry, cookie codec,
                         account pool, sessions, HTTP — names NO platform
  platform-bilibili/     bilibili: login + 5 content extractors
  platform-youtube/      YouTube: login + 4 extractors
  platform-douyin/       抖音: login + 3 extractors
  platform-tencent/      腾讯视频: QQ ptlogin2 QR + video
  platform-iqiyi/        爱奇艺: QR + video
  platform-youku/        优酷: cookie login + video
  platform-wmpvp/        完美世界电竞 (news.wmpvp.com): video / image-text post
  platform-astralparty/  吉星派对: public match-replay files
  platform-xiaohongshu/  小红书: login methods + note/video descriptors
  platform-generic/      any host: cookie import + file / yt-dlp catch-alls
  extractor-abi/           effects + composition root; the C ABI (libextractor.so)
  extractor-cli/           the `cx` binary
hosts/dsh/               the DeepSeek Harness plugin (JS extractors + UI)
```

Each platform is its own crate, so a site's quirks cannot leak into another's
code and adding a site is adding a crate.

**Naming rule:** a platform is named after its **host**, not an abbreviation —
`platform-wmpvp` for `news.wmpvp.com`, `platform-astralparty` for the game whose
CDN is `feimogames.com`. "Which site is this?" must be answerable from the
directory name alone; `platform-pwp` was not, so it was renamed.

`extractor-core` holds the shared machinery and is handed the platform list by
`extractor-abi` — the kernel itself names none of them.

Two boundary tests enforce this on every build:

* `tests/purity.rs` — the kernel touches no filesystem, environment, process,
  socket, terminal, clock or entropy source. Those arrive as `Storage` and
  `Ambient` arguments.
* `tests/platform_purity.rs` — the same rule for **every** `platform-*` crate,
  discovered by directory scan so a new crate is covered automatically — plus a
  check that no platform names a harness.

## Content kinds and metadata

An extractor advertises `{ id, platform, label, kind, patterns, requiresLogin,
metadataKeys }`. `id` is `<platform>.<name>` and globally unique.

Results carry an **open string→JSON metadata map**. Common keys are conventions,
not schema — `title`, `author`, `id`, `url`, `description`, `duration`,
`thumbnail`, `created_at` — and any extractor adds its own without touching the
kernel, the tools or the UI. An article adds `text`, `images`, `word_count`,
`cvid`; a replay adds `map_name`, `mode`, `round`, `players`.

| extractor | kind | notes |
|---|---|---|
| `bilibili.video` | video | yt-dlp owns the player protocol; login raises quality |
| `bilibili.article` | article | `/read/cv…`; parses `__INITIAL_STATE__.detail.modules`, emits Markdown + images |
| `bilibili.opus` | post | `/opus/…`; same renderer, distinct id and metadata |
| `bilibili.audio` / `.live` | audio / live | advertised descriptors |
| `wmpvp.video` / `wmpvp.note` | video / post | one public JSON API; CDN needs a `Referer` |
| `astralparty.replay` | replay | public protobuf match file; **the file is the deliverable** |
| `generic.file` | file | direct links, content-type decides the extension |
| `generic.ytdlp` | video | yt-dlp's 1800-site catalogue as the long tail |

### Articles are not videos

`bilibili.article` and `bilibili.opus` exist precisely because yt-dlp has no
extractor for them. Both pages embed the same opus front-end state
(`window.__INITIAL_STATE__.detail.modules`), so one walker serves both while the
ids stay separate. The walker renders text, headings, quotes, code fences, lists
and rules, collects every inline and header image, and writes a `.md` artifact
plus the pictures. `params: {"images": false}` skips the downloads.

Three details that matter: the state blob is brace-matched rather than
regex-truncated (it contains semicolons inside strings); 19-digit opus ids are
read from the raw text because `JSON.parse` rounds them; and bilibili serves a
3 KB shell page for most datacentre requests, so the fetch retries with jitter
until the real page arrives instead of reporting the site as unsupported.

### Game replays

A 吉星派对 replay is a public static protobuf file, not a video:
`sereplaycn.feimogames.com/prod/<16-digit id>` (CN) or `sereplayjp…` (INTL); the
two regions' ids do not overlap. The deliverable is the match file — rendering
needs the game client, which this project does not attempt. Map, mode, rounds
and per-player settlement stats are parsed as convenience metadata.

## Credentials are applied wherever they help

The account pool exists so a logged-in jar gets used — not only where login is
mandatory. Each extractor declares one of three values:

| `credentials` | meaning | cookies sent? |
|---|---|---|
| `required` | extraction fails without a login (members-only, paid) | yes |
| `enhances` | works anonymously, but a login improves it | **yes** |
| `none` | public resource that ignores credentials | no |

`enhances` is the common case and the reason this is three-valued rather than a
boolean: bilibili serves a video at 360p anonymously and 1080p+ with `SESSDATA`;
an article may be fans-only or paywalled; a YouTube video may be age-gated; a
"direct" file link may sit behind auth. A boolean "needs login" collapsed all of
that to `false` and silently dropped the credential the user had set up.

So `bilibili.article`, `bilibili.opus`, `wmpvp.video`, `wmpvp.note`,
`generic.file` and `generic.ytdlp` are all `enhances` and all receive the jar.
Only `astralparty.replay` is `none` — a public CDN that ignores cookies, where
sending them would just leak them to a third party.

Credentials are scoped to the **host**, which is a separate question from which
extractor runs: an article and a video on bilibili share one jar. Extractors that
fetch directly convert the materialized Netscape file into a `Cookie:` header
with domain/path/secure/expiry filtering (`hosts/dsh/lib/content/cookies.js`), so
a cookie for one site never rides along to another. The header is a credential:
it goes into the request and never into a result, metadata, or log.

## Login: platform × method

Orthogonal dimensions, unchanged by the widening:

* **platform** — which site owns the *credentials* (host-scoped, independent of
  content kind).
* **method** — `qr`, `manual`, `sms`, `password`, `browser`. Every platform
  advertises the full enum with `implemented: false` where unsupported, so a
  picker is complete and an unsupported combination returns a structured
  refusal.

Up to 10 accounts per platform; the pinned one is used first and the rest rotate
on rejection.

YouTube deliberately offers no OAuth: a Google bearer token does not authorize
cookie-authenticated requests, so it would report "logged in" while every fetch
stayed anonymous.

## Storage

One root: `$EXTRACTOR_HOME` → `$VLOGIN_HOME` (legacy) →
`$XDG_DATA_HOME/content-extractor` → `~/.local/share/content-extractor`, falling back
to an existing `video-login` directory so an upgrade does not orphan credentials.

```
accounts/<platform>.json     account pool: labels, active pin, metadata
accounts/<platform>.jar.txt  active account's cookies, Netscape format
cookies/<platform>.json      legacy single-account file, read-only, migrated
sessions/<id>.json           in-flight login sessions, mode 0600
```

Sessions are files because the flows are multi-step while the callers are
one-shot CLI invocations and stateless HTTP requests.

## C ABI

Two symbols, JSON in, JSON out:

```c
const char *extractor_version(void);
int64_t     extractor_call(const char *request, char *out, uint64_t out_len);
```

The caller owns the output buffer; Rust only writes into it, and there is
deliberately no `extract_free` — the usual alternative requires both allocators
to agree and leaks on every forgetful binding. The return value is the bytes
needed **including** the trailing NUL, or `-1` for a null/non-UTF-8 request. Grow
and retry when it exceeds your buffer. Note the unit is **bytes**, not JS
`String.length`.

Ops: `platforms` (alias `channels`), `extractors`, `candidates`, `resolve`,
`route`, `status`, `accounts`, `start`, `browse`, `poll`, `submit`, `import`,
`check`, `pin`, `logout`, `clear`, `cancel`, `export`, `materialize`, `sweep`.

Every response is `{"ok":true,"result":…}` or
`{"ok":false,"error":{"kind":…,"message":…}}` with `kind` one of `channel`,
`cookie`, `http`, `io`, `json`, `other`. Request fields accept `platform` with
`channel` as a legacy alias.

## Security

* No response ever contains a cookie value. Only `export` / `materialize` return
  secret text, and they exist to feed an extractor's cookie file.
* The one cookie-derived value a host may render is a **count**.
* Errors carry status codes and truncated bodies only.
* Session files are `0600` inside `0700` directories; ids are validated before
  touching the filesystem.
* `browser` extraction drives a Chromium-family browser over loopback DevTools
  rather than decrypting the profile's SQLite — the latter needs three more
  dependencies and breaks when browser and reader are on different machines.

## Build

```sh
nix build            # or: cargo build --release
nix develop          # cargo, rustc, clippy, rustfmt, rust-analyzer, just
cargo test --workspace
cargo clippy --workspace --all-targets   # clean; no warnings
cd hosts/dsh && npm test
```

**Stage new files before `nix build`.** A flake source is the git *index*, not
the working tree: an untracked file is invisible to the build, which then
succeeds while shipping a package missing that file. Verify the output actually
contains what you added — a green build is not proof.

## License

MIT — see [LICENSE](./LICENSE).
