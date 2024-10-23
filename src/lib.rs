use anyhow::anyhow;
use anyhow::Result;
use chrono::{DateTime, Utc};
use core::time;
use feed_rs::parser;
use flume::{Receiver, Sender};
use futures::future::OkInto;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Serialize;
use std::{cell::RefCell, cmp::min, rc::Rc, str::FromStr, sync::Arc, time::Duration};
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
    Client, Event, Stanza,
};
use tracing::debug;
use ulid::Ulid;

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
    jid: BareJid,
    pub client: Arc<Mutex<StartTlsClient>>,
    online: Arc<Mutex<bool>>,
    items_tx: Sender<Items>,
    items_rx: Receiver<Items>,
    roster_tx: Sender<Roster>,
    roster_rx: Receiver<Roster>,
}

impl Connection {
    pub async fn new(jid: String, password: String) -> Result<Self> {
        let jid = BareJid::from_str(&jid.clone())?;
        let client = Client::new(jid.clone(), password);
        let client = Arc::new(Mutex::new(client));
        let (items_tx, items_rx) = flume::unbounded();
        let (roster_tx, roster_rx) = flume::unbounded();
        let conn = Connection {
            jid,
            client,
            online: Arc::new(Mutex::new(false)),
            items_tx,
            items_rx,
            roster_tx,
            roster_rx,
        };
        conn.event_loop();
        loop {
            let online = conn.online.lock().await;
            if *online {
                break;
            }
        }
        Ok(conn)
    }

    pub fn event_loop(&self) {
        let online = Arc::clone(&self.online);
        let items_tx = self.items_tx.clone();
        let client = Arc::clone(&self.client);
        let roster_tx = self.roster_tx.clone();
        task::spawn(async move {
            loop {
                let mut client = client.lock().await;
                if let Ok(Some(event)) = timeout(Duration::from_millis(10), client.next()).await {
                    debug!("event: {:?}", event);
                    match event {
                        Event::Online { .. } => {
                            debug!("online");
                            // tx.send_async(Signal::Online).await?;
                            let mut online = online.lock().await;
                            *online = true;
                        }
                        Event::Stanza(Stanza::Iq(iq)) => match iq.payload {
                            IqType::Result(Some(result)) => {
                                debug!("{result:?}");
                                if result.ns() == "http://jabber.org/protocol/pubsub" {
                                    let event = PubSub::try_from(result.clone())?;
                                    match event {
                                        PubSub::Items(items) => {
                                            items_tx.send_async(items).await?;
                                        }
                                        _ => {}
                                    }
                                }
                                if result.ns() == "jabber:iq:roster" {
                                    let roster = Roster::try_from(result.clone())?;
                                    roster_tx.send_async(roster).await?;
                                }
                            }
                            IqType::Error(err) => {
                                debug!("stanza error: {err:?}")
                            }
                            _ => {}
                        },
                        other => {
                            debug!("Other: {other:?}");
                        }
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        });
    }

    pub async fn get_roster(&mut self) -> Result<Roster> {
        let s = "<query xmlns='jabber:iq:roster'/>";
        let e = Element::from_str(&s)?;
        let iqtype = IqType::Get(e);
        let ulid = Ulid::new();
        let iq = Iq {
            id: ulid.to_string(),
            to: Some(self.jid.clone().into()),
            from: None,
            payload: iqtype,
        };
        {
            let mut client = self.client.lock().await;
            client.send_stanza(iq.into()).await?;
        }
        let roster = self.roster_rx.recv_async().await?;
        Ok(roster)
    }

    pub async fn get_items(&mut self, jid: &str, node: &str) -> Result<Option<Items>> {
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
        let items = self.items_rx.recv_timeout(Duration::from_millis(100));
        let items = match items {
            Ok(items) => Some(items),
            _ => None,
        };
        Ok(items)
    }
}

pub fn convert_to_entries(items: Items) -> Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
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

                entries.push(entry);
            }
        }
    }
    Ok(entries)
}
