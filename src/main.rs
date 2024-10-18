use std::{env, str::FromStr, time::Duration};

use anyhow::Result;
use axum::{extract::Path, response::Html, routing::get, Router};
use dotenv::dotenv;
use flume::{Receiver, Sender};
use tokio_stream::StreamExt;
use tokio_xmpp::{
    jid::BareJid,
    minidom::Element,
    parsers::iq::{Iq, IqType},
    Client,
};
use tracing::{debug, error};
use ulid::Ulid;
use xmppsocial::match_event;

async fn command_loop(
    http_response: Receiver<String>,
    content_request: Sender<String>,
) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);
    loop {
        tokio::select! {
        Ok(jid) = http_response.recv_async() => {
            debug!("grabbing {jid} from http request");
            let s = "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <items node='urn:xmpp:microblog:0'/>
          </pubsub>";
            let e = Element::from_str(s)?;
            let iqtype = IqType::Get(e);
            let ulid = Ulid::new();
            let iq = Iq {
                id: ulid.to_string(),
                to: Some(BareJid::from_str(&jid)?.into()),
                from: None,
                payload: iqtype,
            };
            // let stanza = Stanza::Iq(iq);
            client.send_stanza(iq.into()).await?;
        },

        Some(event) = client.next() => {
            debug!("event: {:?}", event);
            match_event(event, &content_request)?;
        }


        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let (http_request, http_response) = flume::unbounded();
    let (content_request, content_response) = flume::unbounded();
    tokio::spawn(run_server(http_request, content_response));
    command_loop(http_response, content_request).await?;

    Ok(())
}

async fn run_server(
    http_request: Sender<String>,
    content_response: Receiver<String>,
) -> Result<()> {
    let app = Router::new().route(
        "/:jid",
        get(|Path(jid): Path<String>| handler(jid, http_request, content_response)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    debug!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handler(
    jid: String,
    http_request: Sender<String>,
    content_response: Receiver<String>,
) -> Html<String> {
    debug!("inside handler");
    // todo change this to a request param

    let res = http_request.send_async(jid).await;
    if let Err(e) = res {
        error!("error sending request: {}", e);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut s = String::new();
    loop {
        debug!("looping in request handler");

        tokio::select! {
            Ok(item) = content_response.recv_async() => {
                debug!("web output: {:?}", item);
                s.push_str(&item);
            }
        }
        if content_response.is_empty() {
            debug!("content_response is empty");
            break;
        }
    }

    Html(s)
}
