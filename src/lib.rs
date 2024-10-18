use std::{env, str::FromStr};

use anyhow::Result;
use feed_rs::parser;
use flume::Sender;
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

fn send_item(items: Items, tx: &Sender<String>) -> Result<()> {
    for item in items.items {
        if let Some(payload) = &item.payload {
            let payload_s = String::from(payload);
            debug!("payload: {:?}", payload_s);
            let parsed = parser::parse(payload_s.as_bytes())?;
            for entry in parsed.entries {
                let mut link_s = String::new();
                for link in entry.links {
                    link_s.push_str(&format!("<a href='{}'>{}</a>  ", link.href, link.href));
                }

                let s = format!(
                    "<div class='has-background-black-ter mb-5'><div >{}</div><div>{}</div><div>{}</div></div>\n\n",
                    entry.title.unwrap().content,
                    entry.content.unwrap_or_default().body.unwrap_or_default(),
                    link_s
                );
                debug!("entry: {:?}", s);
                tx.send(s)?;
            }
        }
    }
    Ok(())
}

pub async fn match_event(
    event: Event,
    http_request: &Sender<String>,
    tx: &Sender<String>,
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
                                send_item(items, tx)?;
                            }
                            _ => debug!("event: {:?}", event),
                        }
                    }
                    if element.ns() == "jabber:iq:roster" {
                        debug!("roster: {:?}", element);
                        let roster = Roster::try_from(element.clone())?;
                        for item in roster.items {
                            debug!("item: {:?}", item);
                            http_request.send_async(item.jid.to_string()).await?;
                        }
                    }
                }
                if let IqType::Error(error) = &iq.payload {
                    debug!("error: {:?}", error);
                    tx.send(format!("error: {:?}", error))?;
                }
            }
            _ => debug!("stanza: {:?}", stanza),
        },
        _ => debug!("event: {:?}", event),
    }
    Ok(())
}
