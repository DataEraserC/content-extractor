// Astral Party (吉星派对 / 星引擎 Party) replay extractor.
//
// ## What a "replay" is here
//
// The game's match replays are NOT video files. They are public, static
// protobuf snapshots served by the replay CDN:
//
//     CN:   https://sereplaycn.feimogames.com/prod/<replayId>
//     INTL: https://sereplayjp.feimogames.com/prod/<replayId>
//
// No credentials are needed. A replay id is 16 digits: unix-seconds (10) +
// room number (6). The two regions' ids do not overlap — an id from one
// region 404s on the other.
//
// The file body is a custom header followed by a stream of `model.Room`
// protobuf frames (one per game state change) plus recorded protocol
// packets. We parse it WITHOUT a protobuf schema dependency: a minimal
// wire-format reader walks tag/varint/length-delimited fields, which is
// enough for the metadata we surface (room, map, players, per-player final
// stats). This mirrors the approach of the community tool AstralParty_Dex
// (MIT), whose frame-location heuristic we follow: Room frames start with
// field 1 (fixed64 room id) followed by field 2 (room name), so the byte
// pattern `09 <roomid-le64> 12` locates every frame; the length prefix
// `12 <varint>` sits 1–5 bytes before the frame start.
//
// ## Result shape
//
// kind='replay'; files=[{role:'replay', path:<id>.bin}, {role:'metadata'}];
// metadata carries the open-map conventions (id, url, title, created_at)
// plus replay-specific keys (map_id, map_name, mode, round, players[]).
// Per-player stats use the game's own settlement semantics:
//   kills = kill_count + kill_pve_monster + kill_thief
//   gold  = cond.gold + cond.initGoldCount

import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'

/** Replay CDN per region. Public static files; no cookies involved. */
export const REPLAY_HOSTS = Object.freeze({
  cn: 'https://sereplaycn.feimogames.com/prod/',
  intl: 'https://sereplayjp.feimogames.com/prod/',
})

/** Fallback map names (the live table ships in the game client; these are the known ones). */
const MAP_NAMES = Object.freeze({
  81005: '龙宫游乐园（旧）', 81007: '梦想号-上层甲板', 82007: '星趴·梦想号',
  82008: '御魂庆典', 82010: '水乡古镇', 82012: '魔法学院', 82013: '龙宫游乐园',
  82014: '幽魂暗巷', 82015: '园林中庭', 82016: '异变图书馆', 82017: '森秋幻境',
  83001: '海选赛运动场', 83002: '淘汰赛运动场', 83003: '决赛大赛场',
})

/** Room.mapType → mode label. */
const MODE_NAMES = Object.freeze({ 1: '标准对战', 2: '合作挑战', 3: '限时玩法', 4: '合作挑战' })

/** ConditionalInfo field numbers → stat keys (settlement-relevant subset). */
const COND_FIELDS = Object.freeze({
  1: 'kill_count', 2: 'total_damage', 3: 'pk_damage_max', 4: 'total_die',
  5: 'total_injured', 13: 'gold', 14: 'kill_thief', 20: 'transfer_gold',
  21: 'init_gold', 23: 'kill_pve_monster', 24: 'treatment_score',
  25: 'move_point', 26: 'dice_six_count', 27: 'final_kill_boss', 28: 'self_die',
})

/**
 * Validate a replay id: 16 digits (10-digit unix seconds + 6-digit room).
 * @param {string} id - candidate id.
 * @returns {boolean}
 */
export function isReplayId(id) {
  return /^\d{16}$/.test(String(id ?? ''))
}

/**
 * Build the replay URL for an id and region.
 * @param {string} id - replay id.
 * @param {string} [region] - 'cn' (default) or 'intl'.
 * @returns {string} the CDN URL.
 */
export function replayUrl(id, region = 'cn') {
  const base = REPLAY_HOSTS[region] ?? REPLAY_HOSTS.cn
  return `${base}${id}`
}

// ── Minimal protobuf wire reader (schema-less) ──────────────────────────────

/**
 * Read one varint.
 * @param {Buffer} buf - source bytes.
 * @param {number} pos - start offset.
 * @returns {[number|null, number]} value (null when truncated) and next offset.
 */
