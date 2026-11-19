// bilibili video extraction — delegates to yt-dlp, which owns this site's
// player protocol (WBI signing, format ladders, DASH merging).
//
// This is a thin, *explicit* wrapper: it exists so `bilibili.video` is a
// distinct id from `bilibili.article`. Before the split, one bilibili-shaped
// extractor claimed every bilibili URL, so an article was handed to yt-dlp and
// failed as "unsupported site". Now the caller chooses, and a video URL routed
// here still gets the whole yt-dlp machinery plus the platform's cookie jar.

import { genericYtdlp } from '../generic/ytdlp.js'

/** bilibili video (`/video/BV…` or `/video/av…`). */
export const bilibiliVideo = {
  id: 'bilibili.video',
  platform: 'bilibili',
  label: '哔哩哔哩视频',
  kind: 'video',
  patterns: ['bilibili.com/video/'],
  // Public videos work anonymously; a login raises the quality tier, so the
  // registry rotates this platform's cookie jars when any are stored.
  credentials: 'enhances',
  metadataKeys: [
    'bvid', 'aid', 'view_count', 'like_count', 'danmaku_count', 'uploader_id', 'pages',
  ],

  /** Also accept the b23.tv short link, which redirects to a video page. */
  suggests(input) {
    const url = String(input.url ?? '').toLowerCase()
    return url.includes('bilibili.com/video/') || url.includes('b23.tv/')
  },

  /**
   * Download one bilibili video with metadata.
   * @param {{url?: string}} input - extract input.
   * @param {object} opts - extract options (cookiesPath carries the login jar).
   * @returns {Promise<object>} ExtractResult.
   */
  async extract(input, opts) {
    const result = await genericYtdlp.extract(input, opts)
    // Stamp the platform-specific id when yt-dlp reported one.
    const info = result.extra?.info ?? {}
    if (info.id) result.metadata.bvid = info.id
    if (info.uploader_id) result.metadata.uploader_id = info.uploader_id
    if (info.danmaku_count !== undefined) result.metadata.danmaku_count = info.danmaku_count
    if (Array.isArray(info.entries)) result.metadata.pages = info.entries.length
    return result
  },
}
