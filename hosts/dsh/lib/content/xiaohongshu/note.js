// 小红书 note extraction — image/text notes and video notes.
//
// ## Why DOM-first, not the signed API
//
// The site's Web API (`/api/sns/web/v1/feed`) needs `x-s` / `x-s-common`
// signatures plus a short-lived, source-bound `xsec_token`. Those change with
// the site's front end and are a maintenance treadmill. The note page itself
// is server-rendered with `window.__INITIAL_STATE__`, which carries the full
// note (title, text, images, author, stats, and a direct video stream). This
// module reads that state — the same approach `bilibili/article.js` uses for
// bilibili's column pages.
//
// ## Two ids, one page shape
//
// A `/explore/<note_id>` URL cannot say whether the note is image/text or
// video, so both extractors advertise the same host pattern and tie on
// specificity. The caller picks explicitly; selecting the wrong one is an
// actionable error naming the other id, never a silent empty result.
//
// ## Credentials
//
// Most notes need a session. `channelForUrl` resolves this platform from the
// registry's `hosts`, so the account pool's jar is attached to every fetch and
// rotated on rejection. `params.images === false` skips image downloads for a
// text-only result.

import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { genericYtdlp } from '../generic/ytdlp.js'
import { cookieHeaderFrom, withCookie } from '../cookies.js'

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36'
const REFERER = 'https://www.xiaohongshu.com/'

/** 24-hex note id as used by xiaohongshu. */
const NOTE_ID_RE = /[0-9a-f]{24}/i

/**
 * Pull `window.__INITIAL_STATE__` out of a page.
 *
 * Brace-matched rather than regex-to-semicolon (the blob contains strings with
 * semicolons and nested objects). Xiaohongshu emits bare `undefined` literals,
 * which are invalid JSON, so they are normalised to `null` in the value
 * position before parsing.
 * @param {string} html - page source.
 * @returns {object} the parsed state.
 */
export function initialStateOf(html) {
  const relaxed = /window\.__INITIAL_STATE__\s*=\s*/.exec(html)
  if (!relaxed) throw new Error('no __INITIAL_STATE__ in the page')
  const start = relaxed.index + relaxed[0].length
  let depth = 0
  let inString = false
  let escaped = false
  for (let i = start; i < html.length; i += 1) {
    const ch = html[i]
    if (inString) {
      if (escaped) escaped = false
      else if (ch === '\\') escaped = true
      else if (ch === '"') inString = false
      continue
    }
    if (ch === '"') inString = true
    else if (ch === '{') depth += 1
    else if (ch === '}') {
      depth -= 1
      if (depth === 0) {
        const text = html.slice(start, i + 1).replace(/\bundefined\b/g, 'null')
        try {
          return JSON.parse(text)
        } catch (error) {
          throw new Error(`__INITIAL_STATE__ is not valid JSON: ${error.message}`)
        }
      }
    }
  }
  throw new Error('__INITIAL_STATE__ is truncated')
}

/**
 * Resolve the note record from a parsed state.
 * @param {object} state - parsed __INITIAL_STATE__.
 * @param {string} noteId - the requested note id.
 * @returns {object} the `note` record.
 */
export function noteOf(state, noteId) {
  const map = state?.note?.noteDetailMap
  if (!map || typeof map !== 'object') {
    throw new Error('the page state has no note.noteDetailMap — not a xiaohongshu note page')
  }
  const entry = map[noteId] ?? Object.values(map).find((value) => value?.note)
  const note = entry?.note
  if (!note) throw new Error(`the page state has no note record for id ${noteId}`)
  return note
}

/**
 * Extract the note id from a URL. Short links are resolved separately before
 * this is called; both long forms carry the id as their last path segment.
 * @param {string} url - note page URL.
 * @returns {string} note id, or '' when absent.
 */
export function noteIdOf(url) {
  const match = NOTE_ID_RE.exec(String(url ?? ''))
  return match ? match[0] : ''
}

/**
 * Extract a real note URL from a short-link landing page.
 *
 * `xhslink.com` 302s to one of two places depending on the site's risk
 * control: the note page itself, or a `/404` / login interstitial whose
 * `redirectPath` query parameter holds the real note URL. Split out as a pure
 * function so the extraction is testable without network access.
 * @param {string} finalUrl - the URL the short link redirected to.
 * @returns {string} a note URL with a 24-hex id, or '' when none is present.
 */
