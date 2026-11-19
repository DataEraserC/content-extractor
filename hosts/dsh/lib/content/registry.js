// The extractor registry: what exists, and who *could* handle an input.
//
// Mirrors the Rust kernel's `extract_core::registry`: `candidates` is plural
// and advisory, `extractorOf` resolves an explicitly chosen id, and there is no
// first-match-wins function anywhere in this file.
//
// Adding a platform is adding a directory under `lib/content/<platform>/` and
// one import + one array entry below. Nothing else changes: not the tools, not
// the routes, not the page.

import { assertExtractor, specificity, suggests } from './contract.js'
import { bilibiliArticle, bilibiliOpus } from './bilibili/article.js'
import { bilibiliVideo } from './bilibili/video.js'
import { wmpvpNote, wmpvpVideo } from './wmpvp/post.js'
import { astralReplay } from './astralparty/replay.js'
import { genericFile } from './generic/file.js'
import { genericYtdlp } from './generic/ytdlp.js'

/**
 * Every registered extractor.
 *
 * Order only affects tie-breaking in `candidates`; it never decides selection.
 * Site-specific extractors are listed before the catch-alls for readability.
 */
const EXTRACTORS = [
  bilibiliVideo,
  bilibiliArticle,
  bilibiliOpus,
  wmpvpVideo,
  wmpvpNote,
  astralReplay,
  genericFile,
  genericYtdlp,
]

for (const extractor of EXTRACTORS) assertExtractor(extractor)

const ids = EXTRACTORS.map((extractor) => extractor.id)
if (new Set(ids).size !== ids.length) {
  throw new Error(`duplicate extractor ids: ${ids.join(', ')}`)
}

/** Machine ids of every extractor, for pickers and status lines. */
export const EXTRACTOR_IDS = Object.freeze(ids)

/**
 * Resolve an **explicitly chosen** extractor id.
 * @param {string} id - e.g. `bilibili.article`.
 * @returns {object} the Extractor.
 * @throws when unknown, listing the known ids.
 */
export function extractorOf(id) {
  const found = EXTRACTORS.filter((candidate) => candidate.id === id)[0]
  if (!found) {
    throw new Error(`unknown extractor "${id}" (known: ${EXTRACTOR_IDS.join(', ')})`)
  }
  return found
}

/**
 * Which extractors *might* handle this input — plural, ranked, advisory.
 *
 * Never selects. An empty list means nothing advertises this shape and the
 * caller should say so rather than guess.
 * @param {{url?: string, id?: string}} input - what to extract.
 * @returns {Array<{extractor: object, score: number, reason: string}>}
 */
export function candidates(input) {
  const out = EXTRACTORS.filter((extractor) => suggests(extractor, input)).map((extractor) => ({
    extractor: describe(extractor),
    score: specificity(extractor),
    reason: `${extractor.id} handles ${extractor.kind} content and its patterns match`,
  }))
  // Most specific first; ties keep registration order (stable sort).
  out.sort((a, b) => b.score - a.score)
  return out
}

/** The serialisable descriptor of an extractor (no functions). */
function describe(extractor) {
  return {
    id: extractor.id,
    platform: extractor.platform,
    label: extractor.label,
    kind: extractor.kind,
    patterns: extractor.patterns,
    requiresLogin: Boolean(extractor.requiresLogin),
    metadataKeys: extractor.metadataKeys ?? [],
  }
}

/**
 * Every extractor's descriptor, for a picker or a status line.
 * @returns {Array<object>}
 */
export function listExtractors() {
  return EXTRACTORS.map(describe)
}

/**
 * Extract with an **explicitly chosen** extractor.
 *
 * `input.extractor` is required. When it is absent this throws with the
 * candidate list attached, so a caller that forgot to choose gets an actionable
 * error instead of a silent guess.
 *
 * @param {object} options - extraction options.
 * @param {{url?: string, id?: string, params?: object, extractor?: string}} options.input - what to extract.
 * @param {string} options.outDir - absolute output directory (workspace-confined).
 * @param {string} [options.format] - engine-specific format selector.
 * @param {Array<{id: string}>} [options.accounts] - accounts to try, pinned first.
 * @param {(accountId: string) => Promise<string>} [options.materialize] - writes
 *   one account's cookies to a 0600 file and returns its path.
 * @param {number} options.timeoutMs - hard deadline spanning every attempt.
 * @param {string} [options.binary] - yt-dlp binary override.
 * @returns {Promise<object>} the ExtractResult plus `attempt`/`accountId`.
 * @throws when no extractor was chosen, or the LAST error when every account failed.
 */
export async function extract({ input, outDir, format, accounts = [], materialize, timeoutMs, binary }) {
  const chosen = String(input.extractor ?? '')
  if (chosen === '') {
    const possible = candidates(input)
    const hint = possible.length > 0
      ? possible.map((entry) => entry.extractor.id).join(', ')
      : '(none — nothing advertises this input)'
    throw new Error(
      'extract needs an explicit "extractor" id; auto-selection is deliberately not done. ' +
      `Candidates for this input: ${hint}`,
    )
  }
  const extractor = extractorOf(chosen)
  const deadline = Date.now() + timeoutMs
  // Extractors that never read cookies get exactly one anonymous attempt.
  const order = extractor.requiresLogin && accounts.length > 0 ? accounts : [null]
  let last = null
  for (let index = 0; index < order.length; index += 1) {
    const account = order[index]
    const remaining = deadline - Date.now()
    if (remaining <= 0) break
    try {
      const cookiesPath = account === null || !materialize ? '' : await materialize(account.id)
      const value = await extractor.extract(input, { outDir, format, cookiesPath, timeoutMs: remaining, binary })
      return {
        ...value,
        source: { extractor: extractor.id, platform: extractor.platform, ...(value.source ?? {}) },
        accountId: account === null ? null : account.id,
        attempt: index,
      }
    } catch (error) {
      last = error
    }
  }
  throw last ?? new Error('extract had nothing to try')
}