// bilibili article & opus extraction — text content, NOT video.
//
// ## Why these are separate from bilibili.video
//
// `bilibili.com/read/cv…` (column article) and `bilibili.com/opus/<id>`
// (dynamic / image-text post) are text content. yt-dlp has no extractor for
// them: it falls back to its generic path and fails, which looks like "this
// site is unsupported" rather than "you picked the wrong extractor". So they
// get their own ids and their own parser.
//
// ## One renderer, two ids
//
// Both pages are rendered by the same opus front-end and embed the same state
// blob:
//
//     window.__INITIAL_STATE__ = { detail: { basic, modules: [...] } }
//
// `modules` is a list of typed blocks — MODULE_TYPE_TITLE / AUTHOR / CONTENT /
// STAT / COLLECTION / TOP … — so one walker serves both. They keep distinct ids
// because their canonical URLs, id semantics (cv number vs opus id) and
// metadata differ.
//
// Content paragraphs (`module_content.paragraphs`) carry `para_type`:
//   1 = text (a node list of words), 2 = pictures, 6 = link card,
// plus optional heading / list / code / blockquote / line blocks. The walker
// renders text faithfully and collects every image URL, so an article's
// pictures are downloadable artifacts rather than lost markup.

import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36'

/** Pause between retries. bilibili's anti-bot stub clears after a short wait. */
function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/**
 * Read page HTML.
 *
 * bilibili serves a ~3 KB shell instead of the article on a majority of
 * requests from a datacentre IP; the real ~53 KB page appears intermittently
 * (measured: roughly 1 in 5). It is not a permanent failure and not a captcha to
 * solve, so this keeps asking with a short jittered gap until the real page
 * arrives or the caller's deadline runs out. The error names the observed
 * behaviour so a caller knows retrying later is the remedy.
 */
async function fetchHtml(url, timeoutMs) {
  const deadline = Date.now() + Math.max(20000, timeoutMs)
  let attempts = 0
  let last = ''
  while (Date.now() < deadline) {
    attempts += 1
    const controller = new AbortController()
    const perTry = Math.min(20000, Math.max(3000, deadline - Date.now()))
    const timer = setTimeout(() => controller.abort(), perTry)
    try {
      const response = await fetch(url, {
        signal: controller.signal,
        headers: {
          'user-agent': UA,
          accept: 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
          'accept-language': 'zh-CN,zh;q=0.9,en;q=0.8',
          referer: 'https://www.bilibili.com/',
        },
      })
      if (!response.ok) throw new Error(`HTTP ${response.status}`)
      const text = await response.text()
      if (text.includes('__INITIAL_STATE__')) return text
      last = `shell page without state (${text.length} bytes)`
    } catch (error) {
      last = error.message
    } finally {
      clearTimeout(timer)
    }
    if (Date.now() >= deadline) break
    // Jitter avoids locking step with whatever rotates the response.
    await sleep(600 + Math.floor(Math.random() * 700))
  }
  throw new Error(
    `cannot read the bilibili page after ${attempts} attempts (${url.slice(0, 100)}): ${last}. ` +
    'bilibili returns a shell page for most datacentre requests; retry later or supply a login cookie.',
  )
}

/**
 * Pull the `window.__INITIAL_STATE__` object out of a page.
 * @param {string} html - page source.
 * @returns {object} the parsed state.
 * @throws when the blob is absent or unparseable.
 */
