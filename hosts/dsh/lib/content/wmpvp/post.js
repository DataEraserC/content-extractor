// 完美世界电竞 — news.wmpvp.com 社区内容提取器。
//
// 目录以主机名 wmpvp 命名（而非缩写），这样"这是哪个站点"从目录名就能回答。
//
// ## What this owns
//
// Community post pages like
//   https://news.wmpvp.com/community-detail.html?id=392049040
// (and the PC variant community-pcDetail.html). The page is a Vue SPA with
// no server-rendered content, so yt-dlp's generic extractor finds nothing.
// The data comes from one public JSON API the page itself calls:
//
//   GET https://appengine.wmpvp.com/steamcn/community/post/getPostById
//       ?postId=<id>&token=
//
// No credentials are needed for public posts (`token` may be empty). The
// response's `result.post` carries everything: title, author
// (`communityUserItem.nickname`), summary/content, tags, counts, and — for
// video posts — `videoInfo.playInfoList[]` with direct MP4 `playURL`s on
// highlights.wmpvp.com. Those CDN links require a `Referer:
// https://news.wmpvp.com/` header (bare requests get 403), which this
// extractor sends.
//
// ## Result shape
//
// Video posts → kind='video': the best-quality MP4 plus the open metadata
// map (title, author, id, url, description, duration, thumbnail, created_at,
// plus site-specific keys: vid, steam_id, tags, read_count, like_count,
// reply_count, play_formats). Image/text posts → kind='note': images are
// downloaded when present, text rides in metadata. A `.meta.json` sidecar
// is written next to the artifacts either way.

import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'

/** The public post API the SPA calls. */
export const POST_API = 'https://appengine.wmpvp.com/steamcn/community/post/getPostById'

/** Referer the highlights CDN checks; without it every playURL 403s. */
const CDN_REFERER = 'https://news.wmpvp.com/'

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36'

/**
 * Extract the post id from a community-detail URL, or validate a bare id.
 * @param {{url?: string, id?: string}} input - fetch input.
 * @returns {string|null} the numeric post id, or null when not a 完美世界电竞 post.
 */
export function postIdOf(input) {
  const url = String(input.url ?? '')
  const match = /[?&]id=(\d+)/.exec(url)
  if (match && /(^|\.)news\.wmpvp\.com$/i.test(hostOf(url))) return match[1]
  const id = String(input.id ?? '')
  return /^\d+$/.test(id) ? id : null
}

/** Hostname of a URL, '' when unparseable. @param {string} url */
function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase()
  } catch {
    return ''
  }
}

/**
 * Pick the best play format: highest pixel area, ties broken by byte size.
 * @param {Array<object>} list - videoInfo.playInfoList.
 * @returns {object|null} the chosen entry, or null when none is playable.
 */
export function bestFormat(list) {
  const usable = (list ?? []).filter((entry) => entry && entry.playURL && entry.status !== 'Deleted')
  if (usable.length === 0) return null
  return usable.reduce((best, entry) => {
    const area = (entry.width ?? 0) * (entry.height ?? 0)
    const bestArea = (best.width ?? 0) * (best.height ?? 0)
    if (area !== bestArea) return area > bestArea ? entry : best
    return (entry.size ?? 0) > (best.size ?? 0) ? entry : best
  })
}

/**
 * Project the API's post object onto the open metadata map.
 * @param {object} post - result.post from getPostById.
 * @returns {object} metadata (open map; site-specific keys included).
 */
