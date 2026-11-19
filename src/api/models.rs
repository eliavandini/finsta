//! Lenient models for the parts of Instagram's private API that Finsta uses.
//!
//! Instagram is inconsistent about types (ids are sometimes numbers, sometimes
//! strings; timestamps are sometimes strings) and frequently adds or drops
//! fields, so everything here defaults instead of failing.

use serde::{Deserialize, Deserializer, Serialize};
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

/// A value that may be `null` (GraphQL answers spell out missing fields).
pub fn de_or_default<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
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
    #[serde(default, deserialize_with = "de_or_default")]
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
    pub has_viewer_saved: bool,
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
    /// Sent back with likes, as the website does.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub organic_tracking_token: Option<String>,
    /// Reels: the song or sound they use.
    #[serde(default)]
    pub clips_metadata: Option<Value>,
    #[serde(default)]
    pub music_metadata: Option<Value>,
    /// Stories: stickers for a song, a shared post, a link and mentions.
    #[serde(default)]
    pub story_music_stickers: Option<Value>,
    #[serde(default)]
    pub story_feed_media: Option<Value>,
    #[serde(default)]
    pub story_link_stickers: Option<Value>,
    #[serde(default)]
    pub story_bloks_stickers: Option<Value>,
    /// Stories that reshare someone else's story.
    #[serde(default)]
    pub reshared_story_media_author: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Location {
    #[serde(default, deserialize_with = "de_string")]
    pub name: String,
}

/// The song or sound a post, reel or story uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Song {
    /// Instagram's id for the sound.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub cover: Option<String>,
}

impl Song {
    /// "Title · Artist".
    pub fn label(&self) -> String {
        match (self.title.is_empty(), self.artist.is_empty()) {
            (false, false) => format!("{} · {}", self.title, self.artist),
            (false, true) => self.title.clone(),
            _ => self.artist.clone(),
        }
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The part of a song a photo post or a photo story plays.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub url: String,
    pub start_ms: u64,
    pub duration_ms: Option<u64>,
}

/// The first non-empty value under `key`, anywhere in `v` (a few levels deep).
fn find_key<'a>(v: &'a Value, key: &str, depth: u32) -> Option<&'a Value> {
    match v {
        Value::Object(o) => o
            .get(key)
            .filter(|x| !x.is_null() && x.as_str() != Some(""))
            .or_else(|| (depth > 0).then(|| o.values().find_map(|x| find_key(x, key, depth - 1))).flatten()),
        Value::Array(a) if depth > 0 => a.iter().find_map(|x| find_key(x, key, depth - 1)),
        _ => None,
    }
}

fn clip_in(music: &Value) -> Option<Clip> {
    let url = ["progressive_download_url", "fast_start_progressive_download_url", "web_30s_preview_download_url"]
        .into_iter()
        .filter_map(|k| find_key(music, k, 4))
        .map(text_of)
        .find(|u| u.starts_with("http"))?;
    let ms = |k: &str| find_key(music, k, 4).map(value_i64).filter(|n| *n >= 0).map(|n| n as u64);
    Some(Clip {
        url,
        start_ms: ms("audio_asset_start_time_in_ms").unwrap_or(0),
        duration_ms: ms("overlap_duration_in_ms").filter(|n| *n > 0),
    })
}

fn licensed_song(info: &Value) -> Option<Song> {
    let song = Song {
        id: [&info["audio_cluster_id"], &info["audio_asset_id"], &info["id"]]
            .into_iter()
            .map(text_of)
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        title: text_of(&info["title"]),
        artist: text_of(&info["display_artist"]),
        cover: [&info["cover_artwork_thumbnail_uri"], &info["cover_artwork_uri"]]
            .into_iter()
            .map(text_of)
            .find(|s| s.starts_with("http")),
    };
    (!song.title.is_empty() || !song.artist.is_empty()).then_some(song)
}

