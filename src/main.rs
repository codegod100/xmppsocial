use std::{
    env,
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::Result;
use axum::{routing::get, Router};
use dotenv::dotenv;
use flume::{Receiver, Sender};
use futures::executor::block_on;
use tokio_xmpp::{connect::StartTlsServerConnector, jid::BareJid, Client};
use tracing::{debug, error};
use xmppsocial::{send_request, wait_stream};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    // rustls::crypto::ring::default_provider()
    //     .install_default()
    //     .unwrap();
    dotenv().ok();

    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);

    // channel for http request
    let (tx, rx) = flume::unbounded();

    // channel for xmpp request
    let (tx_xmpp, rx_xmpp) = flume::unbounded();

    thread::spawn(move || {
        for command in rx.iter() {
            // let client = Arc::clone(&client);
            // let mut client = client.lock().unwrap();
            match command {
                Command::GrabNode(jid) => {
                    debug!("grabbing node: {}", jid);
                    tx_xmpp.send("hello".to_string()).unwrap();
                    // let res = send_request(&mut client, &jid).await;
                    // if let Err(e) = res {
                    //     error!("error sending request: {}", e);
                    // }
                }
            }
        }
    });

    tokio::spawn(async move {
        debug!("STARTING SERVER");
        let res = run_server(tx, rx_xmpp).await;
        if let Err(e) = res {
            error!("error starting server: {}", e);
        }
    });

    wait_stream(&mut client).await?;
    Ok(())
}

async fn run_server(tx: Sender<Command>, rx_xmpp: Receiver<String>) -> Result<()> {
    let app = Router::new().route("/", get(|| handler(tx, rx_xmpp)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    debug!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

enum Command {
    GrabNode(String),
}

async fn handler(tx: Sender<Command>, rx_xmpp: Receiver<String>) -> String {
    // todo change this to a request param
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone()).unwrap();

    let res = tx.send(Command::GrabNode(jid.to_string()));
    if let Err(e) = res {
        error!("error sending request: {}", e);
    }
    rx_xmpp.recv().unwrap()
}
