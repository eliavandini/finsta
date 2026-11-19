use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Everything needed to talk to Instagram as the signed-in user.
///
/// The session is a set of instagram.com cookies (most importantly `sessionid`)
/// plus a stable per-install device id. Each signed-in account has one, stored
/// with 0600 permissions in `$XDG_CONFIG_HOME/finsta/accounts/<user id>.json`.
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
    /// For the account switcher; refreshed whenever this account is used.
    #[serde(default)]
    pub profile_pic_url: String,
}

impl Session {
    pub fn from_cookies(cookies: BTreeMap<String, String>) -> Result<Self> {
        let mut session = Session {
            cookies,
            device_uuid: uuid::Uuid::new_v4().to_string(),
            www_claim: "0".into(),
            username: String::new(),
            user_agent: String::new(),
            profile_pic_url: String::new(),
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

    fn dir() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("finsta"))
    }

    /// Each account's session, by user id: `accounts/<id>.json`.
    fn account_path(id: &str) -> Option<PathBuf> {
        (!id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
            .then(|| Self::dir().map(|d| d.join("accounts").join(format!("{id}.json"))))
            .flatten()
    }

    fn read(path: &std::path::Path) -> Option<Self> {
        let session: Session = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
        session.cookies.get("sessionid").is_some_and(|s| !s.is_empty()).then_some(session)
    }

    /// The account in use: the one last chosen, or else any signed-in one.
    pub fn load() -> Option<Self> {
        Self::migrate();
        let current = fs::read_to_string(Self::dir()?.join("current")).unwrap_or_default();
        Self::account_path(current.trim())
            .and_then(|p| Self::read(&p))
            .or_else(|| Self::all().into_iter().next())
    }

    /// Every signed-in account, by username.
    pub fn all() -> Vec<Self> {
        let Some(dir) = Self::dir() else { return Vec::new() };
        let mut list: Vec<Session> = fs::read_dir(dir.join("accounts"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| Self::read(&e.path()))
            .collect();
        list.sort_by(|a, b| a.username.cmp(&b.username));
        list
    }

    /// Use this account from now on (also at the next start).
    pub fn make_current(&self) -> Result<()> {
        let dir = Self::dir().context("No config directory")?;
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("current"), self.user_id())?;
        Ok(())
    }

    /// The single `session.json` of older versions becomes an account.
    fn migrate() {
        let Some(old) = Self::dir().map(|d| d.join("session.json")) else { return };
        let Some(session) = Self::read(&old) else { return };
        if session.save().is_ok() && session.make_current().is_ok() {
            let _ = fs::remove_file(old);
        }
    }

    /// Saved to its account's own file, so a late save from an account
    /// you've switched away from can't overwrite another one.
    pub fn save(&self) -> Result<()> {
        let path = Self::account_path(&self.user_id()).context("No config directory")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // Only you can read it: it holds your session.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Forget this account on this computer.
    pub fn delete(&self) {
        if let Some(path) = Self::account_path(&self.user_id()) {
            let _ = fs::remove_file(path);
        }
    }
}
