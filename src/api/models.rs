//! Lenient models for the parts of Instagram's private API that Finsta uses.
//!
//! Instagram is inconsistent about types (ids are sometimes numbers, sometimes
//! strings; timestamps are sometimes strings) and frequently adds or drops
//! fields, so everything here defaults instead of failing.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub fn de_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

pub fn de_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    })
}

pub fn de_opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let s = de_string(d)?;
    Ok(if s.is_empty() { None } else { Some(s) })
}

pub fn value_i64(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Value::String(s) => s
            .parse::<i64>()
            .ok()
            .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
            .unwrap_or(0),
        Value::Bool(b) => *b as i64,
        _ => 0,
    }
}

pub fn de_i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(value_i64(&Value::deserialize(d)?))
}

pub fn de_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::String(s) => s == "true" || s == "1",
        _ => false,
    })
}

/// Parse a JSON value into `T`, returning `None` instead of an error.
pub fn parse<T: for<'de> Deserialize<'de>>(v: &Value) -> Option<T> {
    if v.is_null() {
        return None;
    }
    serde_json::from_value(v.clone()).ok()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FriendshipStatus {
    #[serde(default, deserialize_with = "de_bool")]
    pub following: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub followed_by: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub outgoing_request: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImageCandidate {
    #[serde(default, deserialize_with = "de_string")]
    pub url: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub width: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub height: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImageVersions {
    #[serde(default)]
    pub candidates: Vec<ImageCandidate>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct User {
    #[serde(default, deserialize_with = "de_id")]
    pub pk: String,
    #[serde(default, deserialize_with = "de_string")]
    pub username: String,
    #[serde(default, deserialize_with = "de_string")]
    pub full_name: String,
    #[serde(default, deserialize_with = "de_string")]
    pub profile_pic_url: String,
    #[serde(default)]
    pub hd_profile_pic_url_info: Option<ImageCandidate>,
    #[serde(default, deserialize_with = "de_bool")]
    pub is_private: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub is_verified: bool,
    #[serde(default, deserialize_with = "de_string")]
    pub biography: String,
    #[serde(default, deserialize_with = "de_string")]
    pub external_url: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub follower_count: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub following_count: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub media_count: i64,
    #[serde(default)]
    pub friendship_status: Option<FriendshipStatus>,
}

impl User {
    pub fn display_name(&self) -> String {
        if self.full_name.trim().is_empty() {
            self.username.clone()
        } else {
            self.full_name.clone()
        }
    }

    pub fn best_pic(&self) -> String {
        match &self.hd_profile_pic_url_info {
            Some(c) if !c.url.is_empty() => c.url.clone(),
            _ => self.profile_pic_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caption {
    #[serde(default, deserialize_with = "de_string")]
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Media {
    #[serde(default, deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, deserialize_with = "de_string")]
    pub code: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub media_type: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub taken_at: i64,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub caption: Option<Caption>,
    #[serde(default, deserialize_with = "de_i64")]
    pub like_count: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub comment_count: i64,
    #[serde(default, deserialize_with = "de_bool")]
    pub has_liked: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub like_and_view_counts_disabled: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub comments_disabled: bool,
    #[serde(default)]
    pub image_versions2: Option<ImageVersions>,
    #[serde(default)]
    pub video_versions: Option<Vec<ImageCandidate>>,
    #[serde(default)]
    pub carousel_media: Option<Vec<Media>>,
    #[serde(default, deserialize_with = "de_i64")]
    pub original_width: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub original_height: i64,
    #[serde(default, deserialize_with = "de_string")]
    pub product_type: String,
    #[serde(default)]
    pub injected: Option<Value>,
    #[serde(default)]
    pub ad_id: Option<Value>,
    #[serde(default)]
    pub ad_action: Option<Value>,
    #[serde(default)]
    pub location: Option<Location>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Location {
    #[serde(default, deserialize_with = "de_string")]
    pub name: String,
}

impl Media {
    pub fn is_video(&self) -> bool {
        self.media_type == 2
    }

    pub fn is_carousel(&self) -> bool {
        self.media_type == 8 || self.carousel_media.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Sponsored content and anything Instagram injected into the feed.
    pub fn is_ad(&self) -> bool {
        self.injected.is_some() || self.ad_id.is_some() || self.ad_action.is_some()
    }

    /// The smallest image that is at least `min_width` wide (or the largest one).
    pub fn image_url(&self, min_width: i64) -> Option<String> {
        let candidates = match &self.image_versions2 {
            Some(v) if !v.candidates.is_empty() => &v.candidates,
            _ => {
                return self
                    .carousel_media
                    .as_ref()
                    .and_then(|c| c.first())
                    .and_then(|m| m.image_url(min_width));
            }
        };
        let mut sorted: Vec<&ImageCandidate> = candidates.iter().collect();
        sorted.sort_by_key(|c| c.width);
        sorted
            .iter()
            .find(|c| c.width >= min_width)
            .or(sorted.last())
            .map(|c| c.url.clone())
            .filter(|u| !u.is_empty())
    }

    pub fn video_url(&self) -> Option<String> {
        self.video_versions
            .as_ref()?
            .iter()
            .max_by_key(|v| v.width)
            .map(|v| v.url.clone())
            .filter(|u| !u.is_empty())
    }

    /// Height divided by width, clamped to what Instagram allows in feed.
    pub fn aspect(&self) -> f64 {
        let (w, h) = if self.original_width > 0 && self.original_height > 0 {
            (self.original_width, self.original_height)
        } else if let Some(c) = self
            .image_versions2
            .as_ref()
            .and_then(|v| v.candidates.first())
            .filter(|c| c.width > 0 && c.height > 0)
        {
            (c.width, c.height)
        } else if let Some(first) = self.carousel_media.as_ref().and_then(|c| c.first()) {
            return first.aspect();
        } else {
            (1, 1)
        };
        (h as f64 / w as f64).clamp(0.5, 1.25)
    }

    pub fn caption_text(&self) -> String {
        self.caption.as_ref().map(|c| c.text.clone()).unwrap_or_default()
    }

    pub fn web_url(&self) -> Option<String> {
        if self.code.is_empty() {
            return None;
        }
        let kind = if self.product_type == "clips" { "reel" } else { "p" };
        Some(format!("https://www.instagram.com/{kind}/{}/", self.code))
    }
}

/// A user's stories ("reel" in Instagram's API).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Reel {
    #[serde(default, deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub user: User,
    #[serde(default)]
    pub items: Vec<Media>,
    #[serde(default, deserialize_with = "de_i64")]
    pub seen: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub latest_reel_media: i64,
}

impl Reel {
    pub fn has_unseen(&self) -> bool {
        self.latest_reel_media > self.seen
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Comment {
    #[serde(default, deserialize_with = "de_string")]
    pub text: String,
    #[serde(default)]
    pub user: User,
    #[serde(default, deserialize_with = "de_i64")]
    pub created_at: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub comment_like_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Thread {
    #[serde(default, deserialize_with = "de_id")]
    pub thread_id: String,
    #[serde(default, deserialize_with = "de_string")]
    pub thread_title: String,
    #[serde(default)]
    pub users: Vec<User>,
    #[serde(default, deserialize_with = "de_bool")]
    pub is_group: bool,
    /// Newest first, as returned by the API.
    #[serde(default)]
    pub items: Vec<DmItem>,
    #[serde(default, deserialize_with = "de_i64")]
    pub last_activity_at: i64,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub oldest_cursor: Option<String>,
    #[serde(default, deserialize_with = "de_bool")]
    pub has_older: bool,
    #[serde(default)]
    pub last_seen_at: Value,
    #[serde(default, deserialize_with = "de_i64")]
    pub read_state: i64,
}

impl Thread {
    pub fn title(&self) -> String {
        if !self.thread_title.trim().is_empty() {
            return self.thread_title.clone();
        }
        match self.users.as_slice() {
            [] => "Just you".into(),
            [one] => one.display_name(),
            many => many
                .iter()
                .map(|u| u.username.clone())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    pub fn is_group_chat(&self) -> bool {
        self.is_group || self.users.len() > 1
    }

    pub fn last_item(&self) -> Option<&DmItem> {
        self.items.iter().max_by_key(|i| i.timestamp)
    }

    pub fn is_unread(&self, viewer_id: &str) -> bool {
        let Some(last) = self.last_item() else {
            return false;
        };
        if last.user_id == viewer_id {
            return false;
        }
        let seen = &self.last_seen_at[viewer_id];
        if seen.is_object() {
            let seen_item = seen["item_id"].as_str().unwrap_or_default();
            if seen_item == last.item_id {
                return false;
            }
            return value_i64(&seen["timestamp"]) < last.timestamp;
        }
        self.read_state == 1
    }

    pub fn user(&self, pk: &str) -> Option<&User> {
        self.users.iter().find(|u| u.pk == pk)
    }
}

/// One message in a direct thread. The payload depends on `item_type`, so the
/// rest of the object is kept around as JSON and interpreted by [`DmItem::content`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DmItem {
    #[serde(default, deserialize_with = "de_id")]
    pub item_id: String,
    #[serde(default, deserialize_with = "de_id")]
    pub user_id: String,
    /// Microseconds since the epoch.
    #[serde(default, deserialize_with = "de_i64")]
    pub timestamp: i64,
    #[serde(default, deserialize_with = "de_string")]
    pub item_type: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone)]
pub enum DmContent {
    Text(String),
    Like(String),
    Link {
        text: String,
    },
    Media {
        image: Option<String>,
        video: Option<String>,
        aspect: f64,
    },
    Post(Box<Media>),
    Shared {
        title: String,
        subtitle: String,
        preview: Option<String>,
        url: Option<String>,
    },
    Voice {
        url: Option<String>,
        duration_ms: i64,
    },
    Gif(Option<String>),
    Info(String),
}

fn str_at(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for key in path {
        cur = &cur[*key];
    }
    match cur {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

impl DmItem {
    fn get(&self, key: &str) -> &Value {
        self.extra.get(key).unwrap_or(&Value::Null)
    }

    pub fn timestamp_secs(&self) -> i64 {
        // Microseconds; tolerate APIs that send milliseconds or seconds.
        match self.timestamp {
            t if t > 100_000_000_000_000 => t / 1_000_000,
            t if t > 100_000_000_000 => t / 1_000,
            t => t,
        }
    }

    pub fn reactions(&self) -> Vec<String> {
        self.get("reactions")["emojis"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e["emoji"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn replied_to_text(&self) -> Option<String> {
        let r = self.get("replied_to_message");
        if r.is_null() {
            return None;
        }
        let text = str_at(r, &["text"]);
        Some(if text.is_empty() {
            "a message".into()
        } else {
            truncate(&text, 80)
        })
    }

    pub fn content(&self) -> DmContent {
        match self.item_type.as_str() {
            "text" => DmContent::Text(str_at(self.get("text"), &[])),
            "like" => DmContent::Like(non_empty(str_at(self.get("like"), &[])).unwrap_or("❤️".into())),
            "link" => {
                let link = self.get("link");
                DmContent::Link {
                    text: non_empty(str_at(link, &["text"]))
                        .unwrap_or_else(|| str_at(link, &["link_context", "link_url"])),
                }
            }
            "media" | "raven_media" | "visual_media" => {
                let raw = match self.item_type.as_str() {
                    "visual_media" => &self.get("visual_media")["media"],
                    t => self.get(t),
                };
                match parse::<Media>(raw) {
                    Some(m) if m.image_url(0).is_some() || m.video_url().is_some() => {
                        DmContent::Media {
                            image: m.image_url(640),
                            video: m.video_url(),
                            aspect: m.aspect(),
                        }
                    }
                    _ => DmContent::Info(if self.item_type == "media" {
                        "Photo unavailable".into()
                    } else {
                        "Disappearing photo or video — open the Instagram app to view it".into()
                    }),
                }
            }
            "media_share" => match parse::<Media>(self.get("media_share")) {
                Some(m) => DmContent::Post(Box::new(m)),
                None => DmContent::Info("Shared post unavailable".into()),
            },
            "clip" | "felix_share" => {
                let raw = match self.item_type.as_str() {
                    "clip" => &self.get("clip")["clip"],
                    _ => &self.get("felix_share")["video"],
                };
                match parse::<Media>(raw) {
                    Some(m) => {
                        let owner = m.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
                        DmContent::Shared {
                            title: format!("Reel by @{owner}"),
                            subtitle: truncate(&m.caption_text(), 120),
                            preview: m.image_url(320),
                            url: m.web_url().or_else(|| Some(format!("https://www.instagram.com/reel/{}/", m.code))),
                        }
                    }
                    None => DmContent::Info("Shared reel unavailable".into()),
                }
            }
            "story_share" => {
                let share = self.get("story_share");
                match parse::<Media>(&share["media"]) {
                    Some(m) if m.image_url(0).is_some() => {
                        let owner = m.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
                        DmContent::Shared {
                            title: format!("Story by @{owner}"),
                            subtitle: str_at(share, &["text"]),
                            preview: m.image_url(320),
                            url: None,
                        }
                    }
                    _ => DmContent::Info(
                        non_empty(str_at(share, &["message"])).unwrap_or("Story unavailable".into()),
                    ),
                }
            }
            "reel_share" => {
                let share = self.get("reel_share");
                let kind = str_at(share, &["type"]);
                let title = match kind.as_str() {
                    "reaction" => "Reacted to a story",
                    "mention" => "Mentioned in a story",
                    _ => "Replied to a story",
                };
                let media = parse::<Media>(&share["media"]);
                DmContent::Shared {
                    title: title.into(),
                    subtitle: str_at(share, &["text"]),
                    preview: media.and_then(|m| m.image_url(320)),
                    url: None,
                }
            }
            "voice_media" => {
                let audio = &self.get("voice_media")["media"]["audio"];
                DmContent::Voice {
                    url: non_empty(str_at(audio, &["audio_src"])),
                    duration_ms: value_i64(&audio["duration"]),
                }
            }
            "animated_media" => {
                let images = &self.get("animated_media")["images"];
                DmContent::Gif(
                    non_empty(str_at(images, &["fixed_height", "url"]))
                        .or_else(|| non_empty(str_at(images, &["fixed_height", "webp"]))),
                )
            }
            "action_log" => DmContent::Info(
                non_empty(str_at(self.get("action_log"), &["description"]))
                    .unwrap_or("Chat updated".into()),
            ),
            "placeholder" => DmContent::Info(
                non_empty(str_at(self.get("placeholder"), &["message"]))
                    .unwrap_or("This message is unavailable".into()),
            ),
            "profile" => match parse::<User>(self.get("profile")) {
                Some(u) => DmContent::Shared {
                    title: format!("@{}", u.username),
                    subtitle: u.full_name.clone(),
                    preview: non_empty(u.profile_pic_url.clone()),
                    url: Some(format!("https://www.instagram.com/{}/", u.username)),
                },
                None => DmContent::Info("Shared profile".into()),
            },
            t if t.contains("xma") => {
                let raw = self.get(t);
                let x = if raw.is_array() { &raw[0] } else { raw };
                let header = str_at(x, &["header_title_text"]);
                let title = non_empty(str_at(x, &["title_text"])).unwrap_or_default();
                DmContent::Shared {
                    title: non_empty(header.clone()).unwrap_or_else(|| {
                        if t.contains("clip") || t.contains("reel") {
                            "Shared reel".into()
                        } else if t.contains("story") {
                            "Shared story".into()
                        } else {
                            "Shared post".into()
                        }
                    }),
                    subtitle: truncate(&title, 120),
                    preview: non_empty(str_at(x, &["preview_url"]))
                        .or_else(|| non_empty(str_at(x, &["preview_url_info", "url"]))),
                    url: non_empty(str_at(x, &["target_url"])),
                }
            }
            other => match non_empty(str_at(self.get("text"), &[])) {
                Some(text) => DmContent::Text(text),
                None => DmContent::Info(format!(
                    "Unsupported message type ({})",
                    if other.is_empty() { "unknown" } else { other }
                )),
            },
        }
    }
}

impl DmContent {
    /// A one-line summary for the conversation list.
    pub fn summary(&self) -> String {
        match self {
            DmContent::Text(t) => t.lines().next().unwrap_or_default().to_string(),
            DmContent::Like(l) => l.clone(),
            DmContent::Link { text } => text.clone(),
            DmContent::Media { video: Some(_), .. } => "Sent a video".into(),
            DmContent::Media { .. } => "Sent a photo".into(),
            DmContent::Post(m) => format!(
                "Shared a post by @{}",
                m.user.as_ref().map(|u| u.username.as_str()).unwrap_or("someone")
            ),
            DmContent::Shared { title, .. } => title.clone(),
            DmContent::Voice { .. } => "Voice message".into(),
            DmContent::Gif(_) => "Sent a GIF".into(),
            DmContent::Info(i) => i.clone(),
        }
    }
}
