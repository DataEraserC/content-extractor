// Live verification of the bilibili ARTICLE extractor against a real page.
// This is the case the old video-only path got wrong: /read/cv… is text, and
// yt-dlp has no extractor for it. Run: node test/live-article.mjs
import { mkdtempSync, readFileSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { candidates, extract } from '../lib/content/registry.js'

const url = 'https://www.bilibili.com/read/cv1/'

// 1) Candidates must offer the article extractor, not the video one.
const found = candidates({ url })
console.log('candidates:', found.map((entry) => `${entry.extractor.id}(${entry.score})`).join(', '))
if (found[0].extractor.id !== 'bilibili.article') throw new Error('article must rank first')
if (found.some((entry) => entry.extractor.id === 'bilibili.video')) {
  throw new Error('the video extractor must not be offered for an article URL')
}

// 2) Extract with the explicitly chosen id.
const outDir = mkdtempSync(join(tmpdir(), 'bili-article-'))
const result = await extract({
  input: { url, extractor: 'bilibili.article' },
  outDir,
  timeoutMs: 180000,
})
console.log('kind:', result.kind, '| extractor:', result.source.extractor)
for (const file of result.files) console.log('file:', file.role, file.path, statSync(file.path).size)
const m = result.metadata
console.log('title:', m.title)
console.log('author:', m.author, '(mid', m.author_id + ')')
console.log('cvid:', m.cvid, '| created_at:', m.created_at)
console.log('word_count:', m.word_count, '| images:', m.image_count)
console.log('stats: like', m.like_count, 'reply', m.reply_count, 'favorite', m.favorite_count)
console.log('collection:', m.collection)
console.log('text head:', String(m.text).slice(0, 80))

if (result.kind !== 'article') throw new Error(`expected article, got ${result.kind}`)
if (m.title !== '未知的光') throw new Error(`title mismatch: ${m.title}`)
if (m.author !== '健行见远渐忘') throw new Error(`author mismatch: ${m.author}`)
if (!m.text || m.text.length < 50) throw new Error('article text missing or too short')
const textFile = result.files.find((f) => f.role === 'text')
if (!textFile) throw new Error('no .md artifact')
const md = readFileSync(textFile.path, 'utf8')
if (!md.includes('# 未知的光')) throw new Error('markdown missing the title heading')
const metaFile = result.files.find((f) => f.role === 'metadata')
const persisted = JSON.parse(readFileSync(metaFile.path, 'utf8'))
if (JSON.stringify(persisted.metadata) !== JSON.stringify(m)) throw new Error('sidecar differs')
console.log('read-back OK; artifacts in', outDir)
