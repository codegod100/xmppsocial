use anyhow::Result;
use dotenv::dotenv;
use feed_rs::parser;
use std::{env, str::FromStr};
use tokio_rustls::rustls;
use tokio_stream::StreamExt;
use tokio_xmpp::{
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        pubsub::PubSub,
    },
    Client, Event, Stanza,
};
use tracing::debug;

#[tokio::main]
async fn main() -> Result<()> {
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .unwrap();
    dotenv().ok();
    tracing_subscriber::fmt::init();
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);
    // client.

    let mut stream_ended = false;
    while !stream_ended {
        if let Some(event) = client.next().await {
            match event {
                Event::Online { .. } => {
                    debug!("online");
                    let s = "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
    <items node='urn:xmpp:microblog:0'/>
  </pubsub>";
                    let e = Element::from_str(s)?;
                    let iqtype = IqType::Get(e);
                    let iq = Iq {
                        to: Some(jid.clone().into()),
                        from: None,
                        id: "232323".to_string(),
                        payload: iqtype,
                    };
                    // let stanza = Stanza::Iq(iq);
                    client.send_stanza(iq.into()).await?;
                }
                Event::Stanza(stanza) => match stanza {
                    Stanza::Iq(iq) => {
                        debug!("iq: {:?}", iq);
                        if let IqType::Result(Some(element)) = iq.payload {
                            let event = PubSub::try_from(element)?;
                            match event {
                                PubSub::Items(items) => {
                                    debug!("items: {:?}", items);
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
                                                debug!(
                                                    "entry: {:?}",
                                                    format!(
                                                        "{} :: {} :: {}",
                                                        entry.title.unwrap().content,
                                                        entry
                                                            .content
                                                            .unwrap_or_default()
                                                            .body
                                                            .unwrap_or_default(),
                                                        link_s
                                                    )
                                                );
                                            }
                                        }
                                    }
                                }
                                _ => debug!("event: {:?}", event),
                            }
                        }
                    }
                    _ => debug!("stanza: {:?}", stanza),
                },
                _ => debug!("event: {:?}", event),
            }
        } else {
            stream_ended = true;
        }
    }
    Ok(())
}
