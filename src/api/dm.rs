//! Direct messages through instagram.com's web inbox.
//!
//! The website no longer serves the old `direct_v2` endpoints. Its inbox page
//! carries the first batch of conversations as a Lightspeed payload (see
//! [`super::lightspeed`]) along with the settings for its chat connection, an
//! MQTT session over a WebSocket. Messages are fetched, sent and marked read
//! by publishing "tasks" on that connection; the answers are Lightspeed
//! payloads too.
//!
//! Everything is translated into the same [`Thread`] / [`DmItem`] shapes the
//! rest of the app already uses. Diagnostics are logged with the `dm:` prefix
//! and never include names or message text.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use super::lightspeed::{self, AttachmentKind, LsAttachment, LsContact, LsMessage, LsThread, Tables};
use super::mqtt::Conn;
use super::{
    Client, InboxPage, LoginRequired, RateLimited, Recipient, Thread, VERIFY, cooldown_remaining, parse,
    start_cooldown,
};

const WEB: &str = "https://www.instagram.com/";
const CHAT: &str = "wss://edge-chat.instagram.com/chat";
/// instagram.com's app id, used when the page doesn't say.
const DEFAULT_APP_ID: i64 = 936619743392459;
const PAGE_KEY: &str = "page/direct/inbox/";
/// How many messages a conversation shows at first, and per "Load Older".
const PAGE_SIZE: usize = 30;

