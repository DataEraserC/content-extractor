// Netscape cookie file → `Cookie:` header, for extractors that fetch directly.
//
// The credential layer hands every extractor a **path** to a 0600 Netscape
// cookie file (see lib/native.js `materialize`). yt-dlp reads that file itself
// via `--cookies`, but an extractor that uses `fetch` has to turn it into a
// header — that is what this module does, so no extractor re-implements the
// parsing and none of them has to know the file format.
//
// ## Secrets boundary
//
// The returned string IS a credential. It goes into an outbound request header
// and nowhere else: never into a result, a metadata map, a log line, or an error
// message. Callers must not put it in `extra` either.
//
// ## Domain matching
//
// A jar can hold rows for several hosts (`.bilibili.com`, `passport.bilibili.com`,
// …). Sending all of them to one host would leak cookies across sites, so rows
// are filtered by the request URL's host using the Netscape rules: a leading dot
// means "this domain and its subdomains", otherwise exact match. Secure rows are
// only sent over https. Expired rows are dropped.

import { readFileSync } from 'node:fs'

/**
 * Parse Netscape cookie text into records.
 * @param {string} text - file contents.
 * @returns {Array<{domain: string, path: string, secure: boolean, expires: number, name: string, value: string}>}
 */
export function parseNetscape(text) {
  const out = []
  for (const raw of String(text ?? '').split('\n')) {
    const line = raw.trim()
    if (line === '' || line.startsWith('#')) continue
    const parts = line.split('\t')
    if (parts.length < 7) continue
    const [domain, , path, secure, expires, name, ...rest] = parts
    if (!name) continue
    out.push({
      domain: domain.toLowerCase(),
      path: path || '/',
      secure: secure.toUpperCase() === 'TRUE',
      expires: Number(expires) || 0,
      name,
      // A cookie value may itself contain tabs in a malformed export; rejoin.
      value: rest.join('\t'),
    })
  }
  return out
}

/** Does a cookie's domain apply to `host`? Netscape leading-dot semantics. */
function domainMatches(domain, host) {
  const clean = domain.startsWith('.') ? domain.slice(1) : domain
  if (domain.startsWith('.')) return host === clean || host.endsWith(`.${clean}`)
  return host === clean
}

/**
 * Build a `Cookie:` header for one URL from a Netscape jar.
 * @param {string} text - jar contents.
 * @param {string} url - the request URL (decides host/path/secure filtering).
 * @param {number} [nowSeconds] - clock for expiry; defaults to now.
 * @returns {string} the header value, or '' when nothing applies. SECRET.
 */
export function cookieHeaderFor(text, url, nowSeconds = Math.floor(Date.now() / 1000)) {
  let target
  try {
    target = new URL(url)
  } catch {
    return ''
  }
  const host = target.hostname.toLowerCase()
  const path = target.pathname || '/'
  const https = target.protocol === 'https:'
  const pairs = []
  const seen = new Set()
  for (const cookie of parseNetscape(text)) {
    if (!domainMatches(cookie.domain, host)) continue
    if (cookie.secure && !https) continue
    if (cookie.expires > 0 && cookie.expires <= nowSeconds) continue
    if (!path.startsWith(cookie.path)) continue
    // Later rows win on duplicate names, matching how a browser jar behaves.
    if (seen.has(cookie.name)) continue
    seen.add(cookie.name)
    pairs.push(`${cookie.name}=${cookie.value}`)
  }
  return pairs.join('; ')
}

/**
 * Read a materialized cookie file and build the header for `url`.
 *
 * Returns '' when no path was given (anonymous attempt) or the file cannot be
 * read — a missing credential must degrade to anonymous, not fail the extract.
 * @param {string} cookiesPath - path from `materialize`, or ''.
 * @param {string} url - request URL.
 * @returns {string} header value, or ''. SECRET.
 */
export function cookieHeaderFrom(cookiesPath, url) {
  if (!cookiesPath) return ''
  try {
    return cookieHeaderFor(readFileSync(cookiesPath, 'utf8'), url)
  } catch {
    return ''
  }
}

/**
 * Merge a cookie header into a headers object, only when non-empty.
 * Keeps call sites from sending an empty `cookie: ''` header.
 * @param {Record<string,string>} headers - base headers.
 * @param {string} cookie - header value (may be '').
 * @returns {Record<string,string>} headers with `cookie` set when present.
 */
export function withCookie(headers, cookie) {
  return cookie ? { ...headers, cookie } : { ...headers }
}