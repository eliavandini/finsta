//! A small client for the parts of Instagram's web API (the JSON endpoints
//! instagram.com itself calls) that Finsta needs. Authentication is the
//! cookies of a normal instagram.com login, sent the way the website sends
//! them, with the user agent of the browser that signed in.

pub mod models;
pub mod session;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

pub use models::*;
pub use session::Session;

const WEB_API: &str = "https://www.instagram.com/api/v1/";
/// instagram.com also calls a few routes on the API host; used when the
/// website's own host doesn't serve a route.
const API_HOST: &str = "https://i.instagram.com/api/v1/";
/// The app id of instagram.com, sent by the website with every API call.
const WEB_APP_ID: &str = "936619743392459";
const ASBD_ID: &str = "359341";
/// Used when the session was pasted in and the browser it came from is unknown.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0";

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

/// Returned when Instagram is throttling requests. Until the cooldown ends,
/// requests fail right away instead of being sent, so Finsta doesn't keep
/// knocking while Instagram has asked it to stop.
#[derive(Debug)]
pub struct RateLimited;

impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Instagram is limiting requests right now. Try again in a few minutes.")
    }
}

impl std::error::Error for RateLimited {}

pub fn is_rate_limited(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RateLimited>().is_some()
}

/// Paused endpoints (ids masked) and when they may be tried again. Instagram
/// throttles endpoints separately, so one refusing doesn't stop the rest.
/// Shared by every client, since the limits apply to the account.
static COOLDOWNS: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

