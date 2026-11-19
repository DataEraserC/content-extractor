// Live end-to-end check against real bilibili + real yt-dlp + real ffmpeg.
//
// Not part of `npm test` (it needs the network and the two binaries):
//
//   node test/live.mjs
//
// Legs:
//   1. video_fetch downloads a short real bilibili video (no login needed).
//   2. video_compress re-encodes it to the QQ-ready MP4.
//   3. video_login start generates a real QR PNG; poll returns the waiting
//      code (86101) — a full confirm needs a human scan, which this script
//      cannot do, so it stops at "waiting" and cleans its state up.

import assert from 'node:assert/strict'
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'

process.env.DSH_HOME = mkdtempSync(join(tmpdir(), 'vf-live-home-'))
// video_fetch downloads only land inside the session workspace, so the fake
// session exec below carries this dir as its header cwd.
const workspace = mkdtempSync(join(tmpdir(), 'vf-live-ws-'))
const exec = { agent: { session: { header: { cwd: workspace } } } }

const VIDEO_URL = 'https://www.bilibili.com/video/BV1DmJX6SEqU' // ~111s, short & stable
const checks = []
async function check(name, body) {
  try {
    await body()
    checks.push(`ok   ${name}`)
  } catch (error) {
    checks.push(`FAIL ${name}: ${error.message}`)
    process.exitCode = 1
  }
}

const mod = await import('../lib/index.js')
const tools = []
const ctx = { tools: { register: (tool) => tools.push(tool) } }
const config = mod.Config({ fetchTimeoutMs: 300000, compressTimeoutMs: 300000 })
mod.apply(ctx, config)
const fetchTool = tools.find((t) => t.name === 'video_fetch')
const compressTool = tools.find((t) => t.name === 'video_compress')
const loginTool = tools.find((t) => t.name === 'video_login')

let downloadedPath = ''

await check('video_fetch: real bilibili download returns an existing file', async () => {
  const r = await fetchTool.execute({ url: VIDEO_URL }, exec)
  downloadedPath = r.text.match(/^(?:Downloaded|Already downloaded): (.+)$/m)?.[1]
  assert.ok(downloadedPath, `no path in result:\n${r.text}`)
  assert.ok(existsSync(downloadedPath), 'file missing')
  assert.ok(statSync(downloadedPath).size > 1024 * 1024, 'file suspiciously small')
  assert.match(r.text, /Size: [\d.]+ MB/)
})

await check('video_fetch: the download landed inside the session workspace (default subdir)', () => {
  assert.equal(dirname(downloadedPath), join(workspace, 'dsh-content-extract'), 'expected the workspace default subdir')
})

await check('video_fetch: second run returns the same file without re-downloading', async () => {
  const mtimeBefore = statSync(downloadedPath).mtimeMs
  const r = await fetchTool.execute({ url: VIDEO_URL }, exec)
  const again = r.text.match(/^Downloaded: (.+)$/m)?.[1]
  assert.equal(again, downloadedPath, 'same path expected')
  assert.equal(statSync(downloadedPath).mtimeMs, mtimeBefore, 'file was re-downloaded (mtime changed)')
})

await check('video_fetch: compress=true produces .small.mp4 via ffmpeg', async () => {
  const r = await fetchTool.execute({ url: VIDEO_URL, compress: true }, exec)
  const out = r.text.match(/^Compressed: (.+?)(?: \(| \d|\n|$)/m)?.[1]
  assert.ok(out, `no compressed line:\n${r.text}`)
  assert.ok(existsSync(out), `compressed file missing: ${out}`)
  const before = Number(r.text.match(/\(([\d.]+) MB →/)?.[1])
  const after = Number(r.text.match(/→ ([\d.]+) MB/)?.[1])
  assert.ok(after > 0 && before > 0, 'sizes unparsed')
  assert.ok(statSync(out).size < statSync(downloadedPath).size, 'compressed not smaller than source')
})

await check('video_compress: standalone on the downloaded file', async () => {
  const r = await compressTool.execute({ file: downloadedPath, crf: 30, maxHeight: 480 }, {})
  assert.match(r.text, /\.small\./)
  const out = r.text.match(/^Compressed: (.+)$/m)?.[1]
  assert.ok(existsSync(out))
  assert.ok(statSync(out).size < statSync(downloadedPath).size)
})

let lastSessionId = ''
await check('video_login start: real QR challenge + pending state', async () => {
  const r = await loginTool.execute({ action: 'start', channel: 'bilibili', method: 'qr' }, {})
  assert.match(r.text, /sessionId: \S+/, `no session in result:\n${r.text}`)
  assert.match(r.text, /QR payload: https?:\/\//, 'no QR payload to scan')
  lastSessionId = r.text.match(/sessionId: (\S+)/)[1]
  const sessionFile = join(process.env.DSH_HOME, 'content-extract', 'sessions', `${lastSessionId}.json`)
  assert.ok(existsSync(sessionFile), `session file missing: ${sessionFile}`)
  const state = JSON.parse(readFileSync(sessionFile, 'utf8'))
  assert.ok(state.state?.qrcode_key || state.state?.jar !== undefined, 'no provider state persisted')
})

await check('video_login poll: single check returns the waiting text', async () => {
  const r = await loginTool.execute({ action: 'poll', sessionId: lastSessionId }, {})
  // Nobody scanned, so this must NOT be a completed login; the exact waiting
  // wording is the provider's own (Chinese for bilibili).
  assert.doesNotMatch(r.text, /Logged in/, 'an unscanned QR cannot be done')
  assert.ok(r.text.trim().length > 0, 'poll must say something')
})

await check('video_login status: no accounts is a structured refusal, not a crash', async () => {
  await assert.rejects(
    () => loginTool.execute({ action: 'status', channel: 'bilibili' }, {}),
    /no accounts for channel bilibili/,
  )
})

await check('video_login cancel: abandons the in-flight session', async () => {
  const r = await loginTool.execute({ action: 'cancel', sessionId: lastSessionId }, {})
  assert.match(r.text, /cancelled/i)
})

// ── cleanup (script-created scratch only) ───────────────────────────────────
rmSync(process.env.DSH_HOME, { recursive: true, force: true })
rmSync(workspace, { recursive: true, force: true })

console.log(checks.join('\n'))
console.log(`\n${checks.filter((c) => c.startsWith('ok')).length}/${checks.length} passed`)