function readVarint(buf, pos) {
  let value = 0
  let shift = 0
  while (pos < buf.length && shift < 64) {
    const byte = buf[pos]
    value |= (byte & 0x7f) << shift
    pos += 1
    if (byte < 128) return [value, pos]
    shift += 7
  }
  return [null, pos]
}

/**
 * Parse a protobuf message into { fieldNumber: [values] } without a schema.
 * Values are numbers (varint/fixed32/fixed64) or Buffers (length-delimited).
 * Tolerant: a corrupt tail stops the walk but keeps the valid prefix, which
 * is exactly what replay frames need (frames run into the next record).
 * Tags are varints like everywhere else in the wire format — field numbers
 * above 15 use a two-byte tag (e.g. field 32 → 0x80 0x02).
 * @param {Buffer} buf - message bytes.
 * @returns {Object<number, Array<number|Buffer>>}
 */
export function pbParse(buf) {
  const out = {}
  let i = 0
  const n = buf.length
  while (i < n - 1) {
    const [tag, afterTag] = readVarint(buf, i)
    if (tag === null) break
    const field = tag >>> 3
    const wire = tag & 7
    if (field === 0 || field > 5000) { i = afterTag; continue }
    if (wire === 0) {
      const [value, next] = readVarint(buf, afterTag)
      if (value === null) break
      ;(out[field] ??= []).push(value)
      i = next
    } else if (wire === 2) {
      const [len, next] = readVarint(buf, afterTag)
      if (len === null || next + len > n) { i = afterTag; continue }
      ;(out[field] ??= []).push(buf.subarray(next, next + len))
      i = next + len
    } else if (wire === 5) {
      if (afterTag + 4 > n) break
      ;(out[field] ??= []).push(buf.readUInt32LE(afterTag))
      i = afterTag + 4
    } else if (wire === 1) {
      if (afterTag + 8 > n) break
      ;(out[field] ??= []).push(Number(buf.readBigUInt64LE(afterTag)))
      i = afterTag + 8
    } else {
      i = afterTag
    }
  }
  return out
}

/** First integer value of a field, or a default. */
function pbInt(fields, key, fallback = 0) {
  const value = fields[key]?.[0]
  return typeof value === 'number' ? value : fallback
}

/** First string value of a field, or ''. */
function pbStr(fields, key) {
  const value = fields[key]?.[0]
  return Buffer.isBuffer(value) ? value.toString('utf8') : ''
}

/**
 * Locate Room frame starts: `09 <roomid fixed64 LE> 12` occurrences.
 * The room id comes from the replay id's low 6 digits when available;
 * otherwise the first plausible `09 … 12 <name>` candidate wins.
 * @param {Buffer} data - whole replay file.
 * @param {string|number} [replayId] - the replay id, when known.
 * @returns {number[]} frame start offsets.
 */
export function roomStarts(data, replayId) {
  const id = Number(replayId)
  let key = null
  if (Number.isFinite(id) && id >= 0) {
    const buf = Buffer.alloc(8)
    buf.writeBigUInt64LE(BigInt(id % 1000000))
    if (data.indexOf(Buffer.concat([Buffer.from([0x09]), buf, Buffer.from([0x12])])) >= 0) key = buf
  }
  if (key === null) {
    // Scan for 09 <8 bytes> 12 <len 1..64> <name without NUL>.
    for (let i = 0; i + 11 < data.length; i += 1) {
      if (data[i] !== 0x09 || data[i + 9] !== 0x12) continue
      const len = data[i + 10]
      if (len >= 1 && len <= 64 && i + 11 + len < data.length && !data.subarray(i + 11, i + 11 + len).includes(0)) {
        key = data.subarray(i + 1, i + 9)
        break
      }
    }
  }
  const starts = []
  if (key !== null) {
    const pattern = Buffer.concat([Buffer.from([0x09]), key, Buffer.from([0x12])])
    let at = 0
    for (;;) {
      at = data.indexOf(pattern, at)
      if (at < 0) break
      starts.push(at)
      at += 1
    }
  }
  return starts
}

/**
 * Find the `12 <varint length>` prefix that ends exactly at a frame start.
 * @param {Buffer} data - whole file.
 * @param {number} start - frame start offset.
 * @returns {number|null} frame length, or null when no prefix fits.
 */
function frameLengthAt(data, start) {
  for (const back of [3, 2, 1, 4, 5]) {
    const pos = start - back
    if (pos >= 1 && data[pos] === 0x12) {
      const [len, after] = readVarint(data, pos + 1)
      if (len && after === start) return len
    }
  }
  return null
}

