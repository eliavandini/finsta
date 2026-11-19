//! GIF search through GIPHY, with your own API key (Preferences).

use anyhow::{Result, bail};
use serde_json::Value;

/// A GIF from GIPHY: its id, a still frame to show, and its page.
#[derive(Debug, Clone)]
pub struct Gif {
    pub id: String,
    pub still: String,
    pub aspect: f64,
    pub page: String,
}

impl super::Client {
    /// GIFs matching `query`, or trending ones when it's empty. Only the key
    /// and the query go to GIPHY, never anything about your account.
    pub async fn search_gifs(&self, key: &str, query: &str) -> Result<Vec<Gif>> {
        // Can point at a mock server for testing (see dev/mock_api.py).
        let base = std::env::var("FINSTA_GIPHY_BASE").unwrap_or_else(|_| "https://api.giphy.com/v1/gifs/".into());
        let query = query.trim();
        let mut q = vec![("api_key", key.trim()), ("limit", "24")];
        let path = if query.is_empty() {
            "trending"
        } else {
            q.push(("q", query));
            "search"
        };
        // Errors leave out the address: it holds the key.
        let res = self.inner.http.get(format!("{base}{path}")).query(&q).send().await.map_err(|e| e.without_url())?;
        let status = res.status();
        if status == 401 || status == 403 {
            bail!("GIPHY didn't accept the API key");
        }
        if !status.is_success() {
            bail!("GIPHY answered {status}");
        }
        let v: Value = res.json().await.map_err(|e| e.without_url())?;
        Ok(parse(&v))
    }
}

fn parse(v: &Value) -> Vec<Gif> {
    let s = |x: &Value| x.as_str().unwrap_or_default().to_string();
    let n = |x: &Value| x.as_str().and_then(|t| t.parse::<f64>().ok()).or(x.as_f64()).unwrap_or(0.0);
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let still = &g["images"]["fixed_width_still"];
            let (id, url) = (s(&g["id"]), s(&still["url"]));
            if id.is_empty() || url.is_empty() {
                return None;
            }
            let (w, h) = (n(&still["width"]), n(&still["height"]));
            Some(Gif { id, still: url, aspect: if w > 0.0 { h / w } else { 1.0 }, page: s(&g["url"]) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_results() {
        let v = serde_json::json!({"data": [
            {"id": "abc", "url": "https://giphy.com/gifs/abc", "images": {"fixed_width_still": {"url": "https://media.giphy.com/s.gif", "width": "200", "height": "100"}}},
            {"id": "", "images": {}},
        ]});
        let gifs = super::parse(&v);
        assert_eq!(gifs.len(), 1);
        assert_eq!(gifs[0].id, "abc");
        assert!((gifs[0].aspect - 0.5).abs() < 1e-9);
    }
}
