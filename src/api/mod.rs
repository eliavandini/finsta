//! A small client for the endpoints of Instagram's private (mobile) API that
//! Finsta needs. Authentication reuses the cookies of a normal instagram.com
//! web login.

pub mod models;
pub mod session;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

pub use models::*;
pub use session::Session;

const DEFAULT_API: &str = "https://i.instagram.com/api/v1/";
const USER_AGENT: &str = "Instagram 361.0.0.35.82 (iPhone14,3; iOS 17_5_1; en_US; en; scale=3.00; 1284x2778; 674117118) AppleWebKit/420+";
const APP_ID: &str = "124024574287414";

/// Returned (inside `anyhow::Error`) when Instagram no longer accepts the session.
#[derive(Debug)]
pub struct LoginRequired;

impl fmt::Display for LoginRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Your Instagram session has expired. Please sign in again.")
    }
}

impl std::error::Error for LoginRequired {}

pub fn is_login_required(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LoginRequired>().is_some()
}

#[derive(Debug, Default, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_max_id: Option<String>,
    pub more_available: bool,
}

#[derive(Debug, Default, Clone)]
pub struct TimelinePage {
    pub posts: Vec<Media>,
    pub next_max_id: Option<String>,
    pub more_available: bool,
    /// Instagram's "You're all caught up" marker was reached. Everything after
    /// it is suggested content from accounts you don't follow, so we stop there.
    pub caught_up: bool,
}

#[derive(Debug, Default, Clone)]
pub struct InboxPage {
    pub threads: Vec<Thread>,
    pub oldest_cursor: Option<String>,
    pub has_older: bool,
}

/// Who a message is sent to.
#[derive(Debug, Clone)]
pub enum Recipient {
    Thread(String),
    User(String),
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    session: RwLock<Session>,
    base: String,
    pigeon_session: String,
}

fn timezone_offset() -> String {
    chrono::Local::now().offset().local_minus_utc().to_string()
}

fn random_token() -> String {
    // Instagram's clients use a 19 digit random number as mutation token.
    let n = uuid::Uuid::new_v4().as_u128() % 88_888_888_888_888_888;
    (6_800_011_111_111_111_111u64 + n as u64).to_string()
}