/**
 * Extract one player's record from a Room frame's player submessage.
 * @param {Buffer} segment - model.Player bytes.
 * @returns {object|null} player record, or null when unparseable.
 */
function playerOf(segment) {
  if (!Buffer.isBuffer(segment)) return null
  const player = pbParse(segment)
  const uid = pbInt(player, 1)
  if (!uid) return null
  const heroSeg = player[10]?.[0]
  const hero = Buffer.isBuffer(heroSeg) ? pbParse(heroSeg) : {}
  const condSeg = hero[47]?.[0]
  const cond = Buffer.isBuffer(condSeg) ? pbParse(condSeg) : {}
  const stats = {}
  for (const [field, name] of Object.entries(COND_FIELDS)) {
    const value = cond[Number(field)]?.[0]
    if (typeof value === 'number') stats[name] = value
  }
  return {
    uid,
    nick: pbStr(player, 2),
    level: pbInt(player, 25),
    hero_id: pbInt(hero, 2),
    skin: pbInt(hero, 46),
    gold: pbInt(hero, 7),
    stats,
  }
}

/**
 * Parse a replay file into structured metadata.
 *
 * Frames are walked from the END backwards (up to 12): the final snapshot
 * may have some players' hero/cond cleared, so each player takes the most
 * recent non-empty stats — the same merge the settlement screen uses.
 *
 * @param {Buffer} data - whole replay file bytes.
 * @param {string|number} [replayId] - the replay id, when known.
 * @returns {object} { metadata, players } — metadata is the open map.
 * @throws when no Room frame can be located (not a replay file).
 */
export function parseReplay(data, replayId) {
  const starts = roomStarts(data, replayId)
  if (starts.length === 0) throw new Error('no Room frames found — this does not look like an Astral Party replay')

  // First parseable frame gives room-level facts.
  let head = null
  for (const start of starts) {
    const len = frameLengthAt(data, start)
    if (!len) continue
    const fields = pbParse(data.subarray(start, start + len))
    if (fields[9]?.length) { head = { start, fields }; break }
  }
  if (head === null) {
    // Tolerant fallback: parse from just after the room-id field.
    const fields = pbParse(data.subarray(starts[0] + 9))
    head = { start: starts[0], fields }
  }

  const headFields = head.fields
  const mapId = pbInt(headFields, 5)
  const createTime = pbInt(headFields, 7)
  const startTime = pbInt(headFields, 30)
  // mapType may only be present in later frames; take the last non-zero one.
  let mapType = pbInt(headFields, 36)
  const merged = new Map()
  let lastRound = 0
  let lastState = 0
  for (const start of starts.slice(-12).reverse()) {
    const len = frameLengthAt(data, start)
    const fields = len
      ? pbParse(data.subarray(start, start + len))
      : pbParse(data.subarray(start + 9))
    lastRound = Math.max(lastRound, pbInt(fields, 14))
    lastState = pbInt(fields, 11) || lastState
    if (!mapType) mapType = pbInt(fields, 36)
    for (const segment of fields[9] ?? []) {
      const player = playerOf(segment)
      if (player === null) continue
      const cur = merged.get(player.uid) ?? { uid: player.uid, nick: '', hero_id: 0, skin: 0, gold: 0, stats: {} }
      if (player.nick && !cur.nick) cur.nick = player.nick
      if (player.hero_id) cur.hero_id = player.hero_id
      if (player.skin) cur.skin = player.skin
      if (player.gold) cur.gold = player.gold
      if (Object.keys(player.stats).length > 0 && Object.keys(cur.stats).length === 0) cur.stats = player.stats
      merged.set(player.uid, cur)
    }
    if (merged.size > 0 && [...merged.values()].every((player) => Object.keys(player.stats).length > 0)) break
  }

  const roomName = pbStr(headFields, 2)
  const players = [...merged.values()].map((player) => {
    const stats = { ...player.stats }
    // Settlement semantics: kills include monsters and thieves; gold includes
    // starting gold. Proto3 omits zero-valued fields on the wire, so a missing
    // kill_count still means 0 kills — the derived totals are always present.
    stats.kills = (stats.kill_count ?? 0) + (stats.kill_pve_monster ?? 0) + (stats.kill_thief ?? 0)
    if (stats.gold !== undefined) stats.gold_total = (stats.gold ?? 0) + (stats.init_gold ?? 0)
    return { ...player, stats }
  })

  const metadata = {
    id: replayId === undefined ? undefined : String(replayId),
    title: `吉星派对回放 ${replayId ?? ''}`.trim(),
    room_id: pbInt(headFields, 1),
    room_name: roomName,
    map_id: mapId,
    map_name: MAP_NAMES[mapId] ?? (mapId ? `地图${mapId}` : ''),
    mode: MODE_NAMES[mapType] ?? (mapType ? `模式${mapType}` : ''),
    round: lastRound,
    state: lastState,
    created_at: createTime || undefined,
    started_at: startTime || undefined,
    frame_count: starts.length,
    players,
  }
  return { metadata, players }
}