export function projectPost(post) {
  const user = post.communityUserItem ?? {}
  const base = post.videoInfo?.videoBase ?? {}
  const metadata = {}
  const put = (key, value) => {
    if (value !== undefined && value !== null && value !== '') metadata[key] = value
  }
  put('title', post.title)
  put('author', user.nickname)
  put('author_id', post.userId === undefined ? undefined : String(post.userId))
  put('id', post.id === undefined ? undefined : String(post.id))
  put('url', post.postUrl)
  put('description', post.content ?? post.summary)
  put('thumbnail', base.coverURL)
  put('duration', base.duration === undefined ? undefined : Number(base.duration))
  // gmtCreate is epoch milliseconds; the common convention is seconds.
  put('created_at', post.gmtCreate === undefined ? undefined : Math.floor(post.gmtCreate / 1000))
  put('vid', post.vid)
  put('steam_id', post.steamId)
  put('read_count', post.readTotalCount)
  put('like_count', post.likeCountTotal)
  put('reply_count', post.replyCount)
  put('tags', (post.postTagDTOList ?? []).map((tag) => tag.name).filter(Boolean))
  put('author_avatar', user.avatar)
  const formats = (post.videoInfo?.playInfoList ?? []).map((entry) => ({
    definition: entry.definition,
    width: entry.width,
    height: entry.height,
    size: entry.size,
    format: entry.format,
    url: entry.playURL,
  }))
  if (formats.length > 0) metadata.play_formats = formats
  return metadata
}

/**
 * Download one URL to outDir with the CDN referer attached.
 * @param {string} url - source URL.
 * @param {string} path - destination file path.
 * @param {number} timeoutMs - hard deadline.
 * @returns {Promise<number>} bytes written.
 */
async function downloadWithReferer(url, path, timeoutMs) {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  let response
  try {
    response = await fetch(url, { signal: controller.signal, headers: { 'user-agent': UA, referer: CDN_REFERER } })
  } catch (error) {
    throw new Error(`download failed (${url.slice(0, 120)}): ${error.message}`)
  } finally {
    clearTimeout(timer)
  }
  if (!response.ok) throw new Error(`download failed (${url.slice(0, 120)}): HTTP ${response.status}`)
  const data = Buffer.from(await response.arrayBuffer())
  await writeFile(path, data)
  return data.length
}

/**
 * Fetch one community post's JSON and project its metadata.
 * Shared by both 完美世界电竞 extractors: the API call is identical, only what gets
 * downloaded afterwards differs.
 * @param {{url?: string, id?: string}} input - extract input.
 * @param {number} timeoutMs - hard deadline for the API call.
 * @returns {Promise<{id: string, post: object, metadata: object}>}
 */
async function fetchPost(input, timeoutMs) {
  const id = postIdOf(input)
  if (id === null) throw new Error('a 完美世界电竞 post needs a community-detail URL with ?id=… or a numeric id')
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), Math.max(1000, timeoutMs))
  let body
  try {
    const response = await fetch(`${POST_API}?postId=${id}&token=`, {
      signal: controller.signal,
      headers: { 'user-agent': UA, 'x-requested-with': 'XMLHttpRequest', platform: 'h5_web' },
    })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    body = await response.json()
  } catch (error) {
    throw new Error(`完美世界电竞 post API failed (postId=${id}): ${error.message}`)
  } finally {
    clearTimeout(timer)
  }
  if (body.code !== 1 || !body.result?.post) {
    throw new Error(`完美世界电竞 post API returned no post (postId=${id}): ${body.message ?? `code ${body.code}`}`)
  }
  const post = body.result.post
  const metadata = projectPost(post)
  if (!metadata.url) metadata.url = `https://news.wmpvp.com/community-detail.html?id=${id}`
  return { id, post, metadata }
}

/** Write the metadata sidecar and assemble the result envelope. */
async function finish(kind, id, outDir, files, metadata, post) {
  const metaPath = join(outDir, `wmpvp-${id}.meta.json`)
  const payload = { kind, source: { extractor: `wmpvp.${kind === 'video' ? 'video' : 'note'}`, id, url: metadata.url }, metadata, extra: { post } }
  await writeFile(metaPath, JSON.stringify(payload, null, 2), 'utf8')
  files.push({ path: metaPath, role: 'metadata' })
  return { kind, source: { id, url: metadata.url }, files, metadata, extra: { post } }
}

/** Both 完美世界电竞 extractors share these URL hints; the id alone cannot disambiguate. */
const WMPVP_PATTERNS = ['news.wmpvp.com/community-', 'id=']

/** Refine the advisory match to a real community-detail URL carrying an id. */
function wmpvpSuggests(input) {
  const url = String(input.url ?? '')
  return /^https?:\/\/news\.wmpvp\.com\/community-(detail|pcDetail)\.html\?/i.test(url) && /[?&]id=\d+/.test(url)
}