export function noteUrlFromRedirect(finalUrl) {
  let final
  try {
    final = new URL(String(finalUrl ?? ''))
  } catch {
    return ''
  }
  // An interstitial's query can itself contain a 24-hex id (inside the
  // encoded redirectPath), so check the pathname before matching the id —
  // otherwise a /404 page looks like a note page and is fed to yt-dlp.
  const interstitial = /(^|\/)(404|login)(\/|$)/i.test(final.pathname)
  if (!interstitial && noteIdOf(final.pathname) && /xiaohongshu\.com/i.test(final.host)) {
    return final.href
  }
  const redirectPath = final.searchParams.get('redirectPath') ?? ''
  if (!redirectPath) return ''
  let real = redirectPath
  if (!/^https?:\/\//i.test(real)) real = `https://www.xiaohongshu.com${real.startsWith('/') ? '' : '/'}${real}`
  if (/(^|\/)(404|login)(\/|$)/i.test(new URL(real).pathname)) return ''
  return noteIdOf(real) ? real : ''
}

/**
 * Follow a share short link to a final note URL that carries `xsec_token`.
 *
 * Note: the token here is whatever the short link carried. Anonymous server
 * reads are still 302'd by the site — resolving the URL does not defeat the
 * login gate; it only gives yt-dlp a URL it can sign and fetch on its own.
 * @param {string} url - possibly a short link.
 * @param {number} timeoutMs - deadline for the redirect.
 * @returns {Promise<string>} the resolved URL.
 */
export async function resolveUrl(url, timeoutMs) {
  const target = String(url ?? '')
  if (!/^https?:\/\//i.test(target)) throw new Error(`unsupported URL scheme (need http/https): ${target.slice(0, 80)}`)
  if (!/xhslink\.com/i.test(target)) return target
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    const response = await fetch(target, { signal: controller.signal, redirect: 'follow', headers: { 'user-agent': UA } })
    const final = response.url || target
    const noteUrl = noteUrlFromRedirect(final)
    if (noteUrl) return noteUrl
    throw new Error(
      `share short link did not resolve to a note page (landed on ${final.slice(0, 100)}). ` +
      'Copy the full link with xsec_token from a signed-in browser instead.',
    )
  } finally {
    clearTimeout(timer)
  }
}

/**
 * Read a note page's HTML with the platform's cookie jar attached.
 * @param {string} url - note URL.
 * @param {number} timeoutMs - hard deadline.
 * @param {string} cookiesPath - Netscape jar path.
 * @returns {Promise<string>} page source.
 */
export async function fetchHtml(url, timeoutMs, cookiesPath = '') {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    const response = await fetch(url, {
      signal: controller.signal,
      headers: withCookie({
        'user-agent': UA,
        accept: 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
        'accept-language': 'zh-CN,zh;q=0.9,en;q=0.8',
        referer: REFERER,
      }, cookieHeaderFrom(cookiesPath, url)),
    })
    if (!response.ok) {
      throw new Error(
        `HTTP ${response.status} reading ${url.slice(0, 100)}. ` +
        'Most notes need a login: import a cookie (action=import) or pass the full share URL with xsec_token.',
      )
    }
    const text = await response.text()
    // A 302 to /404 or a login interstitial still carries __INITIAL_STATE__
    // (with only `global`, no noteDetailMap). The site gates anonymous server
    // reads regardless of token freshness, so say that rather than blaming an
    // expired token — the note can be perfectly public in a browser.
    if (!text.includes('__INITIAL_STATE__') || !text.includes('noteDetailMap')) {
      throw new Error(
        `no note state in the page for ${url.slice(0, 100)}: the site redirects anonymous server-side ` +
        'requests to /404 or login, not a token problem. Import a login cookie with ' +
        '`/content-login xiaohongshu manual` (or the browser method), or use xiaohongshu.video ' +
        'with a full share URL carrying xsec_token.',
      )
    }
    return text
  } finally {
    clearTimeout(timer)
  }
}

/** Read the `xsec_token` query parameter, if the share URL carried one. */
function tokenOf(url) {
  try {
    return new URL(url).searchParams.get('xsec_token') ?? ''
  } catch {
    return ''
  }
}

/**
 * Project a note record onto the open metadata map.
 * @param {object} note - note record from the page state.
 * @param {string} url - canonical page URL.
 * @param {string} noteId - the note id.
 * @param {object} extra - optional resolved URL / token.
 * @returns {{metadata: object, images: Array<object>, videoUrl: string}}
 */