fn web_base() -> String {
    std::env::var("FINSTA_WEB_BASE").unwrap_or_else(|_| WEB.to_string())
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Ids the website generates for epochs and messages: a millisecond
/// timestamp shifted left by 22 bits, with random low bits.
fn time_id() -> i64 {
    let rand = (uuid::Uuid::new_v4().as_u128() & 0x3f_ffff) as i64;
    (now_ms() << 22) | rand
}

#[derive(Default)]
pub(super) struct DmState {
    config: Option<PageConfig>,
    page_loaded: Option<Instant>,
    store: Store,
    conn: Option<Conn>,
    last_connect_attempt: Option<Instant>,
    /// Which payload kinds have had their shapes logged already.
    logged: HashSet<String>,
}

#[derive(Clone, Default)]
struct PageConfig {
    app_id: i64,
    client_id: String,
    endpoint: String,
    version: Option<String>,
    /// The id the chat connection logs in as.
    mqtt_user: String,
    /// Ids from the page that could be the viewer's id in the chat system.
    viewer_candidates: HashSet<i64>,
}

#[derive(Default)]
struct Store {
    threads: HashMap<i64, LsThread>,
    contacts: HashMap<i64, LsContact>,
    participants: HashMap<i64, BTreeSet<i64>>,
    /// (thread, contact) -> read watermark ms
    reads: HashMap<(i64, i64), i64>,
    /// Per thread, ordered by (timestamp, id).
    messages: HashMap<i64, BTreeMap<(i64, String), LsMessage>>,
    /// message id -> (thread, timestamp)
    message_index: HashMap<String, (i64, i64)>,
    attachments: HashMap<String, Vec<LsAttachment>>,
    more_threads: bool,
    /// The viewer's id in the chat system (not necessarily their profile id).
    viewer: Option<i64>,
}

impl Store {
    fn apply(&mut self, t: Tables) {
        for th in t.threads {
            if th.key != 0 {
                self.threads.insert(th.key, th);
            }
        }
        for c in t.contacts {
            if c.id == 0 {
                continue;
            }
            let entry = self.contacts.entry(c.id).or_default();
            entry.id = c.id;
            if !c.name.is_empty() {
                entry.name = c.name;
            }
            if !c.username.is_empty() {
                entry.username = c.username;
            }
            if !c.picture.is_empty() {
                entry.picture = c.picture;
            }
        }
        for (thread, contact, read) in t.participants.into_iter().chain(t.read_receipts) {
            if thread == 0 || contact == 0 {
                continue;
            }
            self.participants.entry(thread).or_default().insert(contact);
            let r = self.reads.entry((thread, contact)).or_default();
            *r = (*r).max(read);
        }
        for m in t.messages {
            if m.id.is_empty() || m.thread == 0 {
                continue;
            }
            if let Some((thread, ts)) = self.message_index.get(&m.id).copied()
                && let Some(list) = self.messages.get_mut(&thread)
            {
                list.remove(&(ts, m.id.clone()));
            }
            self.message_index.insert(m.id.clone(), (m.thread, m.ts_ms));
            self.messages.entry(m.thread).or_default().insert((m.ts_ms, m.id.clone()), m);
        }
        for a in t.attachments {
            let list = self.attachments.entry(a.message_id.clone()).or_default();
            if !list.iter().any(|x| x.kind == a.kind && x.url == a.url) {
                list.push(a);
            }
        }
        for mid in t.deleted_messages {
            if let Some((thread, ts)) = self.message_index.remove(&mid)
                && let Some(list) = self.messages.get_mut(&thread)
            {
                list.remove(&(ts, mid));
            }
        }
        if let Some(more) = t.more_threads {
            self.more_threads = more;
        }
    }

    /// Work out the viewer's chat id: the page's candidate that takes part in
    /// the most conversations, or else whoever takes part in nearly all.
    fn detect_viewer(&mut self, candidates: &HashSet<i64>) {
        let mut counts: HashMap<i64, usize> = HashMap::new();
        for members in self.participants.values() {
            for m in members {
                *counts.entry(*m).or_default() += 1;
            }
        }
        let from_page = candidates
            .iter()
            .filter_map(|c| counts.get(c).map(|n| (*n, *c)))
            .max();
        let everywhere = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .filter(|(_, n)| **n * 10 >= self.participants.len() * 8 && self.participants.len() >= 2)
            .map(|(id, _)| *id);
        let viewer = from_page.map(|(_, id)| id).or(everywhere);
        if viewer.is_some() && viewer != self.viewer {
            eprintln!(
                "finsta: dm: viewer id found ({})",
                if from_page.is_some() { "from page" } else { "in every conversation" }
            );
            self.viewer = viewer;
        }
    }

    fn others(&self, key: i64) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .participants
            .get(&key)
            .map(|p| p.iter().copied().filter(|id| Some(*id) != self.viewer).collect())
            .unwrap_or_default();
        // 1:1 conversations are often keyed by the other person's id.
        if ids.is_empty() && self.contacts.contains_key(&key) && Some(key) != self.viewer {
            ids.push(key);
        }
        ids
    }

    fn user_value(&self, id: i64) -> Value {
        let c = self.contacts.get(&id).cloned().unwrap_or_default();
        json!({
            "pk": id.to_string(),
            "username": c.username,
            "full_name": c.name,
            "profile_pic_url": c.picture,
        })
    }

    fn sender(&self, id: i64, viewer_pk: &str) -> String {
        if Some(id) == self.viewer { viewer_pk.to_string() } else { id.to_string() }
    }

    fn item_value(&self, m: &LsMessage, viewer_pk: &str) -> Value {
        let mut v = json!({
            "item_id": m.id,
            "user_id": self.sender(m.sender, viewer_pk),
            "timestamp": m.ts_ms * 1000,
        });
        let attachment = self.attachments.get(&m.id).and_then(|l| l.first());
        if !m.text.is_empty() {
            v["item_type"] = "text".into();
            v["text"] = m.text.clone().into();
            return v;
        }
        let Some(a) = attachment else {
            v["item_type"] = "placeholder".into();
            v["placeholder"] = json!({"message": "This kind of message isn't supported yet. Open it on instagram.com."});
            return v;
        };
        let image = a.preview.clone().or_else(|| a.url.clone()).unwrap_or_default();
        match a.kind {
            AttachmentKind::Photo if !image.is_empty() => {
                v["item_type"] = "media".into();
                v["media"] = json!({
                    "media_type": 1,
                    "original_width": 1080,
                    "original_height": 1080,
                    "image_versions2": {"candidates": [{"url": image, "width": 1080, "height": 1080}]},
                });
            }
            AttachmentKind::Video if a.url.is_some() => {
                v["item_type"] = "media".into();
                v["media"] = json!({
                    "media_type": 2,
                    "original_width": 720,
                    "original_height": 1280,
                    "image_versions2": {"candidates": [{"url": image, "width": 720, "height": 1280}]},
                    "video_versions": [{"url": a.url, "width": 720, "height": 1280}],
                });
            }
            AttachmentKind::Share if a.url.is_some() => {
                let url = a.url.clone().unwrap_or_default();
                let text = if a.title.is_empty() { url.clone() } else { format!("{}\n{url}", a.title) };
                v["item_type"] = "link".into();
                v["link"] = json!({"text": text, "link_context": {"link_url": url}});
            }
            AttachmentKind::Audio => {
                v["item_type"] = "placeholder".into();
                v["placeholder"] = json!({"message": "Voice message. Listen on instagram.com."});
            }
            _ => {
                v["item_type"] = "placeholder".into();
                v["placeholder"] = json!({"message": "Attachment. Open it on instagram.com."});
            }
        }
        v
    }

    /// The conversation as the rest of the app expects it, with up to
    /// `limit` messages older than `before_ms` (newest first).
    fn thread_value(&self, key: i64, viewer_pk: &str, limit: usize, before_ms: Option<i64>) -> Value {
        let th = self.threads.get(&key).cloned().unwrap_or(LsThread { key, ..Default::default() });
        let others = self.others(key);
        let mut users: Vec<Value> = others.iter().map(|id| self.user_value(*id)).collect();
        if users.is_empty() && !th.name.is_empty() {
            // Nobody known but the conversation's own name and picture.
            users.push(json!({"pk": key.to_string(), "full_name": th.name, "profile_pic_url": th.picture}));
        }
        let empty = BTreeMap::new();
        let all = self.messages.get(&key).unwrap_or(&empty);
        let selected: Vec<&LsMessage> = all
            .values()
            .rev()
            .filter(|m| before_ms.is_none_or(|b| m.ts_ms < b))
            .take(limit)
            .collect();
        let mut items: Vec<Value> = selected.iter().map(|m| self.item_value(m, viewer_pk)).collect();
        if items.is_empty() && before_ms.is_none() && !th.snippet.is_empty() {
            // Only the inbox preview is known; the chat view ignores this item.
            items.push(json!({
                "item_id": format!("snippet:{key}"),
                "user_id": self.sender(th.snippet_sender, viewer_pk),
                "timestamp": th.last_activity_ms * 1000,
                "item_type": "text",
                "text": th.snippet,
            }));
        }
        let oldest = selected.last();
        let read_ms = self
            .viewer
            .and_then(|v| self.reads.get(&(key, v)).copied())
            .unwrap_or(0)
            .max(th.last_read_ms);
        let last_activity = th
            .last_activity_ms
            .max(all.keys().next_back().map(|(ts, _)| *ts).unwrap_or(0));
        json!({
            "thread_id": key.to_string(),
            "thread_title": if others.len() > 1 || th.thread_type == 2 { th.name.clone() } else { String::new() },
            "is_group": others.len() > 1 || th.thread_type == 2,
            "users": users,
            "items": items,
            "last_activity_at": last_activity * 1000,
            "last_seen_at": {viewer_pk: {"item_id": "", "timestamp": read_ms * 1000}},
            "has_older": selected.len() == limit || (oldest.is_some() && before_ms.is_some()),
            "oldest_cursor": oldest.map(|m| format!("{}:{}", m.ts_ms, m.id)),
        })
    }

    fn sorted_keys(&self) -> Vec<i64> {
        let mut keys: Vec<(i64, i64)> = self
            .threads
            .values()
            .map(|t| {
                let newest = self
                    .messages
                    .get(&t.key)
                    .and_then(|m| m.keys().next_back())
                    .map(|(ts, _)| *ts)
                    .unwrap_or(0);
                (t.last_activity_ms.max(newest), t.key)
            })
            .collect();
        keys.sort_by(|a, b| b.cmp(a));
        keys.into_iter().map(|(_, k)| k).collect()
    }
}