export function initialStateOf(html) {
  const marker = 'window.__INITIAL_STATE__='
  const relaxed = /window\.__INITIAL_STATE__\s*=\s*/.exec(html)
  if (!relaxed) throw new Error('no __INITIAL_STATE__ in the page')
  void marker
  const start = relaxed.index + relaxed[0].length
  // Brace-match rather than regex-to-semicolon: the blob contains strings with
  // semicolons and nested objects, so a lazy regex truncates it.
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
        const text = html.slice(start, i + 1)
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

/** Index the module list by its `module_type`. */
function modulesOf(state) {
  const list = state?.detail?.modules
  if (!Array.isArray(list)) throw new Error('the page state has no detail.modules — not an article/opus page')
  const out = {}
  for (const entry of list) {
    if (entry?.module_type) out[entry.module_type] = entry
  }
  return out
}

/** Concatenate a paragraph's word nodes into plain text. */
function wordsOf(paragraph) {
  const nodes = paragraph?.text?.nodes
  if (!Array.isArray(nodes)) return ''
  return nodes.map((node) => node?.word?.words ?? '').join('')
}

/**
 * Render the content modules into text plus an ordered image list.
 * @param {object} contentModule - the MODULE_TYPE_CONTENT entry.
 * @returns {{text: string, images: Array<{url: string, width?: number, height?: number}>}}
 */
export function renderContent(contentModule) {
  const paragraphs = contentModule?.module_content?.paragraphs
  if (!Array.isArray(paragraphs)) return { text: '', images: [] }
  const lines = []
  const images = []
  for (const paragraph of paragraphs) {
    if (paragraph?.heading?.text) {
      lines.push(`## ${paragraph.heading.text}`)
      continue
    }
    if (paragraph?.blockquote) {
      const quoted = wordsOf(paragraph.blockquote)
      if (quoted) lines.push(`> ${quoted}`)
      continue
    }
    if (paragraph?.code?.text) {
      // One block, so the fenced code stays contiguous when paragraphs are
      // joined with blank lines.
      lines.push(['```', paragraph.code.text, '```'].join('\n'))
      continue
    }
    if (paragraph?.line) {
      lines.push('---')
      continue
    }
    const pics = paragraph?.pic?.pics
    if (Array.isArray(pics) && pics.length > 0) {
      for (const pic of pics) {
        if (!pic?.url) continue
        images.push({ url: pic.url, width: pic.width, height: pic.height })
        lines.push(`![](${pic.url})`)
      }
      continue
    }
    if (Array.isArray(paragraph?.list?.items)) {
      for (const item of paragraph.list.items) {
        const text = wordsOf(item)
        if (text) lines.push(`- ${text}`)
      }
      continue
    }
    const text = wordsOf(paragraph)
    if (text) lines.push(text)
  }
  return { text: lines.join('\n\n'), images }
}

/**
 * Read an id that may exceed JS's safe integer range.
 *
 * bilibili's opus ids are 19-digit snowflakes; `JSON.parse` silently rounds
 * them (…204 becomes …200). The raw text is scanned for the exact digits so the
 * id a caller gets is the id the site uses.
 * @param {string} html - page source.
 * @returns {string} the exact `detail.basic.comment_id_str` / top-level id, or ''.
 */
export function exactIdOf(html) {
  // `"id":38554821905721204` at the top level of the state blob.
  const numeric = /"id"\s*:\s*(\d{6,})/.exec(html)
  if (numeric) return numeric[1]
  const quoted = /"(?:comment_id_str|rid_str|id_str)"\s*:\s*"(\d{6,})"/.exec(html)
  return quoted ? quoted[1] : ''
}

/**
 * The canonical cv number of an article page.
 *
 * `state.id` is the internal opus id (a 19-digit snowflake), NOT the cv number
 * the URL carries — using it would name files and metadata after a number the
 * user never sees. `module_copyright.right_text` is the site's own "cv1" label,
 * and the URL path is the other authoritative source; both are preferred.
 * @param {object} state - parsed page state.
 * @param {string} url - the page URL.
 * @returns {string} the cv number, or '' when neither source has one.
 */
export function cvidOf(state, url) {
  const fromUrl = /\/read\/cv(\d+)/i.exec(String(url ?? ''))
  if (fromUrl) return fromUrl[1]
  const list = state?.detail?.modules
  if (Array.isArray(list)) {
    for (const entry of list) {
      const text = entry?.module_copyright?.right_text
      const match = typeof text === 'string' ? /^cv(\d+)$/i.exec(text.trim()) : null
      if (match) return match[1]
    }
  }
  return ''
}

/**
 * Project an article/opus page state onto the open metadata map.
 * @param {object} state - parsed __INITIAL_STATE__.
 * @param {string} url - canonical page URL.
 * @param {'article'|'post'} kind - which extractor is asking.
 * @returns {{metadata: object, images: Array<object>}}
 */
export function projectArticle(state, url, kind, exactId = '') {
  const modules = modulesOf(state)
  const basic = state?.detail?.basic ?? {}
  const title = modules.MODULE_TYPE_TITLE?.module_title?.text ?? basic.title ?? ''
  const author = modules.MODULE_TYPE_AUTHOR?.module_author ?? {}
  const stat = modules.MODULE_TYPE_STAT?.module_stat ?? {}
  const collection = modules.MODULE_TYPE_COLLECTION?.module_collection
  const { text, images } = renderContent(modules.MODULE_TYPE_CONTENT)
  const topPics = modules.MODULE_TYPE_TOP?.module_top?.display?.album?.pics
  if (Array.isArray(topPics)) {
    for (const pic of topPics) {
      if (pic?.url && !images.some((image) => image.url === pic.url)) {
        images.unshift({ url: pic.url, width: pic.width, height: pic.height })
      }
    }
  }

  const metadata = {}
  const put = (key, value) => {
    if (value !== undefined && value !== null && value !== '') metadata[key] = value
  }
  put('title', title)
  put('author', author.name)
  put('author_id', author.mid === undefined ? undefined : String(author.mid))
  put('url', url)
  put('description', modules.MODULE_TYPE_BOTTOM?.module_bottom?.share_info?.summary)
  put('thumbnail', images[0]?.url)
  put('created_at', author.pub_ts === undefined ? undefined : Number(author.pub_ts))
  put('text', text)
  put('word_count', text.length)
  put('images', images.map((image) => image.url))
  put('image_count', images.length)
  put('view_count', stat.view?.count)
  put('like_count', stat.like?.count)
  put('coin_count', stat.coin?.count)
  put('favorite_count', stat.favorite?.count)
  put('reply_count', stat.comment?.count)
  put('share_count', stat.forward?.count)
  if (collection?.name) put('collection', collection.name)
  // Prefer the exactly-scanned id over the JSON-parsed one, which rounds
  // 19-digit snowflakes.
  const snowflake = exactId || String(state?.id ?? basic.comment_id_str ?? '')
  if (kind === 'article') {
    const cvid = cvidOf(state, url)
    put('id', cvid)
    put('cvid', cvid)
    // The internal snowflake is kept separately: it is what the opus API uses.
    put('opus_id', snowflake)
  } else {
    put('id', snowflake)
    put('opus_id', snowflake)
  }
  put('author_avatar', author.face)
  put('pub_location', author.pub_location_text)
  return { metadata, images }
}

/** Download one image, tolerating a failure so one bad URL cannot sink a post. */
async function downloadImage(url, path, timeoutMs, referer) {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    const response = await fetch(url, {
      signal: controller.signal,
      headers: { 'user-agent': UA, referer },
    })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = Buffer.from(await response.arrayBuffer())
    await writeFile(path, data)
    return data.length
  } finally {
    clearTimeout(timer)
  }
}

