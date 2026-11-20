# 小红书（Xiaohongshu）extractor 实现方案

> Draft — 面向 `content-extractor`（本仓库，`github:DataEraserC/content-extractor`）。
> 目标是新增小红书笔记抓取，完全复用现有的「显式选择 + 开放 metadata map + 账户池」契约，
> 不引入新的抽象层，也不依赖签名算法逆向。

> **实施修正（2026-11-20）**：本文 §3/§5/§7 里 `xiaohongshu.video` 从
> `__INITIAL_STATE__` 补 metadata 的设计**未落地**。实测发现 SSR 页读是匿名服务端
> fetch，小红书一律 302 到 /404，视频路径会因此在 yt-dlp 运行前就失败。当前的
> `xiaohongshu.video` 直接把 URL 交给 yt-dlp 的内置 XiaoHongShu extractor（它自带
> 签名与 cookie 处理），只用 yt-dlp 的 info JSON 投影 metadata，不读页面；`note` 路径
> 仍按下文的 SSR 方案解析，但需要登录态。短链解析见 `noteUrlFromRedirect`。下面保留
> 原设计作为背景记录。

## 1. 结论先行

小红书抓取**可行且成本低**，因为本项目的三块现成基础设施正好覆盖它的三个难点：

| 难点 | 现成设施 | 结论 |
| --- | --- | --- |
| 反爬签名 `x-s`/`x-s-common` | 不做 API，改走笔记页 SSR 的 `window.__INITIAL_STATE__`（DOM-first） | 绕过签名，与社区主流做法一致 |
| 90% 笔记需登录 | `platform-*` 的登录协议 + 账户池 + cookie 轮换（`manual`/`browser` 已现成） | 直接复用 |
| 视频笔记下载 | 本地 yt-dlp `2026.08.19` 已内置 `XiaoHongShu` extractor | 复用 `generic.ytdlp`，不必重写 |

真正需要新写的代码只有一处：**图文笔记的 DOM 解析 + 图片下载**（约 200 行 JS，仿 `bilibili/article.js`）。
其余都是 descriptor 声明和注册接线。

## 2. 关键架构事实（决定改哪些文件）

本项目的 extractor 是**双层分工**，这一点务必先认清：

- **Rust `platform-*` crate**：只实现 `Provider`（登录协议）+ `extractors()`（`ExtractorInfo` 描述符：id / platform / label / kind / patterns / credentials / metadata_keys）。
  **它不做实际抓取**——`extractor-core::content` 里明确写了「descriptors are data, not dispatch」。
- **JS `hosts/dsh/lib/content/<platform>/*.js`**：真正的 `extract()` 实现，产出文件 + 开放 metadata map，注册进 `lib/content/registry.js` 的 `EXTRACTORS` 数组。

因此新增小红书要**两处都改**，且两侧的 id / kind / patterns / credentials / metadataKeys 必须逐字一致：Rust 侧给 `cx candidates` / CLI / 其他宿主看，JS 侧给 DSH 的 `content_candidates` 工具看。

### 改动清单（9 个文件）

**Rust 侧（描述符 + 登录协议）**
1. `Cargo.toml` — workspace `members` 加 `"crates/platform-xiaohongshu"`
2. `crates/platform-xiaohongshu/Cargo.toml` — 新建，只依赖 `extractor-core` + `serde_json`（照抄 `platform-wmpvp/Cargo.toml`）
3. `crates/platform-xiaohongshu/src/lib.rs` — 新建，`Xiaohongshu` struct + `Provider` impl
4. `crates/extractor-abi/Cargo.toml` — 加 `platform-xiaohongshu = { path = "../platform-xiaohongshu" }`
5. `crates/extractor-abi/src/service.rs` — `platforms()` 的 vec 加一行 `platform_xiaohongshu::Xiaohongshu::new()`（放在 `platform_generic` 之前）

**JS 侧（实际抓取）**
6. `hosts/dsh/lib/content/xiaohongshu/note.js` — 新建，图文 + 视频两个 extractor 的实现
7. `hosts/dsh/lib/content/registry.js` — import 两个 extractor，`EXTRACTORS` 数组加两项
8. `hosts/dsh/test/content.test.js` — 加候选/解析/投影单测
9. `hosts/dsh/README.md` — Tools / content layer 表格加红书行（自描述文档要跟上）

> 不需要改：`flake.nix`（cargoBuildFlags 已是 `-p extractor-abi`，自动带入新 crate；`hosts/dsh` 整个目录传给 npm 构建）、nur-packages（纯 re-export）、nix-config（bundle 已加载，工具是通用的）。