impl Client {
    pub fn new(session: Session) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");
        let base = std::env::var("FINSTA_API_BASE").unwrap_or_else(|_| DEFAULT_API.to_string());
        Client {
            inner: Arc::new(Inner {
                http,
                session: RwLock::new(session),
                base,
                pigeon_session: format!("UFS-{}-0", uuid::Uuid::new_v4()),
            }),
        }
    }

    pub fn session(&self) -> Session {
        self.inner.session.read().unwrap().clone()
    }

    pub fn user_id(&self) -> String {
        self.inner.session.read().unwrap().user_id()
    }

    fn device_uuid(&self) -> String {
        self.inner.session.read().unwrap().device_uuid.clone()
    }

    fn headers(&self) -> HeaderMap {
        let s = self.inner.session.read().unwrap();
        let mut h = HeaderMap::new();
        let mut put = |k: &'static str, v: &str| {
            if let Ok(v) = HeaderValue::from_str(v) {
                h.insert(HeaderName::from_static(k), v);
            }
        };
        put("user-agent", USER_AGENT);
        put("x-ig-app-id", APP_ID);
        put("x-ig-capabilities", "36r/F/8=");
        put("x-ig-connection-type", "WiFi");
        put("x-ig-app-locale", "en_US");
        put("x-ig-device-locale", "en_US");
        put("x-ig-mapped-locale", "en_US");
        put("x-ig-timezone-offset", &timezone_offset());
        put("x-ig-www-claim", if s.www_claim.is_empty() { "0" } else { &s.www_claim });
        put("x-ig-device-id", &s.device_uuid);
        put("x-pigeon-session-id", &self.inner.pigeon_session);
        put("x-ads-opt-out", "1");
        put("x-fb-http-engine", "Liger");
        put("x-csrftoken", &s.csrf_token());
        put("accept-language", "en-US");
        let cookie = s
            .cookies
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        put("cookie", &cookie);
        h
    }

    /// Keep cookies Instagram rotates (csrftoken, rur, ...) up to date.
    fn absorb_headers(&self, headers: &HeaderMap) {
        let mut changed = false;
        let mut s = self.inner.session.write().unwrap();
        for value in headers.get_all("set-cookie") {
            let Ok(value) = value.to_str() else { continue };
            let Some((name, rest)) = value.split_once('=') else { continue };
            let val = rest.split(';').next().unwrap_or_default().trim().trim_matches('"');
            let name = name.trim();
            // Never let a response wipe the session itself.
            if val.is_empty() || name.is_empty() || name == "sessionid" {
                continue;
            }
            if s.cookies.get(name).map(String::as_str) != Some(val) {
                s.cookies.insert(name.to_string(), val.to_string());
                changed = true;
            }
        }
        if let Some(claim) = headers.get("x-ig-set-www-claim").and_then(|v| v.to_str().ok())
            && !claim.is_empty()
            && claim != s.www_claim
        {
            s.www_claim = claim.to_string();
            changed = true;
        }
        if changed {
            let _ = s.save();
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let req = req.headers(self.headers()).build()?;
        let what = format!("{} {}", req.method(), req.url().path());
        let resp = match self.inner.http.execute(req).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("finsta: {what} failed: {e}");
                return Err(e.into());
            }
        };
        let status = resp.status();
        let final_url = resp.url().clone();
        self.absorb_headers(resp.headers());
        let text = resp.text().await?;
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let message = json["message"].as_str().unwrap_or_default().to_string();
        let failed = !status.is_success() || json.is_null() || json["status"].as_str() == Some("fail");
        if failed {
            // Never logs cookies or headers, only what Instagram said.
            eprintln!("finsta: {what} -> HTTP {status}: {}", describe_failure(&json, &text, &final_url));
        }

        // Checked before the 401 test: Instagram also answers 401 with
        // "Please wait a few minutes" when it throttles a valid session.
        if status.as_u16() == 429 || message.contains("wait a few minutes") {
            bail!("Instagram is rate limiting requests. Try again in a few minutes.");
        }
        if message == "checkpoint_required" || message == "challenge_required" || json["challenge"].is_object() {
            bail!(
                "Instagram wants to verify this login. Open instagram.com in a browser, confirm it was you, then try again."
            );
        }
        if status.as_u16() == 401
            || message == "login_required"
            || json["require_login"].as_bool() == Some(true)
            || json["logout_reason"].is_number()
        {
            return Err(LoginRequired.into());
        }
        if !status.is_success() || json["status"].as_str() == Some("fail") {
            if message.is_empty() {
                bail!("Instagram returned an error ({status})");
            }
            bail!("Instagram: {message}");
        }
        if json.is_null() {
            bail!("Instagram returned an unexpected response");
        }
        Ok(json)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.inner.base, path.trim_start_matches('/'))
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let req = self.inner.http.get(self.url(path)).query(query);
        self.send(req).await
    }

    pub async fn post(&self, path: &str, form: &[(&str, String)]) -> Result<Value> {
        let req = self.inner.http.post(self.url(path)).form(form);
        self.send(req).await
    }

    /// POST with the `signed_body` envelope newer endpoints expect.
    pub async fn post_signed(&self, path: &str, data: Value) -> Result<Value> {
        let body = format!("SIGNATURE.{data}");
        self.post(path, &[("signed_body", body)]).await
    }

    // ----- Users ---------------------------------------------------------

    pub async fn user_info(&self, pk: &str) -> Result<User> {
        let v = self.get(&format!("users/{pk}/info/"), &[]).await?;
        parse(&v["user"]).ok_or_else(|| anyhow!("Could not read profile"))
    }

    pub async fn friendship(&self, pk: &str) -> Result<FriendshipStatus> {
        let v = self.get(&format!("friendships/show/{pk}/"), &[]).await?;
        Ok(parse(&v).unwrap_or_default())
    }

    pub async fn set_following(&self, pk: &str, follow: bool) -> Result<FriendshipStatus> {
        let action = if follow { "create" } else { "destroy" };
        let v = self
            .post_signed(
                &format!("friendships/{action}/{pk}/"),
                json!({
                    "user_id": pk,
                    "_uuid": self.device_uuid(),
                    "radio_type": "wifi-none",
                    "container_module": "profile",
                }),
            )
            .await?;
        Ok(parse(&v["friendship_status"]).unwrap_or_default())
    }

    pub async fn search_users(&self, query: &str) -> Result<Vec<User>> {
        let v = self
            .get(
                "users/search/",
                &[
                    ("q", query.to_string()),
                    ("count", "20".into()),
                    ("timezone_offset", timezone_offset()),
                ],
            )
            .await?;
        Ok(parse(&v["users"]).unwrap_or_default())
    }

    /// The ids of every account the signed-in user follows.
    pub async fn following_ids(&self) -> Result<HashSet<String>> {
        let me = self.user_id();
        let rank_token = uuid::Uuid::new_v4().to_string();
        let mut ids = HashSet::new();
        let mut max_id: Option<String> = None;
        for _ in 0..40 {
            let mut q = vec![("count", "200".to_string()), ("rank_token", rank_token.clone())];
            if let Some(m) = &max_id {
                q.push(("max_id", m.clone()));
            }
            let v = self.get(&format!("friendships/{me}/following/"), &q).await?;
            let users: Vec<User> = parse(&v["users"]).unwrap_or_default();
            ids.extend(users.into_iter().map(|u| u.pk));
            max_id = parse::<Value>(&v["next_max_id"])
                .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
            if max_id.is_none() {
                break;
            }
        }
        Ok(ids)
    }

    pub async fn user_feed(&self, pk: &str, max_id: Option<String>) -> Result<Page<Media>> {
        let mut q = vec![("count", "12".to_string())];
        if let Some(m) = max_id {
            q.push(("max_id", m));
        }
        let v = self.get(&format!("feed/user/{pk}/"), &q).await?;
        Ok(Page {
            items: parse(&v["items"]).unwrap_or_default(),
            next_max_id: parse::<Value>(&v["next_max_id"]).map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            }),
            more_available: v["more_available"].as_bool().unwrap_or(false),
        })
    }

    // ----- Home feed -----------------------------------------------------

    pub async fn timeline(&self, max_id: Option<String>, refresh: bool) -> Result<TimelinePage> {
        let uuid = self.device_uuid();
        let mut form: Vec<(&str, String)> = vec![
            ("_uuid", uuid.clone()),
            ("device_id", uuid.clone()),
            ("phone_id", uuid),
            ("is_prefetch", "0".into()),
            ("is_pull_to_refresh", if refresh { "1" } else { "0" }.into()),
            ("feed_view_info", "[]".into()),
            ("seen_posts", String::new()),
            ("unseen_posts", String::new()),
            ("battery_level", "100".into()),
            ("is_charging", "1".into()),
            ("will_sound_on", "0".into()),
            ("is_on_screen", "true".into()),
            ("is_dark_mode", "0".into()),
            ("timezone_offset", timezone_offset()),
            ("request_id", uuid::Uuid::new_v4().to_string()),
            ("session_id", self.inner.pigeon_session.clone()),
        ];
        match max_id {
            Some(m) => {
                form.push(("reason", "pagination".into()));
                form.push(("max_id", m));
            }
            None => form.push((
                "reason",
                if refresh { "pull_to_refresh" } else { "cold_start_fetch" }.into(),
            )),
        }
        let v = self.post("feed/timeline/", &form).await?;

        let mut page = TimelinePage {
            next_max_id: v["next_max_id"].as_str().map(str::to_string),
            more_available: v["more_available"].as_bool().unwrap_or(false),
            ..Default::default()
        };
        let empty = vec![];
        let items = v["feed_items"]
            .as_array()
            .or_else(|| v["items"].as_array())
            .unwrap_or(&empty);
        for item in items {
            if item.get("end_of_feed_demarcator").is_some() {
                page.caught_up = true;
                break;
            }
            let raw = if let Some(m) = item.get("media_or_ad") {
                m
            } else if item.get("media_type").is_some() {
                item
            } else {
                continue; // suggested users, netego units, ...
            };
            if let Some(media) = parse::<Media>(raw)
                && !media.is_ad()
                && media.user.is_some()
            {
                page.posts.push(media);
            }
        }
        Ok(page)
    }

    pub async fn like(&self, media_id: &str, like: bool) -> Result<()> {
        let action = if like { "like" } else { "unlike" };
        self.post_signed(
            &format!("media/{media_id}/{action}/"),
            json!({
                "media_id": media_id,
                "_uuid": self.device_uuid(),
                "radio_type": "wifi-none",
                "module_name": "feed_timeline",
                "container_module": "feed_timeline",
                "delivery_class": "organic",
                "is_carousel_bumped_post": "false",
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn comments(&self, media_id: &str) -> Result<Vec<Comment>> {
        let v = self
            .get(
                &format!("media/{media_id}/comments/"),
                &[("can_support_threading", "true".into())],
            )
            .await?;
        Ok(parse(&v["comments"]).unwrap_or_default())
    }

    pub async fn add_comment(&self, media_id: &str, text: &str) -> Result<Comment> {
        let v = self
            .post_signed(
                &format!("media/{media_id}/comment/"),
                json!({
                    "comment_text": text,
                    "_uuid": self.device_uuid(),
                    "idempotence_token": uuid::Uuid::new_v4().to_string(),
                    "container_module": "comments_v2_feed_timeline",
                    "delivery_class": "organic",
                    "radio_type": "wifi-none",
                }),
            )
            .await?;
        parse(&v["comment"]).ok_or_else(|| anyhow!("Comment was not posted"))
    }

    // ----- Stories -------------------------------------------------------

    pub async fn reels_tray(&self) -> Result<Vec<Reel>> {
        let v = match self.get("feed/reels_tray/", &[]).await {
            Ok(v) => v,
            Err(e) if is_login_required(&e) => return Err(e),
            Err(_) => {
                self.post(
                    "feed/reels_tray/",
                    &[
                        ("reason", "cold_start".into()),
                        ("_uuid", self.device_uuid()),
                        ("timezone_offset", timezone_offset()),
                        ("tray_session_id", uuid::Uuid::new_v4().to_string()),
                        ("request_id", uuid::Uuid::new_v4().to_string()),
                        ("page_size", "50".into()),
                    ],
                )
                .await?
            }
        };
        Ok(parse(&v["tray"]).unwrap_or_default())
    }

    pub async fn reels_media(&self, reel_ids: &[String]) -> Result<HashMap<String, Reel>> {
        let mut q: Vec<(&str, String)> = reel_ids.iter().map(|id| ("reel_ids", id.clone())).collect();
        q.push(("source", "reel_feed_timeline".into()));
        let v = self.get("feed/reels_media/", &q).await?;
        let mut out = HashMap::new();
        if let Some(map) = v["reels"].as_object() {
            for (id, raw) in map {
                if let Some(reel) = parse::<Reel>(raw) {
                    out.insert(id.clone(), reel);
                }
            }
        }
        if let Some(list) = v["reels_media"].as_array() {
            for raw in list {
                if let Some(reel) = parse::<Reel>(raw) {
                    out.entry(reel.id.clone()).or_insert(reel);
                }
            }
        }
        Ok(out)
    }

    // ----- Direct messages -----------------------------------------------

    pub async fn inbox(&self, cursor: Option<String>) -> Result<InboxPage> {
        let mut q = vec![
            ("visual_message_return_type", "unseen".to_string()),
            ("persistentBadging", "true".into()),
            ("thread_message_limit", "5".into()),
            ("limit", "25".into()),
        ];
        if let Some(c) = cursor {
            q.push(("cursor", c));
            q.push(("direction", "older".into()));
        }
        let v = self.get("direct_v2/inbox/", &q).await?;
        let inbox = &v["inbox"];
        Ok(InboxPage {
            threads: parse(&inbox["threads"]).unwrap_or_default(),
            oldest_cursor: inbox["oldest_cursor"].as_str().map(str::to_string),
            has_older: inbox["has_older"].as_bool().unwrap_or(false),
        })
    }

    pub async fn thread(&self, thread_id: &str, cursor: Option<String>) -> Result<Thread> {
        let mut q = vec![
            ("visual_message_return_type", "unseen".to_string()),
            ("limit", "20".into()),
        ];
        if let Some(c) = cursor {
            q.push(("cursor", c));
            q.push(("direction", "older".into()));
        }
        let v = self.get(&format!("direct_v2/threads/{thread_id}/"), &q).await?;
        parse(&v["thread"]).ok_or_else(|| anyhow!("Could not read conversation"))
    }

    /// The existing 1:1 thread with `user_pk`, if there is one.
    pub async fn thread_with_user(&self, user_pk: &str) -> Result<Option<Thread>> {
        let v = self
            .get(
                "direct_v2/threads/get_by_participants/",
                &[("recipient_users", format!("[{user_pk}]")), ("limit", "20".into())],
            )
            .await?;
        Ok(parse::<Thread>(&v["thread"]).filter(|t| !t.thread_id.is_empty()))
    }

    /// Sends a text message and returns the id of the thread it landed in.
    pub async fn send_text(&self, to: &Recipient, text: &str) -> Result<Option<String>> {
        let token = random_token();
        let uuid = self.device_uuid();
        let mut form: Vec<(&str, String)> = vec![
            ("action", "send_item".into()),
            ("is_x_transport_forward", "false".into()),
            ("send_silently", "false".into()),
            ("is_shh_mode", "0".into()),
            ("send_attribution", "inbox".into()),
            ("client_context", token.clone()),
            ("mutation_token", token.clone()),
            ("offline_threading_id", token),
            ("_uuid", uuid.clone()),
            ("device_id", uuid),
        ];
        match to {
            Recipient::Thread(id) => form.push(("thread_ids", format!("[{id}]"))),
            Recipient::User(pk) => form.push(("recipient_users", format!("[[{pk}]]"))),
        }
        let link_re = regex::Regex::new(r"https?://\S+").unwrap();
        let links: Vec<&str> = link_re.find_iter(text).map(|m| m.as_str()).collect();
        let method = if links.is_empty() {
            form.push(("text", text.to_string()));
            "text"
        } else {
            form.push(("link_text", text.to_string()));
            form.push(("link_urls", serde_json::to_string(&links)?));
            "link"
        };
        let v = self
            .post(&format!("direct_v2/threads/broadcast/{method}/"), &form)
            .await?;
        let payload = if v["payload"].is_object() { &v["payload"] } else { &v["payload"][0] };
        Ok(payload["thread_id"].as_str().map(str::to_string))
    }

    pub async fn mark_seen(&self, thread_id: &str, item_id: &str) -> Result<()> {
        self.post(
            &format!("direct_v2/threads/{thread_id}/items/{item_id}/seen/"),
            &[
                ("action", "mark_seen".into()),
                ("thread_id", thread_id.into()),
                ("item_id", item_id.into()),
                ("_uuid", self.device_uuid()),
                ("use_unified_inbox", "true".into()),
                ("client_context", random_token()),
                ("offline_threading_id", random_token()),
            ],
        )
        .await?;
        Ok(())
    }

    /// Tell Instagram a disappearing photo or video was opened (or replayed).
    /// The sender sees this, and Instagram stops serving the media once the
    /// allowed views are used up.
    pub async fn mark_visual_seen(&self, thread_id: &str, item_id: &str, replay: bool) -> Result<()> {
        let action = if replay { "item_replayed" } else { "item_seen" };
        let csrf = self.inner.session.read().unwrap().csrf_token();
        self.post(
            &format!("direct_v2/visual_threads/{thread_id}/{action}/"),
            &[
                ("item_ids", format!("[{item_id}]")),
                ("_uuid", self.device_uuid()),
                ("_csrftoken", csrf),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn logout(&self) -> Result<()> {
        let uuid = self.device_uuid();
        self.post(
            "accounts/logout/",
            &[
                ("one_tap_app_login", "false".into()),
                ("phone_id", uuid.clone()),
                ("_uuid", uuid.clone()),
                ("device_id", uuid),
            ],
        )
        .await?;
        Ok(())
    }
}

/// A one-line summary of a failed response for the log.
fn describe_failure(json: &Value, text: &str, url: &reqwest::Url) -> String {
    if json.is_object() {
        let out = json.to_string();
        if out.chars().count() > 400 {
            return out.chars().take(400).chain(std::iter::once('…')).collect();
        }
        return out;
    }
    let title = text
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(t, _)| t.split_whitespace().collect::<Vec<_>>().join(" "));
    match title {
        Some(t) => format!("HTML page \"{t}\" at {}{}", url.host_str().unwrap_or_default(), url.path()),
        None if text.is_empty() => format!("empty body at {}{}", url.host_str().unwrap_or_default(), url.path()),
        None => format!("{} bytes of non-JSON at {}{}", text.len(), url.host_str().unwrap_or_default(), url.path()),
    }
}
