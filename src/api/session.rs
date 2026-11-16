use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Everything needed to talk to Instagram as the signed-in user.
///
/// The session is a set of instagram.com cookies (most importantly `sessionid`)
/// plus a stable per-install device id. It is stored with 0600 permissions in
/// `$XDG_CONFIG_HOME/finsta/session.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub cookies: BTreeMap<String, String>,
    pub device_uuid: String,
    #[serde(default)]
    pub www_claim: String,
    #[serde(default)]
    pub username: String,
    /// The browser user agent the session was created with; API calls use
    /// the same one. Empty for pasted cookies (a default is used).
    #[serde(default)]
    pub user_agent: String,
}

impl Session {
    pub fn from_cookies(cookies: BTreeMap<String, String>) -> Result<Self> {
        let mut session = Session {
            cookies,
            device_uuid: uuid::Uuid::new_v4().to_string(),
            www_claim: "0".into(),
            username: String::new(),
            user_agent: String::new(),
        };
        let sessionid = session
            .cookies
            .get("sessionid")
            .map(|s| s.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        if sessionid.is_empty() {
            bail!("The sessionid cookie is missing");
        }
        session.cookies.insert("sessionid".into(), sessionid);
        if session.user_id().is_empty() {
            bail!("Could not read the user id from the sessionid cookie");
        }
        if !session.cookies.contains_key("ds_user_id") {
            let id = session.user_id();
            session.cookies.insert("ds_user_id".into(), id);
        }
        Ok(session)
    }

    /// The numeric id of the signed-in account.
    pub fn user_id(&self) -> String {
        if let Some(id) = self.cookies.get("ds_user_id").filter(|s| !s.is_empty()) {
            return id.clone();
        }
        // A sessionid looks like "<user id>%3A<token>%3A<n>..."
        let sid = self.cookies.get("sessionid").cloned().unwrap_or_default();
        let sid = sid.replace("%3A", ":").replace("%3a", ":");
        let id = sid.split(':').next().unwrap_or_default();
        if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
            id.to_string()
        } else {
            String::new()
        }
    }

    pub fn csrf_token(&self) -> String {
        self.cookies.get("csrftoken").cloned().unwrap_or_default()
    }

    fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("finsta").join("session.json"))
    }

    pub fn load() -> Option<Self> {
        let data = fs::read_to_string(Self::path()?).ok()?;
        let session: Session = serde_json::from_str(&data).ok()?;
        if session.cookies.get("sessionid").is_none_or(|s| s.is_empty()) {
            return None;
        }
        Some(session)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("No config directory")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn delete() {
        if let Some(path) = Self::path() {
            let _ = fs::remove_file(path);
        }
    }
}
