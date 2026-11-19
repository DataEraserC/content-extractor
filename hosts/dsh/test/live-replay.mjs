// Live verification of the generic fetch layer against the real replay CDN.
// Downloads replay 1795093645690603 (the user's example), parses it, and
// writes the artifacts into a scratch workspace. Run: node test/live-replay.mjs
import { mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { extract } from '../lib/content/registry.js'

const outDir = mkdtempSync(join(tmpdir(), 'replay-live-'))
const result = await extract({
  input: { id: '1795093645690603', extractor: 'astralparty.replay' },
  outDir,
  timeoutMs: 120000,
})
console.log('kind:', result.kind)
console.log('extractor:', result.source.extractor)
for (const file of result.files) console.log('file:', file.role, file.path, file.bytes ?? '')
console.log('metadata:', JSON.stringify(result.metadata, null, 2))
// Read back the persisted map and confirm it matches the in-memory result.
const metaFile = result.files.find((f) => f.role === 'metadata')
const persisted = JSON.parse(readFileSync(metaFile.path, 'utf8'))
if (JSON.stringify(persisted.metadata) !== JSON.stringify(result.metadata)) {
  throw new Error('persisted metadata differs from the returned map')
}
console.log('read-back OK; artifacts in', outDir)