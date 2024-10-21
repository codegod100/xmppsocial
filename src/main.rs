use std::{env, str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::{request, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Form, Router,
};
use dotenv::dotenv;
use flume::{Receiver, Sender};
use serde::Deserialize;
use tera::{Context, Tera};
use tokio::{sync::Mutex, time::sleep};
use tokio_stream::StreamExt;
use tokio_xmpp::{
    connect::starttls::StartTlsClient,
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        roster::Roster,
    },
    Client, Event, Stanza,
};
use tracing::{debug, error};
use ulid::Ulid;
use xmppsocial::{match_event, AppState, Entry, Signal, XMLEntry};

async fn content_stanza(jid: BareJid, entry: &XMLEntry) -> Result<Stanza> {
    // debug!("got {entry}; attempting to publish");
    let mut tera = Tera::new("templates/**/*")?;
    tera.autoescape_on(vec![]);
    let mut context = Context::new();
    context.insert("entry", &entry);
    let template = tera.render("entry.tera.atom", &context)?;
    let s = format!(
        "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
                <publish node='urn:xmpp:microblog:0'>
                  <item id='{}'>
                    {}
                  </item>
                </publish>
        </pubsub>",
        entry.id, template
    );
    let ulid = Ulid::new().to_string();
    let e = Element::from_str(&s)?;
    let iqtype = IqType::Set(e);
    let iq = Iq {
        id: ulid,
        to: Some(jid.into()),
        from: None,
        payload: iqtype,
    };
    let stanza: Stanza = iq.into();
    debug!("{stanza:?}");

    Ok(stanza)
}

async fn command_loop(
    xmpp_rx: Receiver<Signal>,
    http_tx: Sender<Signal>,
    xmpp_tx: Sender<Signal>,
) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);
    loop {
        tokio::select! {
             Ok(signal) = xmpp_rx.recv_async() =>{
                match signal {
                    Signal::XMLEntry(entry) => {
                        let stanza = content_stanza(jid.clone(), &entry).await?;
                        client.send_stanza(stanza).await?;
                    }
                    Signal::Roster => {
                        debug!("roster response");

                        let s = "<query xmlns='jabber:iq:roster'/>";
                        let e = Element::from_str(&s)?;
                        let iqtype = IqType::Get(e);
                        let ulid = Ulid::new();
                        let iq = Iq {
                            id: ulid.to_string(),
                            to: Some(jid.clone().into()),
                            from: None,
                            payload: iqtype,
                        };
                        client.send_stanza(iq.into()).await?;
                        // todo figure out a smart way to wait
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        http_tx.send_async(Signal::EntryBreak).await?;
                    }
                    Signal::Jid(jid) => {
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
                        client.send_stanza(iq.into()).await?;
                    }
                    _ => debug!("some other thing"),
                }
            },
            event = client.next() => {
                    debug!("event: {:?}", event);
                    match event {
                        Some(Event::Online { .. }) => {}
                        Some(event) => match_event(event, http_tx.clone(), xmpp_tx.clone()).await?,
                        _ => {}
                    }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let (http_tx, http_rx) = flume::unbounded();
    let (xmpp_tx, xmpp_rx) = flume::unbounded();
    tokio::spawn(run_server(http_rx, xmpp_tx.clone()));
    command_loop(xmpp_rx, http_tx, xmpp_tx).await?;

    Ok(())
}

async fn run_server(http_rx: Receiver<Signal>, xmpp_tx: Sender<Signal>) -> Result<()> {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/", post(publish_handler))
        .with_state((http_rx, xmpp_tx));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    debug!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Deserialize)]
struct Input {
    message: String,
}

#[axum::debug_handler]
async fn publish_handler(
    State((http_rx, xmpp_tx)): State<(Receiver<Signal>, Sender<Signal>)>,
    Form(input): Form<Input>,
) -> Result<String, AppError> {
    let jid = env::var("JID").expect("JID is not set");
    let content = input.message;
    let uri = jid;
    let entry = XMLEntry::quick_post(&content, &uri);

    xmpp_tx.send_async(Signal::XMLEntry(entry)).await?;

    Ok(String::new())
}

#[axum::debug_handler]
async fn index_handler(
    State((http_rx, xmpp_tx)): State<(Receiver<Signal>, Sender<Signal>)>,
) -> Result<Html<String>, AppError> {
    debug!("index handler");
    {
        xmpp_tx.send_async(Signal::Roster).await?;
        debug!("after roster sent");
    }
    // wait for the response, todo figure out how to do this with async

    let mut entries = Vec::new();
    while let Ok(signal) = http_rx.recv_async().await {
        match signal {
            Signal::Entry(entry) => {
                debug!("entry: {:?}", entry);
                entries.push(entry);
            }
            Signal::EntryBreak => {
                break;
            }
            _ => {}
        }
    }

    entries.sort_by(|a, b| b.published.cmp(&a.published));
    let mut context = Context::new();
    context.insert("entries", &entries);

    let mut tera = Tera::new("templates/**/*.html")?;
    tera.autoescape_on(vec![]);
    let template = tera.render("index.tera.html", &context)?;
    Ok(Html(template))
}

struct AppError(anyhow::Error);
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