/** Shared implementation for the article and opus extractors. */
async function extractText(kind, id, input, { outDir, timeoutMs, params }) {
  const url = String(input.url ?? '')
  if (!/^https?:\/\//i.test(url)) throw new Error(`${id} needs a page URL`)
  const deadline = Date.now() + timeoutMs
  // Give the page fetch most of the budget: the shell-page retry loop needs it,
  // and image downloads afterwards are individually short.
  const html = await fetchHtml(url, Math.max(20000, deadline - Date.now() - 15000))
  const state = initialStateOf(html)
  const { metadata, images } = projectArticle(state, url, kind, exactIdOf(html))

  const files = []
  const stem = kind === 'article' ? `bili-cv${metadata.cvid ?? 'x'}` : `bili-opus${metadata.opus_id ?? 'x'}`
  // The text is a first-class artifact, not only a metadata field.
  const textPath = join(outDir, `${stem}.md`)
  const body = [`# ${metadata.title ?? ''}`, '', metadata.text ?? ''].join('\n')
  await writeFile(textPath, body, 'utf8')
  files.push({ path: textPath, role: 'text', bytes: Buffer.byteLength(body) })

  // Images are downloaded unless the caller opted out.
  if (params?.images !== false) {
    let index = 0
    for (const image of images) {
      const remaining = deadline - Date.now()
      if (remaining <= 0) break
      index += 1
      const ext = (/\.(jpe?g|png|gif|webp)(?:\?|$)/i.exec(image.url)?.[1] ?? 'jpg').toLowerCase()
      const imagePath = join(outDir, `${stem}-img${index}.${ext}`)
      try {
        const bytes = await downloadImage(image.url, imagePath, remaining, 'https://www.bilibili.com/')
        files.push({ path: imagePath, role: 'image', bytes })
      } catch { /* keep going: partial images beat no article */ }
    }
  }

  const metaPath = join(outDir, `${stem}.meta.json`)
  const payload = { kind, source: { extractor: id, url }, metadata }
  await writeFile(metaPath, JSON.stringify(payload, null, 2), 'utf8')
  files.push({ path: metaPath, role: 'metadata' })
  return { kind, source: { url, id: metadata.id }, files, metadata, extra: { state } }
}

/** bilibili column article (`/read/cv…`). */
export const bilibiliArticle = {
  id: 'bilibili.article',
  platform: 'bilibili',
  label: '哔哩哔哩专栏文章',
  kind: 'article',
  patterns: ['bilibili.com/read/'],
  requiresLogin: false,
  metadataKeys: [
    'cvid', 'author_id', 'text', 'images', 'word_count', 'view_count',
    'like_count', 'coin_count', 'favorite_count', 'collection',
  ],

  /** @returns {Promise<object>} ExtractResult with a .md file and images. */
  async extract(input, opts) {
    return extractText('article', 'bilibili.article', input, opts)
  },
}

/** bilibili opus / dynamic post (`/opus/<id>`). */
export const bilibiliOpus = {
  id: 'bilibili.opus',
  platform: 'bilibili',
  label: '哔哩哔哩动态/图文',
  kind: 'post',
  patterns: ['bilibili.com/opus/'],
  requiresLogin: false,
  metadataKeys: ['opus_id', 'author_id', 'text', 'images', 'view_count', 'like_count'],

  /** @returns {Promise<object>} ExtractResult with a .md file and images. */
  async extract(input, opts) {
    return extractText('post', 'bilibili.opus', input, opts)
  },
}