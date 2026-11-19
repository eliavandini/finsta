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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

mod query;
mod slide;

use super::lightspeed::{self, AttachmentKind, LsAttachment, LsContact, LsMessage, LsThread, Tables};
use super::mqtt::Conn;
use super::{
    Client, InboxPage, User, LoginRequired, RateLimited, Recipient, Thread, VERIFY, WEB_API, cooldown_remaining, parse,
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
    store: Store,
    conn: Option<Conn>,
    last_connect_attempt: Option<Instant>,
    /// Whether the conversations have been loaded at least once.
    inbox_loaded: bool,
    last_sync_attempt: Option<Instant>,
    /// instagram.com's current inbox (GraphQL).
    slide: slide::SlideState,
    /// Which payload kinds have had their shapes logged already.
    logged: HashSet<String>,
}

#[derive(Clone, Default)]
struct PageConfig {
    app_id: i64,
    client_id: String,
    endpoint: String,
    version: Option<String>,
    /// (where it came from, id) to log in to the chat connection with, best first.
    mqtt_users: Vec<(&'static str, String)>,
    /// Ids from the page that could be the viewer's id in the chat system.
    viewer_candidates: HashSet<i64>,
    /// The GraphQL query that loads the inbox: (name, id).
    doc: Option<(String, String)>,
    /// `sync_params` for the inbox database.
    mailbox_params: String,
    dtsg: Option<String>,
    lsd: Option<String>,
    /// The account GraphQL requests act as ("av").
    av: String,
    /// Extra form fields the website sends with its GraphQL requests.
    site: Vec<(&'static str, String)>,
    /// MqttWebConfig.hostNameOverride, when it names one of Meta's hosts.
    host_override: Option<String>,
    /// Chat login settings from MqttWebConfig: cp, ecp, chat_on, st.
    mqtt_params: Value,
    /// The website's GraphQL operations: name -> id.
    ops: BTreeMap<String, String>,
    /// The device id the inbox query subscribes to updates with.
    iris_device_id: String,
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
    /// (name, id) of GraphQL queries the page names together with their ids.
    queries: Vec<(String, String)>,
    /// Message-related query and preloader names (diagnostics only).
    names: BTreeSet<String>,
    /// Message-related object keys (diagnostics only).
    dm_keys: BTreeSet<String>,
}

const DM_WORDS: [&str; 9] = ["Direct", "IGD", "LS", "Lightspeed", "Inbox", "Thread", "Message", "Mqtt", "Mailbox"];

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 150 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// "adp_FooQueryRelayPreloader_64a1b2c3d4e5f_0" -> "adp_FooQueryRelayPreloader"
fn strip_suffix_ids(s: &str) -> String {
    let parts: Vec<&str> = s.split('_').collect();
    let is_id = |p: &&str| {
        !p.is_empty()
            && (p.chars().all(|c| c.is_ascii_digit())
                || (p.len() >= 6 && p.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())))
    };
    let keep = parts.iter().rposition(|p| !is_id(p)).map(|i| i + 1).unwrap_or(parts.len());
    parts[..keep].join("_")
}

fn digits(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() && s.len() <= 25 && s.chars().all(|c| c.is_ascii_digit()) => Some(s.clone()),
        Value::Number(n) if n.is_u64() => Some(n.to_string()),
        _ => None,
    }
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
            if let Some(id) = ["queryID", "queryId", "doc_id"].iter().find_map(|k| m.get(*k).and_then(digits)) {
                let name = ["queryName", "preloaderID", "name", "operationName"]
                    .iter()
                    .find_map(|k| m.get(*k)?.as_str())
                    .filter(|n| is_identifier(n))
                    .unwrap_or("unnamed");
                page.queries.push((strip_suffix_ids(name), id));
            }
            for (k, x) in m {
                if page.dm_keys.len() < 200 && is_identifier(k) && !k.chars().all(|c| c.is_ascii_digit()) {
                    let lower = k.to_ascii_lowercase();
                    if ["lightspeed", "igd", "direct", "inbox", "thread", "message", "mailbox", "mqtt"]
                        .iter()
                        .any(|w| lower.contains(w))
                    {
                        page.dm_keys.insert(k.clone());
                    }
                }
                if k.starts_with("lightspeed_web_request")
                    && let Some(p) = x["payload"].as_str()
                {
                    page.payloads.push((k.clone(), p.to_string()));
                }
                scan(x, page);
            }
        }
        Value::String(s)
            if page.names.len() < 300
                && is_identifier(s)
                && (s.starts_with("adp_")
                    || ["Query", "Mutation", "Subscription", "RelayOperation"].iter().any(|e| s.ends_with(e)))
                && DM_WORDS.iter().any(|w| s.contains(w)) =>
        {
            page.names.insert(strip_suffix_ids(s));
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
    page.scripts = script_urls(html);
    page
}

/// Script bundle addresses in `text`: the website's (or the mock's, in tests).
fn script_urls(text: &str) -> Vec<String> {
    let unescaped = text.replace("\\/", "/");
    let base = web_base();
    let local = if base == WEB { String::new() } else { format!("|{}", regex::escape(base.trim_end_matches('/'))) };
    let js = regex::Regex::new(&format!(
        r#"(?:https://static\.cdninstagram\.com{local})/rsrc\.php/[A-Za-z0-9_\-./]+\.js(?:\?[A-Za-z0-9_\-=&.%]*)?"#
    ))
    .unwrap();
    let mut seen = HashSet::new();
    js.find_iter(&unescaped).map(|m| m.as_str().to_string()).filter(|u| seen.insert(u.clone())).collect()
}

/// Whether Finsta may download a script from `url`: only the website's own
/// hosts (or the mock's).
fn script_allowed(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    let host = u.host_str().unwrap_or_default();
    let base_host = url::Url::parse(&web_base()).ok().and_then(|b| b.host_str().map(str::to_string));
    (u.scheme() == "https" && (host.ends_with(".cdninstagram.com") || host == "www.instagram.com"))
        || base_host.as_deref() == Some(host) && web_base() != WEB
}

/// Script addresses inside a config object (such as a web worker's).
fn collect_urls(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s)
            if (s.starts_with("http") || s.starts_with('/'))
                && (s.contains(".js") || s.contains("rsrc.php") || s.contains("worker")) =>
        {
            let url = if s.starts_with('/') { format!("{}{s}", web_base().trim_end_matches('/')) } else { s.clone() };
            if script_allowed(&url) && !out.contains(&url) {
                out.push(url);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_urls(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_urls(x, out)),
        _ => {}
    }
}

/// A config object's keys with the kind of each value (never the values).
fn shape(v: &Value) -> String {
    fn kind(v: &Value) -> &'static str {
        match v {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "num",
            Value::String(s) if s.starts_with("http") || s.starts_with('/') => "url",
            Value::String(_) => "str",
            Value::Array(_) => "list",
            Value::Object(_) => "obj",
        }
    }
    match v {
        Value::Object(m) => m.iter().map(|(k, x)| format!("{k}:{}", kind(x))).collect::<Vec<_>>().join(","),
        other => kind(other).to_string(),
    }
}