impl Media {
    /// The audio of a photo post's or photo story's song, if Instagram sent
    /// it (videos carry their sound themselves).
    pub fn song_clip(&self) -> Option<Clip> {
        [&self.story_music_stickers, &self.music_metadata]
            .into_iter()
            .flatten()
            .find_map(clip_in)
    }

    /// The field names under a post's music (for the log; no values).
    pub fn music_fields(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        fn walk(v: &Value, prefix: &str, out: &mut std::collections::BTreeSet<String>, depth: u32) {
            match v {
                Value::Object(o) if depth > 0 => {
                    for (k, x) in o {
                        let name = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                        if x.is_object() || x.is_array() {
                            walk(x, &name, out, depth - 1);
                        } else if !x.is_null() {
                            out.insert(name);
                        }
                    }
                }
                Value::Array(a) if depth > 0 => {
                    if let Some(first) = a.first() {
                        walk(first, &format!("{prefix}[0]"), out, depth - 1);
                    }
                }
                _ => {}
            }
        }
        for (name, v) in [("stickers", &self.story_music_stickers), ("music", &self.music_metadata)] {
            if let Some(v) = v {
                walk(v, name, &mut out, 4);
            }
        }
        out.into_iter().collect()
    }

    /// The song or sound a reel or story uses.
    pub fn song(&self) -> Option<Song> {
        let null = Value::Null;
        let sticker = self.story_music_stickers.as_ref().and_then(|v| v.get(0)).unwrap_or(&null);
        if let Some(s) = licensed_song(&sticker["music_asset_info"]) {
            return Some(s);
        }
        for meta in [&self.clips_metadata, &self.music_metadata].into_iter().flatten() {
            if let Some(s) = licensed_song(&meta["music_info"]["music_asset_info"]) {
                return Some(s);
            }
            let sound = &meta["original_sound_info"];
            let title = text_of(&sound["original_audio_title"]);
            let artist = text_of(&sound["ig_artist"]["username"]);
            if !title.is_empty() || !artist.is_empty() {
                return Some(Song {
                    id: [&sound["audio_asset_id"], &sound["audio_cluster_id"], &sound["original_media_id"]]
                        .into_iter()
                        .map(text_of)
                        .find(|s| !s.is_empty())
                        .unwrap_or_default(),
                    title: if title.is_empty() { "Original audio".into() } else { title },
                    artist,
                    cover: Some(text_of(&sound["ig_artist"]["profile_pic_url"])).filter(|u| u.starts_with("http")),
                });
            }
        }
        None
    }

    /// The code of the post or reel a story shares.
    pub fn shared_post_code(&self) -> Option<String> {
        self.story_feed_media
            .as_ref()?
            .as_array()?
            .iter()
            .filter_map(|m| m["media_code"].as_str())
            .find(|c| !c.is_empty())
            .map(str::to_string)
    }

    /// Whether the shared post is a reel.
    pub fn shared_post_is_reel(&self) -> bool {
        self.story_feed_media
            .as_ref()
            .and_then(|v| v.get(0))
            .and_then(|m| m["product_type"].as_str())
            .is_some_and(|t| t == "clips")
    }

    /// Who posted the story this one reshares.
    pub fn reshared_from(&self) -> Option<User> {
        let v = self.reshared_story_media_author.as_ref()?;
        let user = User {
            pk: v["id"].as_str().or(v["pk"].as_str()).unwrap_or("").to_string(),
            username: v["username"].as_str().unwrap_or("").to_string(),
            profile_pic_url: v["profile_pic_url"].as_str().unwrap_or("").to_string(),
            ..Default::default()
        };
        (!user.username.is_empty()).then_some(user)
    }

    /// The address a story's link sticker points to.
    pub fn story_link(&self) -> Option<String> {
        self.story_link_stickers
            .as_ref()?
            .as_array()?
            .iter()
            .filter_map(|l| l["story_link"]["url"].as_str())
            .find(|u| u.starts_with("http"))
            .map(str::to_string)
    }