export function projectNote(note, url, noteId, { resolvedUrl = url } = {}) {
  const metadata = {}
  const put = (key, value) => {
    if (value !== undefined && value !== null && value !== '') metadata[key] = value
  }
  const user = note.user ?? {}
  const interact = note.interactInfo ?? {}

  put('note_id', noteId ?? note.noteId ?? '')
  put('id', noteId ?? note.noteId ?? '')
  put('title', note.title ?? note.desc?.slice(0, 60) ?? '')
  put('description', note.desc)
  put('author', user.nickname)
  put('author_id', user.userId)
  put('avatar', user.avatar)
  const token = tokenOf(resolvedUrl)
  put('xsec_token', token)
  put('url', url)
  put('created_at', note.time)
  put('ip_location', note.ipLocation)
  put('liked_count', interact.likedCount)
  put('collected_count', interact.collectedCount)
  put('comment_count', interact.commentCount)
  put('share_count', interact.shareCount)
  if (Array.isArray(note.tagList)) {
    const tags = note.tagList.map((tag) => tag.name).filter(Boolean)
    if (tags.length > 0) put('tags', tags)
  }

  const images = []
  if (Array.isArray(note.imageList)) {
    for (const image of note.imageList) {
      const imageUrl = image?.urlDefault ?? image?.url_pre ?? image?.url
      if (!imageUrl) continue
      images.push({ url: imageUrl, width: image.width, height: image.height })
    }
  }
  put('images', images.map((image) => image.url))
  put('image_count', images.length)
  put('thumbnail', images[0]?.url)

  // The direct stream is present on video notes; keep it in metadata for
  // callers and as a hint, but the video bytes themselves come from yt-dlp.
  let videoUrl = ''
  const streams = note.video?.media?.stream
  if (streams) {
    for (const codec of ['h264', 'h265', 'av1']) {
      const candidate = streams[codec]?.[0]?.masterUrl
      if (candidate) {
        videoUrl = candidate
        break
      }
    }
  }
  put('video_url', videoUrl)
  const duration = note.video?.capa?.duration
  if (typeof duration === 'number') put('duration', duration)
  return { metadata, images, videoUrl }
}

/** Download one image, tolerating a failure so one bad URL cannot sink a note. */
async function downloadImage(url, path, timeoutMs, cookiesPath = '') {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    const response = await fetch(url, {
      signal: controller.signal,
      headers: withCookie({ 'user-agent': UA, referer: REFERER }, cookieHeaderFrom(cookiesPath, url)),
    })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = Buffer.from(await response.arrayBuffer())
    await writeFile(path, data)
    return data.length
  } finally {
    clearTimeout(timer)
  }
}

/**
 * Shared page reader: resolve short link, fetch HTML, parse state, project.
 * @param {object} input - extract input.
 * @param {object} opts - extract options (cookiesPath, timeoutMs).
 * @returns {Promise<object>} state pieces plus projected data.
 */
async function readNote(input, opts) {
  const original = String(input.url ?? '')
  const deadline = Date.now() + opts.timeoutMs
  const resolvedUrl = await resolveUrl(original, Math.min(20000, Math.max(3000, deadline - Date.now())))
  const html = await fetchHtml(resolvedUrl, Math.max(20000, deadline - Date.now() - 15000), opts.cookiesPath)
  const state = initialStateOf(html)
  const id = noteIdOf(resolvedUrl) || noteIdOf(original)
  if (!id) throw new Error(`no 24-digit note id found in ${original.slice(0, 120)}`)
  const note = noteOf(state, id)
  const projected = projectNote(note, original, id, { resolvedUrl })
  return { id, note, html, projected }
}

