//! Direct messages through instagram.com's current inbox: plain GraphQL
//! operations on "Slide" mailboxes, threads and messages (the website calls
//! them "...OffMsys...", i.e. not on the old Lightspeed/Msys protocol).
//!
//! The inputs and answers below were read from the website's own operation
//! definitions, which `dm.rs` saves to `~/.cache/finsta/webcode/`:
//!
//! - `PolarisDirectInboxQuery`: the mailbox and its first 15 conversations
//! - `IGDThreadListOffMsysPaginationQuery`: more conversations
//! - `IGDMessageListOffMsysQuery`: a conversation's messages, newest first
//! - `IGDirectTextSendMutation`: send a text message
//! - `useIGDMarkThreadAsReadMutation`: mark a conversation as read up to a
//!   message (inputs as in the website's `useIGDMarkThreadAsRead`)
//!
//! Logs describe structure only (counts, field and type names), never names
//! or message text.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::{DmState, query, time_id};
use crate::api::{Client, InboxPage, Recipient, Thread, parse};

const INBOX: &str = "PolarisDirectInboxQuery";
const MORE_THREADS: &str = "IGDThreadListOffMsysPaginationQuery";
const MESSAGES: &str = "IGDMessageListOffMsysQuery";
const SEND_TEXT: &str = "IGDirectTextSendMutation";
const MARK_READ: &str = "useIGDMarkThreadAsReadMutation";
const STORY_REPLY: &str = "IGDirectStoryShareReplyMutation";
const REACT: &str = "IGDirectReactionSendMutation";

/// Values the website computes on its side ("provided variables").
const MAX_UNREAD: i64 = 20;
const MESSAGE_PAGE: i64 = 20;
const THREAD_PAGE: i64 = 15;

#[derive(Default)]
pub(super) struct SlideState {
    /// The viewer's mailbox, needed to page through conversations.
    mailbox: Option<String>,
    /// Conversation (SlideThread id) -> its fields (`as_ig_direct_thread`).
    pub(super) threads: HashMap<String, Value>,
    next_cursor: Option<String>,
    has_next: bool,
    /// The viewer's id in the messaging system (senders use these ids).
    viewer_fbid: Option<String>,
    /// Message id -> its time (ms), for marking as read.
    message_times: HashMap<String, i64>,
    /// Message id -> the id the website's reactions use (SlideMessage.id).
    reaction_ids: HashMap<String, String>,
    /// Content type names seen so far (for logging new ones once).
    content_types: BTreeSet<String>,
    logged: HashSet<String>,
}