/// Object key paths (never values) below `v`, for logging a response's structure.
fn key_paths(v: &Value, prefix: &str, depth: usize, out: &mut BTreeSet<String>) {
    if depth == 0 || out.len() > 200 {
        return;
    }
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, x) in m {
                let k = if is_identifier(k) && !k.chars().all(|c| c.is_ascii_digit()) { k.as_str() } else { "#" };
                key_paths(x, &format!("{prefix}.{k}"), depth - 1, out);
            }
        }
        // Arrays: their length, then the structure of the first element.
        Value::Array(a) if !a.is_empty() => key_paths(&a[0], &format!("{prefix}[{}]", a.len()), depth - 1, out),
        Value::Array(_) => {
            out.insert(format!("{prefix}[0]"));
        }
        other => {
            let kind = match other {
                Value::Null => "null",
                Value::Bool(_) => "b",
                Value::Number(_) => "n",
                Value::String(_) => "s",
                _ => "{}",
            };
            out.insert(format!("{prefix}:{kind}"));
        }
    }
}

/// What an id field looks like, without its value.
fn id_format(v: &Value) -> String {
    match v {
        Value::Null => "absent".into(),
        Value::String(s) if s.is_empty() => "empty".into(),
        Value::String(s) if s == "0" => "\"0\"".into(),
        Value::Number(n) if n.as_i64() == Some(0) => "0".into(),
        Value::String(s) if s.chars().all(|c| c.is_ascii_digit()) => format!("{} digits", s.len()),
        Value::Number(n) => format!("number of {} digits", n.to_string().len()),
        Value::String(s) => format!("text of {} chars", s.len()),
        _ => "other".into(),
    }
}

/// An address's scheme, host, path and query keys (never query values).
fn describe_url(s: &str) -> String {
    match url::Url::parse(s) {
        Ok(u) => format!(
            "{}://{}{} (query keys {:?})",
            u.scheme(),
            u.host_str().unwrap_or_default(),
            u.path(),
            u.query_pairs().map(|(k, _)| k.into_owned()).collect::<Vec<_>>()
        ),
        Err(_) if s.is_empty() => "absent".into(),
        Err(_) => format!("not an address ({} chars)", s.len()),
    }
}

fn meta_host(h: &str) -> bool {
    !h.is_empty()
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && [".facebook.com", ".instagram.com", ".fbcdn.net", ".meta.com", ".messenger.com"].iter().any(|d| h.ends_with(d))
}

/// Which of the page's ids are the same number, with digit counts only.
fn id_relations(named: &[(&str, Option<i64>)]) -> String {
    let mut groups: Vec<(i64, Vec<&str>)> = Vec::new();
    let mut missing = Vec::new();
    for (name, v) in named {
        match v {
            Some(v) => match groups.iter_mut().find(|(g, _)| g == v) {
                Some((_, names)) => names.push(name),
                None => groups.push((*v, vec![name])),
            },
            None => missing.push(*name),
        }
    }
    let mut parts: Vec<String> =
        groups.iter().map(|(v, names)| format!("{} ({} digits)", names.join("="), v.to_string().len())).collect();
    if !missing.is_empty() {
        parts.push(format!("missing: {}", missing.join(",")));
    }
    parts.join("; ")
}

/// A short, printable excerpt of the website's own code around `needle`.
fn snippet(text: &str, needle: &str) -> Option<String> {
    let i = text.find(needle)?;
    let mut start = i.saturating_sub(60);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (i + 200).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    Some(text[start..end].chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '.' }).collect())
}

fn find_version(text: &str) -> Option<String> {
    let re = regex::Regex::new(r#"__d\("LSVersion"[\s\S]{0,80}?"(\d{10,25})""#).unwrap();
    re.captures(text).map(|c| c[1].to_string())
}

/// The GraphQL query that loads the inbox: (name, id). Prefers Instagram's own variant.
fn find_doc(text: &str) -> Option<(String, String)> {
    let re = regex::Regex::new(
        r#"__d\("(LSPlatformGraphQLLightspeedRequest(?:ForIGD)?Query)_[A-Za-z]*RelayOperation"[\s\S]{0,80}?"(\d{8,25})""#,
    )
    .unwrap();
    let mut best = None;
    for c in re.captures_iter(text) {
        let found = (c[1].to_string(), c[2].to_string());
        if found.0.ends_with("ForIGDQuery") {
            return Some(found);
        }
        best.get_or_insert(found);
    }
    best
}

fn numeric(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) if s.len() >= 5 && s.chars().all(|c| c.is_ascii_digit()) => s.parse().ok(),
        _ => None,
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Details from the website's scripts: the protocol version and the inbox query.
#[derive(Clone, Default)]
struct Meta {
    version: Option<String>,
    doc: Option<(String, String)>,
    /// Message-related GraphQL operations the scripts define: name -> id.
    ops: BTreeMap<String, String>,
}

/// Bumped when the way scripts are searched changes, so old results are redone.
const META_SCANNER: u64 = 11;

/// How a story reply was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryReply {
    /// As a reply to the story.
    Reply,
    /// As a message with a link to the story (when replies didn't work).
    Message,
}

/// Website code that calls the operations Finsta uses: saved (to
/// webcode/callers/) to work out the inputs the website passes them.
const CALLER_NEEDLES: &[&str] = &[
    "PolarisProfilePostsQuery",
    "PolarisProfilePageContentQuery",
    "PolarisStoriesV3ReelPageStandaloneQuery",
    "PolarisStoriesV3TrayContainerQuery",
    "usePolarisLikeMediaXIGLikeMutation",
    "PolarisAPIReelSeenMutation",
    "usePolarisStoryLikesLikeMutation",
    "IGDirectStoryShareReplyMutation",
    "IGDirectTextSendMutation",
    "useIGDMarkThreadAsReadMutation",
    "device_id_for_iris_subscription",
    "PolarisDirectInboxQuery",
    "IGDMessageListOffMsysQuery",
    "IGDThreadListOffMsysPaginationQuery",
    "PolarisDirectIrisSubscription",
    "IGDSlideDeltaProcessorQuery",
    // How the website builds story replies and reactions.
    "story_share_reply",
    "IGDirectReactionSendMutation",
    "xig_direct_reaction_send",
    // Search, saved posts and single posts.
    "PolarisSearchBoxRefetchableQuery",
    "PolarisPostRootQuery",
    "IGDOmniPickerSearchResultsListQuery",
    // How the website builds what it sends in chats (reactions, shared
    // posts, reels and stories), and who calls those builders.
    "publish({commitMutationConfig",
    "IGDMAPISend",
    "IGDirectMediaShareMutation",
    "IGDirectStoryShareMutation",
    "IGDirectReelShareMutation",
    "IGDirectLinkShareMutation",
    // Sending photos, videos and GIFs: the upload, and the website's GIF picker.
    "IGDirectMediaSendMutation",
    "IGDirectAnimatedMediaSendMutation",
    "mediaStaging",
    "rupload",
    "IGDComposerRichContentGifs",
    // Saving posts, and folders.
    "usePolarisSaveMediaSaveMutation",
    "usePolarisMediaSaveToCollectionAddMediaMutation",
    "PolarisAPIEditCollectionAddMediaMutation",
    // The hashtag page.
    "PolarisTagPage",
    "PolarisHashtag",
    "explore/tags/",
];

