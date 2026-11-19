// Live proof that a stored credential reaches a NON-video extractor.
//
// The bug this guards: `requiresLogin: false` made the registry skip the cookie
// jar entirely, so only video benefited from a login. This script imports a
// throwaway cookie into the bilibili account pool, then extracts an ARTICLE and
// asserts the request actually carried it — by pointing the extractor at a
// local server that echoes back the headers it received.
//
// No real credential is used or needed: the value is a synthetic marker.
// Run: node test/live-cookie-propagation.mjs
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { cookieHeaderFrom } from '../lib/content/cookies.js'
import { extractorOf } from '../lib/content/registry.js'
import { usesCredentials } from '../lib/content/contract.js'

const MARKER = 'propagation-probe'
const outDir = mkdtempSync(join(tmpdir(), 'cred-live-'))
const jarPath = join(outDir, 'probe.jar.txt')
writeFileSync(
  jarPath,
  `# Netscape HTTP Cookie File\n127.0.0.1\tFALSE\t/\tFALSE\t0\tPROBE\t${MARKER}\n`,
  { mode: 0o600 },
)

// A server that records the Cookie header it was handed.
let received = null
const server = createServer((req, res) => {
  received = req.headers.cookie ?? ''
  res.writeHead(200, { 'content-type': 'application/octet-stream' })
  res.end(Buffer.from('probe'))
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const port = server.address().port
const url = `http://127.0.0.1:${port}/file.bin`

try {
  // 1) The header builder scopes the jar to the request host.
  const header = cookieHeaderFrom(jarPath, url)
  assert.equal(header, `PROBE=${MARKER}`)
  console.log('cookie header built:', header)

  // 2) generic.file is `enhances`, so the registry must attach credentials.
  const fileExtractor = extractorOf('generic.file')
  assert.equal(fileExtractor.credentials, 'enhances')
  assert.ok(usesCredentials(fileExtractor.credentials))
  await fileExtractor.extract({ url }, { outDir, timeoutMs: 15000, cookiesPath: jarPath })
  assert.equal(received, `PROBE=${MARKER}`, 'the non-video extractor did not send the cookie')
  console.log('generic.file sent cookie:', received)

  // 3) Same for the article extractor's page fetch (it must forward cookiesPath).
  received = null
  const article = extractorOf('bilibili.article')
  assert.equal(article.credentials, 'enhances')
  await assert.rejects(
    () => article.extract({ url }, { outDir, timeoutMs: 4000, cookiesPath: jarPath }),
    /cannot read the bilibili page|no __INITIAL_STATE__/,
  )
  assert.equal(received, `PROBE=${MARKER}`, 'the article extractor did not send the cookie')
  console.log('bilibili.article sent cookie:', received)

  // 4) The public replay CDN must NOT receive it.
  const replay = extractorOf('astralparty.replay')
  assert.equal(replay.credentials, 'none')
  assert.ok(!usesCredentials(replay.credentials))
  console.log('astralparty.replay credentials: none (no cookie sent)')

  console.log('\nOK: credentials propagate to non-video extractors, and stay off public CDNs')
} finally {
  server.close()
}