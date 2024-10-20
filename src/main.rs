use std::{env, str::FromStr, time::Duration};

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
use tokio_stream::StreamExt;
use tokio_xmpp::{
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        roster::Roster,
    },
    Client, Stanza,
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

async fn command_loop(state: &AppState) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);

    loop {
        tokio::select! {
        Ok(Signal::XMLEntry(entry)) = state.rx.recv_async()=>{
            let stanza = content_stanza(jid.clone(), &entry).await?;
            client.send_stanza(stanza).await?;
        }
        Ok(Signal::Roster) = state.rx.recv_async() => {
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
        },
        Ok(Signal::Jid(jid)) = state.rx.recv_async() => {
            debug!("grabbing {jid} from http request");
            let s =
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'>
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
            match_event(event, &state, &mut client).await?;
        }


        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let state = AppState::new();

    tokio::spawn(run_server(state.clone()));
    command_loop(&state).await?;

    Ok(())
}

async fn run_server(state: AppState) -> Result<()> {
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
    state: State<AppState>,
    Form(input): Form<Input>,
) -> Result<String, AppError> {
    let jid = env::var("JID").expect("JID is not set");
    let content = input.message;
    let uri = jid;
    let entry = XMLEntry::quick_post(&content, &uri);

    state.tx.send_async(Signal::XMLEntry(entry)).await?;

    Ok(String::new())
}

#[axum::debug_handler]
async fn index_handler(state: State<AppState>) -> Result<Html<String>, AppError> {
    debug!("index handler");

    // send signal to request roster
    state.tx.send_async(Signal::Roster).await?;
    // wait for the response, todo figure out how to do this with async
    // tokio::time::sleep(Duration::from_secs(1)).await;

    let mut entries = Vec::new();
    loop {
        debug!("looping in index handler");

        tokio::select! {
            Ok(Signal::Entry(entry)) = state.rx.recv_async() => {
                debug!("entry: {:?}", entry);
                entries.push(entry);
            },
            Ok(Signal::EntryBreak) = state.rx.recv_async()=>{
                break
            }
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
