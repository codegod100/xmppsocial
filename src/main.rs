use std::{
    cell::RefCell,
    env,
    rc::Rc,
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::Duration,
};

use anyhow::Result;
use axum::{routing::get, Router};
use dotenv::dotenv;
use flume::{Receiver, Sender};
use tokio::{pin, time};
use tokio_stream::StreamExt;
use tokio_xmpp::{
    connect::StartTlsServerConnector,
    jid::BareJid,
    minidom::Element,
    parsers::iq::{Iq, IqType},
    Client,
};
use tracing::{debug, error};
use xmppsocial::{match_event, send_request, wait_stream};

enum Command {
    GrabNode(String),
    StartServer,
}

async fn command_loop(
    tx: Sender<Command>,
    rx: Receiver<Command>,
    tx_xmpp: Sender<String>,
) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);
    for command in rx.iter() {
        match command {
            Command::GrabNode(jid) => {
                debug!("grabbing node");
                let s = "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <items node='urn:xmpp:microblog:0'/>
          </pubsub>";
                let e = Element::from_str(s)?;
                let iqtype = IqType::Get(e);
                let iq = Iq {
                    to: Some(BareJid::from_str(&jid)?.into()),
                    from: None,
                    id: "232323".to_string(),
                    payload: iqtype,
                };
                // let stanza = Stanza::Iq(iq);
                client.send_stanza(iq.into()).await?;
            }
            Command::StartServer => {
                debug!("before loop");
                let sleep = time::sleep(Duration::from_secs(5));
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        _ = &mut sleep => {
                            debug!("operation timeout");
                            break;
                        }
                        res = client.next() => {
                            debug!("res: {:?}", res);
                        }
                    }
                }
                debug!("broke loop");
                tx.send(Command::StartServer)?;
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .unwrap();
    dotenv().ok();

    // channel for http request
    let (tx, rx) = flume::unbounded();

    // // channel for xmpp request
    let (tx_xmpp, rx_xmpp) = flume::unbounded();

    tx.send(Command::StartServer)?;
    let mx = tx.clone();
    // command loop, lets put everything inside of this
    tokio::spawn(async move { command_loop(mx, rx, tx_xmpp).await });

    let res = run_server(tx, rx_xmpp).await;
    if let Err(e) = res {
        error!("error starting server: {}", e);
    }

    Ok(())
}

async fn run_server(tx: Sender<Command>, rx_xmpp: Receiver<String>) -> Result<()> {
    let app = Router::new().route("/", get(|| handler(tx, rx_xmpp)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    debug!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handler(tx: Sender<Command>, rx_xmpp: Receiver<String>) -> String {
    debug!("inside handler");
    // todo change this to a request param
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone()).unwrap();

    let res = tx.send(Command::GrabNode(jid.to_string()));
    debug!("grabbed node");
    if let Err(e) = res {
        error!("error sending request: {}", e);
    }
    rx_xmpp.recv().unwrap()
}
