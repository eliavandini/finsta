//! More of what instagram.com offers: single posts, saved posts, search, and
//! downloading media.

use std::collections::HashSet;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::{Client, Media, Page, User, WEB_REFERER, is_login_required, is_rate_limited, parse};

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
                if let Some(m) = found.into_iter().find(|m| m.code == code) {
                    return Ok(m);
                }
                eprintln!("finsta: post query answered without the post; trying the older route");
            }
            Err(e) if is_login_required(&e) || is_rate_limited(&e) => return Err(e),
            Err(e) => eprintln!("finsta: post query failed ({e}); trying the older route"),
        }
        let pk = shortcode_pk(code).ok_or_else(|| anyhow!("That isn't a link to a post"))?;
        let v = self.get(&format!("media/{pk}/info/"), &[]).await?;
        parse::<Media>(&v["items"][0]).ok_or_else(|| anyhow!("This post isn't available"))
    }

    /// Your saved posts, a page at a time.
    pub async fn saved_posts(&self, max_id: Option<String>) -> Result<Page<Media>> {
        let mut query = Vec::new();
        if let Some(id) = &max_id {
            query.push(("max_id", id.clone()));
        }
        let v = self.get("feed/saved/posts/", &query).await?;
        let items: Vec<Media> = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| parse::<Media>(if i["media"].is_object() { &i["media"] } else { i }))
            .collect();
        let next = v["next_max_id"].as_str().map(str::to_string).filter(|s| !s.is_empty());
        let more = v["more_available"].as_bool().unwrap_or(next.is_some());
        eprintln!("finsta: saved: {} posts, more {more}", items.len());
        Ok(Page { items, next_max_id: next, more_available: more })
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

    /// Posts matching `query` (words or a #hashtag), as on instagram.com's
    /// search results page: one short, fixed list.
    pub async fn search_posts(&self, query: &str) -> Result<Vec<Media>> {
        let v = self
            .get(
                "fbsearch/web/top_serp/",
                &[("enable_metadata", "true".into()), ("query", query.to_string())],
            )
            .await?;
        let mut posts = Vec::new();
        collect_media(&v, &mut HashSet::new(), &mut posts);
        posts.truncate(24);
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
    }
}
