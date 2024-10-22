use std::{cell::RefCell, cmp::min, rc::Rc, str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use chrono::{DateTime, Utc};
use feed_rs::parser;
use flume::{Receiver, Sender};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Serialize;
use tokio::{sync::Mutex, task, time::timeout};
use tokio_stream::StreamExt;
use tokio_xmpp::{
    connect::starttls::StartTlsClient,
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        pubsub::{pubsub::Items, PubSub},
        roster::Roster,
    },
    Event, Stanza,
};
use tracing::debug;
use ulid::Ulid;

#[derive(Debug)]
pub enum Signal {
    XMLEntry(XMLEntry),
    Entry(Entry),
    Roster,
    Jid(String),
    Items((String, Items)),
    EntryBreak,
    Online,
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

#[derive(Serialize, Debug)]
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
        let title = format!("{}...", content[..smaller].to_string());
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

pub struct Connection {
    pub client: Arc<Mutex<StartTlsClient>>,
    online: bool,
    tx: Sender<Signal>,
    rx: Receiver<Signal>,
}

async fn event_match(client: Arc<Mutex<StartTlsClient>>, tx: Sender<Signal>) -> Result<()> {
    loop {
        debug!("looping");
        let mut client = client.lock().await;
        if let Ok(Some(event)) = timeout(Duration::from_secs(1), client.next()).await {
            debug!("event: {:?}", event);
            match event {
                Event::Online { .. } => {
                    debug!("online");
                    tx.send_async(Signal::Online).await?;
                }
                Event::Stanza(Stanza::Iq(iq)) => match iq.payload {
                    IqType::Result(Some(result)) => {
                        debug!("{result:?}");
                        if result.ns() == "http://jabber.org/protocol/pubsub" {
                            let event = PubSub::try_from(result.clone())?;
                            match event {
                                PubSub::Items(items) => {
                                    tx.send_async(Signal::Items((iq.id, items))).await?;
                                }
                                _ => debug!("event: {:?}", event),
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

impl Connection {
    pub fn new(client: StartTlsClient) -> Self {
        let client = Arc::new(Mutex::new(client));
        let (tx, rx) = flume::unbounded();
        let c = Arc::clone(&client);
        let t = tx.clone();
        task::spawn(async move { event_match(c, t).await });
        Connection {
            client,
            online: false,
            tx,
            rx,
        }
    }

    pub async fn get_items(&mut self, jid: &str, node: &str) -> Result<Items> {
        loop {
            if let Ok(Signal::Online) = self.rx.recv_async().await {
                break;
            }
        }
        let s = format!(
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <items node='{node}'/>
            </pubsub>"
        );
        let e = Element::from_str(&s)?;
        let iqtype = IqType::Get(e);
        let ulid = Ulid::new();
        let iq = Iq {
            id: ulid.to_string(),
            to: Some(BareJid::from_str(&jid)?.into()),
            from: None,
            payload: iqtype,
        };
        {
            let mut client = self.client.lock().await;
            client.send_stanza(iq.into()).await?;
        }
        let items: Items;
        let ulid = ulid.to_string();
        loop {
            if let Ok(Ok(Signal::Items((id, _items)))) =
                timeout(Duration::from_secs(1), self.rx.recv_async()).await
            {
                if id == ulid {
                    items = _items;
                    break;
                }
            }
        }
        // let items = match self.rx.recv_async().await? {
        //     Signal::Items((id, items)) => items,
        //     _ => {}
        // };
        Ok(items)
    }
}

fn send_item(items: Items, http_tx: Sender<Signal>) -> Result<()> {
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
                http_tx.send(Signal::Entry(entry))?;
            }
        }
    }
    Ok(())
}

pub async fn match_event(
    event: Event,
    http_tx: Sender<Signal>,
    xmpp_tx: Sender<Signal>,
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
                                send_item(items, http_tx)?;
                            }
                            _ => debug!("event: {:?}", event),
                        }
                    }
                    if element.ns() == "jabber:iq:roster" {
                        debug!("roster: {:?}", element);
                        let roster = Roster::try_from(element.clone())?;
                        for item in roster.items {
                            debug!("item: {:?}", item);
                            xmpp_tx
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
