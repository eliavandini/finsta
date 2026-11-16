//! Decoding "Lightspeed", the format instagram.com's web inbox receives its
//! messages in.
//!
//! A payload is a small program: nested arrays where `[5, "name", args...]`
//! calls a stored procedure (such as `insertMessage`) that the website runs
//! against its local database. We don't run the program; we collect every
//! procedure call and read the ones we understand.
//!
//! Procedure arguments are positional and undocumented. The positions used
//! here are the best known ones; [`Tables::from_calls`] checks each against
//! what the value looks like and [`log_shapes`] logs the shape of every
//! procedure (types and lengths only, never content), so wrong positions show
//! up in the log.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Call {
    pub name: String,
    pub args: Vec<Value>,
}

/// Collect the procedure calls in a payload (`{"name":..,"step":[..]}` or a bare step).
pub fn decode(payload: &str) -> anyhow::Result<Vec<Call>> {
    let v: Value = serde_json::from_str(payload)?;
    let step = if v.get("step").is_some() { &v["step"] } else { &v };
    let mut calls = Vec::new();
    walk(step, &mut calls);
    Ok(calls)
}

fn walk(step: &Value, calls: &mut Vec<Call>) {
    let Value::Array(items) = step else { return };
    if items.first().and_then(Value::as_i64) == Some(5)
        && let Some(name) = items.get(1).and_then(Value::as_str)
    {
        calls.push(Call {
            name: name.to_string(),
            args: items[2..].iter().map(literal).collect(),
        });
    }
    // Both branches of conditionals are walked: conditions depend on the
    // website's local database, which we don't have.
    for item in items {
        walk(item, calls);
    }
}

/// An argument's value: plain JSON, `[19, "123"]` (a 64 bit integer sent as a
/// string) or `[9]` (undefined). Expressions and variables aren't evaluated.
fn literal(v: &Value) -> Value {
    match v {
        Value::Array(a) => match a.first().and_then(Value::as_i64) {
            Some(19) => a
                .get(1)
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<i64>().ok())
                .map(Value::from)
                .unwrap_or(Value::Null),
            _ => Value::Null,
        },
        other => other.clone(),
    }
}