    /// Accounts mentioned in a story.
    pub fn mentions(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for st in self.story_bloks_stickers.as_ref().and_then(Value::as_array).into_iter().flatten() {
            if let Some(u) = st["bloks_sticker"]["sticker_data"]["ig_mention"]["username"].as_str()
                && !u.is_empty()
                && !out.iter().any(|o| o == u)
            {
                out.push(u.to_string());
            }
        }
        out
    }

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
    #[serde(default, deserialize_with = "de_or_default")]
    pub user: User,
    #[serde(default, deserialize_with = "de_or_default")]
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
    #[serde(default, deserialize_with = "de_id")]
    pub pk: String,
    #[serde(default, deserialize_with = "de_string")]
    pub text: String,
    #[serde(default)]
    pub user: User,
    #[serde(default, deserialize_with = "de_i64")]
    pub created_at: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub comment_like_count: i64,
    /// Replies to it: how many, and the few that come with it.
    #[serde(default, deserialize_with = "de_i64")]
    pub child_comment_count: i64,
    #[serde(default, deserialize_with = "de_or_default")]
    pub preview_child_comments: Vec<Comment>,
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
    /// Notifications turned off for it on Instagram.
    #[serde(default, deserialize_with = "de_bool")]
    pub muted: bool,
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

/// A photo or video sent with "View once" or "Allow replay".
#[derive(Debug, Clone)]
pub struct Disappearing {
    /// Only present while you're still allowed to open it.
    pub image: Option<String>,
    pub video: Option<String>,
    pub is_video: bool,
    /// "once" or "replayable".
    pub view_mode: String,
    pub seen_user_ids: Vec<String>,
    /// For messages you sent: what happened to it ("raven_opened", ...).
    pub action: String,
}

impl Disappearing {
    pub fn is_available(&self) -> bool {
        self.image.is_some() || self.video.is_some()
    }

    /// How many times the recipient may open it.
    pub fn allowed_views(&self) -> u32 {
        if self.view_mode == "replayable" { 2 } else { 1 }
    }