## 3. 技术路线：DOM-first，不碰签名

### 3.1 为什么不用接口

小红书 Web API（`/api/sns/web/v1/feed` 等）需要 `x-s`、`x-t`、`x-s-common`、`xsec_token`。调研结论一致：签名算法（`mnsv2` + 自定义 Base64）随前端版本频繁变动，`xsec_token` 短时效且绑定 `xsec_source`，靠逆向维护是持续失血的路线。

笔记页 `/explore/<note_id>?xsec_token=…` 的 SSR HTML 内嵌 `window.__INITIAL_STATE__`，`note.noteDetailMap` 下有完整的标题、正文、图片列表、作者、互动数、视频地址。已有 MCP 爬虫项目明确选择「DOM extraction from `window.__INITIAL_STATE__` instead of API calls」以换取抗封与低维护。这与本项目 `bilibili/article.js` 的做法完全同构——**直接复用那套花括号配对解析法**。

### 3.2 抓取流程

```
URL (explore / discovery/item / xhslink 短链)
  │
  ├─ 短链 → HTTP 跟随重定向，取最终 explore URL（含 xsec_token）
  ├─ detect kind：URL 无法区分图文/视频 → 两个 extractor 平局，调用者显式选（同 wmpvp）
  │
  ├─ xiaohongshu.note :
  │     fetch HTML（带 UA + Referer + 账户池 cookie，必要时重试）
  │     → 花括号配对取 __INITIAL_STATE__（注意把裸 undefined 替换成 null）
  │     → 投影 metadata，下载 imageList[] 图片
  │     → 写 <id>.md + 图片 + <id>.meta.json
  │
  └─ xiaohongshu.video:
        委托 yt-dlp（generic.ytdlp 适配层，传 cookiesPath），
        从 __INITIAL_STATE__ 补 duration / 无水印地址等 metadata
        → 视频文件 + .meta.json
```

### 3.3 URL 形态与 id

- 长链：`https://www.xiaohongshu.com/explore/<note_id>?xsec_token=…&xsec_source=…`
- 长链：`https://www.xiaohongshu.com/discovery/item/<note_id>?xsec_token=…`
- 短链：`https://xhslink.com/…` → `fetch(url, { redirect: 'follow' })` 取 `response.url`
- note_id：24 位十六进制字符串（如 `69821980000000000e03c95f`）

`patterns` 设计为 `['xiaohongshu.com']`（主域），短链域 `xhslink.com` 靠 `route`/`hosts` 解析，不写进 pattern 以免 specificity 失真。

## 4. 两个 extractor 的契约（Rust 描述符 / JS 对象逐字一致）

| id | platform | label | kind | credentials | patterns |
| --- | --- | --- | --- | --- | --- |
| `xiaohongshu.note` | `xiaohongshu` | 小红书图文笔记 | `Post` | `enhances` | `['xiaohongshu.com']` |
| `xiaohongshu.video` | `xiaohongshu` | 小红书视频笔记 | `Video` | `enhances` | `['xiaohongshu.com']` |

**为什么两者平局**：note_id 对应的页面类型（图文 vs 视频）无法从 URL 判断，和 `wmpvp.video`/`wmpvp.note` 完全相同的情况——按项目铁律「candidates 只列不猜，两者都给，调用者选」，选错时报错点名另一个 id。

**为什么 `enhances` 而非 `required`**：分享链接带 `xsec_token` 时未登录也能读到部分公开笔记；登录提升为无水印/高清/全文。这正是三态 credential 设计要表达的中间情况，匿名尝试失败的错误里明确提示「补登录 cookie 或换完整分享链接」。

**metadataKeys**（声明给 picker，map 本身开放）：

```
note:  note_id, xsec_token, author, author_id, avatar, text, images, image_count,
       liked_count, collected_count, comment_count, share_count, tags, ip_location, created_at
video: note_id, xsec_token, author, author_id, duration, video_url, thumbnail,
       liked_count, collected_count, comment_count, share_count, tags, created_at
```

通用键沿用约定：`title` / `author` / `id`(=note_id) / `url` / `description` / `thumbnail` / `created_at`。站点特有键通过 `metadataKeys` 声明即可，**不改内核/工具/UI**。

## 5. 登录协议（Rust `Provider`）

`ChannelInfo`：

