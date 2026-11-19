// Live verification of the video path: metadata probe + a real short download
// through the content layer (bilibili.video extractor). Run: node test/live-video.mjs
import { mkdtempSync, readFileSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { extract } from '../lib/content/registry.js'
import { probeInfo } from '../lib/content/generic/ytdlp.js'

const url = 'https://www.bilibili.com/video/BV1Gstz6qE95'

// 1) Metadata-only probe.
const probed = await probeInfo({ url, timeoutMs: 60000 })
console.log('probe metadata:', JSON.stringify(probed.metadata, null, 2))
if (!probed.metadata.title || !probed.metadata.author || !probed.metadata.id) {
  throw new Error('probe missed title/author/id')
}

// 2) Full fetch (download + metadata sidecar), lowest quality to keep it small.
const outDir = mkdtempSync(join(tmpdir(), 'video-live-'))
const result = await extract({
  input: { url, extractor: 'bilibili.video' },
  outDir,
  format: 'bv*+ba/b',
  timeoutMs: 300000,
})
console.log('kind:', result.kind, '| extractor:', result.source.extractor)
for (const file of result.files) console.log('file:', file.role, file.path, statSync(file.path).size)
const metaFile = result.files.find((f) => f.role === 'metadata')
const persisted = JSON.parse(readFileSync(metaFile.path, 'utf8'))
if (persisted.metadata.id !== result.metadata.id) throw new Error('persisted metadata id differs')
console.log('read-back OK; artifacts in', outDir)