// ----- The inbox page ----------------------------------------------------

#[derive(Default)]
struct ParsedPage {
    /// `define` entries: module name -> config object.
    defines: HashMap<String, Value>,
    /// (key, payload) of every embedded Lightspeed request.
    payloads: Vec<(String, String)>,
    scripts: Vec<String>,
}

fn scan(v: &Value, page: &mut ParsedPage) {
    match v {
        Value::Array(a) => {
            if a.len() >= 3
                && let (Some(name), true, true) = (a[0].as_str(), a[1].is_array(), a[2].is_object())
            {
                page.defines.entry(name.to_string()).or_insert_with(|| a[2].clone());
            }
            for x in a {
                scan(x, page);
            }
        }
        Value::Object(m) => {
            for (k, x) in m {
                if k.starts_with("lightspeed_web_request")
                    && let Some(p) = x["payload"].as_str()
                {
                    page.payloads.push((k.clone(), p.to_string()));
                }
                scan(x, page);
            }
        }
        _ => {}
    }
}

fn parse_page(html: &str) -> ParsedPage {
    let mut page = ParsedPage::default();
    let script = regex::Regex::new(r#"(?s)<script type="application/json"[^>]*>(.*?)</script>"#).unwrap();
    for cap in script.captures_iter(html) {
        if let Ok(v) = serde_json::from_str::<Value>(&cap[1]) {
            scan(&v, &mut page);
        }
    }
    let unescaped = html.replace("\\/", "/");
    let js = regex::Regex::new(r#"https://static\.cdninstagram\.com/rsrc\.php/[A-Za-z0-9_\-./]+\.js(?:\?[A-Za-z0-9_\-=&.%]*)?"#)
        .unwrap();
    let mut seen = HashSet::new();
    for m in js.find_iter(&unescaped) {
        if seen.insert(m.as_str().to_string()) {
            page.scripts.push(m.as_str().to_string());
        }
    }
    page
}

fn find_version(text: &str) -> Option<String> {
    let re = regex::Regex::new(r#"__d\("LSVersion"[\s\S]{0,80}?"(\d{10,25})""#).unwrap();
    re.captures(text).map(|c| c[1].to_string())
}

fn numeric(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) if s.len() >= 5 && s.chars().all(|c| c.is_ascii_digit()) => s.parse().ok(),
        _ => None,
    }
}

fn version_cache_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("finsta").join("lightspeed.json"))
}

