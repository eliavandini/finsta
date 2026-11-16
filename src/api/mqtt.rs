//! A minimal MQTT 3.1 client over WebSocket: just enough for instagram.com's
//! chat connection (connect, subscribe, publish, acknowledge, ping).
//!
//! Requests go out on `/ls_req` with a `request_id`; answers arrive on
//! `/ls_resp` with the same id. Anything that arrives without a matching
//! request (new messages, read receipts, ...) is queued as a push.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

pub struct Conn {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    pushes: Arc<Mutex<Vec<Value>>>,
    alive: Arc<AtomicBool>,
    next_packet: AtomicI64,
    next_request: AtomicI64,
}

fn remaining_length(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
}

fn mqtt_string(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn packet(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![header];
    remaining_length(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

fn connect_packet(client_id: &str, username: &str, keepalive: u16) -> Vec<u8> {
    let mut body = Vec::new();
    mqtt_string("MQIsdp", &mut body);
    body.push(3); // protocol level
    body.push(0x80 | 0x02); // username, clean session
    body.extend_from_slice(&keepalive.to_be_bytes());
    mqtt_string(client_id, &mut body);
    mqtt_string(username, &mut body);
    packet(0x10, &body)
}

fn subscribe_packet(id: u16, topics: &[&str]) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    for t in topics {
        mqtt_string(t, &mut body);
        body.push(0);
    }
    packet(0x82, &body)
}

fn publish_packet(topic: &str, payload: &[u8], id: Option<u16>) -> Vec<u8> {
    let mut body = Vec::new();
    mqtt_string(topic, &mut body);
    if let Some(id) = id {
        body.extend_from_slice(&id.to_be_bytes());
    }
    body.extend_from_slice(payload);
    packet(if id.is_some() { 0x32 } else { 0x30 }, &body)
}

enum Packet {
    ConnAck(u8),
    Publish { topic: String, id: Option<u16>, payload: Vec<u8> },
    Other,
}

/// Split complete packets off the front of `buf`.
fn parse_packets(buf: &mut Vec<u8>) -> Result<Vec<Packet>> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 2 {
            break;
        }
        let (mut len, mut mult, mut i) = (0usize, 1usize, 1usize);
        loop {
            let Some(&b) = buf.get(i) else { return Ok(out) };
            len += (b & 0x7f) as usize * mult;
            mult *= 128;
            i += 1;
            if b & 0x80 == 0 {
                break;
            }
            if i > 4 {
                bail!("bad MQTT length");
            }
        }
        if buf.len() < i + len {
            break;
        }
        let header = buf[0];
        let body: Vec<u8> = buf[i..i + len].to_vec();
        buf.drain(..i + len);
        out.push(match header >> 4 {
            2 => Packet::ConnAck(*body.get(1).unwrap_or(&255)),
            3 => {
                let qos = (header >> 1) & 3;
                let tlen = u16::from_be_bytes([body[0], body[1]]) as usize;
                let topic = String::from_utf8_lossy(&body[2..2 + tlen]).to_string();
                let mut p = 2 + tlen;
                let id = if qos > 0 {
                    let id = u16::from_be_bytes([body[p], body[p + 1]]);
                    p += 2;
                    Some(id)
                } else {
                    None
                };
                Packet::Publish { topic, id, payload: body[p..].to_vec() }
            }
            _ => Packet::Other,
        });
    }
    Ok(out)
}