fn int(args: &[Value], i: usize) -> i64 {
    match args.get(i) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn string(args: &[Value], i: usize) -> String {
    match args.get(i) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

fn is_ms_timestamp(n: i64) -> bool {
    (1_200_000_000_000..2_500_000_000_000).contains(&n)
}

#[derive(Debug, Clone, Default)]
pub struct LsThread {
    pub key: i64,
    pub name: String,
    pub picture: String,
    pub last_activity_ms: i64,
    pub last_read_ms: i64,
    pub snippet: String,
    pub snippet_sender: i64,
    pub thread_type: i64,
}

#[derive(Debug, Clone, Default)]
pub struct LsContact {
    pub id: i64,
    pub name: String,
    pub username: String,
    pub picture: String,
}

#[derive(Debug, Clone, Default)]
pub struct LsMessage {
    pub thread: i64,
    pub id: String,
    pub sender: i64,
    pub ts_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentKind {
    Photo,
    Video,
    Audio,
    Share,
    Other,
}

#[derive(Debug, Clone)]
pub struct LsAttachment {
    pub message_id: String,
    pub kind: AttachmentKind,
    /// Full size media, or what a shared item links to.
    pub url: Option<String>,
    pub preview: Option<String>,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Tables {
    pub threads: Vec<LsThread>,
    pub contacts: Vec<LsContact>,
    /// (thread, contact, read watermark ms)
    pub participants: Vec<(i64, i64, i64)>,
    pub messages: Vec<LsMessage>,
    pub attachments: Vec<LsAttachment>,
    /// (thread, contact, read watermark ms)
    pub read_receipts: Vec<(i64, i64, i64)>,
    pub deleted_messages: Vec<String>,
    /// Whether older conversations exist, if the payload said.
    pub more_threads: Option<bool>,
}

impl Tables {
    pub fn from_calls(calls: &[Call]) -> Tables {
        let mut t = Tables::default();
        for c in calls {
            let a = &c.args;
            match c.name.as_str() {
                "deleteThenInsertThread" | "upsertThread" => t.threads.push(LsThread {
                    last_activity_ms: int(a, 0),
                    last_read_ms: int(a, 1),
                    snippet: string(a, 2),
                    name: string(a, 3),
                    picture: string(a, 4),
                    key: int(a, 7),
                    thread_type: int(a, 9),
                    snippet_sender: int(a, 18),
                }),
                "verifyContactRowExists" | "deleteThenInsertContact" | "upsertContact" => {
                    t.contacts.push(LsContact {
                        id: int(a, 0),
                        picture: string(a, 2),
                        name: string(a, 3),
                        username: username_guess(a),
                    })
                }
                "addParticipantIdToGroupThread" => t.participants.push((int(a, 0), int(a, 1), int(a, 2))),
                "insertMessage" | "upsertMessage" | "deleteThenInsertMessage" => t.messages.push(LsMessage {
                    text: string(a, 0),
                    thread: int(a, 3),
                    ts_ms: int(a, 5),
                    id: string(a, 8),
                    sender: int(a, 10),
                }),
                "updateReadReceipt" => t.read_receipts.push((int(a, 1), int(a, 2), int(a, 0))),
                "deleteMessage" | "deleteThenInsertMessageTombstone" => {
                    if let Some(mid) = a.iter().filter_map(Value::as_str).find(|s| s.starts_with("mid.")) {
                        t.deleted_messages.push(mid.to_string());
                    }
                }
                "upsertSyncGroupThreadsRange" | "upsertInboxThreadsRange" => {
                    if let Some(more) = a.iter().find_map(Value::as_bool) {
                        t.more_threads = Some(more);
                    }
                }
                name if name.starts_with("insert") && name.ends_with("Attachment") => {
                    if let Some(att) = attachment(name, a) {
                        t.attachments.push(att);
                    }
                }
                _ => {}
            }
        }
        t
    }

    /// A content-free report of how well the assumed argument positions fit.
    /// `known_threads` / `known_contacts` are ids already loaded earlier.
    pub fn check(&self, known_threads: &HashSet<i64>, known_contacts: &HashSet<i64>) -> String {
        let n = self.messages.len();
        let keys: HashSet<i64> = self.threads.iter().map(|t| t.key).chain(known_threads.iter().copied()).collect();
        let contacts: HashSet<i64> =
            self.contacts.iter().map(|c| c.id).chain(known_contacts.iter().copied()).collect();
        let count = |f: &dyn Fn(&LsMessage) -> bool| self.messages.iter().filter(|m| f(m)).count();
        let tn = self.threads.len();
        let tcount = |f: &dyn Fn(&LsThread) -> bool| self.threads.iter().filter(|t| f(t)).count();
        format!(
            "threads {tn} (activity ts {}/{tn}, read ts {}/{tn}, key>0 {}/{tn}, type {:?}), \
             contacts {} (named {}), participants {} (known thread {}), \
             messages {n} (mid {}/{n}, ts {}/{n}, known thread {}/{n}, known sender {}/{n}, text {}/{n}), \
             attachments {} (linked to a message {}), more threads {:?}",
            tcount(&|t| is_ms_timestamp(t.last_activity_ms)),
            tcount(&|t| t.last_read_ms == 0 || is_ms_timestamp(t.last_read_ms)),
            tcount(&|t| t.key > 0),
            self.threads.iter().map(|t| t.thread_type).collect::<std::collections::BTreeSet<_>>(),
            self.contacts.len(),
            self.contacts.iter().filter(|c| !c.name.is_empty()).count(),
            self.participants.len(),
            self.participants.iter().filter(|p| keys.contains(&p.0)).count(),
            count(&|m| m.id.starts_with("mid.")),
            count(&|m| is_ms_timestamp(m.ts_ms)),
            count(&|m| keys.contains(&m.thread)),
            count(&|m| contacts.contains(&m.sender)),
            count(&|m| !m.text.is_empty()),
            self.attachments.len(),
            self.attachments
                .iter()
                .filter(|a| self.messages.iter().any(|m| m.id == a.message_id))
                .count(),
            self.more_threads,
        )
    }
}

/// Instagram contacts carry a username somewhere after the display name; pick
/// the first value that looks like one.
fn username_guess(a: &[Value]) -> String {
    a.iter()
        .enumerate()
        .skip(4)
        .filter_map(|(_, v)| v.as_str())
        .find(|s| {
            !s.is_empty()
                && s.len() <= 30
                && s.chars().any(|c| c.is_ascii_alphabetic())
                && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
        })
        .unwrap_or_default()
        .to_string()
}

/// Attachments are read by what their values look like rather than by
/// position, which differs between attachment kinds.
fn attachment(name: &str, a: &[Value]) -> Option<LsAttachment> {
    let message_id = a.iter().filter_map(Value::as_str).find(|s| s.starts_with("mid."))?.to_string();
    let urls: Vec<&str> = a.iter().filter_map(Value::as_str).filter(|s| is_url(s)).collect();
    let is_video = |u: &&str| u.contains(".mp4") || u.contains("video");
    let kind = if name.contains("Xma") {
        AttachmentKind::Share
    } else if name.contains("Blob") {
        if urls.iter().any(is_video) {
            AttachmentKind::Video
        } else if urls.iter().any(|u| u.contains(".m4a") || u.contains("audio")) {
            AttachmentKind::Audio
        } else {
            AttachmentKind::Photo
        }
    } else if name.contains("Sticker") {
        AttachmentKind::Photo
    } else {
        AttachmentKind::Other
    };
    let preview = urls.iter().find(|u| !is_video(u)).map(|u| u.to_string());
    let url = match kind {
        AttachmentKind::Video => urls.iter().find(|u| is_video(u)).map(|u| u.to_string()),
        AttachmentKind::Share => urls
            .iter()
            .find(|u| u.contains("instagram.com/") && !u.contains("cdninstagram") && !u.contains("fbcdn"))
            .or(urls.last())
            .map(|u| u.to_string()),
        _ => preview.clone(),
    };
    // A short human readable string, if any (e.g. the shared post's title).
    let title = a
        .iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty() && !s.starts_with("http") && !s.starts_with("mid.") && s.contains(' '))
        .unwrap_or_default()
        .to_string();
    Some(LsAttachment { message_id, kind, url, preview, title })
}

/// Log one line per procedure: how often it was called and the shape of its
/// first call's arguments, e.g. `insertMessage x12 [s14,-,i1,thr,-,tms,...]`.
/// Shapes are types and lengths only, never content.
pub fn log_shapes(what: &str, calls: &[Call], known_ids: &HashSet<i64>, viewer_ids: &HashSet<i64>) {
    let mut by_name: BTreeMap<&str, (usize, &Call)> = BTreeMap::new();
    for c in calls {
        by_name.entry(&c.name).or_insert((0, c)).0 += 1;
    }
    eprintln!("finsta: dm: {what}: {} calls, {} procedures", calls.len(), by_name.len());
    for (name, (count, first)) in by_name {
        let shape: Vec<String> = first.args.iter().map(|v| shape(v, known_ids, viewer_ids)).collect();
        eprintln!("finsta: dm:   {name} x{count} [{}]", shape.join(","));
    }
}

fn shape(v: &Value, known_ids: &HashSet<i64>, viewer_ids: &HashSet<i64>) -> String {
    match v {
        Value::Null => "-".into(),
        Value::Bool(_) => "b".into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) if viewer_ids.contains(&i) => "me".into(),
            Some(i) if known_ids.contains(&i) => "id".into(),
            Some(i) if is_ms_timestamp(i) => "tms".into(),
            Some(i) => format!("i{}", i.unsigned_abs().to_string().len()),
            None => "f".into(),
        },
        Value::String(s) if s.is_empty() => "''".into(),
        Value::String(s) if s.starts_with("mid.") => "mid".into(),
        Value::String(s) if is_url(s) => "url".into(),
        Value::String(s) if s.chars().all(|c| c.is_ascii_digit()) => format!("sd{}", s.len()),
        Value::String(s) => format!("s{}", s.chars().count()),
        Value::Array(_) => "[]".into(),
        Value::Object(_) => "{}".into(),
    }
}

/// Every integer id that appears where a thread or contact id is expected,
/// for labelling shapes.
pub fn known_ids(t: &Tables) -> HashSet<i64> {
    t.threads
        .iter()
        .map(|x| x.key)
        .chain(t.contacts.iter().map(|c| c.id))
        .filter(|id| *id != 0)
        .collect()
}