/// Places in the website's own code worth quoting in the log while working
/// out how its messaging works now. Only ever matched against public scripts.
const NEEDLES: [&str; 11] = [
    "\"LSVersion\"",
    "ls_sv",
    "version_id",
    "/ls_req",
    "hostNameOverride",
    "edge-chat",
    "bootloader-endpoint",
    "IGDAWMainV4",
    "DGWHostnameOverride",
    "lightspeed_web_request",
    "MqttWebConfig",
];

fn version_cache_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("finsta").join("lightspeed.json"))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cached_meta() -> Option<Meta> {
    let text = std::fs::read_to_string(version_cache_path()?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let meta = Meta {
        version: v["version"].as_str().map(str::to_string),
        doc: v["doc_name"].as_str().zip(v["doc_id"].as_str()).map(|(a, b)| (a.to_string(), b.to_string())),
        ops: v["ops"]
            .as_object()
            .map(|o| o.iter().filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_string()))).collect())
            .unwrap_or_default(),
    };
    // A found version goes stale with website releases; a search that found
    // nothing is expensive (all scripts) and not worth repeating daily.
    let max_age = if meta.version.is_some() {
        24 * 3600
    } else if !meta.ops.is_empty() {
        3 * 24 * 3600
    } else {
        7 * 24 * 3600
    };
    if v["scanner"].as_u64() != Some(META_SCANNER) || now_secs().saturating_sub(v["saved"].as_u64()?) > max_age {
        return None;
    }
    Some(meta)
}

fn save_meta(meta: &Meta) {
    let Some(path) = version_cache_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = json!({
        "scanner": META_SCANNER,
        "saved": now_secs(),
        "version": meta.version,
        "doc_name": meta.doc.as_ref().map(|d| &d.0),
        "doc_id": meta.doc.as_ref().map(|d| &d.1),
        "ops": meta.ops,
    });
    let _ = std::fs::write(path, v.to_string());
}

/// An answer from instagram.com's GraphQL endpoint.
struct GraphqlAnswer {
    status: u16,
    bytes: usize,
    parts: usize,
    html: bool,
    json: Option<Value>,
}

impl GraphqlAnswer {
    /// Log the answer's structure: key paths, array sizes and errors, never
    /// values. `area` prefixes the lines ("dm", "web").
    fn log(&self, area: &str, what: &str) {
        let Some(v) = &self.json else {
            let kind = if self.html { "an HTML page" } else { "not JSON" };
            eprintln!("finsta: {area}: {what} -> HTTP {}, {} bytes, {kind}", self.status, self.bytes);
            return;
        };
        let mut paths = BTreeSet::new();
        key_paths(&v["data"], "data", 14, &mut paths);
        let top: Vec<&String> = v.as_object().map(|o| o.keys().collect()).unwrap_or_default();
        eprintln!(
            "finsta: {area}: {what} -> HTTP {}, {} bytes in {} part(s), keys {top:?}, data {paths:?}",
            self.status, self.bytes, self.parts
        );
        for e in v["errors"].as_array().into_iter().flatten().take(4) {
            let text = |k: &str| e[k].as_str().unwrap_or_default().chars().take(200).collect::<String>();
            let path: Vec<String> = e["path"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| match p {
                    Value::String(s) if is_identifier(s) => s.clone(),
                    Value::Number(_) => "#".into(),
                    _ => "?".into(),
                })
                .collect();
            eprintln!(
                "finsta: {area}: {what} error: code {} severity {} path {path:?} \"{}\" {}",
                e["code"],
                e["severity"].as_str().unwrap_or("-"),
                text("message"),
                text("description")
            );
        }
        if !v["error"].is_null() {
            let summary: String = v["errorSummary"].as_str().unwrap_or_default().chars().take(160).collect();
            let description: String = v["errorDescription"].as_str().unwrap_or_default().chars().take(160).collect();
            eprintln!("finsta: {area}: {what} error {}: \"{summary}\" {description}", v["error"]);
        }
    }
}

/// Where the website's code modules for the operations Finsta needs are kept
/// (public code, for working out their inputs and answers).
fn webcode_dir() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("finsta").join("webcode"))
}

/// Operations whose code is worth keeping: messages, profiles, stories, feed.
fn interesting_op(name: &str) -> bool {
    [
        "IGD", "useIGD", "IGDirect", "PolarisDirect", "PolarisProfile", "PolarisStor", "PolarisFeed", "PolarisPost",
        "PolarisUser", "PolarisMedia", "PolarisTimeline", "usePolarisLikeMedia", "usePolarisStor", "usePolarisReel",
        "PolarisAPI", "PolarisReel", "PolarisSearch", "usePolarisSave", "PolarisSaved", "usePolarisComment",
        "usePolarisMediaSaveToCollection", "PolarisShareSheet", "PolarisTagPage", "PolarisHashtag",
    ]
    .iter()
    .any(|p| name.starts_with(p))
}

fn found<T>(x: &Option<T>) -> &'static str {
    if x.is_some() { "found" } else { "unknown" }
}

fn task(label: &str, payload: Value, queue: String) -> (String, Value, String) {
    (label.to_string(), payload, queue)
}