/** 小红书图文笔记 (`/explore/<id>` or `/discovery/item/<id>`). */
export const xiaohongshuNote = {
  id: 'xiaohongshu.note',
  platform: 'xiaohongshu',
  label: '小红书图文笔记',
  kind: 'post',
  patterns: ['xiaohongshu.com'],
  credentials: 'enhances',
  metadataKeys: [
    'note_id', 'xsec_token', 'author', 'author_id', 'avatar', 'text', 'images',
    'image_count', 'liked_count', 'collected_count', 'comment_count', 'share_count',
    'tags', 'ip_location', 'created_at',
  ],

  /**
   * @param {{url?: string}} input - extract input.
   * @param {object} opts - extract options.
   * @returns {Promise<object>} ExtractResult with a .md file, images, .meta.json.
   */
  async extract(input, opts) {
    const { id, note, projected } = await readNote(input, opts)
    if (projected.videoUrl) {
      throw new Error(
        `this note (${id}) is a VIDEO note — use extractor="xiaohongshu.video" instead; ` +
        'the URL cannot say which kind it is, so both are offered.',
      )
    }
    const { metadata, images } = projected
    const deadline = Date.now() + opts.timeoutMs
    const stem = `xhs-${id}`
    const files = []

    const body = ['# ' + (metadata.title ?? ''), '', metadata.description ?? ''].join('\n')
    const textPath = join(opts.outDir, `${stem}.md`)
    await writeFile(textPath, body, 'utf8')
    files.push({ path: textPath, role: 'text', bytes: Buffer.byteLength(body) })
    metadata.text = metadata.description ?? ''
    metadata.word_count = metadata.text.length

    if (opts.params?.images !== false) {
      let index = 0
      for (const image of images) {
        const remaining = deadline - Date.now()
        if (remaining <= 0) break
        index += 1
        const ext = (/\.(jpe?g|png|gif|webp)(?:\?|$)/i.exec(image.url)?.[1] ?? 'jpg').toLowerCase()
        const imagePath = join(opts.outDir, `${stem}-img${index}.${ext}`)
        try {
          const bytes = await downloadImage(image.url, imagePath, remaining, opts.cookiesPath)
          files.push({ path: imagePath, role: 'image', bytes })
        } catch {
          // keep going: partial images beat no note
        }
      }
    }

    const metaPath = join(opts.outDir, `${stem}.meta.json`)
    await writeFile(metaPath, JSON.stringify({ kind: 'post', source: { extractor: this.id, url: input.url, id }, metadata }, null, 2), 'utf8')
    files.push({ path: metaPath, role: 'metadata' })
    return { kind: 'post', source: { url: input.url, id }, files, metadata, extra: { note } }
  },
}

/** 小红书视频笔记 (`/explore/<id>` or `/discovery/item/<id>`). */
export const xiaohongshuVideo = {
  id: 'xiaohongshu.video',
  platform: 'xiaohongshu',
  label: '小红书视频笔记',
  kind: 'video',
  patterns: ['xiaohongshu.com'],
  credentials: 'enhances',
  // These are the keys yt-dlp's XiaoHongShu extractor actually returns. The
  // SSR-only fields (liked/collected/share counts, ip_location) are not here
  // because this path never reads the page — see the note on extract().
  metadataKeys: [
    'note_id', 'author_id', 'duration', 'thumbnail', 'description',
    'video_url', 'upload_date',
  ],

  /**
   * Download a video note.
   *
   * Deliberately does NOT read `window.__INITIAL_STATE__` first. That was the
   * original design, but it is an anonymous server-side fetch: xiaohongshu
   * 302s those requests to /404, so the page read failed before yt-dlp ever
   * ran — even when yt-dlp itself would have succeeded on the same URL. The
   * bytes and their metadata (title, description, author id, duration,
   * thumbnail) all come from yt-dlp's built-in XiaoHongShu extractor, which
   * owns URL signing and the cookie jar. Resolve a short link to a real note
   * URL first only when needed.
   *
   * A note may be image/text rather than video; yt-dlp then reports no video
   * formats, and the error is surfaced verbatim. The caller picked this id,
   * so the actionable alternative is `xiaohongshu.note`.
   *
   * @param {{url?: string}} input - extract input.
   * @param {object} opts - extract options.
   * @returns {Promise<object>} ExtractResult with the video and .meta.json.
   */
  async extract(input, opts) {
    const original = String(input.url ?? '')
    const deadline = Date.now() + opts.timeoutMs
    const url = await resolveUrl(original, Math.min(20000, Math.max(3000, deadline - Date.now())))
    const result = await genericYtdlp.extract({ ...input, url }, opts)

    // yt-dlp already wrote a metadata sidecar from its own info. Enrich it
    // with the xiaohongshu-specific keys and make this extractor the source
    // of record, then rewrite the sidecar so it matches what we return.
    const id = result.metadata.id ?? noteIdOf(url) ?? noteIdOf(original)
    result.metadata.note_id = id
    result.source.id = id
    result.source.extractor = this.id
    const token = tokenOf(url) || tokenOf(original)
    if (token) result.metadata.xsec_token = token
    const metaPath = result.files.find((file) => file.role === 'metadata')?.path
    if (metaPath) {
      await writeFile(metaPath, JSON.stringify({ kind: 'video', source: result.source, metadata: result.metadata }, null, 2), 'utf8')
    }
    return result
  },
}