impl Conn {
    /// Open the WebSocket, log in with `username` (a JSON string), subscribe
    /// to `topics` and start the background reader, writer and keepalive.
    pub async fn connect(
        url: &str,
        headers: &[(&str, String)],
        client_id: &str,
        username: &str,
        topics: &[&str],
    ) -> Result<Conn> {
        let mut req = url.into_client_request().context("bad chat address")?;
        for (k, v) in headers {
            if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                req.headers_mut().insert(k, v);
            }
        }
        let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(req))
            .await
            .map_err(|_| anyhow!("timed out connecting to chat"))?
            .context("could not connect to chat")?;
        let (mut sink, mut stream) = ws.split();

        sink.send(Message::Binary(connect_packet(client_id, username, 60).into())).await?;
        let mut buf = Vec::new();
        let code = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(msg) = stream.next().await {
                if let Message::Binary(b) = msg? {
                    buf.extend_from_slice(&b);
                    for p in parse_packets(&mut buf)? {
                        if let Packet::ConnAck(code) = p {
                            return Ok::<u8, anyhow::Error>(code);
                        }
                    }
                }
            }
            bail!("chat connection closed during login")
        })
        .await
        .map_err(|_| anyhow!("chat login timed out"))??;
        if code != 0 {
            bail!("chat login refused (MQTT code {code})");
        }
        sink.send(Message::Binary(subscribe_packet(1, topics).into())).await?;

        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let pending: Pending = Arc::default();
        let pushes: Arc<Mutex<Vec<Value>>> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));

        {
            let alive = alive.clone();
            tokio::spawn(async move {
                let mut ping = tokio::time::interval(Duration::from_secs(30));
                ping.tick().await;
                loop {
                    tokio::select! {
                        msg = out_rx.recv() => {
                            let Some(bytes) = msg else { break };
                            if sink.send(Message::Binary(bytes.into())).await.is_err() { break; }
                        }
                        _ = ping.tick() => {
                            if sink.send(Message::Binary(vec![0xC0, 0x00].into())).await.is_err() { break; }
                        }
                    }
                    if !alive.load(Ordering::Relaxed) {
                        break;
                    }
                }
                alive.store(false, Ordering::Relaxed);
                let _ = sink.close().await;
            });
        }
        {
            let alive = alive.clone();
            let pending = pending.clone();
            let pushes = pushes.clone();
            let out = out.clone();
            tokio::spawn(async move {
                let reason = loop {
                    let msg = match stream.next().await {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => break e.to_string(),
                        None => break "closed".to_string(),
                    };
                    let Message::Binary(b) = msg else {
                        if let Message::Close(f) = msg {
                            break format!("closed by server ({f:?})");
                        }
                        continue;
                    };
                    buf.extend_from_slice(&b);
                    let packets = match parse_packets(&mut buf) {
                        Ok(p) => p,
                        Err(e) => break e.to_string(),
                    };
                    for p in packets {
                        let Packet::Publish { topic, id, payload } = p else { continue };
                        if let Some(id) = id {
                            let mut ack = vec![0x40, 0x02];
                            ack.extend_from_slice(&id.to_be_bytes());
                            let _ = out.send(ack);
                        }
                        let Ok(v) = serde_json::from_slice::<Value>(&payload) else {
                            eprintln!("finsta: dm: {} bytes on {topic} that aren't JSON", payload.len());
                            continue;
                        };
                        let waiter = v["request_id"]
                            .as_i64()
                            .and_then(|rid| pending.lock().unwrap().remove(&rid));
                        match waiter {
                            Some(tx) => {
                                let _ = tx.send(v);
                            }
                            None if topic == "/ls_resp" => pushes.lock().unwrap().push(v),
                            None => {}
                        }
                    }
                };
                eprintln!("finsta: dm: chat connection ended: {reason}");
                alive.store(false, Ordering::Relaxed);
                pending.lock().unwrap().clear();
            });
        }

        Ok(Conn {
            out,
            pending,
            pushes,
            alive,
            next_packet: AtomicI64::new(2),
            next_request: AtomicI64::new(1),
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn next_request_id(&self) -> i64 {
        self.next_request.fetch_add(1, Ordering::Relaxed)
    }

    pub fn publish(&self, topic: &str, payload: &str) -> Result<()> {
        let id = (self.next_packet.fetch_add(1, Ordering::Relaxed) % 65535 + 1) as u16;
        self.out
            .send(publish_packet(topic, payload.as_bytes(), Some(id)))
            .map_err(|_| anyhow!("chat connection is closed"))
    }

    /// Publish `msg` (which carries `request_id`) on /ls_req and wait for the answer.
    pub async fn request(&self, msg: &Value, timeout: Duration) -> Result<Value> {
        let rid = msg["request_id"].as_i64().ok_or_else(|| anyhow!("request without id"))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(rid, tx);
        if let Err(e) = self.publish("/ls_req", &msg.to_string()) {
            self.pending.lock().unwrap().remove(&rid);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => bail!("chat connection closed"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&rid);
                bail!("Instagram didn't answer in time")
            }
        }
    }

    pub fn take_pushes(&self) -> Vec<Value> {
        std::mem::take(&mut *self.pushes.lock().unwrap())
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}