    pub fn kind(&self) -> &'static str {
        if self.is_video { "video" } else { "photo" }
    }
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
        /// Height / width of the preview.
        aspect: f64,
    },
    Voice {
        url: Option<String>,
        duration_ms: i64,
    },
    /// A GIF or a sticker (no bubble); `aspect` is height / width.
    Gif { url: Option<String>, aspect: f64, sticker: bool },
    Disappearing(Disappearing),
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

    /// The emoji `user` reacted with, if any.
    pub fn reaction_by(&self, user: &str) -> Option<String> {
        self.get("reactions")["emojis"]
            .as_array()?
            .iter()
            .find(|e| e["sender_id"].as_str() == Some(user))
            .and_then(|e| e["emoji"].as_str().map(str::to_string))
    }

    /// Set (or with None, remove) `user`'s reaction, as shown here.
    pub fn set_reaction(&mut self, user: &str, emoji: Option<&str>) {
        let mut list: Vec<Value> = self.get("reactions")["emojis"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e["sender_id"].as_str() != Some(user))
            .collect();
        if let Some(emoji) = emoji {
            list.push(serde_json::json!({"emoji": emoji, "sender_id": user}));
        }
        self.extra.insert("reactions".into(), serde_json::json!({"emojis": list}));
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

    /// The object describing a disappearing ("visual"/"raven") message.
    fn visual(&self) -> &Value {
        let v = self.get("visual_media");
        if v.is_object() { v } else { self.get("raven_media") }
    }

    /// A field of the visual message; older responses put some of these on the item itself.
    fn visual_field(&self, key: &str) -> &Value {
        let v = &self.visual()[key];
        if v.is_null() { self.get(key) } else { v }
    }

    fn disappearing(&self) -> DmContent {
        let visual = self.visual();
        let raw = if visual["media"].is_object() { &visual["media"] } else { visual };
        let media = parse::<Media>(raw).unwrap_or_default();
        let view_mode = non_empty(str_at(self.visual_field("view_mode"), &[])).unwrap_or("once".into());
        let has_media = media.image_url(0).is_some() || media.video_url().is_some();
        if view_mode == "permanent" && has_media {
            // The sender chose "Keep in chat": it's an ordinary photo or video.
            return DmContent::Media {
                image: media.image_url(640),
                video: media.video_url(),
                aspect: media.aspect(),
            };
        }
        let seen_user_ids = self
            .visual_field("seen_user_ids")
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        DmContent::Disappearing(Disappearing {
            image: media.image_url(1080),
            video: media.video_url(),
            is_video: media.is_video() || media.video_url().is_some(),
            view_mode,
            seen_user_ids,
            action: str_at(self.visual_field("expiring_media_action_summary"), &["type"]),
        })
    }

    /// The parts of a message that can change after it was sent and that
    /// affect how it's shown (reactions, whether a disappearing photo was opened).
    pub fn state_key(&self) -> String {
        let mut key = self.reactions().join(",");
        if let DmContent::Disappearing(d) = self.content() {
            key.push_str(&format!(
                "|{}|{}|{}",
                d.seen_user_ids.join(","),
                d.action,
                d.is_available()
            ));
        }
        key
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
            "media" => match parse::<Media>(self.get("media")) {
                Some(m) if m.image_url(0).is_some() || m.video_url().is_some() => DmContent::Media {
                    image: m.image_url(640),
                    video: m.video_url(),
                    aspect: m.aspect(),
                },
                _ => DmContent::Info("Photo unavailable".into()),
            },
            "raven_media" | "visual_media" => self.disappearing(),
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
                            aspect: 16.0 / 9.0,
                        }
                    }
                    None => DmContent::Info("Shared reel unavailable".into()),
                }
            }
            "xma_share" => {
                let x = self.get("xma_share");
                let kind = str_at(x, &["kind"]);
                let author = str_at(x, &["author"]);
                let what = match kind.as_str() {
                    "post" => "Post",
                    "reel" => "Reel",
                    "story" => "Story",
                    _ => "Shared",
                };
                DmContent::Shared {
                    title: if author.is_empty() { what.to_string() } else { format!("{what} by {author}") },
                    subtitle: truncate(&str_at(x, &["caption"]), 160),
                    preview: non_empty(str_at(x, &["preview"])),
                    url: non_empty(str_at(x, &["url"])),
                    aspect: x["aspect"].as_f64().filter(|a| *a > 0.2 && *a < 5.0).unwrap_or(1.0).min(16.0 / 9.0),
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
                            aspect: 16.0 / 9.0,
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
                    aspect: 16.0 / 9.0,
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
                let a = self.get("animated_media");
                let size = &a["images"]["fixed_height"];
                let (w, h) = (value_i64(&size["width"]), value_i64(&size["height"]));
                DmContent::Gif {
                    url: non_empty(str_at(size, &["url"])).or_else(|| non_empty(str_at(size, &["webp"]))),
                    aspect: if w > 0 && h > 0 { h as f64 / w as f64 } else { 1.0 },
                    sticker: a["is_sticker"].as_bool() == Some(true),
                }
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
                    aspect: 1.0,
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
                    aspect: 1.0,
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
            DmContent::Gif { sticker: true, .. } => "Sent a sticker".into(),
            DmContent::Gif { .. } => "Sent a GIF".into(),
            DmContent::Disappearing(d) => format!("Sent a disappearing {}", d.kind()),
            DmContent::Info(i) => i.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comment_replies() {
        let list: Vec<Comment> = parse(&json!([
            {"pk": 17, "text": "hi", "child_comment_count": 2, "preview_child_comments": [{"pk": "18", "text": "hey"}]},
            {"pk": 19, "text": "no replies", "preview_child_comments": null},
        ]))
        .unwrap();
        assert_eq!((list[0].pk.as_str(), list[0].child_comment_count), ("17", 2));
        assert_eq!(list[0].preview_child_comments[0].pk, "18");
        assert!(list[1].preview_child_comments.is_empty());
    }

    /// A song's audio and the part of it a post or story plays.
    #[test]
    fn song_clips() {
        let base = json!({"pk": "1", "id": "1_2", "media_type": 1, "user": {"pk": "2", "username": "x"}});
        let mut post = base.clone();
        post["music_metadata"] = json!({"music_info": {
            "music_asset_info": {"title": "t", "progressive_download_url": "https://a/song.m4a"},
            "music_consumption_info": {"audio_asset_start_time_in_ms": 2000, "overlap_duration_in_ms": 8000}}});
        let clip = serde_json::from_value::<Media>(post).unwrap().song_clip().unwrap();
        assert_eq!((clip.url.as_str(), clip.start_ms, clip.duration_ms), ("https://a/song.m4a", 2000, Some(8000)));

        let mut story = base.clone();
        story["story_music_stickers"] = json!([{"music_asset_info": {"web_30s_preview_download_url": "https://a/p.m4a"}}]);
        let clip = serde_json::from_value::<Media>(story).unwrap().song_clip().unwrap();
        assert_eq!((clip.url.as_str(), clip.start_ms, clip.duration_ms), ("https://a/p.m4a", 0, None));

        // instagram.com's own answers name the song but not its audio.
        let mut named = base;
        named["music_metadata"] = json!({"music_info": {"music_asset_info": {"title": "t"}}});
        assert!(serde_json::from_value::<Media>(named).unwrap().song_clip().is_none());
    }

    /// instagram.com's GraphQL answers spell out absent fields as null.
    #[test]
    fn graphql_shapes_parse() {
        let post = json!({
            "pk": "3600000000000000001", "id": "3600000000000000001_42", "code": "Cabc", "media_type": 1,
            "taken_at": 1700000000, "caption": null, "carousel_media": null, "video_versions": null,
            "location": null, "like_count": 3, "has_liked": null,
            "image_versions2": {"candidates": [{"url": "u", "width": 1080, "height": 1350}]},
            "user": {"pk": "42", "username": "x", "profile_pic_url": "p", "friendship_status": null, "hd_profile_pic_url_info": null},
        });
        let m: Media = parse(&post).unwrap();
        assert_eq!(m.id, "3600000000000000001_42");
        assert!(m.caption.is_none() && !m.has_liked);
        let reel = json!({"id": "42", "user": null, "items": null, "seen": null, "latest_reel_media": 5});
        let r: Reel = parse(&reel).unwrap();
        assert!(r.items.is_empty() && r.user.pk.is_empty());
        let r: Reel = parse(&json!({"id": "42", "items": [post], "image_versions2": {"candidates": null}})).unwrap();
        assert_eq!(r.items.len(), 1);
    }

    #[test]
    fn songs() {
        let reel: Media = parse(&json!({"id": "1", "clips_metadata": {"music_info": {"music_asset_info": {
            "audio_cluster_id": 77, "title": "Night Drive", "display_artist": "Neon Coast", "cover_artwork_thumbnail_uri": "https://x/c.jpg"}}}}))
        .unwrap();
        let song = reel.song().unwrap();
        assert_eq!((song.id.as_str(), song.label().as_str()), ("77", "Night Drive · Neon Coast"));
        assert_eq!(song.cover.as_deref(), Some("https://x/c.jpg"));
        let original: Media = parse(&json!({"id": "2", "clips_metadata": {"music_info": null, "original_sound_info": {
            "audio_asset_id": "88", "original_audio_title": "", "ig_artist": {"username": "someone"}}}}))
        .unwrap();
        assert_eq!(original.song().unwrap().label(), "Original audio · someone");
        let story: Media = parse(&json!({"id": "3", "story_music_stickers": [{"music_asset_info": {"id": "99", "title": "Summer Song"}}]})).unwrap();
        assert_eq!(story.song().unwrap().label(), "Summer Song");
        let plain: Media = parse(&json!({"id": "4"})).unwrap();
        assert!(plain.song().is_none() && !plain.has_viewer_saved);
    }
}
