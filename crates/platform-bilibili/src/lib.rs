//! bilibili — login methods plus the platform's content extractors.
//!
//! ## Content kinds are separate extractors
//!
//! bilibili hosts several kinds of content on one domain, and they do **not**
//! share a parser:
//!
//! | extractor | URL shape | what it is |
//! |---|---|---|
//! | `bilibili.video` | `/video/BV…` or `/video/av…` | a video (yt-dlp handles it) |
//! | `bilibili.article` | `/read/cv…` | a column article: long text + inline images |
//! | `bilibili.opus` | `/opus/<id>` | a dynamic/opus post: modules of text and pics |
//! | `bilibili.audio` | `/audio/au…` | a music track |
//! | `bilibili.live` | `live.bilibili.com/<room>` | a live room |
//!
//! Sending an article to the video extractor produces a failure that looks like
//! "this site is unsupported", which is why each kind is advertised separately
//! and the caller picks. Article and opus pages share one renderer shape
//! (`window.__INITIAL_STATE__.detail.modules`), so they share a parser but keep
//! distinct ids — their metadata and canonical URLs differ.
//!
//! ## Login protocol notes that took real work to pin down
//!
//! * The poll endpoint reports success but frequently puts the auth cookies on
//!   a **redirect hop** (`data.url`) rather than on the poll response itself,
//!   so both are harvested and merged.
//! * Passport endpoints expect the `buvid3` / `b_lsid` fingerprint cookies to
//!   already be in flight. They are issued by the first call, so the session
//!   carries a partial jar forward.
//! * Password login needs RSA-PKCS#1 v1.5 over `salt + password`, with the
//!   public key from `/x/passport-login/web/key`.
//! * Most credential paths are behind geetest now. When the captcha endpoint
//!   returns `type: "geetest"` there is no headless path, and that is reported
//!   as a structured error pointing at the web login rather than a generic
//!   failure.
//!
//! Secrets boundary: `set_cookies` lines and assembled jars are credentials.
//! They go into the session state and then the account store — never into an
//! error string, a log line, or a tool result.

use std::sync::Arc;

use serde_json::{json, Value};

use extractor_core::content::{ContentKind, CredentialUse, ExtractorInfo};
use extractor_core::provider::{
    code_of, data_of, harvest, jar_header, jar_of, merge_jar, message_of, require_method,
    state_of, state_set, state_str, str_param, Challenge, CheckOutcome, Poll, Provider,
};
use extractor_core::ambient::Ambient;
use extractor_core::cookies;
use extractor_core::error::{Error, Result};
use extractor_core::http;
use extractor_core::model::{ChannelInfo, FieldSpec, MethodInfo, MethodKind};
use extractor_core::util::base64_encode;

const DOMAIN: &str = "bilibili.com";
const GENERATE_URL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
const POLL_URL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
const NAV_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
const CAPTCHA_URL: &str = "https://passport.bilibili.com/x/passport-login/captcha";
const SMS_SEND_URL: &str = "https://passport.bilibili.com/x/passport-login/web/sms/send";
const SMS_LOGIN_URL: &str = "https://passport.bilibili.com/x/passport-login/web/login/sms";
const PASSWORD_KEY_URL: &str = "https://passport.bilibili.com/x/passport-login/web/key";
const PASSWORD_LOGIN_URL: &str = "https://passport.bilibili.com/x/passport-login/web/login";

/// Poll codes returned in `data.code`.
mod poll_code {
    pub const OK: i64 = 0;
    pub const WAITING: i64 = 86101;
    pub const SCANNED: i64 = 86090;
    pub const EXPIRED: i64 = 86038;
}

/// Bilibili provider.
pub struct Bilibili;