/// A string or number as a string.
fn s(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// A number or numeric string as a number.
fn n(v: &Value) -> i64 {
    match v {
        Value::Number(x) => x.as_i64().unwrap_or(0),
        Value::String(x) => x.parse().unwrap_or(0),
        _ => 0,
    }
}

fn edges(conn: &Value) -> impl Iterator<Item = &Value> {
    conn["edges"].as_array().into_iter().flatten()
}

fn media(image: &str, width: i64, height: i64, video: Option<&str>) -> Value {
    let (w, h) = if width > 0 && height > 0 { (width, height) } else { (1080, 1080) };
    let mut m = json!({
        "media_type": if video.is_some() { 2 } else { 1 },
        "original_width": w,
        "original_height": h,
        "image_versions2": {"candidates": [{"url": image, "width": w, "height": h}]},
    });
    if let Some(v) = video {
        m["video_versions"] = json!([{"url": v, "width": w, "height": h}]);
    }
    m
}

fn placeholder(v: &mut Value, message: &str) {
    v["item_type"] = "placeholder".into();
    v["placeholder"] = json!({"message": message});
}

impl SlideState {
    fn take_threads(&mut self, conn: &Value) -> usize {
        let mut count = 0;
        for e in edges(conn) {
            let id = s(&e["node"]["id"]);
            let t = &e["node"]["as_ig_direct_thread"];
            if id.is_empty() || !t.is_object() {
                continue;
            }
            if self.viewer_fbid.is_none() {
                let fbid = s(&t["viewer"]["interop_messaging_user_fbid"]);
                if !fbid.is_empty() {
                    self.viewer_fbid = Some(fbid);
                }
            }
            self.threads.insert(id, t.clone());
            count += 1;
        }
        self.next_cursor = conn["page_info"]["end_cursor"].as_str().map(str::to_string);
        self.has_next = conn["page_info"]["has_next_page"].as_bool().unwrap_or(false);
        count
    }

    fn is_viewer(&self, t: &Value, user: &Value, viewer_pk: &str) -> bool {
        let id = s(&user["id"]);
        let fbid = s(&user["interop_messaging_user_fbid"]);
        (!id.is_empty() && (id == viewer_pk || id == s(&t["viewer"]["id"])))
            || (!fbid.is_empty() && Some(&fbid) == self.viewer_fbid.as_ref())
    }

    /// The other participants, as the app's users.
    fn users(&self, t: &Value, viewer_pk: &str) -> Vec<Value> {
        t["users"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|u| !self.is_viewer(t, u, viewer_pk))
            .map(|u| {
                json!({
                    "pk": s(&u["id"]),
                    "username": s(&u["username"]),
                    "full_name": s(&u["full_name"]),
                    "profile_pic_url": s(&u["profile_pic_url"]),
                    "is_verified": u["is_verified"].as_bool().unwrap_or(false),
                })
            })
            .collect()
    }

    /// Who sent a message, as an id the app knows: the viewer's profile id,
    /// another participant's profile id, or else the messaging id itself.
    fn sender(&self, t: &Value, fbid: &str, viewer_pk: &str) -> String {
        if Some(fbid) == self.viewer_fbid.as_deref() {
            return viewer_pk.to_string();
        }
        t["users"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|u| s(&u["interop_messaging_user_fbid"]) == fbid)
            .map(|u| s(&u["id"]))
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| fbid.to_string())
    }

    /// A message as the app's item.
    fn item(&self, t: &Value, m: &Value, viewer_pk: &str) -> Value {
        let message_id = s(&m["message_id"]);
        let mut v = json!({
            "slide_id": s(&m["id"]),
            "item_id": if message_id.is_empty() { s(&m["id"]) } else { message_id },
            "user_id": self.sender(t, &s(&m["sender_fbid"]), viewer_pk),
            "timestamp": n(&m["timestamp_ms"]) * 1000,
        });
        let reactions: Vec<Value> = m["reactions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let emoji = s(&r["reaction"]);
                (!emoji.is_empty()).then(|| json!({"emoji": emoji, "sender_id": self.sender(t, &s(&r["sender_fbid"]), viewer_pk)}))
            })
            .collect();
        if !reactions.is_empty() {
            v["reactions"] = json!({"emojis": reactions});
        }
        let c = &m["content"];
        if !m["tombstone_reason"].is_null() && m["tombstone_reason"] != "" {
            placeholder(&mut v, "This message is no longer available.");
            return v;
        }
        let first = |list: &str| c[list].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        let url = |a: &Value, keys: &[&str]| keys.iter().map(|k| s(&a[*k])).find(|u| !u.is_empty());
        match c["__typename"].as_str().unwrap_or_default() {
            "SlideMessageText" => {
                v["item_type"] = "text".into();
                v["text"] = s(&c["text_body"]).into();
            }
            "SlideMessageImageContent" | "SlideMessageMultiMediaContent" => {
                let a = if c["attachments"].is_array() { first("attachments") } else { first("ordered_photo_video_attachments") };
                match url(&a, &["attachment_cdn_url", "preview_cdn_url", "attachment_cdn_fallback_url", "preview_cdn_fallback_url"]) {
                    Some(image) => {
                        v["item_type"] = "media".into();
                        v["media"] = media(&image, n(&a["preview_width"]), n(&a["preview_height"]), None);
                    }
                    None => placeholder(&mut v, "Photo. Open it on instagram.com."),
                }
            }
            "SlideMessageVideosContent" => {
                let a = first("videos");
                let image = url(&a, &["preview_cdn_url", "preview_cdn_fallback_url"]).unwrap_or_default();
                match url(&a, &["attachment_cdn_url", "attachment_cdn_fallback_url"]) {
                    Some(video) => {
                        v["item_type"] = "media".into();
                        v["media"] = media(&image, n(&a["preview_width"]), n(&a["preview_height"]), Some(&video));
                    }
                    None => placeholder(&mut v, "Video. Watch it on instagram.com."),
                }
            }
            "SlideMessageAnimatedMediaContent" => {
                let a = first("animated_media");
                match url(&a, &["attachment_webp_url", "preview_cdn_url"]) {
                    Some(image) => {
                        v["item_type"] = "media".into();
                        v["media"] = media(&image, n(&a["preview_width"]), n(&a["preview_height"]), None);
                    }
                    None => placeholder(&mut v, "GIF. Open it on instagram.com."),
                }
            }
            "SlideMessageStoreStickerContent"
            | "SlideMessageCutoutStickerXMAContent"
            | "SlideMessageAIStickerXMAContent"
            | "SlideMessageAvatarStickerXMAContent" => match url(c, &["preview_url"]) {
                Some(image) => {
                    v["item_type"] = "media".into();
                    v["media"] = media(&image, n(&c["preview_width"]), n(&c["preview_height"]), None);
                }
                None => placeholder(&mut v, "Sticker."),
            },
            "SlideMessageXMAContent" => {
                let x = &c["xma"];
                let target = s(&x["target_url"]);
                let text: Vec<String> = [&x["header_title_text"], &x["title_text"], &x["caption_body_text"], &c["xma_text_body"]]
                    .iter()
                    .map(|t| s(t))
                    .filter(|t| !t.is_empty())
                    .collect();
                let preview = url(&x["preview_image"], &["url", "fallback_url"]);
                let kind = share_kind(&target);
                if kind.is_some() || (preview.is_some() && !target.is_empty()) {
                    // A shared post, reel or story: a card with its preview.
                    let author = s(&x["header_title_text"]);
                    let caption = [&x["title_text"], &x["caption_body_text"], &c["xma_text_body"]]
                        .iter()
                        .map(|t| s(t))
                        .find(|t| !t.is_empty() && *t != author)
                        .unwrap_or_default();
                    let (w, h) = (n(&x["preview_image"]["width"]), n(&x["preview_image"]["height"]));
                    let portrait = x["__typename"] == "SlideMessagePortraitXMA" || matches!(kind, Some("reel" | "story"));
                    // Height / width, as for posts.
                    let aspect = if w > 0 && h > 0 { h as f64 / w as f64 } else if portrait { 16.0 / 9.0 } else { 1.0 };
                    v["item_type"] = "xma_share".into();
                    v["xma_share"] = json!({
                        "kind": kind.unwrap_or("link"),
                        "author": author,
                        "caption": caption,
                        "preview": preview,
                        "url": target,
                        "aspect": aspect,
                    });
                } else if !target.is_empty() {
                    let mut body = text.join("\n");
                    body = if body.is_empty() { target.clone() } else { format!("{body}\n{target}") };
                    v["item_type"] = "link".into();
                    v["link"] = json!({"text": body, "link_context": {"link_url": target}});
                } else if !text.is_empty() {
                    v["item_type"] = "text".into();
                    v["text"] = text.join("\n").into();
                } else {
                    placeholder(&mut v, "Shared content. Open it on instagram.com.");
                }
            }
            "SlideMessageAudiosContent" => placeholder(&mut v, "Voice message. Listen on instagram.com."),
            "SlideMessageRavenImageContent" => placeholder(&mut v, "Disappearing photo. Open it on instagram.com."),
            "SlideMessageRavenVideoContent" => placeholder(&mut v, "Disappearing video. Open it on instagram.com."),
            _ => {
                let snippet = s(&m["igd_snippet"]);
                let text = s(&c["text_body"]);
                if !text.is_empty() {
                    v["item_type"] = "text".into();
                    v["text"] = text.into();
                } else if !snippet.is_empty() {
                    placeholder(&mut v, &snippet);
                } else {
                    placeholder(&mut v, "This kind of message isn't supported yet. Open it on instagram.com.");
                }
            }
        }
        v
    }

    /// The newest message the inbox carries for a conversation, as a preview
    /// item (the chat view ignores "snippet:" items).
    fn preview(&self, id: &str, t: &Value, viewer_pk: &str) -> Option<Value> {
        let m = edges(&t["slide_messages"]).map(|e| &e["node"]).max_by_key(|m| n(&m["timestamp_ms"]))?;
        let text = s(&m["content"]["text_body"]);
        let text = if text.is_empty() { s(&m["igd_snippet"]) } else { text };
        Some(json!({
            "item_id": format!("snippet:{id}"),
            "user_id": self.sender(t, &s(&m["sender_fbid"]), viewer_pk),
            "timestamp": n(&m["timestamp_ms"]) * 1000,
            "item_type": "text",
            "text": text,
        }))
    }

    /// A conversation as the app expects it.
    fn thread(&self, id: &str, viewer_pk: &str, items: Vec<Value>, has_older: bool, oldest: Option<String>) -> Value {
        let null = Value::Null;
        let t = self.threads.get(id).unwrap_or(&null);
        let mut users = self.users(t, viewer_pk);
        let title = s(&t["thread_title"]);
        let is_group = t["is_group"].as_bool().unwrap_or(users.len() > 1);
        if users.is_empty() && !title.is_empty() {
            users.push(json!({"pk": id, "full_name": title, "profile_pic_url": s(&t["thread_image_url"])}));
        }
        let read_ms = Client::viewer_read_ms(self, t);
        let newest = items.iter().map(|i| n(&i["timestamp"]) / 1000).max().unwrap_or(0);
        let last_activity = n(&t["last_activity_timestamp_ms"]).max(newest);
        json!({
            "thread_id": id,
            "thread_title": if is_group { title } else { String::new() },
            "is_group": is_group,
            "muted": t["is_muted"].as_bool().unwrap_or(false),
            "users": users,
            "items": items,
            "last_activity_at": last_activity * 1000,
            "last_seen_at": {viewer_pk: {"item_id": "", "timestamp": read_ms * 1000}},
            "has_older": has_older,
            "oldest_cursor": oldest,
        })
    }

    fn sorted_ids(&self) -> Vec<String> {
        let mut ids: Vec<(&String, i64)> =
            self.threads.iter().map(|(id, t)| (id, n(&t["last_activity_timestamp_ms"]))).collect();
        ids.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        ids.into_iter().map(|(id, _)| id.clone()).collect()
    }

    /// The 1:1 conversation with `user_pk`, if known.
    fn one_to_one(&self, user_pk: &str, viewer_pk: &str) -> Option<String> {
        self.sorted_ids().into_iter().find(|id| {
            let t = &self.threads[id];
            let users = self.users(t, viewer_pk);
            !t["is_group"].as_bool().unwrap_or(false) && users.len() == 1 && users[0]["pk"] == user_pk
        })
    }
}

