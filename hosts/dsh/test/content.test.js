// Unit tests for the content-extractorion layer: candidate listing (plural,
// never selecting), explicit selection, the open metadata map, the Astral
// Party replay parser (against a synthetic frame in the real CDN's wire
// format), bilibili article parsing, and direct-file naming.
// No network — run with `npm test`.

import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { EXTRACTOR_IDS, candidates, extract, extractorOf, listExtractors } from '../lib/content/registry.js'
import { isReplayId, parseReplay, pbParse, replayUrl, roomStarts } from '../lib/content/astralparty/replay.js'
import { fileNameFor } from '../lib/content/generic/file.js'
import { bestFormat, postIdOf, projectPost } from '../lib/content/wmpvp/post.js'
import { buildProbeArgs, projectInfo } from '../lib/content/generic/ytdlp.js'
import { cvidOf, exactIdOf, initialStateOf, projectArticle, renderContent } from '../lib/content/bilibili/article.js'
import { cookieHeaderFor, parseNetscape, withCookie } from '../lib/content/cookies.js'
import { CREDENTIAL_USE, usesCredentials } from '../lib/content/contract.js'
import { renderMetadata } from '../lib/index.js'
import { initialStateOf as initialStateOfXhs, noteIdOf as noteIdOfXhs, projectNote as projectNoteXhs } from '../lib/content/xiaohongshu/note.js'

const checks = []
/** @param {string} name - check label. @param {() => Promise<void>|void} body - assertions. */
async function check(name, body) {
  try {
    await body()
    checks.push(`ok   ${name}`)
  } catch (error) {
    checks.push(`FAIL ${name}: ${error.message}`)
    process.exitCode = 1
  }
}

// ── candidates are plural; selection is explicit ─────────────────────────────

await check('candidates: an article URL offers the article extractor, not the video one', () => {
  const found = candidates({ url: 'https://www.bilibili.com/read/cv1/' })
  assert.equal(found[0].extractor.id, 'bilibili.article')
  assert.equal(found[0].extractor.kind, 'article')
  // The video extractor must NOT be offered for an article path.
  assert.ok(!found.some((entry) => entry.extractor.id === 'bilibili.video'))
})

await check('candidates: a video URL offers the video extractor first', () => {
  const found = candidates({ url: 'https://www.bilibili.com/video/BV1xx411c7mD' })
  assert.equal(found[0].extractor.id, 'bilibili.video')
  assert.ok(!found.some((entry) => entry.extractor.id === 'bilibili.article'))
})

await check('candidates: opus is its own kind, distinct from article and video', () => {
  const found = candidates({ url: 'https://www.bilibili.com/opus/123456' })
  assert.equal(found[0].extractor.id, 'bilibili.opus')
  assert.equal(found[0].extractor.kind, 'post')
})

await check('candidates: an ambiguous 完美世界电竞 post lists BOTH video and note', () => {
  const found = candidates({ url: 'https://news.wmpvp.com/community-detail.html?id=392049040' })
  const ids = found.map((entry) => entry.extractor.id)
  assert.ok(ids.includes('wmpvp.video') && ids.includes('wmpvp.note'), `got ${ids.join(',')}`)
  // Same specificity: the URL genuinely cannot disambiguate, so neither wins.
  const video = found.find((entry) => entry.extractor.id === 'wmpvp.video')
  const note = found.find((entry) => entry.extractor.id === 'wmpvp.note')
  assert.equal(video.score, note.score)
})

await check('candidates: site extractors outrank the bare-http catch-alls', () => {
  const found = candidates({ url: 'https://www.bilibili.com/read/cv1/' })
  const site = found.find((entry) => entry.extractor.id === 'bilibili.article')
  const generic = found.find((entry) => entry.extractor.id === 'generic.ytdlp')
  assert.ok(site.score > generic.score, 'specificity must rank the site extractor first')
})

await check('candidates: a replay id and CDN url both resolve; a direct file is file-only', () => {
  assert.equal(candidates({ id: '1795093645690603' })[0].extractor.id, 'astralparty.replay')
  assert.equal(
    candidates({ url: 'https://sereplaycn.feimogames.com/prod/1795093645690603' })[0].extractor.id,
    'astralparty.replay',
  )
  const file = candidates({ url: 'https://example.com/clip.mp4' })
  assert.deepEqual(file.map((entry) => entry.extractor.id), ['generic.file'])
})

