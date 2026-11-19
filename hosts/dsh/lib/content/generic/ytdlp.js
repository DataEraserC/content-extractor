// yt-dlp extractor — the video/audio path, wrapping the existing engine.
//
// This is an adapter, not a rewrite: argument construction and process
// handling still live in `lib/ytdlp.js` (pure, unit-tested), and metadata
// comes from yt-dlp's own `--dump-single-json` probe. The content contract
// (`lib/content/contract.js`) maps onto them here.
//
// Metadata keys follow the open-map convention: title, author, id, url,
// description, duration, thumbnail, created_at — plus whatever yt-dlp knows
// that we pass through under `extra.info` for callers that want more.

import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { fetchVideo } from '../../ytdlp.js'
import { resolveBin, runBin, stderrTail } from '../../proc.js'

/** Direct media/file links — these belong to http-file, not to a page extractor. */
const DIRECT_FILE_RE = /\.(mp4|mkv|webm|mov|avi|flv|ts|m3u8|mpd|mp3|m4a|flac|wav|ogg|jpg|jpeg|png|gif|webp|zip|pdf)(\?|#|$)/i

/**
 * Build the argv for a metadata-only probe (`--dump-single-json --skip-download`).
 * @param {object} options - probe options.
 * @param {string} options.url - page URL.
 * @param {string} [options.cookiesPath] - Netscape cookie file.
 * @returns {string[]} argv.
 */
export function buildProbeArgs({ url, cookiesPath = '' }) {
  const args = ['--no-progress', '--skip-download', '--dump-single-json', '--no-playlist']
  if (cookiesPath) args.push('--cookies', cookiesPath)
  args.push('--', url)
  return args
}

/**
 * Project yt-dlp's info JSON onto the open metadata map.
 * Unknown fields are not dropped — the whole info object rides along in
 * `extra.info` so a caller can read anything yt-dlp extracted.
 * @param {object} info - parsed `--dump-single-json` output.
 * @returns {{metadata: object, extra: object}}
 */
export function projectInfo(info) {
  const metadata = {}
  const put = (key, value) => {
    if (value !== undefined && value !== null && value !== '') metadata[key] = value
  }
  put('title', info.title ?? info.fulltitle)
  put('author', info.uploader ?? info.channel ?? info.creator)
  put('author_id', info.uploader_id ?? info.channel_id)
  put('id', info.id)
  put('url', info.webpage_url ?? info.original_url)
  put('description', info.description)
  put('duration', info.duration)
  put('thumbnail', info.thumbnail)
  put('view_count', info.view_count)
  put('like_count', info.like_count)
  put('comment_count', info.comment_count)
  put('created_at', info.timestamp ?? info.release_timestamp)
  put('upload_date', info.upload_date)
  put('extractor', info.extractor_key ?? info.extractor)
  put('tags', Array.isArray(info.tags) && info.tags.length > 0 ? info.tags : undefined)
  put('categories', Array.isArray(info.categories) && info.categories.length > 0 ? info.categories : undefined)
  return { metadata, extra: { info } }
}

/**
 * Probe one URL for metadata without downloading.
 * @param {object} options - see buildProbeArgs plus binary/timeoutMs.
 * @returns {Promise<{metadata: object, extra: object}>}
 * @throws on non-zero exit or unparseable output.
 */
export async function probeInfo({ url, cookiesPath = '', binary = 'yt-dlp', timeoutMs }) {
  const result = await runBin(resolveBin(binary), buildProbeArgs({ url, cookiesPath }), { timeoutMs })
  if (result.timedOut) throw new Error(`yt-dlp metadata probe timed out after ${Math.round(timeoutMs / 1000)}s`)
  // The JSON is the last non-empty line; warnings may precede it on stdout.
  const lines = result.stdout.split('\n').map((line) => line.trim()).filter(Boolean)
  const jsonLine = lines.filter((line) => line.startsWith('{')).pop()
  if (!jsonLine) {
    throw new Error(`yt-dlp metadata probe produced no JSON: ${stderrTail(result.stderr) || stderrTail(result.stdout) || '(empty)'}`)
  }
  let info
  try {
    info = JSON.parse(jsonLine)
  } catch (error) {
    throw new Error(`yt-dlp metadata probe returned invalid JSON: ${error.message}`)
  }
  if (result.code !== 0 && !info.id) {
    throw new Error(`yt-dlp metadata probe exited ${result.code}: ${stderrTail(result.stderr) || 'no output'}`)
  }
  return projectInfo(info)
}

/** The yt-dlp extractor, in the generic Extractor shape. */
export const genericYtdlp = {
  id: 'generic.ytdlp',
  platform: 'generic',
  label: 'yt-dlp（任意站点视频/音频）',
  kind: 'video',
  // Advisory: any http page. Specificity is low so site extractors outrank it.
  patterns: ['http'],
  requiresLogin: false,
  metadataKeys: ['extractor', 'formats', 'view_count', 'like_count', 'tags'],
  // yt-dlp is the one extractor that benefits from a logged-in cookie jar.
  usesCookies: true,

  /**
   * Refine the advisory pattern match: a direct media/file link belongs to
   * `generic.file`, not to a page extractor, so it is excluded here.
   * @param {{url?: string}} input - extract input.
   * @returns {boolean}
   */
  suggests(input) {
    const url = String(input.url ?? '')
    if (!/^https?:\/\//i.test(url)) return false
    if (DIRECT_FILE_RE.test(url)) return false
    try {
      void new URL(url)
    } catch {
      return false
    }
    return true
  },

  /**
   * Download one video, with metadata.
   * @param {{url?: string}} input - fetch input.
   * @param {object} opts - generic extract options.
   * @returns {Promise<object>} FetchResult.
   */
  async extract(input, { outDir, format, cookiesPath = '', timeoutMs, binary = 'yt-dlp' }) {
    const url = String(input.url ?? '')
    const deadline = Date.now() + timeoutMs
    // Probe first: metadata is cheap next to the download, and a probe
    // failure (private video, bad URL) should fail before bytes move.
    const probed = await probeInfo({ url, cookiesPath, binary, timeoutMs: Math.min(60000, Math.max(5000, deadline - Date.now())) })
    const remaining = deadline - Date.now()
    if (remaining <= 0) throw new Error('yt-dlp ran out of time after the metadata probe')
    const fetched = await fetchVideo({ url, outDir, format, cookiesPath, binary, timeoutMs: remaining })
    // Persist the metadata next to the media so the map survives the call.
    const metaPath = `${fetched.path}.meta.json`
    const payload = { kind: 'video', source: { extractor: 'generic.ytdlp', url }, ...probed }
    await writeFile(metaPath, JSON.stringify(payload, null, 2), 'utf8')
    return {
      kind: 'video',
      source: { url, id: probed.metadata.id },
      files: [
        { path: fetched.path, role: 'media' },
        { path: metaPath, role: 'metadata' },
      ],
      metadata: probed.metadata,
      extra: probed.extra,
    }
  },
}