```rust
ChannelInfo {
    id: "xiaohongshu",
    name: "小红书",
    hosts: vec!["xiaohongshu.com", "xhslink.com"],
    cookie_domain: "xiaohongshu.com",
    login_url: "https://www.xiaohongshu.com/",
    methods: vec![
        MethodInfo::live("manual",  "手动导入 Cookie", MethodKind::Import),
        MethodInfo::live("browser", "从浏览器读取",   MethodKind::Browser),
        MethodInfo::reserved("qr",       "扫码登录", MethodKind::Qr),
        MethodInfo::reserved("sms",      "短信验证码登录", MethodKind::Credential),
        MethodInfo::reserved("password", "账号密码登录", MethodKind::Credential),
    ],
}
```

- `manual` / `browser` 用通用实现（`start` 走 `require_method` 后报 unsupported，由宿主引导用户在网页登录并导入 cookie）——与 `platform-wmpvp` 一致，无需写签名协议。
- `check()`：读 cookie 文本，含 `web_session` 则 `logged_in=true`，identity 解析失败留空（不抛错、不泄漏 cookie 值）。
- 二维码登录（`qr`）第一版**标 reserved**，因为红书扫码要轮询 app 端确认且风控严格；作为后续增强，不影响首版可用。

## 6. 关键代码骨架

### 6.1 Rust `crates/platform-xiaohongshu/src/lib.rs`

```rust
//! 小红书 — www.xiaohongshu.com 图文/视频笔记。
//! 按 host 命名；只声明登录协议与内容描述符，抓取在 hosts/dsh/lib/content/xiaohongshu/。
//! 故意不做签名 API：改走笔记页 __INITIAL_STATE__（DOM-first），签名随前端频繁变动。

use std::sync::Arc;
use extractor_core::content::{ContentKind, CredentialUse, ExtractorInfo};
use extractor_core::cookies;
use extractor_core::model::{ChannelInfo, MethodInfo, MethodKind};
use extractor_core::provider::{require_method, Challenge, CheckOutcome, Provider};
use serde_json::Value;

pub struct Xiaohongshu;

impl Xiaohongshu {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<dyn Provider> { Arc::new(Self) }
}

impl Provider for Xiaohongshu {
    fn extractors(&self) -> Vec<ExtractorInfo> {
        vec![
            ExtractorInfo::new("xiaohongshu.note", "xiaohongshu", "小红书图文笔记",
                ContentKind::Post, vec!["xiaohongshu.com"], CredentialUse::Enhances)
                .with_metadata_keys(vec![
                    "note_id", "xsec_token", "author", "author_id", "avatar", "text",
                    "images", "image_count", "liked_count", "collected_count",
                    "comment_count", "share_count", "tags", "ip_location", "created_at",
                ]),
            ExtractorInfo::new("xiaohongshu.video", "xiaohongshu", "小红书视频笔记",
                ContentKind::Video, vec!["xiaohongshu.com"], CredentialUse::Enhances)
                .with_metadata_keys(vec![
                    "note_id", "xsec_token", "author", "author_id", "duration",
                    "video_url", "thumbnail", "liked_count", "collected_count",
                    "comment_count", "share_count", "tags", "created_at",
                ]),
        ]
    }

    fn info(&self) -> ChannelInfo { /* 见 §5 */ }

    fn start(&self, method: &str, _params: &Value, _ambient: extractor_core::Ambient)
        -> extractor_core::Result<(Challenge, Value)> {
        require_method(&self.info(), method)?;
        Err(extractor_core::provider::unsupported(&self.info(), method))
    }

    fn check(&self, cookies_text: &str) -> extractor_core::Result<CheckOutcome> {
        let logged_in = cookies::parse_netscape(cookies_text)
            .iter().any(|c| c.name == "web_session");
        Ok(CheckOutcome { logged_in, identity: String::new() })
    }
}
```

> `purity` 测试会扫描 `platform-*` crate 是否碰 fs/env/process/clock/entropy——本文件只解析字符串，**合规**。

### 6.2 JS `hosts/dsh/lib/content/xiaohongshu/note.js`

复用 `bilibili/article.js` 的三个函数：`fetchHtml`（带 cookie 重试）、`initialStateOf`（花括号配对）、`project*`（投影）。红书特有两点：

1. **裸 `undefined`**：`__INITIAL_STATE__` 里有 `undefined` 字面量，`JSON.parse` 会失败。解析前先 `text.replace(/\bundefined\b/g, 'null')`（仅在值位置，注意别动 key 名——实测里 key 不带 undefined，安全）。
2. **state 路径**：`state.note.noteDetailMap[noteId].note`，字段 `title` / `desc` / `imageList[].urlDefault` / `user.nickname` / `interactInfo.likedCount` / `tagList` / `time` / `ipLocation`。大整数 id 用原文扫描（`exactIdOf` 同款）避免精度丢失。

