use anyhow::Result;
use feed_rs::parser;
use flume::Sender;
use tokio_xmpp::{
    parsers::{
        iq::IqType,
        pubsub::{pubsub::Items, PubSub},
    },
    Event, Stanza,
};
use tracing::debug;

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
                    "<div>{}</div><div>{}</div><div>{}</div>\n\n",
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
                if let IqType::Result(Some(element)) = &iq.payload {
                    let event = PubSub::try_from(element.clone())?;
                    match event {
                        PubSub::Items(items) => {
                            send_item(items, tx)?;
                        }
                        _ => debug!("event: {:?}", event),
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