fn cached_version() -> Option<String> {
    let text = std::fs::read_to_string(version_cache_path()?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let saved = v["saved"].as_u64()?;
    let age = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs().saturating_sub(saved);
    if age > 24 * 3600 {
        return None;
    }
    v["version"].as_str().map(str::to_string)
}

fn save_version(version: &str) {
    let Some(path) = version_cache_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let saved = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = std::fs::write(path, json!({"version": version, "saved": saved}).to_string());
}

fn task(label: &str, payload: Value, queue: String) -> (String, Value, String) {
    (label.to_string(), payload, queue)
}

impl Client {
    fn page_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        let mut put = |k: &'static str, v: &str| {
            if let Ok(v) = HeaderValue::from_str(v) {
                h.insert(HeaderName::from_static(k), v);
            }
        };
        put("user-agent", &self.user_agent());
        put("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8");
        put("accept-language", "en-US,en;q=0.9");
        put("sec-fetch-dest", "document");
        put("sec-fetch-mode", "navigate");
        put("sec-fetch-site", "same-origin");
        put("sec-fetch-user", "?1");
        put("upgrade-insecure-requests", "1");
        put("referer", "https://www.instagram.com/");
        put("cookie", &self.cookie_header());
        h
    }

    async fn load_inbox_page(&self) -> Result<String> {
        if cooldown_remaining(PAGE_KEY).is_some() {
            return Err(RateLimited.into());
        }
        let url = format!("{}direct/inbox/", web_base());
        let resp = match self.inner.http.get(&url).headers(self.page_headers()).send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("finsta: dm: loading the inbox page failed: {e}");
                return Err(e.into());
            }
        };
        let status = resp.status();
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        self.absorb_headers(resp.headers());
        let html = resp.text().await?;
        if !status.is_success() {
            eprintln!("finsta: dm: inbox page -> HTTP {status} {location}");
        }
        if status.is_redirection() {
            if location.contains("/challenge") || location.contains("/checkpoint") {
                bail!(VERIFY);
            }
            if location.contains("/accounts/login") {
                return Err(LoginRequired.into());
            }
            bail!("Instagram redirected the inbox page ({status})");
        }
        if status.as_u16() == 429 {
            start_cooldown(PAGE_KEY, None);
            return Err(RateLimited.into());
        }
        if !status.is_success() {
            bail!("Instagram's inbox page returned an error ({status})");
        }
        Ok(html)
    }

    /// The Lightspeed protocol version, needed for every chat request. It's
    /// defined in one of the website's scripts, so look there (once a day).
    async fn ls_version(&self, html: &str, scripts: &[String]) -> Option<String> {
        if let Some(v) = find_version(html) {
            eprintln!("finsta: dm: protocol version found in the page");
            save_version(&v);
            return Some(v);
        }
        if let Some(v) = cached_version() {
            return Some(v);
        }
        let ua = self.user_agent();
        let mut checked = 0;
        for chunk in scripts.iter().take(160).collect::<Vec<_>>().chunks(6) {
            let fetches = chunk.iter().map(|url| {
                let req = self
                    .inner
                    .http
                    .get(url.as_str())
                    .header("user-agent", &ua)
                    .header("accept", "*/*")
                    .header("referer", "https://www.instagram.com/")
                    .header("sec-fetch-dest", "script")
                    .header("sec-fetch-mode", "no-cors")
                    .header("sec-fetch-site", "cross-site");
                async move { req.send().await.ok()?.text().await.ok() }
            });
            for text in futures_util::future::join_all(fetches).await.into_iter().flatten() {
                checked += 1;
                if let Some(v) = find_version(&text) {
                    eprintln!("finsta: dm: protocol version found in script {checked}/{}", scripts.len());
                    save_version(&v);
                    return Some(v);
                }
            }
        }
        eprintln!("finsta: dm: protocol version not found in {checked} of {} scripts", scripts.len());
        None
    }

    /// Load (or reload) the inbox page: the conversations it carries and the
    /// chat settings.
    async fn dm_load_page(&self, st: &mut DmState) -> Result<()> {
        let html = self.load_inbox_page().await?;
        let page = parse_page(&html);
        st.page_loaded = Some(Instant::now());

        if st.config.is_none() {
            let interesting: Vec<&String> = page
                .defines
                .keys()
                .filter(|k| {
                    ["Mqtt", "LS", "Lightspeed", "CurrentUser", "Version", "IGD", "Direct", "DTSG"]
                        .iter()
                        .any(|p| k.contains(p))
                })
                .collect();
            let keys = |name: &str| {
                page.defines
                    .get(name)
                    .and_then(Value::as_object)
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_else(|| "missing".into())
            };
            eprintln!(
                "finsta: dm: inbox page: {} bytes, {} config modules, {} scripts, payloads {:?}",
                html.len(),
                page.defines.len(),
                page.scripts.len(),
                page.payloads.iter().map(|(k, p)| format!("{k} ({} bytes)", p.len())).collect::<Vec<_>>()
            );
            eprintln!("finsta: dm: config modules of interest: {interesting:?}");
            eprintln!("finsta: dm: MqttWebConfig keys: {}", keys("MqttWebConfig"));
            eprintln!("finsta: dm: MqttWebDeviceID keys: {}", keys("MqttWebDeviceID"));
            eprintln!("finsta: dm: CurrentUserInitialData keys: {}", keys("CurrentUserInitialData"));

            let mqtt = page.defines.get("MqttWebConfig").cloned().unwrap_or_default();
            let user = page.defines.get("CurrentUserInitialData").cloned().unwrap_or_default();
            let mut candidates: HashSet<i64> = user
                .as_object()
                .map(|o| o.values().filter_map(numeric).collect())
                .unwrap_or_default();
            if let Ok(pk) = self.user_id().parse::<i64>() {
                candidates.insert(pk);
            }
            let mqtt_user = numeric(&user["USER_ID"])
                .or_else(|| numeric(&mqtt["fbid"]))
                .map(|n| n.to_string())
                .unwrap_or_else(|| self.user_id());
            st.config = Some(PageConfig {
                app_id: numeric(&mqtt["appID"]).unwrap_or(DEFAULT_APP_ID),
                client_id: page.defines.get("MqttWebDeviceID").and_then(|d| d["clientID"].as_str()).map(str::to_string)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                endpoint: mqtt["endpoint"].as_str().unwrap_or_default().to_string(),
                version: self.ls_version(&html, &page.scripts).await,
                mqtt_user,
                viewer_candidates: candidates,
            });
        }

        if page.payloads.is_empty() {
            bail!("Couldn't find your conversations on instagram.com's inbox page");
        }
        for (key, payload) in &page.payloads {
            self.dm_apply_payload(st, payload, &format!("page {key}"));
        }
        Ok(())
    }

    fn dm_apply_payload(&self, st: &mut DmState, payload: &str, what: &str) {
        let calls = match lightspeed::decode(payload) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("finsta: dm: {what}: payload isn't readable: {e}");
                return;
            }
        };
        let tables = Tables::from_calls(&calls);
        let candidates = st.config.as_ref().map(|c| c.viewer_candidates.clone()).unwrap_or_default();
        if st.logged.insert(what.to_string()) {
            let threads: HashSet<i64> = st.store.threads.keys().copied().collect();
            let contacts: HashSet<i64> = st.store.contacts.keys().copied().collect();
            let mut ids = lightspeed::known_ids(&tables);
            ids.extend(threads.iter().chain(contacts.iter()));
            lightspeed::log_shapes(what, &calls, &ids, &candidates);
            eprintln!("finsta: dm: {what}: {}", tables.check(&threads, &contacts));
        }
        st.store.apply(tables);
        st.store.detect_viewer(&candidates);
    }

    fn dm_apply_response(&self, st: &mut DmState, v: &Value, what: &str) {
        match v["payload"].as_str() {
            Some(p) if !p.is_empty() => self.dm_apply_payload(st, p, what),
            _ => {
                if st.logged.insert(format!("{what} (no payload)")) {
                    let keys = v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
                    eprintln!("finsta: dm: {what}: answer without payload, keys {keys:?}");
                }
            }
        }
    }

    fn dm_drain(&self, st: &mut DmState) {
        let pushes = st.conn.as_ref().map(|c| c.take_pushes()).unwrap_or_default();
        for p in pushes {
            self.dm_apply_response(st, &p, "push");
        }
    }

    /// Make sure the page has been loaded and the chat connection is up (if possible).
    async fn dm_ensure(&self, st: &mut DmState) -> Result<()> {
        if st.config.is_none() {
            self.dm_load_page(st).await?;
        }
        let alive = st.conn.as_ref().is_some_and(|c| c.is_alive());
        let recently = st.last_connect_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(60));
        if !alive && !recently {
            st.last_connect_attempt = Some(Instant::now());
            st.conn = None;
            match self.dm_connect(st).await {
                Ok(conn) => {
                    eprintln!("finsta: dm: chat connected");
                    st.conn = Some(conn);
                }
                Err(e) => eprintln!("finsta: dm: chat connection failed: {e:#}"),
            }
        }
        self.dm_drain(st);
        Ok(())
    }

    async fn dm_connect(&self, st: &DmState) -> Result<Conn> {
        let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
        let Some(version) = cfg.version.clone() else {
            bail!("the protocol version is unknown");
        };
        let mut url = match std::env::var("FINSTA_CHAT_URL") {
            Ok(u) => url::Url::parse(&u)?,
            Err(_) => url::Url::parse(&cfg.endpoint)
                .ok()
                .filter(|u| {
                    u.scheme() == "wss"
                        && u.host_str().is_some_and(|h| h.ends_with(".instagram.com") || h.ends_with(".facebook.com"))
                })
                .unwrap_or_else(|| url::Url::parse(CHAT).unwrap()),
        };
        let sid = (uuid::Uuid::new_v4().as_u128() % 9_007_199_254_740_991) as i64;
        let has = |u: &url::Url, k: &str| u.query_pairs().any(|(q, _)| q == k);
        if !has(&url, "sid") {
            url.query_pairs_mut().append_pair("sid", &sid.to_string());
        }
        if !has(&url, "cid") {
            url.query_pairs_mut().append_pair("cid", &cfg.client_id);
        }
        eprintln!("finsta: dm: connecting to {}{}", url.host_str().unwrap_or_default(), url.path());
        let username = json!({
            "u": cfg.mqtt_user,
            "s": sid,
            "cp": 3,
            "ecp": 10,
            "chat_on": true,
            "fg": true,
            "d": cfg.client_id,
            "ct": "websocket",
            "mqtt_sid": "",
            "aid": cfg.app_id,
            "st": [],
            "pm": [],
            "dc": "",
            "no_auto_fg": true,
            "gas": null,
            "pack": [],
            "php_override": "",
            "p": null,
            "a": self.user_agent(),
            "aids": null,
        })
        .to_string();
        let headers = [
            ("origin", "https://www.instagram.com".to_string()),
            ("user-agent", self.user_agent()),
            ("cookie", self.cookie_header()),
            ("accept-language", "en-US,en;q=0.9".to_string()),
            ("cache-control", "no-cache".to_string()),
            ("pragma", "no-cache".to_string()),
        ];
        let conn = Conn::connect(
            url.as_str(),
            &headers,
            "mqttwsclient",
            &username,
            &["/ls_foreground_state", "/ls_resp"],
        )
        .await?;
        conn.publish("/ls_app_settings", &json!({"ls_fdid": "", "ls_sv": version}).to_string())?;
        Ok(conn)
    }

    /// Run tasks on the chat connection and apply what comes back.
    async fn dm_tasks(&self, st: &mut DmState, what: &str, tasks: Vec<(String, Value, String)>) -> Result<()> {
        let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
        let conn = st
            .conn
            .as_ref()
            .filter(|c| c.is_alive())
            .ok_or_else(|| anyhow!("Couldn't connect to Instagram's chat"))?;
        let rid = conn.next_request_id();
        let tasks: Vec<Value> = tasks
            .into_iter()
            .enumerate()
            .map(|(i, (label, payload, queue))| {
                json!({
                    "failure_count": null,
                    "label": label,
                    "payload": payload.to_string(),
                    "queue_name": queue,
                    "task_id": i,
                })
            })
            .collect();
        let msg = json!({
            "app_id": cfg.app_id.to_string(),
            "payload": json!({
                "epoch_id": time_id(),
                "tasks": tasks,
                "version_id": cfg.version.unwrap_or_default(),
            }).to_string(),
            "request_id": rid,
            "type": 3,
        });
        let answer = match conn.request(&msg, Duration::from_secs(20)).await {
            Ok(a) => a,
            Err(e) => {
                eprintln!("finsta: dm: {what} failed: {e:#}");
                return Err(e);
            }
        };
        // Results can also arrive as separate pushes right after the answer.
        tokio::time::sleep(Duration::from_millis(600)).await;
        self.dm_apply_response(st, &answer, what);
        self.dm_drain(st);
        Ok(())
    }

    // ----- What the UI calls ---------------------------------------------

    pub async fn inbox(&self, cursor: Option<String>) -> Result<InboxPage> {
        let mut st = self.inner.dm.lock().await;
        let first = st.config.is_none();
        self.dm_ensure(&mut st).await?;
        let viewer_pk = self.user_id();
        let connected = st.conn.as_ref().is_some_and(|c| c.is_alive());

        let older_than = if cursor.is_some() {
            let oldest = st.store.sorted_keys().last().copied();
            if let (Some(key), true) = (oldest, connected) {
                let ts = st.store.threads.get(&key).map(|t| t.last_activity_ms).unwrap_or(0);
                let payload = json!({
                    "is_after": 0, "parent_thread_key": 0, "reference_thread_key": key,
                    "reference_activity_timestamp": ts, "additional_pages_to_fetch": 0,
                    "cursor": null, "messaging_tag": null, "sync_group": 1,
                });
                let _ = self.dm_tasks(&mut st, "older conversations", vec![task("145", payload, "trq".into())]).await;
                Some(ts)
            } else {
                return Ok(InboxPage::default());
            }
        } else {
            if !first {
                if connected {
                    let payload = json!({
                        "is_after": 0, "parent_thread_key": 0, "reference_thread_key": 0,
                        "reference_activity_timestamp": 9_999_999_999_999i64, "additional_pages_to_fetch": 0,
                        "cursor": null, "messaging_tag": null, "sync_group": 1,
                    });
                    let _ = self.dm_tasks(&mut st, "refresh conversations", vec![task("145", payload, "trq".into())]).await;
                } else if st.page_loaded.is_none_or(|t| t.elapsed() > Duration::from_secs(300))
                    && let Err(e) = self.dm_load_page(&mut st).await
                {
                    eprintln!("finsta: dm: reloading the inbox failed: {e:#}");
                }
            }
            None
        };

        let threads: Vec<Thread> = st
            .store
            .sorted_keys()
            .into_iter()
            .filter(|k| {
                older_than.is_none_or(|ts| st.store.threads.get(k).is_some_and(|t| t.last_activity_ms < ts))
            })
            .filter_map(|k| parse(&st.store.thread_value(k, &viewer_pk, 1, None)))
            .collect();
        Ok(InboxPage {
            has_older: st.store.more_threads && connected,
            oldest_cursor: Some("older".into()),
            threads,
        })
    }

    pub async fn thread(&self, thread_id: &str, cursor: Option<String>) -> Result<Thread> {
        let key: i64 = thread_id.parse().map_err(|_| anyhow!("Unknown conversation"))?;
        let mut st = self.inner.dm.lock().await;
        self.dm_ensure(&mut st).await?;
        let (ref_ts, ref_mid) = match &cursor {
            Some(c) => {
                let (ts, mid) = c.split_once(':').unwrap_or((c.as_str(), ""));
                (ts.parse::<i64>().unwrap_or(now_ms()), mid.to_string())
            }
            None => (now_ms() + 86_400_000, String::new()),
        };
        if st.conn.as_ref().is_some_and(|c| c.is_alive()) {
            let payload = json!({
                "thread_key": key, "direction": 0, "reference_timestamp_ms": ref_ts,
                "reference_message_id": ref_mid, "sync_group": 1, "cursor": null,
            });
            let what = if cursor.is_some() { "older messages" } else { "messages" };
            let _ = self.dm_tasks(&mut st, what, vec![task("228", payload, format!("mrq.{key}"))]).await;
        }
        if !st.store.threads.contains_key(&key) && !st.store.messages.contains_key(&key) {
            bail!("Couldn't load this conversation");
        }
        let before = cursor.map(|_| ref_ts);
        parse(&st.store.thread_value(key, &self.user_id(), PAGE_SIZE, before))
            .ok_or_else(|| anyhow!("Could not read conversation"))
    }

    /// The existing 1:1 conversation with `user_pk`, if there is one.
    pub async fn thread_with_user(&self, user_pk: &str) -> Result<Option<Thread>> {
        let Ok(id) = user_pk.parse::<i64>() else { return Ok(None) };
        let mut st = self.inner.dm.lock().await;
        self.dm_ensure(&mut st).await?;
        let key = st.store.sorted_keys().into_iter().find(|k| st.store.others(*k) == vec![id]);
        Ok(key.and_then(|k| parse(&st.store.thread_value(k, &self.user_id(), PAGE_SIZE, None))))
    }

    /// Sends a text message and returns the id of the conversation it went to.
    pub async fn send_text(&self, to: &Recipient, text: &str) -> Result<Option<String>> {
        let key: i64 = match to {
            Recipient::Thread(id) => id.parse().map_err(|_| anyhow!("Unknown conversation"))?,
            Recipient::User(pk) => match self.thread_with_user(pk).await? {
                Some(t) => t.thread_id.parse()?,
                None => bail!("Finsta can't start new conversations yet. Send the first message on instagram.com."),
            },
        };
        let mut st = self.inner.dm.lock().await;
        self.dm_ensure(&mut st).await?;
        if !st.conn.as_ref().is_some_and(|c| c.is_alive()) {
            bail!("Couldn't connect to Instagram's chat. Try again in a minute.");
        }
        let has_links = text.contains("http://") || text.contains("https://");
        let send = json!({
            "thread_id": key, "otid": time_id().to_string(), "source": 65537, "send_type": 1,
            "sync_group": 1, "text": text, "initiating_source": 1, "skip_url_preview_gen": 0,
            "text_has_links": i32::from(has_links), "multitab_env": 0,
        });
        let read = json!({"thread_id": key, "last_read_watermark_ts": now_ms(), "sync_group": 1});
        self.dm_tasks(
            &mut st,
            "send",
            vec![task("46", send, key.to_string()), task("21", read, key.to_string())],
        )
        .await?;
        Ok(Some(key.to_string()))
    }

    pub async fn mark_seen(&self, thread_id: &str, item_id: &str) -> Result<()> {
        let Ok(key) = thread_id.parse::<i64>() else { return Ok(()) };
        let mut st = self.inner.dm.lock().await;
        if !st.conn.as_ref().is_some_and(|c| c.is_alive()) {
            return Ok(());
        }
        let ts = st.store.message_index.get(item_id).map(|(_, ts)| *ts).unwrap_or_else(now_ms);
        let read = json!({"thread_id": key, "last_read_watermark_ts": ts, "sync_group": 1});
        self.dm_tasks(&mut st, "mark read", vec![task("21", read, key.to_string())]).await
    }

    /// Opening disappearing photos goes through a part of the chat protocol
    /// Finsta doesn't speak yet.
    pub async fn mark_visual_seen(&self, _thread_id: &str, _item_id: &str, _replay: bool) -> Result<()> {
        bail!("Disappearing photos can't be opened in Finsta yet. Open them on instagram.com.")
    }
}