导出对象形状照抄 `bilibiliArticle` / `bilibiliOpus`，`extract(input, opts)` 返回 `{ kind, source, files, metadata, extra }`；图片下载逐张容错（一张失败不毁掉整篇），`params.images === false` 跳过下载；写 `.meta.json` sidecar。

`xiaohongshu.video` 的实现直接委托 `fetchVideo`（来自 `../../ytdlp.js`），并从 state 读 `duration` / `video.media.stream` 补 metadata——**不复制 `generic/ytdlp.js` 的 argv 构造**，只复用其 `projectInfo` 投影函数。

## 7. 测试策略

**离线单测（`hosts/dsh/test/content.test.js`，`npm test` 无网络）**
- `candidates`：红书 URL 同时给出 `note` 和 `video` 两个候选且 specificity 相等；不给出 bilibili 的 extractor
- `initialStateOf`：对含 `undefined` / 嵌套 / 字符串里分号的 fixture 正确解析
- `noteIdOf`：24 位 id 从长链、短链重定向结果中提取正确
- `projectNote` / `projectVideo`：metadata 键齐全、open map 不被裁剪、cookie 值不进 metadata
- `extractorOf('xiaohongshu.note')` 可解析；拼错 id 时错误列出全部已知 id（含红书）

**Rust 单测（`crates/platform-xiaohongshu/src/lib.rs` 内 `#[cfg(test)]`）**
- 两个 extractor 都声明、同一 URL 都匹配、specificity 相等
- `qr` 是 reserved、只有 `manual`/`browser` implemented
- `check` 对含/不含 `web_session` 的 cookie 文本给出正确 `logged_in`

**约束测试（自动生效，无需改）**
- `tests/purity.rs` / `tests/platform_purity.rs` 按目录扫描，新 crate 自动被覆盖
- `assertExtractor` 在 registry 注册期校验契约，拼错 credentials 会在加载时 loud fail

**联网验证（可选，`test/live-xhs.mjs`）**：带登录 cookie 跑一次真实图文 + 视频笔记，确认 SSR state 结构未漂移。和现有 `live-article.mjs` 同款，默认不跑。

## 8. 风险与回退

| 风险 | 缓解 |
| --- | --- |
| 小红书改 SSR 结构 / `noteDetailMap` 路径变化 | 解析失败抛 actionable error（点名缺哪个字段），不静默返回空；fixture 测试钉死结构 |
| `xsec_token` 过期导致匿名抓取失败 | 错误提示「换完整分享链接或导入登录 cookie」；账户池轮换；不把 token 当长期凭证 |
| yt-dlp 的 XHS extractor 失效 | 视频路径与图文路径解耦，图文本身不受影响；本项目对 yt-dlp 是单一上游，属既有架构权衡 |
| 风控 / 403 频发 | 低频 + 账户轮换 + 明确提示不要用新号；不做高频批量（本项目是单条内容提取工具，不是爬虫） |
| 两侧 descriptor 漂移 | §2 的清单要求逐字一致；`content.test.js` 的 candidates 断言同时覆盖 JS 侧，Rust 侧单测覆盖 Rust 描述符 |

## 9. 范围外（明确不做）

- **不逆向 `x-s` / `x-s-common` 签名**：维护税高、随前端版本漂移，与 DOM-first 路线冲突。
- **不做搜索 / 用户主页批量 / 评论抓取**：本项目是「单条内容提取」，不是数据采集器。
- **不改打包与部署**：nur 是 re-export、nix-config 用通用工具，新增 extractor 自动随上游 flake 生效；只需 `nix flake lock --update-input content-extractor`。
- **不做二维码登录**：先 `manual` + `browser`；`qr` 标 reserved 等需求明确。

## 10. 验收标准

1. `cargo test --workspace` + `cargo clippy --workspace --all-targets` 干净通过（含 purity）
2. `cd hosts/dsh && npm test` 通过，含新增候选/解析/投影断言
3. `cx candidates 'https://www.xiaohongshu.com/explore/<id>'` 输出 `xiaohongshu.note` 与 `xiaohongshu.video` 两个候选
4. DSH 中 `content_candidates` 同样列出两者；`content_extractor extractor="xiaohongshu.note"` 产出 `.md` + 图片 + `.meta.json`，全部在 session workspace 内
5. 视频走 `xiaohongshu.video` 得到 mp4 + metadata；无登录时有清晰报错而非空结果
6. 无 cookie 值出现在任何工具结果、日志、metadata 或提交中
