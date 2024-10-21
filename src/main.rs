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

async fn event_loop(client: Arc<Mutex<StartTlsClient>>, state: Arc<Mutex<AppState>>) -> Result<()> {
    debug!("in event loop");
    loop {
        debug!("waiting on event loop client lock");
        let mut client = client.lock().await;
        debug!("got event loop client lock");
        // let client = client.timeout(Duration::from_secs(1));
        // tokio::pin!(client);

        tokio::select! {
            event = client.next() => {
                debug!("waiting on event loop state lock");
                let state = state.lock().await;
                    debug!("got event loop state lock");
                    debug!("event: {:?}", event);
                    match event {
                        Some(Event::Online { .. }) => {}
                        Some(event) => match_event(event, &state).await?,
                        _ => {}
                    }
            }
            _ = sleep(Duration::from_millis(100)) => {debug!("timeout")}

        }
    }
}
async fn command_loop(state: Arc<Mutex<AppState>>) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let client = Arc::new(Mutex::new(Client::new(jid.clone(), password)));
    let mut online = false;
    tokio::spawn(event_loop(client.clone(), state.clone()));
    // loop {}
    loop {
        let client = client.clone();
        debug!("waiting on command loop state lock");
        tokio::select! {


        state = state.lock()=>{
        debug!("got command loop state lock");

        if state.rx.is_empty() {
            continue;
        }
        let signal = state.rx.recv_async().await?;
        match signal {
            Signal::Online => online = true,
            Signal::XMLEntry(entry) => {
                let stanza = content_stanza(jid.clone(), &entry).await?;
                debug!("waiting on xmlentry client lock");
                let mut client = client.lock().await;
                debug!("got xmlentry client lock");
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
                debug!("waiting on roster response client lock");
                let mut client = client.lock().await;
                debug!("got roster response client lock");
                client.send_stanza(iq.into()).await?;
                debug!("stanza sent");
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
                // let stanza = Stanza::Iq(iq);
                debug!("waiting on jid response client lock");
                let mut client = client.lock().await;
                debug!("got jid response client lock");
                client.send_stanza(iq.into()).await?;
            }
            _ => debug!("some other thing"),
        }}
        _ = sleep(Duration::from_millis(100)) => {debug!{"timeout"}}
    }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let shared_state = Arc::new(Mutex::new(AppState::new()));
    let state = shared_state.clone();
    tokio::spawn(run_server(state));
    let state = shared_state.clone();
    command_loop(state).await?;

    Ok(())
}

async fn run_server(state: Arc<Mutex<AppState>>) -> Result<()> {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/", post(publish_handler))
        .with_state(state);
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
    State(state): State<Arc<Mutex<AppState>>>,
    Form(input): Form<Input>,
) -> Result<String, AppError> {
    let state = state.lock().await;
    let jid = env::var("JID").expect("JID is not set");
    let content = input.message;
    let uri = jid;
    let entry = XMLEntry::quick_post(&content, &uri);

    state.tx.send_async(Signal::XMLEntry(entry)).await?;

    Ok(String::new())
}

#[axum::debug_handler]
async fn index_handler(
    State(state): State<Arc<Mutex<AppState>>>,
) -> Result<Html<String>, AppError> {
    debug!("index handler");
    {
        debug!("waiting on index handler state lock 1");
        let state = state.lock().await;
        debug!("got index handler state lock 1");
        // send signal to request roster
        state.tx.send_async(Signal::Roster).await?;
        debug!("after roster sent");
    }
    // wait for the response, todo figure out how to do this with async
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut entries = Vec::new();
    loop {
        debug!("waiting on index handler state lock 2");
        let state = state.lock().await;
        debug!("got index handler state lock 2");

        tokio::select! {
            Ok(signal) =  state.rx.recv_async() => {
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
            },
            _ = sleep(Duration::from_millis(100)) => {debug!("timeout")}

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
