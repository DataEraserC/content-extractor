// The content-extraction contract.
//
// ## Selection is explicit
//
// There is deliberately **no** first-match-wins dispatch here. One host serves
// many content kinds — bilibili has videos, articles, opus posts, audio and
// live rooms on one domain — so guessing would send an article down the video
// path and produce a failure that looks like "site unsupported".
//
// Two steps instead:
//
//   1. `candidates(input)` → EVERY extractor that might handle it, ranked by
//      pattern specificity. Advisory; a human or model reads the list.
//   2. `extract({ extractor: '<id>', … })` → the caller's explicit choice.
//
// ## An Extractor
//
//     {
//       id:       'bilibili.article',   // globally unique; what callers pass
//       platform: 'bilibili',           // credential owner (see native.js)
//       label:    '哔哩哔哩专栏文章',
//       kind:     'article',            // video|audio|article|post|gallery|
//                                       // live|replay|file|…
//       patterns: ['bilibili.com/read/'],  // advisory URL hints
//       credentials: 'enhances',        // required | enhances | none
//       metadataKeys: [...],            // documented extras; map stays open
//       extract(input, opts): Promise<ExtractResult>,
//     }
//
// `input` is `{ url?, id?, params? }`. `opts` is
// `{ outDir, timeoutMs, cookiesPath, format, binary }`; an extractor uses what
// it needs.
//
// ## The result is an open map
//
//     {
//       kind, source: { extractor, platform, url?, id? },
//       files: [{ path, role, bytes? }],
//       metadata: { … },   // OPEN string→JSON map
//       extra:    { … },   // extractor-private raw payload
//     }
//
// Common metadata keys are conventions, not schema: `title`, `author`, `id`,
// `url`, `description`, `duration`, `thumbnail`, `created_at`. Any extractor
// adds its own keys without touching this file, the tools, or the UI.
//
// Two rules every extractor must keep:
//   * every file lands inside `outDir` (workspace confinement is enforced
//     upstream and must not be widened);
//   * cookie text is read from `cookiesPath` and never echoed into a result.

/** Content kinds the project knows about. Open: a new kind is just a string. */
export const CONTENT_KINDS = Object.freeze([
  'video', 'audio', 'article', 'post', 'gallery', 'live', 'replay', 'file',
])

/**
 * How a credential affects an extractor — three-valued on purpose.
 *
 * A boolean "needs login" conflated two questions and lost the important middle
 * case: bilibili serves a video anonymously at 360p and the same URL at 1080p+
 * with cookies. Login is not *required*, yet applying it is exactly what the
 * user wants. Collapsing that to `false` silently dropped stored credentials.
 *
 *   required — extraction fails without a credential (members-only, paid)
 *   enhances — works anonymously, but a credential improves the result
 *              (quality, full text, more items, fewer blocks). SEND COOKIES.
 *   none     — public resource that ignores credentials; sending them would
 *              only leak them to a third party.
 */
export const CREDENTIAL_USE = Object.freeze(['required', 'enhances', 'none'])

/**
 * Should a stored credential be attached when one exists?
 * True for both `required` and `enhances` — that is the whole point of the pool.
 * @param {string} use - one of CREDENTIAL_USE.
 * @returns {boolean}
 */
export function usesCredentials(use) {
  return use === 'required' || use === 'enhances'
}

/**
 * How specific an extractor's patterns are, for ranking candidates.
 * Total matched character length, so a bare-`http` catch-all ranks below a
 * site-specific pattern even though both match one fragment.
 * @param {object} extractor - an Extractor.
 * @returns {number}
 */
export function specificity(extractor) {
  return (extractor.patterns ?? []).reduce((sum, pattern) => sum + String(pattern).length, 0)
}

/**
 * Does this extractor's pattern set suggest it could handle `input`?
 * Advisory only — a true here means "candidate", never "chosen".
 *
 * An extractor may override with its own `suggests(input)` to refine the
 * pattern hint (e.g. yt-dlp excludes direct file links). The override can only
 * narrow, never widen: patterns must still match.
 * @param {object} extractor - an Extractor.
 * @param {{url?: string, id?: string}} input - what to extract.
 * @returns {boolean}
 */
export function suggests(extractor, input) {
  const url = String(input.url ?? '').toLowerCase()
  const patterns = extractor.patterns ?? []
  // A bare-id form is an independent way in: an extractor that owns ids does
  // not need a URL to match (a replay id has no URL until it is resolved).
  const ownsId = typeof extractor.ownsId === 'function' && Boolean(extractor.ownsId(input))
  const patternsMatch = patterns.length > 0
    && patterns.every((pattern) => url.includes(String(pattern).toLowerCase()))
  if (!ownsId && !patternsMatch) return false
  if (typeof extractor.suggests === 'function') return Boolean(extractor.suggests(input))
  return true
}

/**
 * Validate that a module really implements the Extractor contract.
 * Called at registration so a malformed extractor fails loudly at load rather
 * than silently never matching.
 * @param {object} extractor - candidate.
 * @throws when a required member is missing or mistyped.
 */
export function assertExtractor(extractor) {
  const where = extractor?.id ?? '(anonymous)'
  for (const key of ['id', 'platform', 'label', 'kind']) {
    if (typeof extractor?.[key] !== 'string' || extractor[key] === '') {
      throw new Error(`extractor ${where}: "${key}" must be a non-empty string`)
    }
  }
  if (!Array.isArray(extractor.patterns)) {
    throw new Error(`extractor ${where}: "patterns" must be an array`)
  }
  if (typeof extractor.extract !== 'function') {
    throw new Error(`extractor ${where}: "extract" must be a function`)
  }
  if (!extractor.id.includes('.')) {
    throw new Error(`extractor ${where}: id must be "<platform>.<name>"`)
  }
  if (!CREDENTIAL_USE.includes(extractor.credentials)) {
    throw new Error(
      `extractor ${where}: "credentials" must be one of ${CREDENTIAL_USE.join(', ')} ` +
      `(got ${JSON.stringify(extractor.credentials)})`,
    )
  }
}