/**
 * 完美世界电竞 video extractor — a highlight video post.
 *
 * Refuses a post that carries no playable format, naming `wmpvp.note` as the
 * alternative: choosing the wrong kind must be an actionable error, not a
 * silently empty result.
 */
export const wmpvpVideo = {
  id: 'wmpvp.video',
  platform: 'wmpvp',
  label: '完美世界电竞社区视频',
  kind: 'video',
  patterns: WMPVP_PATTERNS,
  requiresLogin: false,
  metadataKeys: [
    'vid', 'steam_id', 'play_formats', 'read_count', 'like_count',
    'reply_count', 'tags', 'width', 'height',
  ],
  suggests: wmpvpSuggests,

  /**
   * Download the best-quality MP4 of a video post.
   * @param {{url?: string, id?: string}} input - extract input.
   * @param {object} opts - extract options.
   * @returns {Promise<object>} ExtractResult.
   */
  async extract(input, { outDir, timeoutMs }) {
    const deadline = Date.now() + timeoutMs
    const { id, post, metadata } = await fetchPost(input, Math.max(1000, deadline - Date.now()))
    const format = bestFormat(post.videoInfo?.playInfoList)
    if (!format) {
      throw new Error(
        `完美世界电竞 post ${id} has no playable video format — it is probably an image/text post; ` +
        'use extractor "wmpvp.note" instead',
      )
    }
    const remaining = deadline - Date.now()
    if (remaining <= 0) throw new Error('完美世界电竞 post fetched but no time left to download the video')
    const ext = String(format.format ?? 'mp4').replace(/[^a-z0-9]/gi, '') || 'mp4'
    const videoPath = join(outDir, `wmpvp-${id}.${ext}`)
    const bytes = await downloadWithReferer(format.playURL, videoPath, remaining)
    metadata.width = format.width
    metadata.height = format.height
    return finish('video', id, outDir, [{ path: videoPath, role: 'media', bytes }], metadata, post)
  },
}

/**
 * 完美世界电竞 note extractor — an image/text post.
 *
 * Downloads every image it can and puts the text in metadata. A post that
 * turns out to be a video is refused with `wmpvp.video` named, for the same
 * reason as above.
 */
export const wmpvpNote = {
  id: 'wmpvp.note',
  platform: 'wmpvp',
  label: '完美世界电竞社区图文',
  kind: 'post',
  patterns: WMPVP_PATTERNS,
  requiresLogin: false,
  metadataKeys: ['text', 'images', 'image_count', 'read_count', 'like_count', 'tags'],
  suggests: wmpvpSuggests,

  /**
   * Download a note's images and text.
   * @param {{url?: string, id?: string}} input - extract input.
   * @param {object} opts - extract options.
   * @returns {Promise<object>} ExtractResult.
   */
  async extract(input, { outDir, timeoutMs }) {
    const deadline = Date.now() + timeoutMs
    const { id, post, metadata } = await fetchPost(input, Math.max(1000, deadline - Date.now()))
    if (bestFormat(post.videoInfo?.playInfoList)) {
      throw new Error(
        `完美世界电竞 post ${id} is a video post, not an image/text note; use extractor "wmpvp.video" instead`,
      )
    }
    const files = []
    const images = Array.isArray(post.imageList) ? post.imageList : []
    let index = 0
    for (const image of images) {
      const imageUrl = typeof image === 'string' ? image : image?.url ?? image?.imageUrl
      if (!imageUrl) continue
      const remaining = deadline - Date.now()
      if (remaining <= 0) break
      index += 1
      const imagePath = join(outDir, `wmpvp-${id}-img${index}.jpg`)
      try {
        const bytes = await downloadWithReferer(imageUrl, imagePath, remaining)
        files.push({ path: imagePath, role: 'image', bytes })
      } catch { /* one bad image must not sink the whole post */ }
    }
    metadata.image_count = images.length
    metadata.images = images
      .map((image) => (typeof image === 'string' ? image : image?.url ?? image?.imageUrl))
      .filter(Boolean)
    if (post.content) metadata.text = post.content
    return finish('post', id, outDir, files, metadata, post)
  },
}