impl Client {
    /// Whether instagram.com's current GraphQL inbox is in use (rather than
    /// Lightspeed, which the website no longer runs).
    pub(super) fn slide_mode(st: &DmState) -> bool {
        st.config.as_ref().is_some_and(|c| c.version.is_none() && c.ops.contains_key(INBOX))
    }

    /// Run one of the website's operations; log the structure of its answer
    /// the first time (and whenever it reports errors).
    async fn slide_op(&self, st: &mut DmState, name: &str, variables: Value) -> Result<Value> {
        let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
        let id = cfg
            .ops
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("instagram.com changed how messages work ({name} is gone)"))?;
        let mut variables = variables;
        query::fill_providers(name, &mut variables);
        let answer = self.web_graphql(&cfg, name, &id, &variables, super::INBOX_REFERER).await?;
        let errors = answer.json.as_ref().is_some_and(|v| !v["errors"].is_null() || !v["error"].is_null());
        if st.slide.logged.insert(name.to_string()) || errors {
            answer.log("dm", name);
        }
        let Some(v) = answer.json else {
            bail!("Instagram sent an answer Finsta couldn't read");
        };
        if v["data"].is_null() || !v["data"].is_object() {
            let code = v["errors"][0]["code"].as_i64().or(v["error"].as_i64()).unwrap_or(0);
            bail!("Instagram couldn't load this (error {code})");
        }
        Ok(v["data"].clone())
    }

    fn slide_providers(vars: &mut Value) {
        for (k, val) in [
            ("__relay_internal__pv__IGDIsProfessionalAccountGKrelayprovider", json!(false)),
            ("__relay_internal__pv__IGDPinnedThreadsRenderEnabledGKrelayprovider", json!(false)),
            ("__relay_internal__pv__IGDMaxUnreadMessagesCountrelayprovider", json!(MAX_UNREAD)),
            ("__relay_internal__pv__IGDThreadListActionsEnabledGKrelayprovider", json!(false)),
        ] {
            vars[k] = val;
        }
    }

    pub(super) async fn slide_inbox(&self, st: &mut DmState, cursor: Option<String>) -> Result<InboxPage> {
        let viewer_pk = self.user_id();
        let older = cursor.is_some();
        let fetched: Vec<String> = if older {
            let (Some(mailbox), Some(after)) = (st.slide.mailbox.clone(), st.slide.next_cursor.clone()) else {
                return Ok(InboxPage::default());
            };
            let mut vars = json!({
                "id": mailbox, "count": THREAD_PAGE, "cursor": after, "folder": "INBOX", "newer_than_timestamp_ms": null,
            });
            Self::slide_providers(&mut vars);
            let data = self.slide_op(st, MORE_THREADS, vars).await?;
            let conn = &data["fetch__SlideMailbox"]["threads_by_folder"];
            let before: HashSet<String> = st.slide.threads.keys().cloned().collect();
            let count = st.slide.take_threads(conn);
            self.slide_log_threads(st, "older conversations", count);
            st.slide.threads.keys().filter(|k| !before.contains(*k)).cloned().collect()
        } else {
            let cfg = st.config.clone().ok_or_else(|| anyhow!("inbox not loaded"))?;
            let mut vars = json!({"device_id_for_iris_subscription": cfg.iris_device_id});
            Self::slide_providers(&mut vars);
            let data = self.slide_op(st, INBOX, vars).await?;
            let mailbox = &data["get_slide_mailbox_for_iris_subscription"];
            let id = s(&mailbox["id"]);
            if !id.is_empty() {
                st.slide.mailbox = Some(id);
            }
            let keep_cursor = (st.slide.next_cursor.clone(), st.slide.has_next);
            let count = st.slide.take_threads(&mailbox["threads_by_folder"]);
            if st.slide.threads.len() > count {
                // Older pages were loaded before; keep paging from where they ended.
                (st.slide.next_cursor, st.slide.has_next) = keep_cursor;
            }
            self.slide_log_threads(st, "conversations", count);
            Vec::new()
        };
        let ids = if older {
            let mut ids = fetched;
            ids.sort_by_key(|id| -n(&st.slide.threads[id]["last_activity_timestamp_ms"]));
            ids
        } else {
            st.slide.sorted_ids()
        };
        let threads: Vec<Thread> = ids
            .iter()
            .filter_map(|id| {
                let t = &st.slide.threads[id];
                let items: Vec<Value> = st.slide.preview(id, t, &viewer_pk).into_iter().collect();
                parse(&st.slide.thread(id, &viewer_pk, items, false, None))
            })
            .collect();
        Ok(InboxPage { has_older: st.slide.has_next, oldest_cursor: Some("older".into()), threads })
    }

    fn slide_log_threads(&self, st: &mut DmState, what: &str, count: usize) {
        if !st.slide.logged.insert(format!("threads {what}")) {
            return;
        }
        let viewer_pk = self.user_id();
        let slide = &st.slide;
        let mut with_users = 0;
        let mut with_preview = 0;
        let mut groups = 0;
        let mut with_receipt = 0;
        for t in slide.threads.values() {
            with_users += usize::from(!slide.users(t, &viewer_pk).is_empty());
            with_preview += usize::from(edges(&t["slide_messages"]).next().is_some());
            groups += usize::from(t["is_group"].as_bool().unwrap_or(false));
            with_receipt += usize::from(
                t["slide_read_receipts"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|r| Some(s(&r["participant_fbid"])) == slide.viewer_fbid)),
            );
        }
        eprintln!(
            "finsta: dm: {what}: {count} fetched, {} known ({groups} groups, {with_users} with other people, {with_preview} with a preview, {with_receipt} with your read receipt); viewer messaging id {}; mailbox {}; more {}",
            slide.threads.len(),
            if slide.viewer_fbid.is_some() { "found" } else { "unknown" },
            if slide.mailbox.is_some() { "found" } else { "unknown" },
            slide.has_next,
        );
    }

    pub(super) async fn slide_thread(&self, st: &mut DmState, id: &str, cursor: Option<String>) -> Result<Thread> {
        let viewer_pk = self.user_id();
        let vars = json!({
            "id": id,
            "first": MESSAGE_PAGE,
            "__relay_internal__pv__IGDInitialMessagePageCountrelayprovider": MESSAGE_PAGE,
            "after": cursor,
            "before": null,
            "last": null,
            "newer_than_message_id": null,
            "older_than_message_id": null,
        });
        let what = if cursor.is_some() { "older messages" } else { "messages" };
        let data = self.slide_op(st, MESSAGES, vars).await?;
        let t = data["fetch__SlideThread"]["as_ig_direct_thread"].clone();
        let conn = &t["slide_messages"];
        let nodes: Vec<&Value> = edges(conn).map(|e| &e["node"]).filter(|m| m.is_object()).collect();
        let null = Value::Null;
        let meta = st.slide.threads.get(id).unwrap_or(&null).clone();
        // Senders are resolved against the inbox's participant list.
        let lookup = if meta.is_object() { &meta } else { &t };
        let mut items: Vec<Value> = nodes.iter().map(|m| st.slide.item(lookup, m, &viewer_pk)).collect();
        items.sort_by_key(|i| -n(&i["timestamp"]));
        for i in &items {
            st.slide.message_times.insert(s(&i["item_id"]), n(&i["timestamp"]) / 1000);
            if !s(&i["slide_id"]).is_empty() {
                st.slide.reaction_ids.insert(s(&i["item_id"]), s(&i["slide_id"]));
            }
        }

        let types: BTreeSet<String> = nodes.iter().map(|m| s(&m["content"]["__typename"])).collect();
        let new_types: Vec<&String> = types.iter().filter(|t| !st.slide.content_types.contains(*t)).collect();
        let first_check = st.slide.logged.insert(format!("check {what}"));
        if !new_types.is_empty() || first_check {
            let times: Vec<i64> = nodes.iter().map(|m| n(&m["timestamp_ms"])).collect();
            let newest_first = times.windows(2).all(|w| w[0] >= w[1]);
            let resolved = items.iter().filter(|i| {
                let u = s(&i["user_id"]);
                u == viewer_pk || lookup["users"].as_array().into_iter().flatten().any(|x| s(&x["id"]) == u)
            });
            eprintln!(
                "finsta: dm: {what}: {} messages (newest first: {newest_first}, with time {}/{}, senders resolved {}/{}), content types {types:?}, page info {:?}",
                nodes.len(),
                times.iter().filter(|t| **t > 0).count(),
                nodes.len(),
                resolved.count(),
                nodes.len(),
                conn["page_info"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}:{}", !v.is_null() && *v != false)).collect::<Vec<_>>())
            );
            st.slide.content_types.extend(types);
        }
        if !meta.is_object() && t.is_object() {
            st.slide.threads.insert(id.to_string(), t.clone());
        }
        let pi = &conn["page_info"];
        let has_older = pi["has_next_page"].as_bool().unwrap_or(false);
        let oldest = pi["end_cursor"].as_str().map(str::to_string);
        parse(&st.slide.thread(id, &viewer_pk, items, has_older, oldest)).ok_or_else(|| anyhow!("Could not read conversation"))
    }

    pub(super) fn slide_thread_with_user(&self, st: &DmState, user_pk: &str) -> Option<Thread> {
        let viewer_pk = self.user_id();
        let id = st.slide.one_to_one(user_pk, &viewer_pk)?;
        let t = &st.slide.threads[&id];
        let items: Vec<Value> = st.slide.preview(&id, t, &viewer_pk).into_iter().collect();
        parse(&st.slide.thread(&id, &viewer_pk, items, false, None))
    }

    /// React to a message (or take your reaction back: `on` false, with the
    /// emoji you reacted with), with the inputs instagram.com's
    /// IGDMAPISendReaction builds.
    pub(super) async fn slide_react(&self, st: &mut DmState, thread_id: &str, item_id: &str, emoji: &str, on: bool) -> Result<()> {
        let t = st.slide.threads.get(thread_id).cloned().unwrap_or_default();
        let fbid = [&t["thread_fbid"], &t["thread_key"]]
            .into_iter()
            .map(s)
            .find(|x| !x.is_empty())
            .unwrap_or_else(|| thread_id.to_string());
        let message_id = st.slide.reaction_ids.get(item_id).cloned().unwrap_or_else(|| item_id.to_string());
        if st.slide.logged.insert("react ids".into()) {
            eprintln!(
                "finsta: dm: reaction: conversation id from {}, message id {}",
                if !s(&t["thread_fbid"]).is_empty() {
                    "thread_fbid"
                } else if !s(&t["thread_key"]).is_empty() {
                    "thread_key"
                } else {
                    "the app's id"
                },
                if message_id == item_id { "same as the item's" } else { "from the message's own id" }
            );
        }
        let input = json!({
            "emoji": emoji,
            "item_id": "",
            "message_id": message_id,
            "reaction_status": if on { "created" } else { "deleted" },
            "thread_id": fbid,
        });
        let data = self.slide_op(st, REACT, json!({"input": input})).await?;
        if data["xig_direct_reaction_send_with_slide_messaging_response"]["success"] == false {
            bail!("Instagram didn't accept the reaction");
        }
        Ok(())
    }

    /// Reply to a story, with the inputs instagram.com's
    /// IGDMAPISendStoryShareReply builds.
    pub(super) async fn slide_story_reply(&self, st: &mut DmState, owner_pk: &str, media_id: &str, text: &str) -> Result<()> {
        let otid = time_id().to_string();
        let vars = json!({
            "data": {
                "is_shh_mode": false,
                "media_id": media_id.split('_').next().unwrap_or(media_id),
                "reaction_emoji": null,
                "reel_id": owner_pk,
                "sampled": false,
                "share_client_context": otid,
                "text": {"sensitive_string_value": text},
            },
            "send_data": {
                "forwarded_from_thread_id": null,
                "is_forwarded_from_own_message": null,
                "offline_threading_id": otid,
                "recipient_users": json!([owner_pk]).to_string(),
                "thread_id": null,
            },
        });
        let data = self.slide_op(st, STORY_REPLY, vars).await?;
        let sent = &data["direct_story_share_reply_with_slide_message_response"];
        if s(&sent["message_id"]).is_empty() && s(&sent["id"]).is_empty() {
            bail!("Instagram didn't confirm the reply");
        }
        Ok(())
    }

    pub(super) async fn slide_send(&self, st: &mut DmState, to: &Recipient, text: &str) -> Result<Option<String>> {
        let viewer_pk = self.user_id();
        let (thread, recipients) = match to {
            Recipient::Thread(id) => (Some(id.clone()), None),
            Recipient::User(pk) => match st.slide.one_to_one(pk, &viewer_pk) {
                Some(id) => (Some(id), None),
                None => (None, Some(vec![pk.clone()])),
            },
        };
        let igid = thread.as_ref().map(|id| s(&st.slide.threads.get(id).cloned().unwrap_or_default()["thread_id"]));
        if thread.is_some() && igid.as_deref().is_none_or(str::is_empty) {
            bail!("Couldn't find this conversation's id. Reopen it and try again.");
        }
        let vars = json!({
            "ig_thread_igid": igid,
            "recipient_igids": recipients,
            "offline_threading_id": time_id().to_string(),
            "text": text,
            "send_attribution": "direct_thread",
        });
        let data = self.slide_op(st, SEND_TEXT, vars).await?;
        let sent = &data["xig_direct_text_send_with_slide_messaging_response"];
        if s(&sent["message_id"]).is_empty() && s(&sent["id"]).is_empty() {
            bail!("Instagram didn't confirm the message");
        }
        if st.slide.logged.insert("sent".into()) {
            eprintln!("finsta: dm: send: confirmed (message id and time {})", if n(&sent["timestamp_ms"]) > 0 { "present" } else { "missing" });
        }
        Ok(thread)
    }

    /// The viewer's read watermark (ms) in a conversation, 0 if marked unread.
    fn viewer_read_ms(slide: &SlideState, t: &Value) -> i64 {
        if t["marked_as_unread"].as_bool().unwrap_or(false) {
            return 0;
        }
        t["slide_read_receipts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| Some(s(&r["participant_fbid"])) == slide.viewer_fbid)
            .map(|r| n(&r["watermark_timestamp_ms"]))
            .max()
            .unwrap_or(0)
    }

    pub(super) async fn slide_mark_seen(&self, st: &mut DmState, id: &str, message_id: &str) -> Result<()> {
        if message_id.is_empty() || message_id.starts_with("snippet:") {
            return Ok(());
        }
        let Some(t) = st.slide.threads.get(id).cloned() else { return Ok(()) };
        let igid = s(&t["thread_id"]);
        if igid.is_empty() {
            if st.slide.logged.insert("mark read: no id".into()) {
                eprintln!("finsta: dm: mark read: the conversation has no Instagram thread id");
            }
            return Ok(());
        }
        let time = st.slide.message_times.get(message_id).copied().unwrap_or(0);
        if time > 0 && Self::viewer_read_ms(&st.slide, &t) >= time {
            return Ok(()); // Already read (on Instagram too).
        }
        let vars = json!({"data": {"item_id": "", "message_id": message_id}, "metadata": {"ig_thread_igid": igid}});
        let data = self.slide_op(st, MARK_READ, vars).await?;
        let r = &data["xig_direct_item_seen_mutation_with_slide_messaging_response"];
        let watermark = n(&r["watermark_timestamp_ms"]).max(time);
        if st.slide.logged.insert("marked read".into()) {
            eprintln!(
                "finsta: dm: mark read: confirmed (watermark {}, participant {})",
                if n(&r["watermark_timestamp_ms"]) > 0 { "present" } else { "missing" },
                match (s(&r["participant_eimu_id"]), &st.slide.viewer_fbid) {
                    (p, _) if p.is_empty() => "missing",
                    (p, Some(v)) if &p == v => "is you",
                    _ => "another id",
                }
            );
        }
        // Remember it, so the next refresh doesn't show the conversation as unread.
        let viewer = st.slide.viewer_fbid.clone().unwrap_or_else(|| s(&r["participant_eimu_id"]));
        if let Some(t) = st.slide.threads.get_mut(id) {
            t["marked_as_unread"] = false.into();
            let receipt = json!({"participant_fbid": viewer, "watermark_timestamp_ms": watermark.to_string()});
            match t["slide_read_receipts"].as_array_mut() {
                Some(list) => {
                    list.retain(|r| s(&r["participant_fbid"]) != viewer);
                    list.push(receipt);
                }
                None => t["slide_read_receipts"] = json!([receipt]),
            }
        }
        Ok(())
    }
}