impl Bilibili {
    /// Construct a boxed provider for the registry.
    ///
    /// Returns `Arc<dyn Provider>` rather than `Self` on purpose: the
    /// registry stores trait objects, so every platform hands back the same
    /// erased type and the composition root needs no per-platform generics.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

impl Default for Bilibili {
    fn default() -> Self {
        Self
    }
}

impl Provider for Bilibili {
    fn extractors(&self) -> Vec<ExtractorInfo> {
        vec![
            ExtractorInfo::new(
                "bilibili.video",
                "bilibili",
                "哔哩哔哩视频",
                ContentKind::Video,
                vec!["bilibili.com/video/"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "bvid", "aid", "cid", "pages", "view_count", "danmaku_count", "uploader_id",
            ]),
            ExtractorInfo::new(
                "bilibili.article",
                "bilibili",
                "哔哩哔哩专栏文章",
                ContentKind::Article,
                vec!["bilibili.com/read/"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "cvid", "author_id", "text", "images", "word_count",
                "view_count", "like_count", "coin_count", "favorite_count", "collection",
            ]),
            ExtractorInfo::new(
                "bilibili.opus",
                "bilibili",
                "哔哩哔哩动态/图文",
                ContentKind::Post,
                vec!["bilibili.com/opus/"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "opus_id", "author_id", "text", "images", "view_count", "like_count",
            ]),
            ExtractorInfo::new(
                "bilibili.audio",
                "bilibili",
                "哔哩哔哩音频",
                ContentKind::Audio,
                vec!["bilibili.com/audio/"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec!["auid", "lyric", "duration"]),
            ExtractorInfo::new(
                "bilibili.live",
                "bilibili",
                "哔哩哔哩直播",
                ContentKind::Live,
                vec!["live.bilibili.com/"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec!["room_id", "live_status", "area_name"]),
        ]
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            id: "bilibili",
            name: "哔哩哔哩",
            hosts: vec!["bilibili.com", "b23.tv"],
            cookie_domain: DOMAIN,
            login_url: "https://passport.bilibili.com/login",
            methods: vec![
                MethodInfo::live("qr", "扫码登录", MethodKind::Qr),
                MethodInfo::live("manual", "手动导入 Cookie", MethodKind::Import),
                MethodInfo::credential(
                    "sms",
                    "短信验证码登录",
                    vec![FieldSpec { name: "tel".into(), label: "手机号".into(), secret: false }],
                ),
                MethodInfo::credential(
                    "password",
                    "账号密码登录",
                    vec![
                        FieldSpec { name: "username".into(), label: "账号 / 手机号".into(), secret: false },
                        FieldSpec { name: "password".into(), label: "密码".into(), secret: true },
                    ],
                ),
                MethodInfo::live("browser", "从浏览器读取", MethodKind::Browser),
            ],
        }
    }

    fn start(
        &self,
        method: &str,
        params: &Value,
        ambient: Ambient,
    ) -> Result<(Challenge, Value)> {
        require_method(&self.info(), method)?;
        match method {
            "qr" => {
                let response = http::get(GENERATE_URL, &[("accept", "application/json")])?;
                if !response.ok() {
                    return Err(Error::http(format!(
                        "bilibili qrcode generate: HTTP {}",
                        response.status()
                    )));
                }
                let body = response.json()?;
                if code_of(&body) != 0 {
                    return Err(Error::http(format!("bilibili qrcode generate: {}", message_of(&body))));
                }
                let data = data_of(&body);
                let url = data.get("url").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let key = data
                    .get("qrcode_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                // The generate call is where buvid3/b_lsid are issued; they
                // have to ride along with every later poll.
                let jar = harvest(&response.set_cookies, DOMAIN, ambient).unwrap_or_default();
                Ok((
                    Challenge::Qr { url, image: None },
                    state_of(&[("key", json!(key)), ("jar", json!(jar))]),
                ))
            }
            "sms" => {
                // Ask for the captcha token up front: if it comes back as a
                // geetest challenge, fail here with an actionable message
                // instead of after the user has typed their number.
                let token = captcha_token()?;
                let _ = params;
                Ok((
                    Challenge::Fields {
                        prompt: "输入手机号以接收短信验证码".into(),
                        fields: vec![FieldSpec {
                            name: "tel".into(),
                            label: "手机号".into(),
                            secret: false,
                        }],
                    },
                    state_of(&[("captchaToken", json!(token)), ("jar", json!(""))]),
                ))
            }
            "password" => {
                let token = captcha_token()?;
                let _ = params;
                Ok((
                    Challenge::Fields {
                        prompt: "输入账号密码".into(),
                        fields: vec![
                            FieldSpec { name: "username".into(), label: "账号 / 手机号".into(), secret: false },
                        request_password_field(),
                        ],
                    },
                    state_of(&[("captchaToken", json!(token)), ("jar", json!(""))]),
                ))
            }
            "manual" | "browser" => Err(Error::channel(format!(
                "the '{method}' method takes a finished credential; call import or browse directly"
            ))),
            other => Err(Error::channel(format!("unhandled method '{other}'"))),
        }
    }

    fn poll(&self, session: &mut extractor_core::session::Session, ambient: Ambient) -> Result<Poll> {
        let key = state_str(session, "key");
        if key.is_empty() {
            return Err(Error::channel("this session has no qrcode key"));
        }
        let mut query = String::new();
        extractor_core::http::push_query(&mut query, "qrcode_key", &key);
        let url = format!("{POLL_URL}{query}");

        let cookie = jar_header(session);
        let mut headers: Vec<(&str, &str)> = vec![("accept", "application/json")];
        if !cookie.is_empty() {
            headers.push(("cookie", cookie.as_str()));
        }
        let response = http::get(&url, &headers)?;
        if !response.ok() {
            return Err(Error::http(format!("bilibili qrcode poll: HTTP {}", response.status())));
        }
        let body = response.json()?;
        let data = data_of(&body);
        let code = data.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        let raw = data.get("message").and_then(|v| v.as_str()).unwrap_or_default().to_string();

        match code {
            poll_code::OK => {
                let mut cookies = harvest(&response.set_cookies, DOMAIN, ambient).unwrap_or_default();
                if cookies.is_empty() {
                    if let Some(hop) = data.get("url").and_then(|v| v.as_str()) {
                        if let Ok(hop_response) = http::get(hop, &[("accept", "text/html,*/*")]) {
                            cookies = harvest(&hop_response.set_cookies, DOMAIN, ambient).unwrap_or_default();
                        }
                    }
                }
                if cookies.is_empty() {
                    return Err(Error::http(
                        "bilibili reported a successful scan but issued no cookies",
                    ));
                }
                merge_jar(session, &cookies);
                let identity = self.check(&jar_of(session)).map(|o| o.identity).unwrap_or_default();
                Ok(Poll::Done { cookies: jar_of(session), identity })
            }
            poll_code::WAITING => {
                Ok(Poll::Pending { message: or_default(raw, "等待扫码") })
            }
            poll_code::SCANNED => {
                Ok(Poll::Scanned { message: or_default(raw, "已扫码，请在手机上确认") })
            }
            poll_code::EXPIRED => {
                Ok(Poll::Expired { message: or_default(raw, "二维码已过期，请重新获取") })
            }
            other => Err(Error::http(format!("bilibili qrcode poll: unexpected code {other}"))),
        }
    }

    fn submit(
        &self,
        session: &mut extractor_core::session::Session,
        params: &Value,
        ambient: Ambient,
    ) -> Result<Poll> {
        match session.method.as_str() {
            "sms" => {
                // Two calls share one session: `tel` sends the code, `code`
                // exchanges it for a login.
                if let Some(code) = str_param(params, "code") {
                    sms_login(session, code, ambient)
                } else {
                    let tel = str_param(params, "tel").ok_or_else(|| Error::channel("missing 'tel'"))?;
                    sms_send(session, tel, ambient)
                }
            }
            "password" => {
                let username = str_param(params, "username")
                    .ok_or_else(|| Error::channel("missing 'username'"))?;
                let password = str_param(params, "password")
                    .ok_or_else(|| Error::channel("missing 'password'"))?;
                password_login(session, username, password, ambient)
            }
            other => Err(Error::channel(format!(
                "channel 'bilibili' has no submit step for '{other}'"
            ))),
        }
    }

    fn check(&self, cookies_text: &str) -> Result<CheckOutcome> {
        let header = cookies::header_from_netscape(cookies_text);
        if header.is_empty() {
            return Ok(CheckOutcome { logged_in: false, identity: String::new() });
        }
        let response = http::get(NAV_URL, &[("accept", "application/json"), ("cookie", header.as_str())])?;
        let body = response.json().unwrap_or(Value::Null);
        let data = data_of(&body);
        Ok(CheckOutcome {
            logged_in: data.get("isLogin").and_then(|v| v.as_bool()).unwrap_or(false),
            identity: data
                .get("uname")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }
}

/// The password field, split out so the field list stays readable.
fn request_password_field() -> FieldSpec {
    FieldSpec { name: "password".into(), label: "密码".into(), secret: true }
}

/// Fall back to `default` when the site sent an empty message.
fn or_default(message: String, default: &str) -> String {
    if message.is_empty() {
        default.to_string()
    } else {
        message
    }
}

// ── Credential methods ──────────────────────────────────────────────────────

/// Public captcha-token endpoint, or a structured refusal when geetest blocks
/// headless login.
fn captcha_token() -> Result<String> {
    let response = http::get(
        CAPTCHA_URL,
        &[("accept", "application/json"), ("referer", "https://passport.bilibili.com/login")],
    )?;
    if !response.ok() {
        return Err(Error::http(format!("bilibili captcha: HTTP {}", response.status())));
    }
    let body = response.json()?;
    let data = data_of(&body);
    if data
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|k| k.eq_ignore_ascii_case("geetest"))
    {
        return Err(Error::channel(
            "bilibili requires a geetest captcha for credential login, which cannot be \
             solved headlessly; use the QR method, or sign in through a browser at \
             https://passport.bilibili.com/login",
        ));
    }
    Ok(data.get("token").and_then(|v| v.as_str()).unwrap_or_default().to_string())
}

/// SMS step 1: ask bilibili to text a code to `tel`.
fn sms_send(session: &mut extractor_core::session::Session, tel: &str, ambient: Ambient) -> Result<Poll> {
    let token = state_str(session, "captchaToken");
    let response = http::post_form(
        SMS_SEND_URL,
        &[("accept", "application/json"), ("referer", "https://passport.bilibili.com/login")],
        &[
            ("cid", "86"),
            ("tel", tel),
            ("source", "main-mini"),
            ("token", &token),
            ("challenge", ""),
            ("validate", ""),
            ("seccode", ""),
        ],
    )?;
    let body = response.json().unwrap_or(Value::Null);
    if code_of(&body) != 0 {
        return Err(Error::http(format!("bilibili sms send: {}", message_of(&body))));
    }
    merge_jar(session, &harvest(&response.set_cookies, DOMAIN, ambient).unwrap_or_default());
    state_set(session, "tel", json!(tel));
    state_set(session, "step", json!("awaiting_code"));
    Ok(Poll::NeedInput {
        prompt: "验证码已发送，请输入收到的短信验证码".into(),
        fields: vec![FieldSpec { name: "code".into(), label: "短信验证码".into(), secret: false }],
    })
}

/// SMS step 2: exchange the code for a login.
fn sms_login(session: &mut extractor_core::session::Session, code: &str, ambient: Ambient) -> Result<Poll> {
    let tel = state_str(session, "tel");
    let response = http::post_form(
        SMS_LOGIN_URL,
        &[("accept", "application/json"), ("referer", "https://passport.bilibili.com/login")],
        &[
            ("cid", "86"),
            ("tel", &tel),
            ("code", code),
            ("source", "main-mini"),
            ("keep", "0"),
        ],
    )?;
    finish_login(session, &response, ambient)
}

/// Password login: RSA-encrypt `salt + password` and post it.
fn password_login(
    session: &mut extractor_core::session::Session,
    username: &str,
    password: &str,
    ambient: Ambient,
) -> Result<Poll> {
    let key_response = http::get(
        PASSWORD_KEY_URL,
        &[("accept", "application/json"), ("referer", "https://passport.bilibili.com/login")],
    )?;
    let key_body = key_response.json().unwrap_or(Value::Null);
    let key_data = data_of(&key_body);
    let hash = key_data
        .get("hash")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let pem = key_data
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if hash.is_empty() || pem.is_empty() {
        return Err(Error::http("bilibili did not return a password encryption key"));
    }
    let encrypted = rsa_encrypt(&format!("{hash}{password}"), &pem)?;
    let token = state_str(session, "captchaToken");
    let response = http::post_form(
        PASSWORD_LOGIN_URL,
        &[("accept", "application/json"), ("referer", "https://passport.bilibili.com/login")],
        &[
            ("source", "main_web"),
            ("username", username),
            ("password", &encrypted),
            ("keep", "true"),
            ("token", &token),
            ("challenge", ""),
            ("validate", ""),
            ("seccode", ""),
            ("go_url", "https://www.bilibili.com/"),
        ],
    )?;
    finish_login(session, &response, ambient)
}

/// Shared tail of the credential methods: check the API code, harvest cookies
/// from the response or its redirect hop, then confirm with the nav API.
fn finish_login(
    session: &mut extractor_core::session::Session,
    response: &http::Response,
    ambient: Ambient,
) -> Result<Poll> {
    let body = response.json().unwrap_or(Value::Null);
    let code = code_of(&body);
    if code != 0 {
        return Err(Error::http(format!("bilibili login: {} ({code})", message_of(&body))));
    }
    let data = data_of(&body);
    let mut cookies = harvest(&response.set_cookies, DOMAIN, ambient).unwrap_or_default();
    if cookies.is_empty() {
        if let Some(hop) = data.get("url").and_then(|v| v.as_str()) {
            if let Ok(hop_response) = http::get(hop, &[("accept", "text/html,*/*")]) {
                cookies = harvest(&hop_response.set_cookies, DOMAIN, ambient).unwrap_or_default();
            }
        }
    }
    if cookies.is_empty() {
        return Err(Error::http("bilibili accepted the credential but issued no cookies"));
    }
    merge_jar(session, &cookies);
    let identity = Bilibili.check(&jar_of(session)).map(|o| o.identity).unwrap_or_default();
    Ok(Poll::Done { cookies: jar_of(session), identity })
}

/// RSA-PKCS#1 v1.5 encryption with the PEM public key bilibili hands out.
///
/// Falls back to a clear, structured error rather than panicking when the key
/// cannot be parsed — a key format change upstream is plausible and must not
/// crash a long-lived host.
fn rsa_encrypt(plaintext: &str, pem: &str) -> Result<String> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pkcs8::DecodePublicKey;
    use rsa::RsaPublicKey;

    // bilibili has shipped this key as both SubjectPublicKeyInfo and PKCS#1;
    // accept whichever arrives.
    // The two decoders have different error types, so both are flattened to
    // a message before they can share one `?`.
    let key = if pem.contains("BEGIN PUBLIC KEY") {
        RsaPublicKey::from_public_key_pem(pem).map_err(|e| e.to_string())
    } else {
        RsaPublicKey::from_pkcs1_pem(pem).map_err(|e| e.to_string())
    }
    .map_err(|message| Error::http(format!("bilibili password key unreadable: {message}")))?;

    let mut rng = rsa::rand_core::OsRng;
    // PKCS#1 v1.5 is what the endpoint expects; OAEP would be rejected
    // upstream even though the crate offers it.
    key.encrypt(&mut rng, rsa::Pkcs1v15Encrypt, plaintext.as_bytes())
        .map_err(|e| Error::http(format!("bilibili password encryption failed: {e}")))
        .map(|bytes| base64_encode(&bytes))
}
