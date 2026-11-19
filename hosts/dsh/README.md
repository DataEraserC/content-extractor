# dsh-content-extract

Content extraction for [DeepSeek Harness](https://github.com/deepseek-ai/dsh):
per-platform extractors for video, articles, image-text posts, game replays and
plain files, each producing an open metadata map. The caller **chooses** the
extractor — candidates are listed, never auto-selected — and per-platform login
raises the quality tier where credentials help. Optional ffmpeg compression to a
QQ-sendable MP4.

Login is **platform × method**: pick the site (bilibili, douyin, YouTube, Tencent
Video, iQiyi, Youku), then pick how the credential arrives *within* that
channel (QR scan, paste a cookie file, SMS code, password, or drive a real
browser). Not every channel implements every method — the registry advertises
the full set with `implemented` flags, so the picker is complete and asking for
an unsupported method returns a structured refusal instead of quietly doing
nothing.

The engine that does the work is the separate, harness-independent
[`content-extract`](https://github.com/DataEraserC/content-extract) tool: a Rust core, a
C ABI, and a CLI. This plugin is only the integration shell — routes, tools, and
the page — that loads that library and calls it.

## Where you can drive it from

Three surfaces, one implementation each, all writing the same profile row — so
they cannot disagree:

| Surface | What it is | Where it works |
| --- | --- | --- |
| **Web page** | `plugins.row.config` card + a Settings tab | Web GUI only (the browser half declares `platform: "web"`) |
| **Slash commands** | `/content-settings`, `/content-login` | Web composer **and** the TUI — both dispatch through the kernel's command registry |
| **Session tools** | `content_candidates`, `content_extract`, `content_extractors`, `video_fetch`, `video_login`, `video_compress`, `video_settings` | The model calls them, not you |

The page needs a browser and the TUI never renders it; the commands are the
only surface the TUI has. Every change applies to the running plugin
immediately and survives a restart.

## Tools

| Tool | What it does |
| --- | --- |
| `video_fetch` | Download any URL yt-dlp supports (bilibili, YouTube, …) with `yt-dlp`; returns the local file path **plus metadata** (title, author, id, url, description, duration, thumbnail, …) and writes a `.meta.json` sidecar next to the media. Files land inside the session workspace only (see Security). A URL owned by a login site gets its cookie jars, tried in order when one fails. `compress: true` re-encodes the result in the same call; `crf` / `maxHeight` override the settings for that call only. An already-downloaded URL returns the existing file. |
| `content_candidates` | Asks who **could** handle a url/id. Returns EVERY match ranked by specificity; never selects. Read it, then choose. |
| `content_extract` | Extracts with an **explicitly chosen** `extractor` id (required). Returns files plus an **open metadata map** — see below. |
| `content_extractors` | Lists every extractor: id, platform, label, kind, known metadata keys. |
| `video_login` | Per-channel login on a `channel` × `method` contract. `channel` is the site (`bilibili`, `douyin`, `youtube`, `tencent`, `iqiyi`, `youku`); `method` is how the credential arrives (`qr`, `manual`, `sms`, `password`, `browser`); each channel advertises which methods it implements. Actions: `channels` (list channels with display names + methods), `start` (begin a login, returns `sessionId` + a challenge to render), `browse` (open a browser on the server, then `poll`), `poll` (advance a login; `waitMs` blocks), `submit` (send this step's inputs), `import` (store a cookie file from disk via `path`), `status` (probe an account), `pin` (choose the active account), `logout`, `clear`, `cancel`. |
| `video_compress` | Re-encode any local video to H.264 + AAC + faststart with a height cap, writing `<name>.small.<ext>` next to the input. |
| `video_settings` | `get` reads the live values, `set patch={…}` writes them into the active profile's Cordis patch and applies them immediately, `reset` drops the override and restores the inherited value. Same fields and the same validation as the web page. |

## The content layer (`lib/content/`)

The plugin is not video-only, and it does not guess. `content_candidates` lists
the possibilities; `content_extract` runs the one you named:

```js
{
  id:       'bilibili.article',    // <platform>.<name>, globally unique
  platform: 'bilibili',            // credential owner
  label:    '哔哩哔哩专栏文章',
  kind:     'article',             // one content kind per extractor
  patterns: ['bilibili.com/read/'],// ADVISORY hints, never a dispatch
  requiresLogin: false,
  metadataKeys: [...],             // documented extras; the map stays open
  extract(input, opts): Promise<ExtractResult>,
}
```

An `ExtractResult` is `{ kind, source, files: [{path, role, bytes?}], metadata, extra }`.
**`metadata` is an open string→JSON map** — the "map / json" shape. Common keys
are conventions, not schema: `title`, `author`, `id`, `url`, `description`,
`duration`, `thumbnail`, `created_at`. Any extractor may add its own keys
without touching the registry, the tools, or the UI; consumers read what they
know and ignore the rest. A `.meta.json` sidecar is written next to the
artifacts so the map survives the call.

Registered extractors (one directory per platform under `lib/content/`):

| id | owns | kind |
| --- | --- | --- |
| `bilibili.video` | `bilibili.com/video/…`, `b23.tv/…` | `video` |
| `bilibili.article` | `bilibili.com/read/cv…` | `article` |
| `bilibili.opus` | `bilibili.com/opus/…` | `post` |
| `wmpvp.video` | `news.wmpvp.com/community-detail.html?id=…` (video post) | `video` |
| `wmpvp.note` | same URL shape (image/text post) | `post` |
| `astralparty.replay` | a 16-digit replay id, or a `sereplaycn/sereplayjp.feimogames.com/prod/<id>` URL | `replay` |
| `generic.file` | direct file links (by extension or `params.kind='file'`) | `file` |
| `generic.ytdlp` | any other http(s) page (yt-dlp's 1800-site catalogue) | `video` |

`wmpvp.video` and `wmpvp.note` deliberately tie on specificity: the URL cannot say
which one a post is, so both are offered and the caller picks. Choosing wrong is
an actionable error naming the other id, not a silently empty result.

Adding a platform (e.g. a Xiaohongshu note getter) is **one directory under
`lib/content/<platform>/` plus one import and one array entry** in
`lib/content/registry.js`. Nothing else changes.

Name the directory after the **host**, not an abbreviation: `wmpvp` for
`news.wmpvp.com`, so the site is identifiable from the path alone.

### bilibili articles and opus posts are not videos

yt-dlp has no extractor for `/read/cv…` or `/opus/…`; it falls back to its generic
path and fails, which reads as "site unsupported". Both pages embed the same
opus front-end state (`window.__INITIAL_STATE__.detail.modules`), so one walker
serves them while the ids stay distinct. It renders text, headings, quotes, code
fences, lists and rules, collects inline and header images, and writes a `.md`
artifact plus the pictures (`params: {"images": false}` skips downloads).

The state blob is brace-matched rather than regex-truncated (strings inside it
contain semicolons), and 19-digit opus ids are scanned from the raw text because
`JSON.parse` rounds them. `cvid` comes from the URL or the site's own copyright
label — never from the internal snowflake, which is reported separately as
`opus_id`. bilibili returns a ~3 KB shell page for most datacentre requests
(measured ~1 in 5 real), so the page fetch retries with jitter until the real
page arrives or the deadline passes; the error names that behaviour rather than
claiming the site is unsupported.

`video_fetch` refuses a non-video bilibili URL and names the right extractor, so
the old failure mode cannot recur through that tool either.

### Astral Party replays

Replays are public static protobuf files on the game's CDN — no login needed:

```
CN:   https://sereplaycn.feimogames.com/prod/<replayId>
INTL: https://sereplayjp.feimogames.com/prod/<replayId>
```

A replay id is 16 digits (unix seconds + room number); the two regions' ids
do not overlap. `content_extract extractor="astralparty.replay" id="1795093645690603"`
downloads the match file (`<id>.bin`) — **the deliverable is the file itself**;
rendering a replay requires the game client, which this plugin deliberately does
not attempt. As a convenience the extractor also parses the file without a
protobuf dependency (a minimal wire-format reader) and surfaces `map_name`,
`mode`, `round`, `created_at`, and per-player final stats (kills, damage, gold,
transfers, healing, boss kill, …) in the metadata map, using the game's
settlement semantics. `params: {"region":"intl"}` selects the international
server. The frame-location heuristic follows the community tool
[AstralParty_Dex](https://github.com/ZtyanCrany/AstralParty_Dex) (MIT).

### 完美世界电竞（news.wmpvp.com）community posts

`news.wmpvp.com/community-detail.html?id=…` pages are a Vue SPA with no
server-rendered content, so yt-dlp finds nothing there. Both 完美世界电竞 extractors call
the same public JSON API the page uses
(`appengine.wmpvp.com/steamcn/community/post/getPostById`, no credentials
for public posts), project the post onto the metadata map (title, author,
id, url, description, duration, thumbnail, created_at, plus `vid`,
`steam_id`, `tags`, read/like/reply counts and every available
`play_formats` entry). `wmpvp.video` downloads the best-quality MP4 from
highlights.wmpvp.com — that CDN requires a `Referer: https://news.wmpvp.com/`
header, which the extractor sends. `wmpvp.note` downloads the pictures and puts
the text in metadata.

## Credentials reach non-video extractors too

Every extractor declares `credentials: 'required' | 'enhances' | 'none'`:

- **required** — gated content; extraction fails without a login.
- **enhances** — works anonymously, but a login improves quality or access.
  **Stored cookies are still attached.** This is most extractors: a bilibili
  video is 360p anonymously and 1080p+ with `SESSDATA`, an article may be
  fans-only, a direct file link may sit behind auth.
- **none** — a public resource that ignores credentials (`astralparty.replay`).
  Sending cookies there would only leak them.

A boolean "needs login" used to conflate the first two, so only video benefited
from the account pool. `lib/content/cookies.js` now converts the materialized
Netscape jar into a `Cookie:` header for extractors that use `fetch` directly
(article, opus, PWP, direct file), with domain/path/secure/expiry filtering so a
cookie never crosses sites. yt-dlp keeps reading the file via `--cookies`.

`test/live-cookie-propagation.mjs` proves it end-to-end against a local echo
server: the article and file extractors really send the header, and the replay
extractor really does not.

## Web page

The package ships a browser half (`lib/client.js`, declared as `dsh.client`), so
it appears in the web GUI in two places:

- **Plugins page** — a configure card on the `dsh-content-extract` row
  (`plugins.row.config`), showing the summary on the row and the full page in
  its detail view.
- **Settings → Built-in plugins** — a "Content extraction" tab
  (`settings.plugins.tab`).

The page edits the seven tunable settings and drives login for whichever
platform is selected: a labeled **平台** picker, then a **方式** picker showing only
that platform's methods, then the surface the chosen method needs (a QR image to
scan, a cookie-file box, phone + SMS code, username + password, or a browser
link). Stored accounts for the channel are listed with their status and can be
pinned or removed.

It talks to the plugin's own host routes (`/content-extract/state|set|reset|login/*|
challenge`, mirrored under `/api` on the desktop connection) rather than the
generic settings form, because login is not configuration and the session tool
must share one writer with the page.

## Slash commands

Registered into the kernel's command registry (`@deepseek-ai/dsh-commands`), so
the web composer and the TUI both offer them in their slash menu and dispatch
them without sending anything to the model.

```
/content-settings                      # print the live values
/content-settings crf=22 maxHeight=480  # write fields into the profile
/content-settings reset crf             # drop an override, restore the default

/content-login                          # every channel's accounts
/content-login channels                 # each channel and the methods it implements
/content-login start bilibili qr        # mint a login; prints the session and the URL
/content-login poll                     # check it after scanning
/content-login cancel
/content-login pin bilibili <account>
/content-login logout bilibili <account>
/content-login check bilibili
```

`start` → `poll` is two command lines, so the session id is remembered between
them; `poll <sessionId>` overrides it when two logins are in flight. A terminal
prints the QR **URL**, not an image — open it in a browser, or use the web page
if your channel only returns a rendered image.

The registry is reached with `ctx.reflect.get('commands')` rather than
`ctx.commands`, because cordis throws for a service the plugin does not
declare in `inject`, and declaring it would make the whole plugin fail to load
on a composition without the registry. Without it, downloading and the web page
still work and only these two commands are missing.

## Configuration

Tunable at runtime (volatile: a write updates the running plugin in place):

| Key | Default | Meaning |
| --- | --- | --- |
| `downloadDir` | `dsh-content-extract` | Download subdir, resolved inside the session workspace (absolute paths rejected) |
| `format` | `bv*+ba/b` | yt-dlp format selector |
| `crf` / `maxHeight` / `audioBitrate` / `preset` | 26 / 720 / `128k` / `medium` | ffmpeg re-encode settings |
| `downloadEngine` | `ytdlp` | Which downloader `video_fetch` uses; `native` is a reserved slot (see below) |

Restart-only (they change the process environment, so they are ordinary fields):

| Key | Default | Meaning |
| --- | --- | --- |
| `ytdlpBinary` / `ffmpegBinary` | `yt-dlp` / `ffmpeg` | Bare names resolve through `PATH` |
| `fetchTimeoutMs` / `compressTimeoutMs` | 600000 / 900000 | Hard per-invocation deadlines |

## Download engines

`video_fetch` does not call yt-dlp directly — it calls a *downloader*, and yt-dlp
is the engine behind it today. The indirection is deliberate and small:

| Engine | Status |
| --- | --- |
| `ytdlp` | The only implementation today. Spawns `yt-dlp --cookies <file>` with an argv array. |
| `native` | **Reserved.** Intentionally empty. |

**`native` is a reserved slot for a downloader that does not depend on yt-dlp, and
it must not be deleted.** yt-dlp tracks hundreds of sites by reverse-engineering
them, which makes it an excellent default and a single upstream that can break
every site at once. When a site changes its player, the whole plugin waits on an
upstream release. The reserved slot is where an independent path — a direct
HLS/DASH fetcher, a site-specific client, an ffmpeg-driven pull — lands without
reshaping the plugin: `lib/downloaders/index.js` already dispatches by id and
already rotates cookie jars per engine, so adding one is one file plus one registry
entry. Removing the empty slot means the next person re-derives the seam from
scratch, probably mid-incident.

An engine is `{ id, label, available(), fetch({ url, outDir, format, cookiesPath,
timeoutMs }) }` resolving to `{ path, engine }`. It must keep two rules: the file
lands inside `outDir` (the plugin's workspace confinement is enforced upstream and
a new engine must not widen it), and cookie text is read from `cookiesPath` and never
echoed into a result.

## Packaging notes (NUR)

The wrapper must carry `runtimeDeps = [ yt-dlp ffmpeg ]` — the plugin spawns both
by bare name and relies on the wrapper's `PATH`. No paths are hard-coded, so a
user override via `ytdlpBinary` / `ffmpegBinary` still wins.

## Security

- Login cookies are stored **per channel** in `accounts/<channel>.json` (mode
  `0600`, at most 10 accounts) and are **never** included in tool results,
  logs, HTTP responses, or commits. The parent directory is this harness's
  content-extract directory when the harness names one (`DSH_HOME/content-extract`),
  and the library's own default otherwise; the plugin exports it to the library
  as `EXTRACT_HOME`, because the library itself is harness-independent and does
  not read `DSH_HOME`. `video_fetch` materializes one account's cookies to a `0600` file,
  hands the *path* to the engine, and rotates to the next account when the site
  rejects one. `logout` removes one account; `clear` removes the channel's.
- The web routes return login state, account labels and identities, and cookie
  **counts** — never a cookie value.
- `video_login action=import` takes an absolute path to a cookie file already on
  disk — cookie values are never pasted into chat. The web page's paste box
  posts straight to that route and is never echoed back.
- Login sessions (a pending QR poll, an SMS step) are `0600` files under
  `sessions/`, each keyed by a random hex id that is checked before it touches
  the filesystem, so a hostile id cannot be used to walk directories.
- `video_fetch` accepts `http(s)` URLs only; `video_compress` accepts absolute
  local paths only. Both binaries are spawned with argv arrays (no shell).
- `video_fetch` downloads **only inside the session workspace**: `downloadDir`
  is a workspace-relative subdir (default `dsh-content-extract`), absolute paths and
  `..` are rejected on write and again at use, and the resolved directory's
  nearest existing ancestor is realpath-checked against the workspace so a
  symlink planted inside it cannot redirect a download out. A call with no
  session workspace fails closed before touching the filesystem.
- Every external process has a hard deadline (SIGTERM, then SIGKILL).

## Development

```bash
npm test         # unit tests: args, cookies, settings, routes, registration, content layer (offline)
npm run test:live  # real bilibili download + ffmpeg + QR start/poll (needs network)
npm run test:cookie      # credentials reach non-video extractors (offline, local echo server)
npm run test:article     # real bilibili ARTICLE extraction (needs network)
npm run test:replay      # real Astral Party replay download + parse (needs network)
node test/live-video.mjs # real metadata probe + video download via bilibili.video (needs network)
npm run test:wmpvp       # real news.wmpvp.com post download (needs network)
```