await check('candidates: nothing matches → empty list, never a guess', () => {
  assert.deepEqual(candidates({ url: 'file:///etc/passwd' }), [])
  assert.deepEqual(candidates({}), [])
})

await check('extract: refuses to run without an explicit extractor, naming candidates', async () => {
  await assert.rejects(
    () => extract({ input: { url: 'https://www.bilibili.com/read/cv1/' }, outDir: '/tmp', timeoutMs: 1000 }),
    (error) => {
      assert.match(error.message, /explicit "extractor" id/)
      assert.match(error.message, /bilibili\.article/, 'the error must name the candidates')
      return true
    },
  )
})

await check('extractorOf: explicit selection resolves; unknown id lists the known ones', () => {
  assert.equal(extractorOf('bilibili.article').kind, 'article')
  assert.throws(() => extractorOf('nope'), /unknown extractor/)
  assert.throws(() => extractorOf('nope'), /bilibili\.article/)
})

await check('listExtractors: every extractor declares platform, kind and patterns', () => {
  const rows = listExtractors()
  assert.deepEqual(rows.map((row) => row.id), [...EXTRACTOR_IDS])
  for (const row of rows) {
    assert.ok(row.label.length > 0)
    assert.ok(row.platform.length > 0)
    assert.ok(row.kind.length > 0)
    assert.ok(Array.isArray(row.patterns))
    assert.ok(row.id.startsWith(`${row.platform}.`), 'id must be platform-qualified')
  }
})

// ── replay id / url ─────────────────────────────────────────────────────────

await check('isReplayId: 16 digits only', () => {
  assert.ok(isReplayId('1795093645690603'))
  assert.ok(!isReplayId('179509364569060'))   // 15
  assert.ok(!isReplayId('17950936456906030')) // 17
  assert.ok(!isReplayId('179509364569060a'))
  assert.ok(!isReplayId(''))
})

await check('replayUrl: region hosts, cn default', () => {
  assert.equal(replayUrl('1795093645690603'), 'https://sereplaycn.feimogames.com/prod/1795093645690603')
  assert.equal(replayUrl('1795093645690603', 'intl'), 'https://sereplayjp.feimogames.com/prod/1795093645690603')
})

// ── protobuf wire reader ────────────────────────────────────────────────────

/** Encode a varint. @param {number} value @returns {Buffer} */
function varint(value) {
  const out = []
  let v = value
  while (v >= 128) { out.push((v & 0x7f) | 0x80); v >>>= 7 }
  out.push(v)
  return Buffer.from(out)
}

/** Encode one protobuf field. @param {number} fieldNum @param {number} wire @param {Buffer|number} value @returns {Buffer} */
function field(fieldNum, wire, value) {
  const tag = varint((fieldNum << 3) | wire)
  if (wire === 0) return Buffer.concat([tag, varint(value)])
  if (wire === 2) return Buffer.concat([tag, varint(value.length), value])
  if (wire === 1) { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(value)); return Buffer.concat([tag, b]) }
  if (wire === 5) { const b = Buffer.alloc(4); b.writeUInt32LE(value); return Buffer.concat([tag, b]) }
  throw new Error('bad wire')
}

await check('pbParse: varint, string, fixed32/64, nested tolerance', () => {
  const buf = Buffer.concat([
    field(1, 0, 300),                      // varint > 127 (multi-byte)
    field(2, 2, Buffer.from('match')),     // string
    field(5, 5, 82017),                    // fixed32
    field(9, 1, 690603),                   // fixed64
  ])
  const parsed = pbParse(buf)
  assert.equal(parsed[1][0], 300)
  assert.equal(parsed[2][0].toString(), 'match')
  assert.equal(parsed[5][0], 82017)
  assert.equal(parsed[9][0], 690603)
  // A corrupt tail must not lose the valid prefix.
  const torn = Buffer.concat([buf, Buffer.from([0xff, 0xff, 0xff])])
  assert.equal(pbParse(torn)[1][0], 300)
})

// ── replay parsing against a synthetic file ─────────────────────────────────

/**
 * Build one Room frame: id(1,fixed64) name(2) map_id(5,fixed32) create_time(7)
 * players(9) state(11) round(14) start_time(30) mapType(36), each player
 * carrying hero(10) with hero_id(2) gold(7) skin(46) cond(47).
 */
