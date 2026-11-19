// http-file extractor — direct file downloads, the generic tail.
//
// Any http(s) URL that points at a file (by extension or by explicit
// `kind: 'file'`) lands here: images, archives, documents, media segments.
// No site knowledge, no cookies rotation beyond what the caller hands over;
// the response's content-type and the URL path decide the file name.
//
// This is the seam a future "任意文件解析/下载" ask plugs into: a new
// extractor for a specific site (e.g. a Xiaohongshu note getter) registers
// BEFORE this one and returns kind='note' with text + images; everything
// that is just bytes keeps working through here.

import { writeFile } from 'node:fs/promises'
import { extname, join } from 'node:path'
import { cookieHeaderFrom, withCookie } from '../cookies.js'

/** Extensions we treat as direct files when routing. */
const FILE_RE = /\.(mp4|mkv|webm|mov|avi|flv|ts|m3u8|mpd|mp3|m4a|flac|wav|ogg|jpg|jpeg|png|gif|webp|zip|pdf)(\?|#|$)/i

/** Content-type → extension fallback when the URL has none. */
const TYPE_EXT = Object.freeze({
  'video/mp4': '.mp4', 'video/webm': '.webm', 'audio/mpeg': '.mp3', 'audio/mp4': '.m4a',
  'image/jpeg': '.jpg', 'image/png': '.png', 'image/gif': '.gif', 'image/webp': '.webp',
  'application/zip': '.zip', 'application/pdf': '.pdf', 'application/octet-stream': '.bin',
})

/**
 * A safe file name from a URL: last path segment, stripped of query/hash,
 * restricted to a conservative character set, with a content-type fallback.
 * @param {string} url - source URL.
 * @param {string} contentType - response content-type header.
 * @returns {string} file name (never empty, never a path).
 */
export function fileNameFor(url, contentType) {
  let name = ''
  try {
    const path = new URL(url).pathname
    name = decodeURIComponent(path.split('/').filter(Boolean).pop() ?? '')
  } catch { /* keep empty */ }
  name = name.replace(/[^\w.\-()[\] ]+/g, '_').slice(0, 120)
  if (!name || name === '.' || name === '..') name = 'download'
  if (!extname(name)) {
    const type = String(contentType ?? '').split(';')[0].trim().toLowerCase()
    name += TYPE_EXT[type] ?? '.bin'
  }
  return name
}

/** The direct-file extractor, in the generic Extractor shape. */
export const genericFile = {
  id: 'generic.file',
  platform: 'generic',
  label: '直链文件',
  kind: 'file',
  patterns: ['http'],
  credentials: 'enhances',
  metadataKeys: ['content_type', 'bytes'],

  /**
   * Refine the advisory match: direct file links, or an explicit `params.kind === 'file'`.
   * @param {{url?: string, params?: object}} input - fetch input.
   * @returns {boolean}
   */
  suggests(input) {
    const url = String(input.url ?? '')
    if (!/^https?:\/\//i.test(url)) return false
    // An explicit params.kind='file' is the caller insisting; honour it as a
    // suggestion even without a recognised extension.
    if (input.params?.kind === 'file') return true
    return FILE_RE.test(url)
  },

  /**
   * Download one file.
   * @param {{url?: string}} input - fetch input.
   * @param {object} opts - generic extract options.
   * @returns {Promise<object>} FetchResult.
   */
  async extract(input, { outDir, timeoutMs, cookiesPath = '' }) {
    const url = String(input.url ?? '')
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), timeoutMs)
    let response
    try {
      // A direct link can sit behind auth (a private release asset, a
      // members-only file), so a stored jar is attached when one exists.
      response = await fetch(url, {
        signal: controller.signal,
        headers: withCookie({ 'user-agent': 'Mozilla/5.0' }, cookieHeaderFrom(cookiesPath, url)),
      })
    } catch (error) {
      throw new Error(`file download failed (${url.slice(0, 120)}): ${error.message}`)
    } finally {
      clearTimeout(timer)
    }
    if (!response.ok) throw new Error(`file download failed (${url.slice(0, 120)}): HTTP ${response.status}`)
    const data = Buffer.from(await response.arrayBuffer())
    const contentType = response.headers.get('content-type') ?? ''
    const name = fileNameFor(url, contentType)
    const path = join(outDir, name)
    await writeFile(path, data)
    const kind = contentType.startsWith('image/') ? 'image'
      : contentType.startsWith('audio/') ? 'audio'
        : contentType.startsWith('video/') ? 'video' : 'file'
    const metadata = {
      url,
      title: name,
      content_type: contentType || undefined,
      bytes: data.length,
    }
    return {
      kind,
      source: { url },
      files: [{ path, role: 'file', bytes: data.length }],
      metadata,
      extra: {},
    }
  },
}
