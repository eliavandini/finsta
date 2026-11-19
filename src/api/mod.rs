//! A small client for the parts of Instagram's web API (the JSON endpoints
//! instagram.com itself calls) that Finsta needs. Authentication is the
//! cookies of a normal instagram.com login, sent the way the website sends
//! them, with the user agent of the browser that signed in.

mod dm;
mod giphy;
mod lightspeed;
pub mod models;
mod mqtt;
pub mod session;
pub mod web;

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
pub use dm::StoryReply;
pub use giphy::Gif;

const WEB_API: &str = "https://www.instagram.com/api/v1/";
/// The page instagram.com's own GraphQL requests come from.
const WEB_REFERER: &str = "https://www.instagram.com/";
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
/// Shared by every client and account: limits may also apply to the network.
static COOLDOWNS: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

/// "users/123/info/" -> "users/*/info/", "feed/user/name/username/" ->
/// "feed/user/*/username/". Also what request logs show, so they never
/// contain ids or usernames.
fn cooldown_key(path: &str) -> String {
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    segs.iter()
        .enumerate()
        .map(|(i, seg)| {
            let id = !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit() || c == '_');
            let username = segs.get(i + 1) == Some(&"username");
            if id || username { "*" } else { seg }
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
    /// The end: posts older than `FEED_DAYS` were reached, or Instagram's
    /// "You're all caught up" marker (after which it only suggests content
    /// from accounts you don't follow). Finsta stops there.
    pub caught_up: bool,
}

/// How far back the home feed goes. It shows the people you follow in
/// order, newest first, so it needs a fixed end to stay finite.
pub const FEED_DAYS: i64 = 7;

/// Cursors from the GraphQL feed are tagged, so that a page is never
/// continued on the other route.
const FEED_CURSOR: &str = "gql:";

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What a feed page held, for the log (counts and ages only).
#[derive(Default)]
struct FeedStats {
    items: usize,
    posts: usize,
    ads: usize,
    unreadable: usize,
    too_old: usize,
    marker: bool,
    other: std::collections::BTreeMap<String, usize>,
    newest: Option<i64>,
    oldest: Option<i64>,
}

impl FeedStats {
    /// Take a post into the page unless it's an ad or older than the feed goes.
    fn add(&mut self, media: Option<Media>, cutoff: i64, page: &mut TimelinePage) {
        let Some(media) = media.filter(|m| m.user.is_some()) else {
            self.unreadable += 1;
            return;
        };
        if media.is_ad() {
            self.ads += 1;
            return;
        }
        if media.taken_at > 0 {
            self.newest = Some(self.newest.map_or(media.taken_at, |n| n.max(media.taken_at)));
            self.oldest = Some(self.oldest.map_or(media.taken_at, |o| o.min(media.taken_at)));
            if media.taken_at < cutoff {
                self.too_old += 1;
                page.caught_up = true;
                return;
            }
        }
        self.posts += 1;
        page.posts.push(media);
    }

    fn log(&self, what: &str, page: &TimelinePage) {
        let hours = |t: Option<i64>| t.map(|t| format!("{}h", (chrono_now() - t) / 3600)).unwrap_or_else(|| "-".into());
        eprintln!(
            "finsta: feed: {what}: {} items -> {} posts kept, {} older than {FEED_DAYS} days, {} ads, {} unreadable, other {:?}; newest {} old, oldest {}; end marker {}, more {}",
            self.items,
            self.posts,
            self.too_old,
            self.ads,
            self.unreadable,
            self.other,
            hours(self.newest),
            hours(self.oldest),
            self.marker,
            page.more_available,
        );
    }
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
    /// Routes ("feed/user/*/", ...) found to be served only by the API host.
    api_host_routes: RwLock<HashSet<String>>,
    /// Direct messages: instagram.com's web inbox and chat connection.
    dm: tokio::sync::Mutex<dm::DmState>,
}

/// Media ids look like "<media pk>_<owner pk>"; the web endpoints want the pk.
fn media_pk(media_id: &str) -> &str {
    media_id.split('_').next().unwrap_or(media_id)
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
                dm: Default::default(),
            }),
        }
    }

    pub fn session(&self) -> Session {
        self.inner.session.read().unwrap().clone()
    }

    pub fn user_id(&self) -> String {
        self.inner.session.read().unwrap().user_id()
    }

    fn user_agent(&self) -> String {
        let s = self.inner.session.read().unwrap();
        if s.user_agent.is_empty() { DEFAULT_USER_AGENT.to_string() } else { s.user_agent.clone() }
    }

    fn cookie_header(&self) -> String {
        self.inner
            .session
            .read()
            .unwrap()
            .cookies
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
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
        // Remembered per route, not per family: instagram.com serves
        // feed/timeline/ but not feed/user/{id}/.
        let route = pause_key.clone();
        let mut api_host = self.inner.fallback && self.inner.api_host_routes.read().unwrap().contains(&route);
        loop {
            let base = if api_host { &self.inner.api_host } else { &self.inner.base };
            let mut req = self.inner.http.request(method.clone(), format!("{base}{path}")).query(query);
            if let Some(form) = form {
                req = req.form(form);
            }
            let req = req.headers(self.headers(&method, base)).build()?;
            let what = format!(
                "{} {}/{}",
                req.method(),
                req.url().host_str().unwrap_or_default(),
                cooldown_key(req.url().path())
            );
            let resp = match self.inner.http.execute(req).await {
                Ok(r) => r,
                Err(e) => {
                    // Without the address: it can hold what was searched for.
                    let e = e.without_url();
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
    //
    // Profiles, posts, stories and likes go through instagram.com's GraphQL
    // operations first (`web_query`), as the website itself does; the older
    // REST routes it used before remain as fallbacks.

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
            bail!("Could not read this profile");
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
        parse(&raw).ok_or_else(|| anyhow!("Could not read this profile"))
    }

    /// A profile by id, the way instagram.com's profile pages load it now.
    async fn web_profile(&self, pk: &str) -> Result<User> {
        let data = self
            .web_query("PolarisProfilePageContentQuery", json!({"id": pk, "enable_integrity_filters": true}), WEB_REFERER)
            .await?;
        let mut user: User = parse(&data["user"]).ok_or_else(|| anyhow!("Could not read this profile"))?;
        if user.pk.is_empty() {
            user.pk = pk.to_string();
        }
        Ok(user)
    }

    /// The full profile of `user`: through instagram.com's current profile
    /// query when the id is known, otherwise (or if that fails) the older
    /// routes, by username first.
    pub async fn profile(&self, user: &User) -> Result<User> {
        if !user.pk.is_empty() {
            match self.web_profile(&user.pk).await {
                Ok(u) => return Ok(u),
                Err(e) if is_login_required(&e) => return Err(e),
                Err(e) => eprintln!("finsta: profile query failed ({e}); trying the older routes"),
            }
        }
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

    /// The signed-in account's username and first name, from the settings
    /// form: a light check that the session works.
    async fn account(&self) -> Result<(String, String)> {
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
        Ok((username, form["first_name"].as_str().unwrap_or_default().to_string()))
    }

    /// The signed-in account's username (checks that the session works).
    pub async fn account_username(&self) -> Result<String> {
        Ok(self.account().await?.0)
    }

    /// The signed-in account's profile; just the names if it can't be loaded.
    pub async fn current_user(&self) -> Result<User> {
        let (username, first_name) = self.account().await?;
        let me = User { pk: self.user_id(), username: username.clone(), ..Default::default() };
        match self.profile(&me).await {
            Ok(u) => Ok(u),
            Err(e) if is_login_required(&e) => Err(e),
            Err(_) => Ok(User {
                pk: self.user_id(),
                full_name: first_name,
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

    /// A profile's posts, through instagram.com's current profile query
    /// (by username, 12 at a time). `max_id` is the previous page's cursor.
    async fn web_user_feed(&self, username: &str, after: Option<String>) -> Result<Page<Media>> {
        let vars = json!({
            "data": {
                "count": 12,
                "include_reel_media_seen_timestamp": true,
                "include_relationship_info": true,
                "latest_besties_reel_media": true,
                "latest_reel_media": true,
            },
            "username": username,
            "first": 12,
            "after": after,
            "before": null,
            "last": null,
        });
        let data = self.web_query("PolarisProfilePostsQuery", vars, WEB_REFERER).await?;
        let conn = &data["xdt_api__v1__feed__user_timeline_graphql_connection"];
        if !conn.is_object() {
            bail!("Instagram didn't send this profile's posts");
        }
        let items: Vec<Media> = conn["edges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| parse::<Media>(&e["node"]))
            .collect();
        let next = conn["page_info"]["end_cursor"].as_str().map(str::to_string);
        Ok(Page {
            items,
            more_available: conn["page_info"]["has_next_page"].as_bool().unwrap_or(false) && next.is_some(),
            next_max_id: next,
        })
    }

    /// A profile's posts: through instagram.com's current query, else the
    /// older routes (by username, then by id).
    pub async fn user_feed(&self, user: &User, max_id: Option<String>) -> Result<Page<Media>> {
        if !user.username.is_empty() {
            match self.web_user_feed(&user.username, max_id.clone()).await {
                Ok(page) => return Ok(page),
                Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
                Err(e) => eprintln!("finsta: posts query failed ({e}); trying the older routes"),
            }
        }
        let mut q = vec![("count", "12".to_string())];
        if let Some(m) = max_id {
            q.push(("max_id", m));
        }
        let by_name = user
            .username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && !user.username.is_empty();
        let v = if by_name {
            match self.get(&format!("feed/user/{}/username/", user.username), &q).await {
                Ok(v) => v,
                Err(e) if is_rate_limited(&e) || e.is::<LoginRequired>() => return Err(e),
                Err(e) => {
                    eprintln!("finsta: posts by username failed ({e}); trying by id");
                    self.get(&format!("feed/user/{}/", user.pk), &q).await?
                }
            }
        } else {
            self.get(&format!("feed/user/{}/", user.pk), &q).await?
        };
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

    fn feed_device_id(&self) -> String {
        let s = self.inner.session.read().unwrap();
        s.cookies.get("ig_did").cloned().unwrap_or_else(|| s.device_uuid.clone())
    }

    /// The home feed: instagram.com's "Following" feed (the people you
    /// follow, newest first, nothing suggested), back to `FEED_DAYS` days.
    /// The older ranked route is the fallback.
    pub async fn timeline(&self, max_id: Option<String>, refresh: bool) -> Result<TimelinePage> {
        match max_id.as_deref().map(|m| m.strip_prefix(FEED_CURSOR)) {
            Some(Some(cursor)) => return self.web_timeline(Some(cursor.to_string())).await,
            Some(None) => return self.rest_timeline(max_id, refresh).await,
            None => {}
        }
        match self.web_timeline(None).await {
            Ok(page) => Ok(page),
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => Err(e),
            Err(e) => {
                eprintln!("finsta: feed query failed ({e}); trying the older route");
                self.rest_timeline(None, refresh).await
            }
        }
    }

    async fn web_timeline(&self, after: Option<String>) -> Result<TimelinePage> {
        let vars = json!({
            "data": {
                "pagination_source": "following",
                "device_id": self.feed_device_id(),
                "is_async_ads_double_request": "0",
                "is_async_ads_in_headload_enabled": "0",
                "is_async_ads_rti": "0",
                "rti_delivery_backend": "0",
            },
            "first": 12,
            "after": after,
            "before": null,
            "last": null,
            "variant": "following",
        });
        let data = self.web_query("PolarisFeedTimelineRootV2Query", vars, WEB_REFERER).await?;
        let conn = &data["xdt_api__v1__feed__timeline__connection"];
        if !conn.is_object() {
            bail!("Instagram didn't send the feed");
        }
        let cutoff = chrono_now() - FEED_DAYS * 86400;
        let mut page = TimelinePage::default();
        let mut stats = FeedStats::default();
        for e in conn["edges"].as_array().into_iter().flatten() {
            let node = &e["node"];
            stats.items += 1;
            if node["end_of_feed_demarcator"].is_object() {
                stats.marker = true;
                page.caught_up = true;
                break;
            }
            if node["ad"].is_object() {
                stats.ads += 1;
                continue;
            }
            if !node["media"].is_object() {
                let kind = node
                    .as_object()
                    .and_then(|o| o.iter().find(|(k, v)| v.is_object() && *k != "media").map(|(k, _)| k.clone()))
                    .unwrap_or_else(|| "empty".into());
                *stats.other.entry(kind).or_default() += 1;
                continue;
            }
            stats.add(parse::<Media>(&node["media"]), cutoff, &mut page);
        }
        let next = conn["page_info"]["end_cursor"].as_str().filter(|c| !c.is_empty());
        page.more_available = conn["page_info"]["has_next_page"].as_bool().unwrap_or(false) && next.is_some();
        page.next_max_id = next.map(|c| format!("{FEED_CURSOR}{c}"));
        stats.log("following feed", &page);
        Ok(page)
    }

    async fn rest_timeline(&self, max_id: Option<String>, refresh: bool) -> Result<TimelinePage> {
        let device_id = self.feed_device_id();
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
        let cutoff = chrono_now() - FEED_DAYS * 86400;
        let mut stats = FeedStats::default();
        for item in items {
            stats.items += 1;
            if item.get("end_of_feed_demarcator").is_some() {
                stats.marker = true;
                page.caught_up = true;
                break;
            }
            let raw = if let Some(m) = item.get("media_or_ad") {
                m
            } else if item.get("media_type").is_some() {
                item
            } else {
                // suggested users, netego units, ...
                let kind = item.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_else(|| "empty".into());
                *stats.other.entry(kind).or_default() += 1;
                continue;
            };
            stats.add(parse::<Media>(raw), cutoff, &mut page);
        }
        stats.log("ranked feed (older route)", &page);
        Ok(page)
    }

    /// Like or unlike a post, the way instagram.com does it now (inputs as
    /// in the website's usePolarisLikeMedia); the older route only if the
    /// website's operation can't be used.
    pub async fn like(&self, media_id: &str, tracking_token: Option<&str>, like: bool) -> Result<()> {
        let (name, input) = if like {
            (
                "usePolarisLikeMediaXIGLikeMutation",
                json!({"container_module": "feed_timeline", "media_id": media_pk(media_id), "tracking_token": tracking_token}),
            )
        } else {
            (
                "usePolarisLikeMediaXIGUnlikeMutation",
                json!({"media_id": media_pk(media_id), "tracking_token": tracking_token}),
            )
        };
        match self.web_query(name, json!({"input": input}), WEB_REFERER).await {
            Ok(data) => {
                let liked = data["xig_media_like"]["media"]["has_liked"]
                    .as_bool()
                    .or(data["xig_media_unlike"]["media"]["has_liked"].as_bool());
                if liked.is_some_and(|l| l != like) {
                    eprintln!("finsta: like: Instagram reports the post as {}", if like { "not liked" } else { "still liked" });
                }
                return Ok(());
            }
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: like query failed ({e}); trying the older route"),
        }
        let action = if like { "like" } else { "unlike" };
        self.post(&format!("web/likes/{}/{action}/", media_pk(media_id)), &[]).await?;
        Ok(())
    }

    /// Tell Instagram you've watched a story (as instagram.com's
    /// PolarisAPIReelSeen does), so it isn't shown as new again.
    pub async fn mark_story_seen(&self, reel_id: &str, owner_pk: &str, item: &Media) -> Result<()> {
        let now = chrono_now();
        let vars = json!({
            "reelId": reel_id,
            "reelMediaId": media_pk(&item.id),
            "reelMediaOwnerId": owner_pk,
            "reelMediaTakenAt": item.taken_at,
            "viewSeenAt": now,
        });
        match self.web_query("PolarisAPIReelSeenMutation", vars, WEB_REFERER).await {
            Ok(_) => return Ok(()),
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: story seen query failed ({e}); trying the older route"),
        }
        self.post(
            "stories/reel/seen",
            &[
                ("reelMediaId", media_pk(&item.id).to_string()),
                ("reelMediaOwnerId", owner_pk.to_string()),
                ("reelId", reel_id.to_string()),
                ("reelMediaTakenAt", item.taken_at.to_string()),
                ("viewSeenAt", now.to_string()),
            ],
        )
        .await?;
        Ok(())
    }

    /// Like or unlike a story.
    pub async fn story_like(&self, item: &Media, like: bool) -> Result<()> {
        let name = if like { "usePolarisStoryLikesLikeMutation" } else { "usePolarisStoryLikesUnlikeMutation" };
        let input = json!({"media_id": media_pk(&item.id)});
        match self.web_query(name, json!({"input": input}), WEB_REFERER).await {
            Ok(_) => return Ok(()),
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: story like query failed ({e}); trying the older route"),
        }
        let action = if like { "send_story_like" } else { "unsend_story_like" };
        self.post(
            &format!("story_interactions/{action}"),
            &[("media_id", item.id.clone()), ("container_module", "reel_feed_timeline".into())],
        )
        .await?;
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

    /// Replies to a comment, oldest first, a page at a time; with the next
    /// page's cursor, if there's more.
    pub async fn comment_replies(&self, media_id: &str, comment_pk: &str, cursor: Option<&str>) -> Result<(Vec<Comment>, Option<String>)> {
        if comment_pk.is_empty() || !comment_pk.bytes().all(|c| c.is_ascii_digit()) {
            bail!("Can't find the replies to this comment");
        }
        let query: Vec<(&str, String)> = cursor.map(|c| ("max_id", c.to_string())).into_iter().collect();
        let v = self
            .get(&format!("media/{}/comments/{comment_pk}/child_comments/", media_pk(media_id)), &query)
            .await?;
        let next = v["next_max_child_cursor"]
            .as_str()
            .filter(|c| !c.is_empty() && v["has_more_tail_child_comments"].as_bool() == Some(true))
            .map(str::to_string);
        Ok((parse(&v["child_comments"]).unwrap_or_default(), next))
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

    /// The stories of the given reels (user ids), the way instagram.com's
    /// story viewer loads them now; the older route if that fails.
    pub async fn reels_media(&self, reel_ids: &[String]) -> Result<HashMap<String, Reel>> {
        let vars = json!({"reel_ids_arr": reel_ids, "media_id": null, "is_highlight": false});
        match self.web_query("PolarisStoriesV3ReelPageStandaloneQuery", vars, WEB_REFERER).await {
            Ok(data) => {
                let mut out = HashMap::new();
                let list = data["xdt_api__v1__feed__reels_media"]["reels_media"].as_array().cloned().unwrap_or_default();
                for raw in &list {
                    if let Some(reel) = parse::<Reel>(raw) {
                        // Keyed like the request: by the owner's id.
                        let key = if reel_ids.contains(&reel.id) { reel.id.clone() } else { reel.user.pk.clone() };
                        out.insert(key, reel);
                    }
                }
                if !out.is_empty() || list.is_empty() {
                    return Ok(out);
                }
                eprintln!("finsta: stories query: {} reels, none readable; trying the older route", list.len());
            }
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: stories query failed ({e}); trying the older route"),
        }
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