function roomFrame({ roomId, mapId, createTime, round, state, mapType, players }) {
  const parts = [
    field(1, 1, roomId),
    field(2, 2, Buffer.from('match')),
    field(5, 5, mapId),
    field(7, 0, createTime),
    field(11, 0, state),
    field(14, 0, round),
    field(36, 0, mapType),
  ]
  for (const player of players) {
    const cond = Buffer.concat(Object.entries(player.stats ?? {}).map(([k, v]) => field(Number(k), 0, v)))
    const hero = Buffer.concat([
      field(2, 0, player.heroId),
      field(7, 0, player.gold ?? 0),
      field(46, 0, player.skin ?? 0),
      field(47, 2, cond),
    ])
    const seg = Buffer.concat([
      field(1, 0, player.uid),
      field(2, 2, Buffer.from(player.nick)),
      field(25, 0, player.level ?? 1),
      field(10, 2, hero),
    ])
    parts.push(field(9, 2, seg))
  }
  const body = Buffer.concat(parts)
  // Length prefix 12 <varint len> immediately before the frame.
  return Buffer.concat([Buffer.from([0x12]), varint(body.length), body])
}

await check('parseReplay: synthetic two-frame file yields room facts + merged final stats', () => {
  const rid = 1795093645690603
  const roomId = rid % 1000000
  const early = roomFrame({
    roomId, mapId: 82017, createTime: 1795093616, round: 1, state: 10, mapType: 4,
    players: [
      { uid: 111, nick: '甲', heroId: 124, level: 30, stats: { 1: 0, 2: 10, 13: 5, 21: 12 } },
      { uid: 222, nick: '乙', heroId: 108, level: 20, stats: {} },
    ],
  })
  const late = roomFrame({
    roomId, mapId: 82017, createTime: 1795093616, round: 7, state: 30, mapType: 4,
    players: [
      // Final frame cleared player 111's cond — the merge must keep the early stats.
      { uid: 111, nick: '甲', heroId: 124, level: 30, stats: {} },
      { uid: 222, nick: '乙', heroId: 108, level: 20, stats: { 1: 1, 2: 104, 13: 84, 21: 12, 23: 4, 27: 1 } },
    ],
  })
  const data = Buffer.concat([Buffer.from([0x04, 0x10, 0, 0, 0, 0x57]), early, late])
  const starts = roomStarts(data, rid)
  assert.equal(starts.length, 2)
  const { metadata } = parseReplay(data, rid)
  assert.equal(metadata.id, String(rid))
  assert.equal(metadata.room_id, roomId)
  assert.equal(metadata.map_id, 82017)
  assert.equal(metadata.map_name, '森秋幻境')
  assert.equal(metadata.mode, '合作挑战')
  assert.equal(metadata.round, 7)
  assert.equal(metadata.created_at, 1795093616)
  assert.equal(metadata.players.length, 2)
  const p111 = metadata.players.find((p) => p.uid === 111)
  const p222 = metadata.players.find((p) => p.uid === 222)
  assert.equal(p111.nick, '甲')
  assert.equal(p111.stats.total_damage, 10)          // kept from the early frame
  assert.equal(p111.stats.gold_total, 5 + 12)        // settlement gold semantics
  assert.equal(p222.stats.kills, 1 + 4)              // pvp + pve kills
  assert.equal(p222.stats.final_kill_boss, 1)
})

await check('parseReplay: garbage input is refused, not silently empty', () => {
  assert.throws(() => parseReplay(Buffer.from('this is not a replay at all'), 1795093645690603), /no Room frames/)
})

// ── ytdlp probe projection ──────────────────────────────────────────────────

await check('buildProbeArgs: skip-download + dump-single-json, URL behind --', () => {
  const args = buildProbeArgs({ url: 'https://www.bilibili.com/video/BV1xx' })
  assert.ok(args.includes('--skip-download'))
  assert.ok(args.includes('--dump-single-json'))
  assert.deepEqual(args.slice(-2), ['--', 'https://www.bilibili.com/video/BV1xx'])
  const withCookies = buildProbeArgs({ url: 'https://x.test/v', cookiesPath: '/c.txt' })
  assert.ok(withCookies.includes('--cookies') && withCookies.includes('/c.txt'))
})