const GRAPHQL_KEY: &str = "graphql/inbox";
const INBOX_REFERER: &str = "https://www.instagram.com/direct/inbox/";

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

    /// The protocol version (needed for every chat request) and the id of the
    /// GraphQL query that loads the inbox. Both live in the website's scripts,
    /// which change with every release, so they're looked up once a day.
    async fn ls_meta(&self, html: &str, page: &ParsedPage, worker_urls: Vec<String>) -> Meta {
        let mut meta = Meta {
            version: find_version(html),
            doc: find_doc(html)
                .or_else(|| page.queries.iter().find(|(n, _)| n.contains("LightspeedRequest")).cloned()),
            ops: BTreeMap::new(),
        };
        if meta.version.is_some() {
            eprintln!("finsta: dm: protocol version found in the page");
        }
        if let Some((name, _)) = &meta.doc {
            eprintln!("finsta: dm: inbox query ({name}) found in the page");
        }
        if meta.version.is_some() && meta.doc.is_some() {
            save_meta(&meta);
            return meta;
        }
        if let Some(c) = cached_meta() {
            eprintln!(
                "finsta: dm: using saved script details (version {}, inbox query {})",
                found(&c.version),
                found(&c.doc)
            );
            meta.version = meta.version.or(c.version);
            meta.doc = meta.doc.or(c.doc);
            meta.ops = c.ops;
            return meta;
        }

        let ua = self.user_agent();
        let workers: HashSet<String> = worker_urls.iter().cloned().collect();
        let mut queue: VecDeque<String> = worker_urls.into_iter().collect();
        queue.extend(page.scripts.iter().cloned());
        let mut seen: HashSet<String> = HashSet::new();
        let (mut checked, mut failed, mut bytes) = (0usize, 0usize, 0usize);
        let mut mentions = [0usize; NEEDLES.len()];
        let mut snippets: [Option<String>; NEEDLES.len()] = Default::default();
        let op_def = regex::Regex::new(
            r#"__d\("([A-Za-z0-9]+(?:Query|Mutation|Subscription))_[a-z][A-Za-z]*RelayOperation"[\s\S]{0,80}?"(\d{8,25})""#,
        )
        .unwrap();
        let module = regex::Regex::new(r#"__d\("([A-Za-z0-9_]+)(\.graphql|\$Parameters|\.relayprovider)","#).unwrap();
        let any_module = regex::Regex::new(r#"__d\("([A-Za-z0-9_$.\-]+)","#).unwrap();
        let mut saved_callers = 0usize;
        let code_dir = webcode_dir();
        if let Some(dir) = &code_dir {
            let _ = std::fs::remove_dir_all(dir);
            let _ = std::fs::create_dir_all(dir);
        }
        let mut saved_modules = 0usize;
        let op_name = regex::Regex::new(
            r"\b((?:IGD|PolarisDirect|PolarisIGD|IGDirect|PolarisInbox)[A-Za-z0-9]*(?:Query|Mutation|Subscription))\b",
        )
        .unwrap();
        let mut named_ops: BTreeSet<String> = BTreeSet::new();
        while checked + failed < 1000 {
            let mut batch = Vec::new();
            while batch.len() < 8 {
                let Some(u) = queue.pop_front() else { break };
                if script_allowed(&u) && seen.insert(u.clone()) {
                    batch.push(u);
                }
            }
            if batch.is_empty() {
                break;
            }
            let fetches = batch.iter().map(|url| {
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
                async move {
                    let r = req.send().await.ok()?;
                    if !r.status().is_success() {
                        return None;
                    }
                    r.text().await.ok()
                }
            });
            let results = futures_util::future::join_all(fetches).await;
            let mut next = Vec::new();
            for (url, text) in batch.iter().zip(results) {
                let Some(text) = text else {
                    failed += 1;
                    continue;
                };
                checked += 1;
                bytes += text.len();
                let worker = workers.contains(url);
                let place = if worker { "the inbox worker's script".to_string() } else { format!("script {checked}") };
                if meta.version.is_none()
                    && let Some(v) = find_version(&text)
                {
                    eprintln!("finsta: dm: protocol version found in {place}");
                    meta.version = Some(v);
                }
                if meta.doc.is_none()
                    && let Some(d) = find_doc(&text)
                {
                    eprintln!("finsta: dm: inbox query ({}) found in {place}", d.0);
                    meta.doc = Some(d);
                }
                for (i, needle) in NEEDLES.iter().enumerate() {
                    if text.contains(needle) {
                        mentions[i] += 1;
                        if snippets[i].is_none() {
                            snippets[i] = snippet(&text, needle);
                        }
                    }
                }
                for c in op_def.captures_iter(&text) {
                    meta.ops.insert(c[1].to_string(), c[2].to_string());
                }
                if let Some(dir) = &code_dir {
                    for c in module.captures_iter(&text) {
                        if !interesting_op(&c[1]) {
                            continue;
                        }
                        let start = c.get(0).unwrap().start();
                        let rest = &text[start + 4..];
                        let mut len = rest.find("__d(\"").map(|i| i + 4).unwrap_or(rest.len() + 4).min(400_000);
                        while !text.is_char_boundary(start + len) {
                            len -= 1;
                        }
                        let suffix = match &c[2] {
                            ".graphql" => ".graphql",
                            "$Parameters" => ".parameters",
                            _ => ".relayprovider",
                        };
                        let file = dir.join(format!("{}{suffix}.js", &c[1]));
                        if std::fs::write(file, &text[start..start + len]).is_ok() {
                            saved_modules += 1;
                        }
                    }
                    if CALLER_NEEDLES.iter().any(|n| text.contains(n)) {
                        let callers = dir.join("callers");
                        let _ = std::fs::create_dir_all(&callers);
                        let starts: Vec<(usize, String)> =
                            any_module.captures_iter(&text).map(|c| (c.get(0).unwrap().start(), c[1].to_string())).collect();
                        for (i, (start, name)) in starts.iter().enumerate() {
                            let end = starts.get(i + 1).map(|(e, _)| *e).unwrap_or(text.len());
                            let body = &text[*start..end];
                            if saved_callers >= 400
                                || body.len() > 400_000
                                || name.ends_with(".graphql")
                                || name.ends_with("$Parameters")
                                || name.ends_with("RelayOperation")
                                || !CALLER_NEEDLES.iter().any(|n| body.contains(n))
                            {
                                continue;
                            }
                            if std::fs::write(callers.join(format!("{name}.js")), body).is_ok() {
                                saved_callers += 1;
                            }
                        }
                    }
                }
                if named_ops.len() < 400 {
                    named_ops.extend(op_name.captures_iter(&text).map(|c| c[1].to_string()));
                }
                if worker {
                    let inner = script_urls(&text);
                    eprintln!("finsta: dm: the inbox worker's script refers to {} more scripts", inner.len());
                    next.extend(inner);
                }
            }
            for u in next.into_iter().rev() {
                queue.push_front(u);
            }
            if meta.version.is_some() && meta.doc.is_some() {
                break;
            }
        }
        eprintln!(
            "finsta: dm: searched {checked} scripts ({:.1} MB, {failed} failed, {} left)",
            bytes as f64 / 1e6,
            queue.len()
        );
        for (i, needle) in NEEDLES.iter().enumerate() {
            match &snippets[i] {
                Some(s) => eprintln!("finsta: dm: website code, {needle} ({} scripts): {s}", mentions[i]),
                None => eprintln!("finsta: dm: website code, {needle}: not found"),
            }
        }
        let message_ops: Vec<&String> = meta.ops.keys().filter(|n| interesting_op(n) && DM_WORDS.iter().any(|w| n.contains(w))).collect();
        eprintln!(
            "finsta: dm: {} operations with ids in the scripts, {} about messages; saved {saved_modules} code modules and {saved_callers} modules that use them",
            meta.ops.len(),
            message_ops.len()
        );
        let unnamed: Vec<&String> = named_ops.iter().filter(|n| !meta.ops.contains_key(*n)).collect();
        eprintln!("finsta: dm: other message operation names in the scripts: {unnamed:?}");
        save_meta(&meta);
        meta
    }

    /// Read the chat settings from the inbox page (and log their structure).
    async fn page_config(&self, html: &str, page: &ParsedPage) -> PageConfig {
        let def = |name: &str| page.defines.get(name).cloned().unwrap_or(Value::Null);
        let shape_of = |name: &str| page.defines.get(name).map(shape).unwrap_or_else(|| "missing".into());
        let interesting: BTreeSet<&String> = page
            .defines
            .keys()
            .filter(|k| {
                ["Mqtt", "LS", "Lightspeed", "CurrentUser", "Version", "IGD", "Direct", "DTSG", "Worker", "SiteData"]
                    .iter()
                    .any(|p| k.contains(p))
            })
            .collect();
        eprintln!(
            "finsta: dm: inbox page: {} bytes, {} config modules, {} scripts, payloads {:?}",
            html.len(),
            page.defines.len(),
            page.scripts.len(),
            page.payloads.iter().map(|(k, p)| format!("{k} ({} bytes)", p.len())).collect::<Vec<_>>()
        );
        eprintln!("finsta: dm: config modules of interest: {interesting:?}");
        for name in [
            "MqttWebConfig",
            "LSPlatformMessengerSyncParams",
            "IGDAWMainV4WebWorkerResource",
            "IGDMqttWebDeviceID",
            "LSPlatformDGWHostnameOverride",
            "DTSGInitialData",
            "LSD",
            "SiteData",
        ] {
            eprintln!("finsta: dm: {name}: {}", shape_of(name));
        }
        eprintln!("finsta: dm: message-related names in the page: {:?}", page.names);
        eprintln!("finsta: dm: message-related keys in the page: {:?}", page.dm_keys);
        let queries: BTreeSet<&String> = page.queries.iter().map(|(n, _)| n).take(80).collect();
        eprintln!("finsta: dm: queries with ids in the page: {queries:?}");

        let mqtt = def("MqttWebConfig");
        let user = def("CurrentUserInitialData");
        let pk: Option<i64> = self.user_id().parse().ok();
        let named = [
            ("MqttWebConfig.fbid", numeric(&mqtt["fbid"])),
            ("USER_ID", numeric(&user["USER_ID"])),
            ("ACCOUNT_ID", numeric(&user["ACCOUNT_ID"])),
            ("IG_USER_EIMU", numeric(&user["IG_USER_EIMU"])),
            ("NON_FACEBOOK_USER_ID", numeric(&user["NON_FACEBOOK_USER_ID"])),
            ("ds_user_id", pk),
        ];
        eprintln!("finsta: dm: ids: {}", id_relations(&named));
        eprintln!(
            "finsta: dm: id formats: MqttWebConfig.fbid {}, USER_ID {}, ACCOUNT_ID {}",
            id_format(&mqtt["fbid"]),
            id_format(&user["USER_ID"]),
            id_format(&user["ACCOUNT_ID"])
        );
        let mut mqtt_users: Vec<(&'static str, String)> = Vec::new();
        // The website logs in with MqttWebConfig.fbid, whatever it is.
        if let Some(raw) = scalar(&mqtt["fbid"]).filter(|f| numeric(&mqtt["fbid"]).is_none() && f.len() <= 25) {
            mqtt_users.push(("MqttWebConfig.fbid as is", raw));
        }
        for (label, v) in [named[0], named[1], named[4], named[3], named[5]] {
            if let Some(v) = v.filter(|v| *v > 0).map(|v| v.to_string())
                && !mqtt_users.iter().any(|(_, u)| *u == v)
            {
                mqtt_users.push((label, v));
            }
        }
        let endpoint = mqtt["endpoint"].as_str().unwrap_or_default();
        eprintln!(
            "finsta: dm: chat endpoint {}; polling endpoint {}",
            describe_url(endpoint),
            describe_url(mqtt["pollingEndpoint"].as_str().unwrap_or_default())
        );
        let host_override = mqtt["hostNameOverride"].as_str().filter(|h| meta_host(h)).map(str::to_string);
        eprintln!(
            "finsta: dm: chat hostNameOverride: {}; DGW override in use: {}",
            match (&host_override, mqtt["hostNameOverride"].as_str()) {
                (Some(h), _) => h.clone(),
                (None, Some("")) => "empty".into(),
                (None, Some(other)) => format!("not a Meta host ({} chars)", other.len()),
                (None, None) => "absent".into(),
            },
            def("LSPlatformDGWHostnameOverride")["useHostNameOverride"]
        );
        let mut mqtt_params = json!({"cp": 3, "ecp": 10, "chat_on": true, "st": []});
        for (key, field) in [("cp", "clientCapabilities"), ("ecp", "capabilities"), ("chat_on", "chatVisibility")] {
            if mqtt[field].is_number() || mqtt[field].is_boolean() {
                mqtt_params[key] = mqtt[field].clone();
            }
        }
        if let Some(topics) = mqtt["subscribedTopics"].as_array().filter(|t| t.iter().all(Value::is_string)) {
            mqtt_params["st"] = Value::Array(topics.clone());
        }
        eprintln!(
            "finsta: dm: chat login settings: cp {} ecp {} chat_on {} topics {}",
            mqtt_params["cp"],
            mqtt_params["ecp"],
            mqtt_params["chat_on"],
            mqtt_params["st"]
        );
        let av = [named[4].1, named[3].1, named[0].1, named[2].1, named[1].1]
            .into_iter()
            .flatten()
            .find(|v| *v > 0)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "0".into());
        let mut candidates: HashSet<i64> =
            user.as_object().map(|o| o.values().filter_map(numeric).collect()).unwrap_or_default();
        candidates.extend(named.iter().filter_map(|(_, v)| *v));

        let sync = def("LSPlatformMessengerSyncParams");
        let mailbox_params = sync["mailbox"]
            .as_str()
            .filter(|s| serde_json::from_str::<Value>(s).is_ok_and(|v| v.is_object()))
            .map(str::to_string)
            .unwrap_or_else(|| json!({"locale": "en_US"}).to_string());
        let mailbox_keys = serde_json::from_str::<Value>(&mailbox_params)
            .ok()
            .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()))
            .unwrap_or_default();
        eprintln!(
            "finsta: dm: inbox sync settings {} (keys {mailbox_keys:?})",
            if sync["mailbox"].is_string() { "from the page" } else { "assumed" }
        );

        let dtsg = ["DTSGInitialData", "DTSGInitData"].iter().find_map(|n| scalar(&def(n)["token"]));
        let lsd = scalar(&def("LSD")["token"]);
        let site_data = def("SiteData");
        let site: Vec<(&'static str, String)> = [
            ("__spin_r", "__spin_r"),
            ("__spin_b", "__spin_b"),
            ("__spin_t", "__spin_t"),
            ("__hs", "haste_session"),
            ("__hsi", "hsi"),
            ("__rev", "server_revision"),
        ]
        .into_iter()
        .filter_map(|(field, key)| scalar(&site_data[key]).map(|v| (field, v)))
        .collect();
        eprintln!(
            "finsta: dm: request tokens: fb_dtsg {}, lsd {}, site fields {}",
            found(&dtsg),
            found(&lsd),
            site.len()
        );

        let worker_name = def("IGDAWMainV4WebWorkerResource")["name"].as_str().filter(|n| is_identifier(n)).unwrap_or("-").to_string();
        eprintln!(
            "finsta: dm: inbox worker name {worker_name}; WebWorkerConfig: {}; WorkerConfig: {}",
            shape_of("WebWorkerConfig"),
            shape_of("WorkerConfig")
        );
        let mut worker_urls = Vec::new();
        for (name, v) in &page.defines {
            if name.contains("Worker") && (name.contains("IGD") || name.contains("LS")) {
                collect_urls(v, &mut worker_urls);
            }
        }
        eprintln!("finsta: dm: {} inbox worker script(s) to search first", worker_urls.len());
        let meta = self.ls_meta(html, page, worker_urls).await;

        PageConfig {
            app_id: numeric(&mqtt["appID"]).unwrap_or(DEFAULT_APP_ID),
            client_id: ["MqttWebDeviceID", "IGDMqttWebDeviceID"]
                .iter()
                .find_map(|n| def(n)["clientID"].as_str().map(str::to_string))
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            endpoint: endpoint.to_string(),
            version: meta.version,
            mqtt_users,
            viewer_candidates: candidates,
            doc: meta.doc,
            mailbox_params,
            dtsg,
            lsd,
            av,
            site,
            host_override,
            mqtt_params,
            ops: meta.ops,
            iris_device_id: [("IGDMqttWebDeviceID", "clientId"), ("IGDMqttWebDeviceID", "clientID"), ("MqttWebDeviceID", "clientID")]
                .iter()
                .find_map(|(n, k)| def(n)[*k].as_str().filter(|v| !v.is_empty()).map(str::to_string))
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        }
    }

    /// Load (or reload) the inbox page: its chat settings, and the
    /// conversations if it still carries them.
    async fn dm_load_page(&self, st: &mut DmState) -> Result<()> {
        let html = self.load_inbox_page().await?;
        let page = parse_page(&html);
        if st.config.is_none() {
            st.config = Some(self.page_config(&html, &page).await);
        }
        if page.payloads.is_empty() && !st.inbox_loaded {
            eprintln!("finsta: dm: the inbox page carries no conversations; fetching them separately");
        }
        for (key, payload) in &page.payloads {
            if self.dm_apply_payload(st, payload, &format!("page {key}")) {
                st.inbox_loaded = true;
            }
        }
        Ok(())
    }

    /// Apply a Lightspeed payload; false if it couldn't be read at all.
    fn dm_apply_payload(&self, st: &mut DmState, payload: &str, what: &str) -> bool {
        let calls = match lightspeed::decode(payload) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("finsta: dm: {what}: payload isn't readable: {e}");
                return false;
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
        !calls.is_empty()
    }

    fn dm_apply_response(&self, st: &mut DmState, v: &Value, what: &str) -> bool {
        match v["payload"].as_str() {
            Some(p) if !p.is_empty() => self.dm_apply_payload(st, p, what),
            _ => {
                if st.logged.insert(format!("{what} (no payload)")) {
                    let keys = v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
                    eprintln!("finsta: dm: {what}: answer without payload, keys {keys:?}");
                }
                false
            }
        }
    }

    fn dm_drain(&self, st: &mut DmState) {
        let pushes = st.conn.as_ref().map(|c| c.take_pushes()).unwrap_or_default();
        for p in pushes {
            self.dm_apply_response(st, &p, "push");
        }
    }

    /// Make sure the page has been loaded, the chat connection is up (if
    /// possible) and the conversations have been fetched.
    async fn dm_ensure(&self, st: &mut DmState) -> Result<()> {
        if st.config.is_none() {
            self.dm_load_page(st).await?;
        }
        let alive = st.conn.as_ref().is_some_and(|c| c.is_alive());
        let recently = st.last_connect_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(60));
        // Without the protocol version the chat refuses every login (and
        // instagram.com itself no longer uses it for messages), so don't try.
        let usable = st.config.as_ref().is_some_and(|c| c.version.is_some()) || std::env::var("FINSTA_CHAT_URL").is_ok();
        if !alive && !recently && usable {
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
        if !st.inbox_loaded && st.last_sync_attempt.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
            st.last_sync_attempt = Some(Instant::now());
            self.dm_initial_sync(st).await?;
        }
        if !st.inbox_loaded {
            bail!("Couldn't load your conversations from instagram.com");
        }
        Ok(())
    }

    /// Fetch the conversations the way the website does once its inbox page
    /// has loaded: with a GraphQL query, or else over the chat connection.
    async fn dm_initial_sync(&self, st: &mut DmState) -> Result<()> {
        match self.dm_graphql_sync(st).await {
            Ok(true) => {
                st.inbox_loaded = true;
                return Ok(());
            }
            Ok(false) => {}
            Err(e) if e.is::<RateLimited>() || e.is::<LoginRequired>() => return Err(e),
            Err(e) => eprintln!("finsta: dm: inbox query failed: {e:#}"),
        }
        match self.dm_socket_sync(st, 1).await {
            Ok(true) => st.inbox_loaded = true,
            Ok(false) => {}
            Err(e) => eprintln!("finsta: dm: inbox sync over the chat connection failed: {e:#}"),
        }
        Ok(())
    }

    fn sync_request(cfg: &PageConfig, database: i64) -> Value {
        let mut p = json!({
            "database": database,
            "epoch_id": time_id(),
            "failure_count": null,
            "last_applied_cursor": null,
            "sync_params": cfg.mailbox_params,
        });
        if let Some(v) = cfg.version.as_ref().and_then(|v| v.parse::<i64>().ok()) {
            p["version"] = v.into();
        }
        p
    }

    /// One of the website's GraphQL operations, sent the way instagram.com
    /// sends it. Returns the first JSON part of the answer (answers with
    /// deferred parts come as several JSON documents, one per line).
    async fn web_graphql(
        &self,
        cfg: &PageConfig,
        name: &str,
        doc_id: &str,
        variables: &Value,
        referer: &str,
    ) -> Result<GraphqlAnswer> {
        let Some(dtsg) = cfg.dtsg.clone() else {
            bail!("no fb_dtsg token on the inbox page");
        };
        if cooldown_remaining(GRAPHQL_KEY).is_some() {
            return Err(RateLimited.into());
        }
        let jazoest = format!("2{}", dtsg.bytes().map(u32::from).sum::<u32>());
        let mut form: Vec<(&str, String)> = vec![
            ("av", cfg.av.clone()),
            ("__d", "www".into()),
            ("__user", "0".into()),
            ("__a", "1".into()),
            ("__req", "1".into()),
            ("dpr", "1".into()),
            ("__ccg", "GOOD".into()),
            ("__comet_req", "7".into()),
            ("fb_dtsg", dtsg),
            ("jazoest", jazoest),
        ];
        if let Some(lsd) = &cfg.lsd {
            form.push(("lsd", lsd.clone()));
        }
        form.extend(cfg.site.iter().map(|(k, v)| (*k, v.clone())));
        form.extend([
            ("fb_api_caller_class", "RelayModern".to_string()),
            ("fb_api_req_friendly_name", name.to_string()),
            ("variables", variables.to_string()),
            ("server_timestamps", "true".to_string()),
            ("doc_id", doc_id.to_string()),
        ]);
        let mut headers = self.headers(&Method::POST, WEB_API);
        let mut put = |k: &'static str, v: &str| {
            if let Ok(v) = HeaderValue::from_str(v) {
                headers.insert(HeaderName::from_static(k), v);
            }
        };
        put("referer", referer);
        put("x-fb-friendly-name", name);
        if let Some(lsd) = &cfg.lsd {
            put("x-fb-lsd", lsd);
        }
        let resp = self.inner.http.post(format!("{}api/graphql", web_base())).headers(headers).form(&form).send().await?;
        let status = resp.status();
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        self.absorb_headers(resp.headers());
        let text = resp.text().await?;
        if status.as_u16() == 429 {
            eprintln!("finsta: dm: {name} -> HTTP 429");
            start_cooldown(GRAPHQL_KEY, None);
            return Err(RateLimited.into());
        }
        if status.is_redirection() {
            let path = location.split('?').next().unwrap_or_default();
            if path.contains("/challenge") || path.contains("/checkpoint") {
                bail!(VERIFY);
            }
            if path.contains("/accounts/login") {
                return Err(LoginRequired.into());
            }
            bail!("{name} -> HTTP {status} {path}");
        }
        let body = text.trim_start().trim_start_matches("for (;;);");
        let mut parts = body.lines().filter(|l| !l.trim().is_empty());
        let first = parts.next().unwrap_or_default();
        // A pretty-printed answer is one document over several lines.
        let (json, parts) = match serde_json::from_str::<Value>(first) {
            Ok(v) => (Some(v), 1 + parts.count()),
            Err(_) => (serde_json::from_str::<Value>(body).ok(), 1),
        };
        Ok(GraphqlAnswer { status: status.as_u16(), bytes: text.len(), parts, html: body.starts_with('<'), json })
    }

    /// The website's "LSPlatformGraphQLLightspeedRequestForIGDQuery".
    async fn dm_graphql_sync(&self, st: &mut DmState) -> Result<bool> {
        let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
        let Some((name, doc_id)) = cfg.doc.clone() else {
            eprintln!("finsta: dm: skipping the Lightspeed inbox query: its id is unknown");
            return Ok(false);
        };
        let mut request = Self::sync_request(&cfg, 1);
        request.as_object_mut().map(|o| o.remove("failure_count"));
        let variables = json!({
            "deviceId": cfg.client_id,
            "includeChatVisibility": false,
            "requestId": 0,
            "requestPayload": request.to_string(),
            "requestType": 1,
        });
        let answer = self.web_graphql(&cfg, &name, &doc_id, &variables, INBOX_REFERER).await?;
        answer.log("dm", "inbox query");
        let Some(v) = answer.json else { return Ok(false) };
        let mut parsed = ParsedPage::default();
        scan(&v, &mut parsed);
        let mut ok = false;
        for (key, payload) in &parsed.payloads {
            ok |= self.dm_apply_payload(st, payload, &format!("inbox query {key}"));
        }
        Ok(ok)
    }

    /// Ask for a whole database over the chat connection (a "sync" request).
    async fn dm_socket_sync(&self, st: &mut DmState, database: i64) -> Result<bool> {
        let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
        let Some(conn) = st.conn.as_ref().filter(|c| c.is_alive()) else {
            eprintln!("finsta: dm: skipping the chat sync: not connected");
            return Ok(false);
        };
        let msg = json!({
            "app_id": cfg.app_id.to_string(),
            "payload": Self::sync_request(&cfg, database).to_string(),
            "request_id": conn.next_request_id(),
            "type": 1,
        });
        let answer = conn.request(&msg, Duration::from_secs(20)).await?;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let ok = self.dm_apply_response(st, &answer, &format!("sync {database}"));
        self.dm_drain(st);
        Ok(ok)
    }

    async fn dm_connect(&self, st: &DmState) -> Result<Conn> {
        let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
        let mut endpoints: Vec<url::Url> = Vec::new();
        if let Ok(u) = std::env::var("FINSTA_CHAT_URL") {
            endpoints.push(url::Url::parse(&u)?);
        } else {
            let page = url::Url::parse(&cfg.endpoint).ok().filter(|u| {
                u.scheme() == "wss" && u.host_str().is_some_and(|h| h.ends_with(".instagram.com") || h.ends_with(".facebook.com"))
            });
            if let (Some(mut u), Some(host)) = (page.clone(), &cfg.host_override)
                && u.set_host(Some(host)).is_ok()
            {
                endpoints.push(u);
            }
            endpoints.extend(page);
            endpoints.push(url::Url::parse(CHAT).unwrap());
            let mut seen = HashSet::new();
            endpoints.retain(|u| seen.insert(format!("{}{}", u.host_str().unwrap_or_default(), u.path())));
        }
        let sid = (uuid::Uuid::new_v4().as_u128() % 9_007_199_254_740_991) as i64;
        let has = |u: &url::Url, k: &str| u.query_pairs().any(|(q, _)| q == k);
        for url in &mut endpoints {
            if !has(url, "sid") {
                url.query_pairs_mut().append_pair("sid", &sid.to_string());
            }
            if !has(url, "cid") {
                url.query_pairs_mut().append_pair("cid", &cfg.client_id);
            }
        }
        let headers = [
            ("origin", "https://www.instagram.com".to_string()),
            ("user-agent", self.user_agent()),
            ("cookie", self.cookie_header()),
            ("accept-language", "en-US,en;q=0.9".to_string()),
            ("cache-control", "no-cache".to_string()),
            ("pragma", "no-cache".to_string()),
        ];
        let mut last_err = anyhow!("no id to log in to the chat with");
        let mut attempts = 0;
        'endpoints: for url in &endpoints {
            let host = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
            eprintln!("finsta: dm: connecting to {host} (protocol version {})", found(&cfg.version));
            for (label, user) in &cfg.mqtt_users {
                if attempts >= 6 {
                    break 'endpoints;
                }
                attempts += 1;
                let username = json!({
                    "u": user,
                    "s": sid,
                    "cp": cfg.mqtt_params["cp"],
                    "ecp": cfg.mqtt_params["ecp"],
                    "chat_on": cfg.mqtt_params["chat_on"],
                    "fg": true,
                    "d": cfg.client_id,
                    "ct": "websocket",
                    "mqtt_sid": "",
                    "aid": cfg.app_id,
                    "st": cfg.mqtt_params["st"],
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
                match Conn::connect(url.as_str(), &headers, "mqttwsclient", &username, &["/ls_foreground_state", "/ls_resp"])
                    .await
                {
                    Ok(conn) => {
                        eprintln!("finsta: dm: chat login accepted as {label} at {host}");
                        if let Some(v) = &cfg.version {
                            conn.publish("/ls_app_settings", &json!({"ls_fdid": "", "ls_sv": v}).to_string())?;
                        }
                        return Ok(conn);
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        eprintln!("finsta: dm: chat login as {label} at {host}: {msg}");
                        last_err = e;
                        if !msg.contains("refused") {
                            continue 'endpoints;
                        }
                    }
                }
            }
        }
        Err(last_err)
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
        if first {
            self.dm_load_page(&mut st).await?;
        }
        if Self::slide_mode(&st) {
            return self.slide_inbox(&mut st, cursor).await;
        }
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
                } else if st.last_sync_attempt.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
                    st.last_sync_attempt = Some(Instant::now());
                    if let Err(e) = self.dm_graphql_sync(&mut st).await {
                        eprintln!("finsta: dm: refreshing the inbox failed: {e:#}");
                    }
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
        let mut st = self.inner.dm.lock().await;
        if st.config.is_none() {
            self.dm_load_page(&mut st).await?;
        }
        if Self::slide_mode(&st) {
            return self.slide_thread(&mut st, thread_id, cursor).await;
        }
        let key: i64 = thread_id.parse().map_err(|_| anyhow!("Unknown conversation"))?;
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
        let mut st = self.inner.dm.lock().await;
        if st.config.is_none() {
            self.dm_load_page(&mut st).await?;
        }
        if Self::slide_mode(&st) {
            if st.slide.threads.is_empty() {
                drop(st);
                self.inbox(None).await?;
                st = self.inner.dm.lock().await;
            }
            return Ok(self.slide_thread_with_user(&st, user_pk));
        }
        let Ok(id) = user_pk.parse::<i64>() else { return Ok(None) };
        self.dm_ensure(&mut st).await?;
        let key = st.store.sorted_keys().into_iter().find(|k| st.store.others(*k) == vec![id]);
        Ok(key.and_then(|k| parse(&st.store.thread_value(k, &self.user_id(), PAGE_SIZE, None))))
    }

    /// Sends a text message, as a reply to the message `reply_to` if given,
    /// and returns the id of the conversation it went to.
    pub async fn send_text(&self, to: &Recipient, text: &str, reply_to: Option<&str>) -> Result<Option<String>> {
        {
            let mut st = self.inner.dm.lock().await;
            if st.config.is_none() {
                self.dm_load_page(&mut st).await?;
            }
            if Self::slide_mode(&st) {
                return self.slide_send(&mut st, to, text, reply_to).await;
            }
            if reply_to.is_some() {
                // shortcut: the older chat route sends replies as plain messages; add its reply field if accounts still use it.
                eprintln!("finsta: dm: reply sent as a plain message (older chat route)");
            }
        }
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
        let mut st = self.inner.dm.lock().await;
        if Self::slide_mode(&st) {
            return self.slide_mark_seen(&mut st, thread_id, item_id).await;
        }
        let Ok(key) = thread_id.parse::<i64>() else { return Ok(()) };
        if !st.conn.as_ref().is_some_and(|c| c.is_alive()) {
            return Ok(());
        }
        let ts = st.store.message_index.get(item_id).map(|(_, ts)| *ts).unwrap_or_else(now_ms);
        let read = json!({"thread_id": key, "last_read_watermark_ts": ts, "sync_group": 1});
        self.dm_tasks(&mut st, "mark read", vec![task("21", read, key.to_string())]).await
    }

    /// React to a message with an emoji, or take back your reaction (`on`
    /// false, with the emoji you had reacted with).
    /// Send a GIF from GIPHY; as its link if Instagram turns it down.
    pub async fn send_gif(&self, to: &Recipient, gif: &super::Gif, reply_to: Option<&str>) -> Result<Option<String>> {
        let native = {
            let mut st = self.inner.dm.lock().await;
            if st.config.is_none() {
                self.dm_load_page(&mut st).await?;
            }
            if Self::slide_mode(&st) {
                self.slide_send_gif(&mut st, to, &gif.id, reply_to).await
            } else {
                Err(anyhow!("not available on the older chat route"))
            }
        };
        match native {
            Ok(thread) => Ok(thread),
            Err(e) if super::is_login_required(&e) || gif.page.is_empty() => Err(e),
            Err(e) => {
                eprintln!("finsta: dm: GIF send failed ({e}); sending its link");
                self.send_text(to, &gif.page, reply_to).await
            }
        }
    }

    /// Share a story (its numeric id) as a story, not a link.
    pub async fn share_story(&self, to: &Recipient, media_pk: &str) -> Result<()> {
        let mut st = self.inner.dm.lock().await;
        if st.config.is_none() {
            self.dm_load_page(&mut st).await?;
        }
        if !Self::slide_mode(&st) {
            bail!("not available on the older chat route");
        }
        self.slide_story_share(&mut st, to, media_pk).await
    }

    pub async fn react(&self, thread_id: &str, item_id: &str, emoji: &str, on: bool) -> Result<()> {
        let mut st = self.inner.dm.lock().await;
        if st.config.is_none() {
            self.dm_load_page(&mut st).await?;
        }
        if !Self::slide_mode(&st) {
            bail!("Reactions aren't available for this account in Finsta yet.");
        }
        self.slide_react(&mut st, thread_id, item_id, emoji, on).await
    }

    /// Reply to someone's story; the reply arrives in your conversation with them.
    ///
    /// Tries Instagram's story reply, then the older reply route; if neither
    /// works, sends the reply as a message with a link to the story.
    pub async fn story_reply(&self, owner: &User, media_id: &str, text: &str) -> Result<StoryReply> {
        let native = {
            let mut st = self.inner.dm.lock().await;
            if st.config.is_none() {
                self.dm_load_page(&mut st).await?;
            }
            if Self::slide_mode(&st) {
                self.slide_story_reply(&mut st, &owner.pk, media_id, text).await
            } else {
                Err(anyhow!("not available"))
            }
        };
        match native {
            Ok(()) => return Ok(StoryReply::Reply),
            Err(e) if super::is_login_required(&e) => return Err(e),
            Err(e) => eprintln!("finsta: story reply: native route failed ({e})"),
        }
        let pk = super::media_pk(media_id).to_string();
        let form = [
            ("media_id", format!("{pk}_{}", owner.pk)),
            ("reel_id", owner.pk.clone()),
            ("text", text.to_string()),
            ("entry", "reel".to_string()),
            ("action", "send_item".to_string()),
            ("recipient_users", format!("[[{}]]", owner.pk)),
            ("client_context", uuid::Uuid::new_v4().to_string()),
        ];
        match self.post("direct_v2/threads/broadcast/reel_share/", &form).await {
            Ok(v) if v["status"] == "ok" => {
                eprintln!("finsta: story reply: older route worked");
                return Ok(StoryReply::Reply);
            }
            Ok(_) => eprintln!("finsta: story reply: older route didn't confirm"),
            Err(e) if super::is_login_required(&e) => return Err(e),
            Err(e) => eprintln!("finsta: story reply: older route failed ({e})"),
        }
        let link = format!("https://www.instagram.com/stories/{}/{pk}/", owner.username);
        self.send_text(&Recipient::User(owner.pk.clone()), &format!("{text}\n{link}"), None).await?;
        eprintln!("finsta: story reply: sent as a message with a link");
        Ok(StoryReply::Message)
    }

    /// Opening disappearing photos goes through a part of the chat protocol
    /// Finsta doesn't speak yet.
    pub async fn mark_visual_seen(&self, _thread_id: &str, _item_id: &str, _replay: bool) -> Result<()> {
        bail!("Disappearing photos can't be opened in Finsta yet. Open them on instagram.com.")
    }
}
