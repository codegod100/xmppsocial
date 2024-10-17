use anyhow::Result;
use dotenv::dotenv;
use feed_rs::parser;
use flume::{Receiver, Sender};
use std::{env, str::FromStr, thread};
use tokio_rustls::rustls;
use tokio_stream::StreamExt;
use tokio_xmpp::{
    connect::StartTlsServerConnector,
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        pubsub::{pubsub::Items, PubSub},
    },
    Client, Event, Stanza,
};
use tracing::{debug, error};

pub async fn send_request(client: &mut Client<StartTlsServerConnector>, jid: &str) -> Result<()> {
    let s = "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
    <items node='urn:xmpp:microblog:0'/>
  </pubsub>";
    let e = Element::from_str(s)?;
    let iqtype = IqType::Get(e);
    let iq = Iq {
        to: Some(BareJid::from_str(jid)?.into()),
        from: None,
        id: "232323".to_string(),
        payload: iqtype,
    };
    // let stanza = Stanza::Iq(iq);
    client.send_stanza(iq.into()).await?;
    Ok(())
}

fn send_item(items: Items, tx: &Sender<String>) -> Result<()> {
    for item in items.items {
        if let Some(payload) = &item.payload {
            let payload_s = String::from(payload);
            debug!("payload: {:?}", payload_s);
            let parsed = parser::parse(payload_s.as_bytes())?;
            for entry in parsed.entries {
                let mut link_s = String::from("");
                for link in entry.links {
                    link_s.push_str(&link.href);
                    link_s.push_str("  ");
                }

                let s = format!(
                    "{} :: {} :: {}\n\n",
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

pub fn match_event(event: Event, tx: &Sender<String>) -> Result<()> {
    match event {
        Event::Online { .. } => {
            debug!("online");
        }
        Event::Stanza(stanza) => match stanza {
            Stanza::Iq(iq) => {
                debug!("iq: {:?}", iq);
                if let IqType::Result(Some(element)) = iq.payload {
                    let event = PubSub::try_from(element)?;
                    match event {
                        PubSub::Items(items) => {
                            send_item(items, tx)?;
                        }
                        _ => debug!("event: {:?}", event),
                    }
                }
            }
            _ => debug!("stanza: {:?}", stanza),
        },
        _ => debug!("event: {:?}", event),
    }
    Ok(())
}