await check('projectInfo: common keys mapped, unknown fields ride in extra.info', () => {
  const info = {
    id: 'BV1xx', title: '一个视频', uploader: '某UP主', uploader_id: '42',
    webpage_url: 'https://www.bilibili.com/video/BV1xx', description: '简介',
    duration: 61, thumbnail: 'https://i0.hdslb.com/x.jpg', timestamp: 1700000000,
    view_count: 1000, like_count: 50, tags: ['a', 'b'], extractor_key: 'BiliBili',
    some_future_field: { nested: true },
  }
  const { metadata, extra } = projectInfo(info)
  assert.equal(metadata.title, '一个视频')
  assert.equal(metadata.author, '某UP主')
  assert.equal(metadata.author_id, '42')
  assert.equal(metadata.id, 'BV1xx')
  assert.equal(metadata.url, info.webpage_url)
  assert.equal(metadata.description, '简介')
  assert.equal(metadata.duration, 61)
  assert.equal(metadata.created_at, 1700000000)
  assert.deepEqual(metadata.tags, ['a', 'b'])
  assert.equal(extra.info.some_future_field.nested, true)
  // Empty values are omitted, not printed as blank lines.
  const sparse = projectInfo({ id: 'x', title: '', description: null })
  assert.equal(sparse.metadata.title, undefined)
  assert.equal(sparse.metadata.description, undefined)
})

// ── 完美世界电竞 community post projection ───────────────────────────────────────────

await check('postIdOf: detail/pcDetail URLs and bare ids; foreign hosts refused', () => {
  assert.equal(postIdOf({ url: 'https://news.wmpvp.com/community-detail.html?id=392049040' }), '392049040')
  assert.equal(postIdOf({ url: 'https://news.wmpvp.com/community-pcDetail.html?id=123&x=1' }), '123')
  assert.equal(postIdOf({ id: '392049040' }), '392049040')
  assert.equal(postIdOf({ url: 'https://evil.com/community-detail.html?id=1' }), null)
  assert.equal(postIdOf({ url: 'https://news.wmpvp.com/other.html' }), null)
  assert.equal(postIdOf({}), null)
})

await check('bestFormat: highest resolution wins, size breaks ties, deleted skipped', () => {
  const list = [
    { playURL: 'u1', width: 640, height: 360, size: 10, status: 'Normal' },
    { playURL: 'u2', width: 1280, height: 720, size: 20, status: 'Normal' },
    { playURL: 'u3', width: 1280, height: 720, size: 30, status: 'Normal' },
    { playURL: 'u4', width: 1920, height: 1080, size: 5, status: 'Deleted' },
  ]
  assert.equal(bestFormat(list).playURL, 'u3')
  assert.equal(bestFormat([]), null)
  assert.equal(bestFormat(undefined), null)
})

await check('projectPost: title/author/id/url/description + PWP keys from the real API shape', () => {
  const post = {
    id: 392049040, userId: 78780121, steamId: '76561198440121747',
    title: '来自Le_Noir的击杀集锦视频', content: null, summary: '来自Le_Noir的击杀集锦视频',
    vid: 'c0d3baefdacf71f1803ee7f6d4480102',
    videoInfo: {
      videoBase: { coverURL: 'https://highlights.wmpvp.com/image/x.jpg', duration: '148.154' },
      playInfoList: [{ width: 1280, height: 720, size: 74773664, playURL: 'https://highlights.wmpvp.com/sv/x.mp4', definition: 'OD', format: 'mp4', status: 'Normal' }],
    },
    readTotalCount: 14, likeCountTotal: 0, replyCount: 0,
    gmtCreate: 1794072215000,
    postUrl: 'https://news.wmpvp.com/community-detail.html?id=392049040',
    communityUserItem: { nickname: 'Le_Noir', avatar: 'https://cdn.wmpvp.com/a.jpg' },
    postTagDTOList: [{ name: 'CSGO' }, { name: '美女主播（DOTA2）' }],
  }
  const metadata = projectPost(post)
  assert.equal(metadata.title, '来自Le_Noir的击杀集锦视频')
  assert.equal(metadata.author, 'Le_Noir')
  assert.equal(metadata.author_id, '78780121')
  assert.equal(metadata.id, '392049040')
  assert.equal(metadata.url, post.postUrl)
  assert.equal(metadata.description, post.summary) // content null → summary fallback
  assert.equal(metadata.duration, 148.154)
  assert.equal(metadata.created_at, 1794072215) // ms → s
  assert.equal(metadata.thumbnail, post.videoInfo.videoBase.coverURL)
  assert.deepEqual(metadata.tags, ['CSGO', '美女主播（DOTA2）'])
  assert.equal(metadata.read_count, 14)
  assert.equal(metadata.play_formats.length, 1)
  assert.equal(metadata.play_formats[0].url, 'https://highlights.wmpvp.com/sv/x.mp4')
})

