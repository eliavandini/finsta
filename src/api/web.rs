//! More of what instagram.com offers: single posts, saved posts, search, and
//! downloading media.

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::{Client, Clip, Media, Page, User, WEB_REFERER, is_login_required, is_rate_limited, media_pk, parse};

/// A folder of saved posts.
#[derive(Debug, Clone, Default)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub count: Option<i64>,
    pub cover: Option<String>,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Every folder in an answer (the older route and the website's query
/// both describe them with a collection_id and a collection_name).
fn collect_collections(v: &Value, out: &mut Vec<Collection>) {
    match v {
        Value::Object(o) if o.contains_key("collection_id") && o.contains_key("collection_name") => {
            let id = text(&v["collection_id"]);
            let auto = text(&v["collection_type"]).contains("AUTO") || id.contains("AUTO");
            if id.is_empty() || auto || out.iter().any(|c| c.id == id) {
                return;
            }
            let cover = [&v["cover_media"], &v["cover_media_list"][0]]
                .into_iter()
                .filter_map(parse::<Media>)
                .find_map(|m| m.image_url(320));
            out.push(Collection {
                id,
                name: text(&v["collection_name"]),
                count: v["collection_media_count"].as_i64(),
                cover,
            });
        }
        Value::Object(o) => o.values().for_each(|x| collect_collections(x, out)),
        Value::Array(a) => a.iter().for_each(|x| collect_collections(x, out)),
        _ => {}
    }
}

/// The media id behind a post's shortcode (the CODE in /p/CODE/).
pub fn shortcode_pk(code: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut n: u128 = 0;
    for c in code.bytes().take(11) {
        let i = ALPHABET.iter().position(|&a| a == c)?;
        n = n.checked_mul(64)?.checked_add(i as u128)?;
    }
    (n > 0).then(|| n.to_string())
}

/// The post code in an instagram.com link to a post or reel.
pub fn link_code(url: &str) -> Option<String> {
    let path = url.split("instagram.com/").nth(1)?;
    let parts: Vec<&str> = path.split(['/', '?', '#']).collect();
    let at = parts.iter().position(|p| matches!(*p, "p" | "reel" | "reels" | "tv"))?;
    let code = parts.get(at + 1)?;
    (!code.is_empty() && code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
        .then(|| code.to_string())
}

/// The story id in an instagram.com/stories/<user>/<id>/ link.
pub fn story_link_pk(url: &str) -> Option<String> {
    let path = url.split("instagram.com/stories/").nth(1)?;
    let pk = path.split(['/', '?', '#']).nth(1)?;
    (!pk.is_empty() && pk.bytes().all(|c| c.is_ascii_digit())).then(|| pk.to_string())
}

fn looks_like_media(o: &serde_json::Map<String, Value>) -> bool {
    o.get("code").and_then(Value::as_str).is_some_and(|c| !c.is_empty())
        && ["image_versions2", "carousel_media", "video_versions"]
            .iter()
            .any(|k| o.get(*k).is_some_and(|v| !v.is_null()))
}

/// Every post in an answer, wherever it sits (answers nest them differently).
fn collect_media(v: &Value, seen: &mut HashSet<String>, out: &mut Vec<Media>) {
    match v {
        Value::Object(o) if looks_like_media(o) => {
            let code = o["code"].as_str().unwrap_or_default().to_string();
            if seen.insert(code)
                && let Some(m) = parse::<Media>(v)
            {
                out.push(m);
            }
        }
        Value::Object(o) => o.values().for_each(|x| collect_media(x, seen, out)),
        Value::Array(a) => a.iter().for_each(|x| collect_media(x, seen, out)),
        _ => {}
    }
}

/// Every account in an answer's "users" lists.
fn collect_users(v: &Value, out: &mut Vec<User>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if k == "users"
                    && let Some(list) = x.as_array()
                {
                    for u in list {
                        let u = if u["user"].is_object() { &u["user"] } else { u };
                        if let Some(user) = parse::<User>(u).filter(|u| !u.pk.is_empty() && !u.username.is_empty())
                            && !out.iter().any(|o| o.pk == user.pk)
                        {
                            out.push(user);
                        }
                    }
                } else {
                    collect_users(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_users(x, out)),
        _ => {}
    }
}

/// Where a media item can be downloaded from, with a file extension.
pub fn download_urls(media: &Media) -> Vec<(String, &'static str)> {
    let one = |m: &Media| -> Option<(String, &'static str)> {
        if m.is_video()
            && let Some(url) = m.video_url()
        {
            return Some((url, "mp4"));
        }
        m.image_url(i64::MAX).map(|url| (url, "jpg"))
    };
    match media.carousel_media.as_ref().filter(|c| !c.is_empty()) {
        Some(items) => items.iter().filter_map(one).collect(),
        None => one(media).into_iter().collect(),
    }
}

impl Client {
    /// One post or reel, by the code in its link.
    pub async fn media_by_code(&self, code: &str) -> Result<Media> {
        match self.web_query("PolarisPostRootQuery", json!({"shortcode": code}), WEB_REFERER).await {
            Ok(data) => {
                let mut found = Vec::new();
                collect_media(&data, &mut HashSet::new(), &mut found);
                let found_any = !found.is_empty();
                if let Some(m) = found.into_iter().find(|m| m.code == code) {
                    return Ok(m);
                }
                // What came back instead (field names only).
                let items = data["xdt_api__v1__media__shortcode__web_info"]["items"].as_array();
                let has = |k: &str| items.is_some_and(|i| i.iter().any(|m| !m[k].is_null()));
                eprintln!(
                    "finsta: post query answered without the post ({} items; code {}, images {}, user {}, other posts {found_any}); trying the older route",
                    items.map_or(0, Vec::len),
                    has("code"),
                    has("image_versions2"),
                    has("user"),
                );
            }
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: post query failed ({e}); trying the older route"),
        }
        let pk = shortcode_pk(code).ok_or_else(|| anyhow!("That isn't a link to a post"))?;
        self.media_info(&pk).await
    }

    /// A post or story by its numeric id.
    pub async fn media_info(&self, pk: &str) -> Result<Media> {
        let v = self.get(&format!("media/{pk}/info/"), &[]).await?;
        parse::<Media>(&v["items"][0]).ok_or_else(|| anyhow!("This isn't available anymore"))
    }

    /// The audio of a photo post's or photo story's song. instagram.com's
    /// queries leave it out, so it comes from the post's details.
    pub async fn song_clip(&self, media: &Media) -> Result<Option<Clip>> {
        if let Some(clip) = media.song_clip() {
            return Ok(Some(clip));
        }
        let v = self.get(&format!("media/{}/info/", media_pk(&media.id)), &[]).await?;
        let full = parse::<Media>(&v["items"][0]);
        let clip = full.as_ref().and_then(Media::song_clip);
        if clip.is_none() {
            eprintln!(
                "finsta: music: the post's details have no audio for its song (fields {:?})",
                full.as_ref().map(Media::music_fields).unwrap_or_default()
            );
        }
        Ok(clip)
    }

    /// Your saved posts, a page at a time: all of them, or one folder's.
    pub async fn saved_posts(&self, folder: Option<&str>, max_id: Option<String>) -> Result<Page<Media>> {
        let mut query = Vec::new();
        if let Some(id) = &max_id {
            query.push(("max_id", id.clone()));
        }
        let path = match folder {
            Some(id) => format!("feed/collection/{id}/posts/"),
            None => "feed/saved/posts/".to_string(),
        };
        let v = self.get(&path, &query).await?;
        let items: Vec<Media> = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| parse::<Media>(if i["media"].is_object() { &i["media"] } else { i }))
            .collect();
        let next = v["next_max_id"].as_str().map(str::to_string).filter(|s| !s.is_empty());
        let more = v["more_available"].as_bool().unwrap_or(next.is_some());
        eprintln!(
            "finsta: saved: {} posts{}, more {more}",
            items.len(),
            if folder.is_some() { " in a folder" } else { "" }
        );
        Ok(Page { items, next_max_id: next, more_available: more })
    }

    /// Your folders of saved posts (not "All posts" itself).
    pub async fn saved_folders(&self) -> Result<Vec<Collection>> {
        let query = [
            ("collection_types", r#"["ALL_MEDIA_AUTO_COLLECTION","MEDIA"]"#.to_string()),
            ("include_public_only", "0".to_string()),
            ("get_cover_media_lists", "true".to_string()),
        ];
        let mut out = Vec::new();
        match self.get("collections/list/", &query).await {
            Ok(v) => {
                collect_collections(&v, &mut out);
                eprintln!("finsta: saved: {} folders", out.len());
                return Ok(out);
            }
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: saved folders: older route failed ({e}); trying the website's query"),
        }
        let data = self
            .web_query("PolarisSavedCollectionPickerQuery", json!({"first": 50, "after": null}), WEB_REFERER)
            .await?;
        collect_collections(&data, &mut out);
        eprintln!("finsta: saved: {} folders (website query)", out.len());
        Ok(out)
    }

    /// Save or unsave a post. With a folder, it's saved into that folder.
    pub async fn save_post(&self, media: &Media, save: bool, folder: Option<&str>) -> Result<()> {
        let pk = media_pk(&media.id).to_string();
        if folder.is_none() {
            let name = if save { "usePolarisSaveMediaSaveMutation" } else { "usePolarisSaveMediaUnsaveMutation" };
            match self.web_query(name, json!({"input": {"media_id": pk}}), WEB_REFERER).await {
                Ok(data) => {
                    if !data["xig_media_save"].is_object() && !data["xig_media_unsave"].is_object() {
                        eprintln!("finsta: save: Instagram answered without confirming");
                    }
                    return Ok(());
                }
                Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
                Err(e) => eprintln!("finsta: save query failed ({e}); trying the older route"),
            }
        }
        let mut form = vec![("module_name", "feed_timeline".to_string())];
        if let Some(id) = folder {
            let list = if id.bytes().all(|b| b.is_ascii_digit()) { format!("[{id}]") } else { json!([id]).to_string() };
            form.push(("added_collection_ids", list));
        }
        let action = if save { "save" } else { "unsave" };
        let v = self.post(&format!("media/{pk}/{action}/"), &form).await?;
        if v["status"] != "ok" {
            bail!("Instagram didn't confirm it");
        }
        Ok(())
    }

    /// Accounts matching `query` (instagram.com's search box).
    pub async fn search_people(&self, query: &str) -> Result<Vec<User>> {
        let vars = json!({
            "data": {
                "context": "blended",
                "include_reel": "true",
                "query": query,
                "rank_token": "",
                "search_session_id": uuid::Uuid::new_v4().to_string(),
                "search_surface": "web_top_search",
            },
            "hasQuery": true,
        });
        match self.web_query("PolarisSearchBoxRefetchableQuery", vars, WEB_REFERER).await {
            Ok(data) => {
                let mut users = Vec::new();
                collect_users(&data, &mut users);
                if !users.is_empty() {
                    users.truncate(20);
                    return Ok(users);
                }
                eprintln!("finsta: search query found no accounts; trying the older route");
            }
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: search query failed ({e}); trying the older route"),
        }
        self.search_users(query).await
    }

    /// People you can message whose name matches `query` (the search in
    /// instagram.com's messages), to find conversations not loaded yet.
    pub async fn search_contacts(&self, query: &str) -> Result<Vec<User>> {
        let data = self
            .web_query(
                "IGDOmniPickerSearchResultsListQuery",
                json!({"search_text": query}),
                "https://www.instagram.com/direct/inbox/",
            )
            .await?;
        let users: Vec<User> = data["xfb_combinedIGSearchQuery"]["search_messenger_contact_v2"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse::<User>)
            .filter(|u| !u.pk.is_empty() && !u.username.is_empty())
            .take(10)
            .collect();
        Ok(users)
    }

    /// Top posts for a hashtag (with or without the #): one short, fixed
    /// list. (instagram.com's search results page for words is gone.)
    pub async fn search_posts(&self, query: &str) -> Result<Vec<Media>> {
        let Some(tag) = hashtag(query) else { return Ok(Vec::new()) };
        // instagram.com's hashtag page query hasn't been seen yet (the scan
        // looks for it); until then, the app's hashtag routes.
        let form = [
            ("tab", "top".to_string()),
            ("surface", "grid".to_string()),
            ("include_persistent", "0".to_string()),
            ("page", "0".to_string()),
        ];
        let v = match self.post(&format!("tags/{tag}/sections/"), &form).await {
            Ok(v) => v,
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => {
                eprintln!("finsta: search: hashtag sections failed ({e}); trying the hashtag feed");
                self.get(&format!("feed/tag/{tag}/"), &[]).await?
            }
        };
        let mut posts = Vec::new();
        collect_media(&v, &mut HashSet::new(), &mut posts);
        posts.retain(|m| !m.is_ad());
        posts.truncate(24);
        eprintln!("finsta: search: {} posts for a hashtag", posts.len());
        Ok(posts)
    }

    /// The bytes behind a media address (photos and videos on Instagram's CDN).
    pub async fn download(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .inner
            .http
            .get(url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.without_url())?;
        Ok(resp.bytes().await.map_err(|e| e.without_url())?.to_vec())
    }
}

/// The hashtag a search is for: one word, with or without the #.
pub fn hashtag(query: &str) -> Option<String> {
    let tag = query.trim().trim_start_matches('#');
    (!tag.is_empty() && tag.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| tag.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcodes_and_links() {
        assert_eq!(shortcode_pk("B").as_deref(), Some("1"));
        assert_eq!(shortcode_pk("BA").as_deref(), Some("64"));
        assert_eq!(link_code("https://www.instagram.com/p/C0deX_1-/?igsh=x").as_deref(), Some("C0deX_1-"));
        assert_eq!(link_code("https://www.instagram.com/reel/ABC123/").as_deref(), Some("ABC123"));
        assert_eq!(link_code("https://www.instagram.com/someone/p/XYZ/").as_deref(), Some("XYZ"));
        assert_eq!(link_code("https://www.instagram.com/stories/someone/123/"), None);
        assert_eq!(story_link_pk("https://www.instagram.com/stories/someone/123/?igsh=x").as_deref(), Some("123"));
        assert_eq!(story_link_pk("https://www.instagram.com/stories/someone/"), None);
        assert_eq!(hashtag("#Sunset").as_deref(), Some("sunset"));
        assert_eq!(hashtag("sunset"), Some("sunset".into()));
        assert_eq!(hashtag("two words"), None);
    }

    #[test]
    fn folders() {
        let v = json!({"items": [
            {"collection_id": "ALL_MEDIA_AUTO_COLLECTION", "collection_name": "All posts", "collection_type": "ALL_MEDIA_AUTO_COLLECTION"},
            {"collection_id": 17890, "collection_name": "Recipes", "collection_type": "MEDIA", "collection_media_count": 4,
             "cover_media_list": [{"image_versions2": {"candidates": [{"url": "https://x/c.jpg", "width": 640, "height": 640}]}}]},
        ]});
        let mut out = Vec::new();
        collect_collections(&v, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "17890");
        assert_eq!(out[0].count, Some(4));
        assert_eq!(out[0].cover.as_deref(), Some("https://x/c.jpg"));
    }
}
