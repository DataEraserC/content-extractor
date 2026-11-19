// Live verification of the 完美世界电竞 (news.wmpvp.com) extractor against the
// real API + CDN. Downloads the user's example post 392049040 (a ~75 MB
// highlight video), checks metadata, and reads the sidecar back.
// Run: node test/live-wmpvp.mjs
import { mkdtempSync, readFileSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { extract } from '../lib/content/registry.js'

const outDir = mkdtempSync(join(tmpdir(), 'wmpvp-live-'))
const result = await extract({
  input: { url: 'https://news.wmpvp.com/community-detail.html?id=392049040', extractor: 'wmpvp.video' },
  outDir,
  timeoutMs: 600000,
})
console.log('kind:', result.kind, '| extractor:', result.source.extractor)
for (const file of result.files) console.log('file:', file.role, file.path, statSync(file.path).size)
console.log('metadata:', JSON.stringify(result.metadata, null, 2))

if (result.kind !== 'video') throw new Error(`expected a video post, got ${result.kind}`)
if (result.metadata.title !== '来自Le_Noir的击杀集锦视频') throw new Error('title mismatch')
if (result.metadata.author !== 'Le_Noir') throw new Error('author mismatch')
const media = result.files.find((f) => f.role === 'media')
if (!media || statSync(media.path).size < 1024 * 1024) throw new Error('media missing or suspiciously small')
const metaFile = result.files.find((f) => f.role === 'metadata')
const persisted = JSON.parse(readFileSync(metaFile.path, 'utf8'))
if (JSON.stringify(persisted.metadata) !== JSON.stringify(result.metadata)) {
  throw new Error('persisted metadata differs from the returned map')
}
console.log('read-back OK; artifacts in', outDir)