// ── open-map rendering ──────────────────────────────────────────────────────

await check('renderMetadata: known keys labeled, unknown keys pass through verbatim', () => {
  const lines = renderMetadata({
    metadata: { title: 'T', author: 'A', brand_new_key: 'surprise', players: [{ uid: 1 }] },
  })
  assert.ok(lines.includes('Title: T'))
  assert.ok(lines.includes('Author: A'))
  assert.ok(lines.includes('brand_new_key: surprise'))
  assert.ok(lines.some((line) => line.startsWith('players: [{"uid":1}]')))
})

// ── http-file naming ────────────────────────────────────────────────────────

await check('fileNameFor: path segment kept, query stripped, content-type fallback', () => {
  assert.equal(fileNameFor('https://x.test/a/b%20c.png?sig=1', 'image/png'), 'b c.png')
  assert.equal(fileNameFor('https://x.test/download', 'application/zip'), 'download.zip')
  assert.equal(fileNameFor('https://x.test/', 'text/html'), 'download.bin')
  assert.equal(fileNameFor('https://x.test/..', ''), 'download.bin')
  assert.equal(fileNameFor('https://x.test/a/b/c/d.mp4', ''), 'd.mp4')
})


// ── bilibili article / opus parsing (real page-state shape) ──────────────────

/** A minimal but faithful __INITIAL_STATE__ for an article page. */
function articleState() {
  return {
    id: 1,
    detail: {
      basic: { title: '未知的光 - 哔哩哔哩', rid_str: '1', comment_id_str: '1' },
      modules: [
        { module_type: 'MODULE_TYPE_TITLE', module_title: { text: '未知的光' } },
        { module_type: 'MODULE_TYPE_AUTHOR', module_author: { name: '健行见远渐忘', mid: 91221505, pub_ts: '1507815144', face: 'https://i0.hdslb.com/a.jpg', pub_location_text: '发布于北京' } },
        { module_type: 'MODULE_TYPE_TOP', module_top: { display: { album: { pics: [{ url: 'https://i0.hdslb.com/top.jpg', width: 929, height: 273 }] } } } },
        { module_type: 'MODULE_TYPE_CONTENT', module_content: { paragraphs: [
          { para_type: 1, text: { nodes: [{ type: 'TEXT_NODE_TYPE_WORD', word: { words: '天空像是倾倒出的墨水。' } }] } },
          { para_type: 2, pic: { pics: [{ url: 'https://i0.hdslb.com/inline.jpg', width: 500, height: 368 }] } },
          { para_type: 1, text: { nodes: [{ word: { words: '第二段。' } }] } },
        ] } },
        { module_type: 'MODULE_TYPE_COLLECTION', module_collection: { name: '静夜思' } },
        { module_type: 'MODULE_TYPE_STAT', module_stat: { like: { count: 32904 }, comment: { count: 14034 }, favorite: { count: 17567 }, coin: { count: 2688 }, forward: { count: 176 } } },
        { module_type: 'MODULE_TYPE_COPYRIGHT', module_copyright: { right_text: 'cv1' } },
        { module_type: 'MODULE_TYPE_BOTTOM', module_bottom: { share_info: { summary: '天空像是倾倒出的墨水，黑得静谧而深邃。' } } },
      ],
    },
  }
}

await check('initialStateOf: brace-matches a blob containing semicolons and nested objects', () => {
  const state = articleState()
  const html = `<html><script>window.__INITIAL_STATE__=${JSON.stringify(state)};(function(){var x="a;b"})()</script></html>`
  const parsed = initialStateOf(html)
  assert.equal(parsed.id, 1)
  assert.equal(parsed.detail.modules.length, 8)
  assert.throws(() => initialStateOf('<html>nothing</html>'), /no __INITIAL_STATE__/)
  assert.throws(() => initialStateOf('window.__INITIAL_STATE__={"a":1'), /truncated/)
})