/// "users/123/info/" -> "users/*/info/"
fn cooldown_key(path: &str) -> String {
    path.trim_start_matches('/')
        .split('/')
        .map(|seg| {
            if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit() || c == '_') {
                "*"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// How long requests to `path` stay paused after Instagram throttled them.
pub fn cooldown_remaining(path: &str) -> Option<Duration> {
    let until = *COOLDOWNS.lock().unwrap().get(&cooldown_key(path))?;
    until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())
}

/// Whether any endpoint is currently paused.
pub fn any_cooldown() -> bool {
    let now = Instant::now();
    COOLDOWNS.lock().unwrap().values().any(|until| *until > now)
}

fn start_cooldown(key: &str, retry_after: Option<Duration>) {
    let wait = retry_after
        .unwrap_or(Duration::from_secs(120))
        .clamp(Duration::from_secs(30), Duration::from_secs(900));
    eprintln!("finsta: pausing {key} for {}s", wait.as_secs());
    COOLDOWNS.lock().unwrap().insert(key.to_string(), Instant::now() + wait);
}

/// The endpoint `Client::current_user` depends on.
pub const ME_PATH: &str = "accounts/edit/web_form_data/";

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
    api_host: String,
    /// Retry on the API host when instagram.com doesn't serve a route.
    fallback: bool,
    /// Route families ("direct_v2", "feed", ...) found to be served only by the API host.
    api_host_routes: RwLock<HashSet<String>>,
}

/// "direct_v2/threads/123/" -> "direct_v2". Related routes live on the same host.
fn route_key(path: &str) -> String {
    path.split('/').next().unwrap_or_default().to_string()
}

/// Media ids look like "<media pk>_<owner pk>"; the web endpoints want the pk.
fn media_pk(media_id: &str) -> &str {
    media_id.split('_').next().unwrap_or(media_id)
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
            // A redirect means "log in" or "verify it's you"; handle it instead of following it.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP client");
        // Both can point at a mock server for testing (see dev/mock_api.py).
        let custom = std::env::var("FINSTA_API_BASE").ok();
        let custom_host = std::env::var("FINSTA_API_HOST").ok();
        Client {
            inner: Arc::new(Inner {
                http,
                session: RwLock::new(session),
                fallback: custom.is_none() || custom_host.is_some(),
                api_host: custom_host.unwrap_or_else(|| API_HOST.to_string()),
                base: custom.unwrap_or_else(|| WEB_API.to_string()),
                api_host_routes: RwLock::new(HashSet::new()),
            }),
        }
    }

    pub fn session(&self) -> Session {
        self.inner.session.read().unwrap().clone()
    }

    pub fn user_id(&self) -> String {
        self.inner.session.read().unwrap().user_id()
    }

    /// The headers instagram.com sends with its own API calls.
    fn headers(&self, method: &Method, base: &str) -> HeaderMap {
        let s = self.inner.session.read().unwrap();
        let mut h = HeaderMap::new();
        let mut put = |k: &'static str, v: &str| {
            if let Ok(v) = HeaderValue::from_str(v) {
                h.insert(HeaderName::from_static(k), v);
            }
        };
        let ua = if s.user_agent.is_empty() { DEFAULT_USER_AGENT } else { &s.user_agent };
        put("user-agent", ua);
        put("accept", "*/*");
        put("accept-language", "en-US,en;q=0.9");
        put("x-ig-app-id", WEB_APP_ID);
        put("x-asbd-id", ASBD_ID);
        put("x-csrftoken", &s.csrf_token());
        put("x-ig-www-claim", if s.www_claim.is_empty() { "0" } else { &s.www_claim });
        put("x-requested-with", "XMLHttpRequest");
        put("referer", "https://www.instagram.com/");
        put("sec-fetch-dest", "empty");
        put("sec-fetch-mode", "cors");
        put("sec-fetch-site", if base.starts_with(WEB_API) { "same-origin" } else { "same-site" });
        if method == Method::POST {
            put("origin", "https://www.instagram.com");
        }
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

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        form: Option<&[(&str, String)]>,
    ) -> Result<Value> {
        let path = path.trim_start_matches('/');
        let pause_key = cooldown_key(path);
        if cooldown_remaining(path).is_some() {
            return Err(RateLimited.into());
        }
        let route = route_key(path);
        let mut api_host = self.inner.fallback && self.inner.api_host_routes.read().unwrap().contains(&route);
        loop {
            let base = if api_host { &self.inner.api_host } else { &self.inner.base };
            let mut req = self.inner.http.request(method.clone(), format!("{base}{path}")).query(query);
            if let Some(form) = form {
                req = req.form(form);
            }
            let req = req.headers(self.headers(&method, base)).build()?;
            let what = format!(
                "{} {}{}",
                req.method(),
                req.url().host_str().unwrap_or_default(),
                req.url().path()
            );
            let resp = match self.inner.http.execute(req).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("finsta: {what} failed: {e}");
                    return Err(e.into());
                }
            };
            let status = resp.status();
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.absorb_headers(resp.headers());
            let text = resp.text().await?;
            let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

            if status.as_u16() == 404 && !json.is_object() && self.inner.fallback && !api_host {
                eprintln!("finsta: {what} -> HTTP 404 page, retrying on the API host");
                self.inner.api_host_routes.write().unwrap().insert(route.clone());
                api_host = true;
                continue;
            }
            let result = check_response(&what, status, &location, &json, &text);
            if let Err(e) = &result
                && is_rate_limited(e)
            {
                start_cooldown(&pause_key, retry_after);
            }
            return result;
        }
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.call(Method::GET, path, query, None).await
    }

    pub async fn post(&self, path: &str, form: &[(&str, String)]) -> Result<Value> {
        self.call(Method::POST, path, &[], Some(form)).await
    }

    // ----- Users ---------------------------------------------------------

    pub async fn user_info(&self, pk: &str) -> Result<User> {
        let v = self.get(&format!("users/{pk}/info/"), &[]).await?;
        parse(&v["user"]).ok_or_else(|| anyhow!("Could not read profile"))
    }

    /// A profile by username, the way instagram.com's profile pages load it.
    pub async fn user_by_username(&self, username: &str) -> Result<User> {
        let v = self
            .get("users/web_profile_info/", &[("username", username.to_string())])
            .await?;
        let u = &v["data"]["user"];
        if !u.is_object() {
            bail!("Could not read @{username}'s profile");
        }
        let pic = if u["profile_pic_url_hd"].is_string() { &u["profile_pic_url_hd"] } else { &u["profile_pic_url"] };
        let raw = json!({
            "pk": u["id"],
            "username": u["username"],
            "full_name": u["full_name"],
            "profile_pic_url": pic,
            "is_private": u["is_private"],
            "is_verified": u["is_verified"],
            "biography": u["biography"],
            "external_url": u["external_url"],
            "follower_count": u["edge_followed_by"]["count"],
            "following_count": u["edge_follow"]["count"],
            "media_count": u["edge_owner_to_timeline_media"]["count"],
            "friendship_status": {
                "following": u["followed_by_viewer"],
                "followed_by": u["follows_viewer"],
                "outgoing_request": u["requested_by_viewer"],
            },
        });
        parse(&raw).ok_or_else(|| anyhow!("Could not read @{username}'s profile"))
    }

    /// The full profile of `user`: by username when known (what the website
    /// does), otherwise by id.
    pub async fn profile(&self, user: &User) -> Result<User> {
        if user.username.is_empty() {
            return self.user_info(&user.pk).await;
        }
        match self.user_by_username(&user.username).await {
            Ok(u) => Ok(u),
            Err(e) if is_login_required(&e) => Err(e),
            Err(e) => {
                eprintln!("finsta: profile by username failed ({e}); trying by id");
                self.user_info(&user.pk).await
            }
        }
    }

    /// The signed-in account. Read from the same endpoint instagram.com's
    /// settings page uses, since users/{id}/info/ is refused to the website.
    pub async fn current_user(&self) -> Result<User> {
        let v = self.get(ME_PATH, &[]).await?;
        let form = &v["form_data"];
        let username = form["username"].as_str().unwrap_or_default().to_string();
        if username.is_empty() {
            bail!("Could not read your profile");
        }
        {
            let mut s = self.inner.session.write().unwrap();
            if s.username != username {
                s.username = username.clone();
                let _ = s.save();
            }
        }
        match self.user_by_username(&username).await {
            Ok(u) => Ok(u),
            Err(e) if is_login_required(&e) => Err(e),
            Err(_) => Ok(User {
                pk: self.user_id(),
                full_name: form["first_name"].as_str().unwrap_or_default().to_string(),
                username,
                ..Default::default()
            }),
        }
    }

    pub async fn friendship(&self, pk: &str) -> Result<FriendshipStatus> {
        let v = self.get(&format!("friendships/show/{pk}/"), &[]).await?;
        Ok(parse(&v).unwrap_or_default())
    }

    pub async fn set_following(&self, pk: &str, follow: bool) -> Result<FriendshipStatus> {
        let action = if follow { "create" } else { "destroy" };
        let v = self
            .post(
                &format!("friendships/{action}/{pk}/"),
                &[
                    ("container_module", "profile".into()),
                    ("nav_chain", String::new()),
                    ("user_id", pk.to_string()),
                ],
            )
            .await?;
        Ok(parse(&v["friendship_status"]).unwrap_or_default())
    }

    pub async fn search_users(&self, query: &str) -> Result<Vec<User>> {
        let v = self
            .get(
                "web/search/topsearch/",
                &[
                    ("context", "user".to_string()),
                    ("query", query.to_string()),
                    ("include_reel", "false".into()),
                ],
            )
            .await?;
        // Entries are {"position": n, "user": {...}}.
        let users = v["users"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|u| parse::<User>(if u["user"].is_object() { &u["user"] } else { u }))
                    .filter(|u| !u.pk.is_empty())
                    .take(20)
                    .collect()
            })
            .unwrap_or_default();
        Ok(users)
    }

    /// The ids of every account the signed-in user follows.
    pub async fn following_ids(&self) -> Result<HashSet<String>> {
        let me = self.user_id();
        let mut ids = HashSet::new();
        let mut max_id: Option<String> = None;
        for _ in 0..60 {
            let mut q = vec![("count", "100".to_string())];
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
        let device_id = {
            let s = self.inner.session.read().unwrap();
            s.cookies.get("ig_did").cloned().unwrap_or_else(|| s.device_uuid.clone())
        };
        let mut form: Vec<(&str, String)> = vec![
            ("device_id", device_id),
            ("is_async_ads_double_request", "0".into()),
            ("is_async_ads_in_headload_enabled", "0".into()),
            ("is_async_ads_rti", "0".into()),
            ("rti_delivery_backend", "0".into()),
            ("feed_view_info", "[]".into()),
            ("is_pull_to_refresh", if refresh { "1" } else { "0" }.into()),
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
        self.post(&format!("web/likes/{}/{action}/", media_pk(media_id)), &[]).await?;
        Ok(())
    }

    pub async fn comments(&self, media_id: &str) -> Result<Vec<Comment>> {
        let v = self
            .get(
                &format!("media/{}/comments/", media_pk(media_id)),
                &[
                    ("can_support_threading", "true".into()),
                    ("permalink_enabled", "false".into()),
                ],
            )
            .await?;
        Ok(parse(&v["comments"]).unwrap_or_default())
    }

    pub async fn add_comment(&self, media_id: &str, text: &str) -> Result<Comment> {
        let v = self
            .post(
                &format!("web/comments/{}/add/", media_pk(media_id)),
                &[("comment_text", text.to_string()), ("replied_to_comment_id", String::new())],
            )
            .await?;
        // The website gets {"id", "text", "created_time", "from": {...}} back.
        let raw = if v["comment"].is_object() {
            v["comment"].clone()
        } else {
            let from = &v["from"];
            json!({
                "text": v["text"],
                "created_at": v["created_time"],
                "user": {
                    "pk": from["id"],
                    "username": from["username"],
                    "full_name": from["full_name"],
                    "profile_pic_url": from["profile_picture"],
                },
            })
        };
        let mut comment: Comment = parse(&raw).ok_or_else(|| anyhow!("Comment was not posted"))?;
        if comment.text.is_empty() {
            comment.text = text.to_string();
        }
        if comment.user.username.is_empty() {
            comment.user.username = self.session().username;
        }
        Ok(comment)
    }

    // ----- Stories -------------------------------------------------------

    pub async fn reels_tray(&self) -> Result<Vec<Reel>> {
        let v = self.get("feed/reels_tray/", &[("is_following_feed", "false".into())]).await?;
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
        let mut form: Vec<(&str, String)> = vec![
            ("action", "send_item".into()),
            ("client_context", token.clone()),
            ("mutation_token", token.clone()),
            ("offline_threading_id", token),
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
                ("use_unified_inbox", "true".into()),
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
        self.post(
            &format!("direct_v2/visual_threads/{thread_id}/{action}/"),
            &[("item_ids", format!("[{item_id}]"))],
        )
        .await?;
        Ok(())
    }

    pub async fn logout(&self) -> Result<()> {
        self.post(
            "web/accounts/logout/ajax/",
            &[("one_tap_app_login", "0".into()), ("user_id", self.user_id())],
        )
        .await?;
        Ok(())
    }
}

/// Turn Instagram's answer into a value or a useful error, logging failures
/// (never cookies or headers, only what Instagram said).
fn check_response(
    what: &str,
    status: reqwest::StatusCode,
    location: &str,
    json: &Value,
    text: &str,
) -> Result<Value> {
    let message = json["message"].as_str().unwrap_or_default();
    let failed = !status.is_success() || !json.is_object() || json["status"].as_str() == Some("fail");
    if failed {
        eprintln!("finsta: {what} -> HTTP {status}: {}", describe_failure(json, text, location));
    }

    if status.is_redirection() {
        if location.contains("/challenge") || location.contains("/checkpoint") {
            bail!(VERIFY);
        }
        if location.contains("/accounts/login") {
            return Err(LoginRequired.into());
        }
        bail!("Instagram redirected the request unexpectedly ({status})");
    }
    // Checked before the 401 test: Instagram also answers 401 with
    // "Please wait a few minutes" when it throttles a valid session.
    if status.as_u16() == 429 || message.contains("wait a few minutes") {
        return Err(RateLimited.into());
    }
    if message == "checkpoint_required" || message == "challenge_required" || json["challenge"].is_object() {
        bail!(VERIFY);
    }
    if status.as_u16() == 401
        || message == "login_required"
        || json["require_login"].as_bool() == Some(true)
        || json["logout_reason"].is_number()
    {
        return Err(LoginRequired.into());
    }
    if failed {
        if message.is_empty() {
            bail!("Instagram returned an error ({status})");
        }
        bail!("Instagram: {message}");
    }
    Ok(json.clone())
}

const VERIFY: &str =
    "Instagram wants to verify this login. Open instagram.com in a browser, confirm it was you, then try again.";

/// A one-line summary of a failed response for the log.
fn describe_failure(json: &Value, text: &str, location: &str) -> String {
    if !location.is_empty() {
        return format!("redirect to {}", location.split('?').next().unwrap_or_default());
    }
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
        Some(t) => format!("HTML page \"{t}\""),
        None if text.is_empty() => "empty body".to_string(),
        None => format!("{} bytes that aren't JSON", text.len()),
    }
}