/** The Astral Party replay extractor. The deliverable is the match file itself. */
export const astralReplay = {
  id: 'astralparty.replay',
  platform: 'astralparty',
  label: '吉星派对对局回放文件',
  kind: 'replay',
  patterns: ['feimogames.com/prod/'],
  credentials: 'none',
  metadataKeys: [
    'region', 'room_id', 'room_name', 'map_id', 'map_name', 'mode',
    'round', 'state', 'frame_count', 'started_at', 'players',
  ],

  /** Also owns the bare 16-digit id form (no URL to pattern-match). */
  ownsId(input) {
    return isReplayId(input.id)
  },

  /**
   * Refine the advisory match to a real replay CDN URL with a 16-digit id.
   * @param {{url?: string, id?: string}} input - extract input.
   * @returns {boolean}
   */
  suggests(input) {
    if (isReplayId(input.id)) return true
    const url = String(input.url ?? '')
    return /^https?:\/\/sereplay(cn|jp)\.feimogames\.com\/prod\/\d{16}/i.test(url)
  },

  /**
   * Download one replay and parse its metadata.
   * @param {{url?: string, id?: string, params?: object}} input - fetch input.
   * @param {object} opts - generic extract options.
   * @returns {Promise<object>} FetchResult.
   */
  async extract(input, { outDir, timeoutMs }) {
    let id = String(input.id ?? '')
    let region = String(input.params?.region ?? 'cn')
    const url = String(input.url ?? '')
    if (!isReplayId(id) && url) {
      const match = /sereplay(cn|jp)\.feimogames\.com\/prod\/(\d{16})/i.exec(url)
      if (match) {
        region = match[1].toLowerCase() === 'jp' ? 'intl' : 'cn'
        id = match[2]
      }
    }
    if (!isReplayId(id)) throw new Error(`a replay id must be 16 digits (unix seconds + room number): ${id.slice(0, 40)}`)
    if (region !== 'cn' && region !== 'intl') throw new Error(`region must be cn or intl: ${region}`)

    const source = replayUrl(id, region)
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), timeoutMs)
    let response
    try {
      response = await fetch(source, { signal: controller.signal, headers: { 'user-agent': 'Mozilla/5.0' } })
    } catch (error) {
      throw new Error(`replay download failed (${source}): ${error.message}`)
    } finally {
      clearTimeout(timer)
    }
    if (response.status === 404) {
      throw new Error(`replay ${id} not found on the ${region} server (404) — ids are region-specific; try params.region='${region === 'cn' ? 'intl' : 'cn'}'`)
    }
    if (!response.ok) throw new Error(`replay download failed (${source}): HTTP ${response.status}`)
    const data = Buffer.from(await response.arrayBuffer())
    if (data.length < 2000) throw new Error(`replay ${id} response is too small (${data.length} bytes) — not a replay file`)

    const { metadata } = parseReplay(data, id)
    metadata.url = source
    metadata.region = region
    const binPath = join(outDir, `${id}.bin`)
    const metaPath = join(outDir, `${id}.meta.json`)
    await writeFile(binPath, data)
    const payload = { kind: 'replay', source: { extractor: 'astralparty.replay', id, url: source }, metadata }
    await writeFile(metaPath, JSON.stringify(payload, null, 2), 'utf8')
    return {
      kind: 'replay',
      source: { id, url: source },
      files: [
        { path: binPath, role: 'replay', bytes: data.length },
        { path: metaPath, role: 'metadata' },
      ],
      metadata,
      extra: {},
    }
  },
}
