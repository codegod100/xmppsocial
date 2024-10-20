use std::{cmp::min, env, str::FromStr};

use anyhow::Result;
use chrono::{DateTime, Utc};
use feed_rs::parser;
use flume::{Receiver, Sender};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Serialize;
use tokio_xmpp::{
    connect::StartTlsServerConnector,
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        pubsub::{pubsub::Items, PubSub},
        roster::Roster,
    },
    Client, Event, Stanza,
};
use tracing::debug;
use ulid::Ulid;

pub enum Signal {
    XMLEntry(XMLEntry),
    Entry(Entry),
    Roster,
    Jid(String),
    EntryBreak,
}

#[derive(Clone)]
pub struct AppState {
    pub tx: Sender<Signal>,
    pub rx: Receiver<Signal>,
}

impl AppState {
    pub fn new() -> Self {
        let (tx, rx) = flume::unbounded();
        Self { tx, rx }
    }
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub title: String,
    pub uri: String,
    pub published: DateTime<Utc>,
    pub content: String,
    pub links: Vec<String>,
    pub image: Option<String>,
}

#[derive(Serialize)]
pub struct XMLEntry {
    pub id: String,
    pub title: String,
    pub published: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub content: String,
    pub author: String,
    pub summary: String,
    pub jid: String,
    pub escape_jid: String,
}

impl XMLEntry {
    pub fn quick_post(content: &str, jid: &str) -> Self {
        let length = content.len();
        // title is up to 25 bytes of content
        let smaller = min(length, 25);
        let title = content[..smaller].to_string();
        let id = Ulid::new().to_string();
        let jid = jid.to_string();
        let escape_jid = utf8_percent_encode(&jid, NON_ALPHANUMERIC).to_string();
        XMLEntry {
            jid: jid.clone(),
            id,
            title,
            escape_jid,
            published: Utc::now(),
            updated: Utc::now(),
            content: content.to_string(),
            author: format!("xmpp:{}", jid),
            summary: String::new(),
        }
    }
}
fn send_item(items: Items, tx: &Sender<Signal>) -> Result<()> {
    for item in items.items {
        if let Some(payload) = &item.payload {
            let payload_s = String::from(payload);
            debug!("payload: {:?}", payload_s);
            let parsed = parser::parse(payload_s.as_bytes())?;
            for entry in parsed.entries {
                let mut uri = String::new();
                let person = entry.authors.get(0);
                if let Some(person) = person {
                    uri = person.uri.clone().unwrap_or_default();
                }
                let entry = Entry {
                    title: entry.title.unwrap().content,
                    uri,
                    published: entry.published.unwrap_or_default(),
                    content: entry.content.unwrap_or_default().body.unwrap_or_default(),
                    links: entry
                        .links
                        .iter()
                        .filter(|l| match &l.rel {
                            Some(rel) => rel != "alternate" && rel != "replies",
                            None => false,
                        })
                        .map(|l| l.href.to_string())
                        .collect(),
                    image: entry
                        .links
                        .iter()
                        .filter(|l| match &l.rel {
                            Some(rel) => rel == "enclosure",
                            None => false,
                        })
                        .map(|l| l.href.to_string())
                        .next(),
                };

                debug!("entry: {:?}", entry);
                tx.send(Signal::Entry(entry))?;
            }
        }
    }
    Ok(())
}

pub async fn match_event(
    event: Event,
    state: &AppState,
    client: &mut Client<StartTlsServerConnector>,
) -> Result<()> {
    match event {
        Event::Online { .. } => {
            debug!("online");
        }
        Event::Stanza(stanza) => match stanza {
            Stanza::Iq(iq) => {
                debug!("iq: {:?}", iq);
                if let IqType::Result(Some(element)) = &iq.payload {
                    if element.ns() == "http://jabber.org/protocol/pubsub" {
                        let event = PubSub::try_from(element.clone())?;
                        match event {
                            PubSub::Items(items) => {
                                send_item(items, &state.tx)?;
                            }
                            _ => debug!("event: {:?}", event),
                        }
                    }
                    if element.ns() == "jabber:iq:roster" {
                        debug!("roster: {:?}", element);
                        let roster = Roster::try_from(element.clone())?;
                        for item in roster.items {
                            debug!("item: {:?}", item);
                            state
                                .tx
                                .send_async(Signal::Jid(item.jid.to_string()))
                                .await?;
                        }
                    }
                }
                if let IqType::Error(error) = &iq.payload {
                    debug!("error: {:?}", error);
                    // tx.send(format!("error: {:?}", error))?;
                }
            }
            _ => debug!("stanza: {:?}", stanza),
        },
        _ => debug!("event: {:?}", event),
    }
    Ok(())
}