/// What an Instagram link points to: "post", "reel" or "story".
fn share_kind(url: &str) -> Option<&'static str> {
    let path = url.split("instagram.com/").nth(1)?;
    if path.starts_with("p/") {
        Some("post")
    } else if path.starts_with("reel/") || path.starts_with("reels/") {
        Some("reel")
    } else if path.starts_with("stories/") {
        Some("story")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (SlideState, Value) {
        let t = json!({
            "thread_id": "3400001",
            "thread_title": "",
            "is_group": false,
            "last_activity_timestamp_ms": "1700000000000",
            "users": [{"id": "42", "username": "marta", "full_name": "Marta", "profile_pic_url": "p", "interop_messaging_user_fbid": "100042"}],
            "viewer": {"id": "7", "interop_messaging_user_fbid": "100007"},
            "slide_read_receipts": [{"participant_fbid": "100007", "watermark_timestamp_ms": "1600000000000"}],
            "slide_messages": {"edges": [{"node": {"id": "m1", "sender_fbid": "100042", "timestamp_ms": "1700000000000", "content": {"__typename": "SlideMessageText", "text_body": "hi"}}}]},
        });
        let mut st = SlideState::default();
        st.take_threads(&json!({"edges": [{"node": {"id": "T1", "as_ig_direct_thread": t}}], "page_info": {"end_cursor": "c", "has_next_page": true}}));
        (st, t)
    }

    #[test]
    fn threads_map_to_the_app_shape() {
        let (st, t) = sample();
        assert_eq!(st.viewer_fbid.as_deref(), Some("100007"));
        let items: Vec<Value> = st.preview("T1", &t, "7").into_iter().collect();
        let th: Thread = parse(&st.thread("T1", "7", items, false, None)).unwrap();
        assert_eq!(th.thread_id, "T1");
        assert_eq!(th.users.len(), 1);
        assert_eq!(th.users[0].pk, "42");
        assert_eq!(th.items[0].user_id, "42");
        assert!(th.is_unread("7"));
        assert_eq!(st.one_to_one("42", "7").as_deref(), Some("T1"));
    }

    #[test]
    fn senders_and_content() {
        let (st, t) = sample();
        let with_id = json!({"id": "node1", "message_id": "mid.$1", "sender_fbid": "100042", "content": {"__typename": "SlideMessageText", "text_body": "x"}});
        assert_eq!(st.item(&t, &with_id, "7")["item_id"], "mid.$1");
        let mine = json!({"id": "m2", "sender_fbid": "100007", "timestamp_ms": 5, "content": {"__typename": "SlideMessageText", "text_body": "yo"}});
        assert_eq!(st.item(&t, &mine, "7")["user_id"], "7");
        let photo = json!({"id": "m3", "sender_fbid": "100042", "timestamp_ms": 6, "content": {"__typename": "SlideMessageImageContent", "attachments": [{"preview_cdn_url": "u", "preview_width": 10, "preview_height": 20}]}});
        let v = st.item(&t, &photo, "7");
        assert_eq!(v["item_type"], "media");
        assert_eq!(v["media"]["original_height"], 20);
        let raven = json!({"id": "m4", "sender_fbid": "100042", "content": {"__typename": "SlideMessageRavenImageContent", "attachment": {"preview_cdn_url": "secret"}}});
        let v = st.item(&t, &raven, "7");
        assert_eq!(v["item_type"], "placeholder");
        assert!(!v.to_string().contains("secret"));
        let share = json!({"id": "m5", "sender_fbid": "100042", "content": {"__typename": "SlideMessageXMAContent", "xma": {"__typename": "SlideMessagePortraitXMA", "target_url": "https://www.instagram.com/p/x/", "header_title_text": "natgeo"}}});
        let v = st.item(&t, &share, "7");
        assert_eq!(v["item_type"], "xma_share");
        assert_eq!(v["xma_share"]["kind"], "post");
        assert_eq!(v["xma_share"]["author"], "natgeo");
        let reel = json!({"id": "m6", "sender_fbid": "100042", "content": {"__typename": "SlideMessageXMAContent", "xma": {"__typename": "SlideMessagePortraitXMA",
            "target_url": "https://www.instagram.com/reel/r/", "preview_image": {"url": "u", "width": 720, "height": 1280}}}});
        let v = st.item(&t, &reel, "7");
        assert_eq!(v["xma_share"]["kind"], "reel");
        assert!((v["xma_share"]["aspect"].as_f64().unwrap() - 1280.0 / 720.0).abs() < 1e-9);
        let reacted = json!({"id": "m7", "sender_fbid": "100007", "content": {"__typename": "SlideMessageText", "text_body": "x"},
            "reactions": [{"reaction": "😂", "sender_fbid": "100042"}]});
        let item: crate::api::DmItem = parse(&st.item(&t, &reacted, "7")).unwrap();
        assert_eq!(item.reactions(), vec!["😂".to_string()]);
        assert_eq!(item.reaction_by("42").as_deref(), Some("😂"));
    }
}