await check('renderContent: text paragraphs, inline pictures and headings all survive', () => {
  const content = { module_content: { paragraphs: [
    { heading: { text: '小标题' } },
    { para_type: 1, text: { nodes: [{ word: { words: '正文一' } }, { word: { words: '续' } }] } },
    { para_type: 2, pic: { pics: [{ url: 'https://x/1.jpg' }, { url: 'https://x/2.png' }] } },
    { blockquote: { text: { nodes: [{ word: { words: '引用' } }] } } },
    { code: { text: 'let a = 1' } },
    { line: true },
    { list: { items: [{ text: { nodes: [{ word: { words: '项一' } }] } }] } },
  ] } }
  const { text, images } = renderContent(content)
  assert.match(text, /## 小标题/)
  assert.match(text, /正文一续/)
  assert.match(text, /> 引用/)
  assert.match(text, /```\nlet a = 1\n```/)
  assert.match(text, /---/)
  assert.match(text, /- 项一/)
  assert.deepEqual(images.map((image) => image.url), ['https://x/1.jpg', 'https://x/2.png'])
})

await check('projectArticle: article metadata carries text, images, author and stats', () => {
  const { metadata, images } = projectArticle(articleState(), 'https://www.bilibili.com/read/cv1/', 'article')
  assert.equal(metadata.title, '未知的光')
  assert.equal(metadata.author, '健行见远渐忘')
  assert.equal(metadata.author_id, '91221505')
  assert.equal(metadata.cvid, '1')
  assert.equal(metadata.id, '1')
  assert.equal(metadata.created_at, 1507815144)
  assert.equal(metadata.like_count, 32904)
  assert.equal(metadata.reply_count, 14034)
  assert.equal(metadata.collection, '静夜思')
  assert.equal(metadata.description, '天空像是倾倒出的墨水，黑得静谧而深邃。')
  assert.match(metadata.text, /天空像是倾倒出的墨水。/)
  assert.match(metadata.text, /第二段。/)
  assert.equal(images[0].url, 'https://i0.hdslb.com/top.jpg')
  assert.equal(metadata.image_count, 2)
  assert.equal(metadata.word_count, metadata.text.length)
})

await check('cvidOf: the URL cv number wins over the internal snowflake id', () => {
  const state = articleState()          // state.id is 1 here; make it a snowflake
  state.id = 38554821905721204
  assert.equal(cvidOf(state, 'https://www.bilibili.com/read/cv1/'), '1')
  // No URL number → fall back to the site's own copyright label.
  assert.equal(cvidOf(state, 'https://www.bilibili.com/read/mobile?id=1'), '1')
  // Neither source → empty, never the snowflake.
  const bare = { detail: { modules: [] } }
  assert.equal(cvidOf(bare, 'https://www.bilibili.com/read/'), '')
  // The snowflake exceeds Number.MAX_SAFE_INTEGER, so it must come from the
  // raw-text scan, not from JSON.parse.
  const html = `window.__INITIAL_STATE__={"id":38554821905721204,"detail":{"modules":[]}};`
  assert.equal(exactIdOf(html), '38554821905721204')
  const projected = projectArticle(state, 'https://www.bilibili.com/read/cv1/', 'article', exactIdOf(html))
  assert.equal(projected.metadata.cvid, '1')
  assert.equal(projected.metadata.opus_id, '38554821905721204')
})

await check('projectArticle: opus kind uses opus_id, not cvid', () => {
  const { metadata } = projectArticle(articleState(), 'https://www.bilibili.com/opus/1', 'post')
  assert.equal(metadata.opus_id, '1')
  assert.equal(metadata.cvid, undefined)
})

await check('projectArticle: a page without detail.modules is refused, not silently empty', () => {
  assert.throws(() => projectArticle({ id: 1 }, 'https://x', 'article'), /no detail\.modules/)
})


// ── credentials reach every extractor that benefits ─────────────────────────

await check('usesCredentials: enhances counts, not just required', () => {
  // The regression this guards: a boolean "needs login" made enhances look
  // credential-free, so a stored bilibili jar was dropped and video stayed 360p.
  assert.ok(usesCredentials('required'))
  assert.ok(usesCredentials('enhances'))
  assert.ok(!usesCredentials('none'))
})

await check('every extractor declares a valid credentials value', () => {
  for (const row of listExtractors()) {
    assert.ok(CREDENTIAL_USE.includes(row.credentials), `${row.id}: ${row.credentials}`)
  }
  // The public replay CDN must NOT receive cookies.
  assert.equal(extractorOf('astralparty.replay').credentials, 'none')
  // Everything that can benefit must be wired to use them.
  for (const id of ['bilibili.video', 'bilibili.article', 'bilibili.opus', 'wmpvp.video', 'wmpvp.note', 'generic.file', 'generic.ytdlp']) {
    assert.equal(extractorOf(id).credentials, 'enhances', id)
  }
})

await check('extract: an enhances extractor really rotates stored accounts', async () => {
  // Drive the REAL registry path. generic.file is `enhances`; point it at a
  // host that refuses instantly so each attempt fails fast, and count how many
  // times the credential layer is asked to materialize a jar. Before the fix
  // this was zero (one anonymous attempt) because enhances looked login-free.
  const asked = []
  await assert.rejects(() => extract({
    input: { url: 'http://127.0.0.1:1/x.zip', extractor: 'generic.file' },
    outDir: mkdtempSync(join(tmpdir(), 'cred-rot-')),
    accounts: [{ id: 'acct-one' }, { id: 'acct-two' }],
    materialize: async (id) => { asked.push(id); return '' },
    timeoutMs: 8000,
  }))
  assert.deepEqual(asked, ['acct-one', 'acct-two'], 'both accounts must be tried in order')
})

await check('extract: a none extractor never asks for credentials', async () => {
  const asked = []
  await assert.rejects(() => extract({
    input: { id: '0000000000000000', extractor: 'astralparty.replay' },
    outDir: mkdtempSync(join(tmpdir(), 'cred-none-')),
    accounts: [{ id: 'acct-one' }],
    materialize: async (id) => { asked.push(id); return '' },
    timeoutMs: 8000,
  }))
  assert.deepEqual(asked, [], 'a public CDN must not receive cookies')
})

// ── Netscape jar → Cookie header ────────────────────────────────────────────

const JAR = [
  '# Netscape HTTP Cookie File',
  '.bilibili.com\tTRUE\t/\tFALSE\t0\tSESSDATA\tsecret-value',
  '.bilibili.com\tTRUE\t/\tTRUE\t0\tbili_jct\tcsrf',
  'passport.bilibili.com\tFALSE\t/\tFALSE\t0\tDedeUserID\t42',
  '.other.com\tTRUE\t/\tFALSE\t0\tOTHER\tnot-for-bilibili',
  '.bilibili.com\tTRUE\t/\tFALSE\t1\tEXPIRED\tgone',
].join('\n')

await check('parseNetscape: skips comments/short rows, keeps tab-bearing values', () => {
  const rows = parseNetscape(JAR)
  assert.equal(rows.length, 5)
  assert.equal(rows[0].name, 'SESSDATA')
  assert.equal(rows[1].secure, true)
})

await check('cookieHeaderFor: domain-scoped, no cross-site leakage', () => {
  const header = cookieHeaderFor(JAR, 'https://www.bilibili.com/video/BV1')
  assert.match(header, /SESSDATA=secret-value/)
  assert.match(header, /bili_jct=csrf/)
  // Exact-host row must not apply to www.bilibili.com.
  assert.ok(!header.includes('DedeUserID'), 'exact-host cookie leaked to a subdomain')
  // Another site's cookie must never ride along.
  assert.ok(!header.includes('OTHER'), 'cross-site cookie leaked')
  // Expired row dropped.
  assert.ok(!header.includes('EXPIRED'))
})

await check('cookieHeaderFor: secure rows only over https, exact host matches itself', () => {
  assert.ok(!cookieHeaderFor(JAR, 'http://www.bilibili.com/x').includes('bili_jct'))
  assert.match(cookieHeaderFor(JAR, 'https://passport.bilibili.com/login'), /DedeUserID=42/)
})

await check('cookieHeaderFor: path scope respected; bad url yields empty', () => {
  const scoped = '.x.com\tTRUE\t/private\tFALSE\t0\tA\t1'
  assert.match(cookieHeaderFor(scoped, 'https://x.com/private/a'), /A=1/)
  assert.equal(cookieHeaderFor(scoped, 'https://x.com/public'), '')
  assert.equal(cookieHeaderFor(JAR, 'not a url'), '')
})

await check('withCookie: no empty header when anonymous', () => {
  assert.deepEqual(withCookie({ a: '1' }, ''), { a: '1' })
  assert.deepEqual(withCookie({ a: '1' }, 'k=v'), { a: '1', cookie: 'k=v' })
})

// ── xiaohongshu: two tied candidates, DOM parsing, open metadata ────────────

await check('xiaohongshu: a note URL offers both note and video, tied and explicit', () => {
  const found = candidates({ url: 'https://www.xiaohongshu.com/explore/69821980000000000e03c95f' })
  const xhs = found.filter((entry) => entry.extractor.id.startsWith('xiaohongshu.'))
  assert.deepEqual(xhs.map((entry) => entry.extractor.id), ['xiaohongshu.note', 'xiaohongshu.video'])
  assert.equal(xhs[0].score, xhs[1].score)
  assert.equal(xhs[0].extractor.kind, 'post')
  assert.equal(xhs[1].extractor.kind, 'video')
  // Neither candidate outranks the other — the caller must choose.
  assert.ok(!found.some((entry) => entry.extractor.id === 'bilibili.video'))
})

await check('xiaohongshu: discovery/item and short-link host are covered', () => {
  assert.ok(candidates({ url: 'https://www.xiaohongshu.com/discovery/item/69821980000000000e03c95f' })
    .some((entry) => entry.extractor.id === 'xiaohongshu.note'))
  assert.ok(EXTRACTOR_IDS.includes('xiaohongshu.video'))
})

await check('xiaohongshu: both ids resolve, a typo is refused with known ids listed', () => {
  assert.equal(extractorOf('xiaohongshu.note').id, 'xiaohongshu.note')
  assert.equal(extractorOf('xiaohongshu.video').id, 'xiaohongshu.video')
  try {
    extractorOf('xiaohongshu.typo')
    assert.fail('unknown id must throw')
  } catch (error) {
    assert.match(error.message, /xiaohongshu\.note/)
    assert.match(error.message, /xiaohongshu\.video/)
  }
})

await check('xiaohongshu: __INITIAL_STATE__ handles undefined, nested braces and semicolons', () => {
  const html = '<script>window.__INITIAL_STATE__={"note":{"a":"x;y","nested":{"k":1},"gone":undefined}}</script>'
  const state = initialStateOfXhs(html)
  assert.equal(state.note.a, 'x;y')
  assert.equal(state.note.nested.k, 1)
  assert.equal(state.note.gone, null)
})

await check('xiaohongshu: note id extracted from long URL', () => {
  assert.equal(noteIdOfXhs('https://www.xiaohongshu.com/explore/69821980000000000e03c95f?xsec_token=abc'),
    '69821980000000000e03c95f')
})

await check('xiaohongshu: projectNote fills the open map without cookie values', () => {
  const note = {
    noteId: '69821980000000000e03c95f',
    title: '标题', desc: '正文', time: 1700000000000,
    user: { nickname: '作者', userId: 'u1' },
    interactInfo: { likedCount: '12', collectedCount: '3' },
    imageList: [{ urlDefault: 'https://sns-webpic-qc.xhscdn.com/a.jpg' }],
    tagList: [{ name: '美食' }],
  }
  const { metadata, images } = projectNoteXhs(
    note,
    'https://www.xiaohongshu.com/explore/69821980000000000e03c95f',
    '69821980000000000e03c95f',
    { resolvedUrl: 'https://www.xiaohongshu.com/explore/69821980000000000e03c95f?xsec_token=tok' },
  )
  assert.equal(metadata.title, '标题')
  assert.equal(metadata.author, '作者')
  assert.equal(metadata.note_id, '69821980000000000e03c95f')
  assert.equal(metadata.xsec_token, 'tok')
  assert.equal(metadata.image_count, 1)
  assert.deepEqual(metadata.tags, ['美食'])
  assert.equal(images.length, 1)
  assert.ok(!JSON.stringify(metadata).includes('SESSDATA'))
})

console.log(checks.join('\n'))
console.log(`\n${checks.filter((c) => c.startsWith('ok')).length}/${checks.length} passed`)
