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
use xmppsocial::{match_event, Entry, XMLEntry};

async fn command_loop(
    http_request: Sender<String>,
    http_response: Receiver<String>,
    content_request: Sender<Entry>,
    roster_response: Receiver<()>,
    publish_response: Receiver<(String, String)>,
) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);

    loop {
        tokio::select! {
        Ok((entry,id)) = publish_response.recv_async()=>{
            // debug!("got {entry}; attempting to publish");

            let s = format!("<pubsub xmlns='http://jabber.org/protocol/pubsub'>
    <publish node='urn:xmpp:microblog:0'>
      <item id='{}'>
{}
      </item>
    </publish></pubsub>",id,entry);
            let ulid = Ulid::new().to_string();
            let e = Element::from_str(&s)?;
            let iqtype = IqType::Set(e);
            let iq = Iq {
                id: ulid,
                to: Some(jid.clone().into()),
                from: None,
                payload: iqtype,
            };
            let stanza: Stanza = iq.into();
            debug!("{stanza:?}");
            client.send_stanza(stanza).await?;
        },
        Ok(()) = roster_response.recv_async() => {
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
            match_event(event, &http_request, &content_request, &mut client).await?;
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
    let (roster_request, roster_response) = flume::unbounded();
    let (publish_request, publish_response) = flume::unbounded();
    tokio::spawn(run_server(
        http_request.clone(),
        content_response,
        roster_request,
        publish_request,
    ));
    command_loop(
        http_request,
        http_response,
        content_request,
        roster_response,
        publish_response,
    )
    .await?;

    Ok(())
}

#[derive(Clone)]
struct AppState {
    publish_request: Sender<(String, String)>,
}

async fn run_server(
    http_request: Sender<String>,
    content_response: Receiver<Entry>,
    roster_request: Sender<()>,
    publish_request: Sender<(String, String)>,
) -> Result<()> {
    let state = AppState {
        publish_request: publish_request,
    };
    let app = Router::new()
        .route(
            "/",
            get(|| handler(http_request, content_response, roster_request)),
        )
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
    let mut tera = Tera::new("templates/**/*")?;
    tera.autoescape_on(vec![]);
    let mut context = Context::new();
    context.insert("entry", &entry);
    let template = tera.render("entry.tera.atom", &context)?;
    state
        .publish_request
        .send_async((template, entry.id))
        .await?;

    Ok(String::new())
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

async fn handler(
    http_request: Sender<String>,
    content_response: Receiver<Entry>,
    roster_request: Sender<()>,
) -> Result<Html<String>, AppError> {
    debug!("inside handler");
    // todo change this to a request param

    // let res = http_request.send_async(jid).await;
    // if let Err(e) = res {
    //     error!("error sending request: {}", e);
    // }
    roster_request.send_async(()).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut entries = Vec::new();
    loop {
        debug!("looping in request handler");

        tokio::select! {
            Ok(item) = content_response.recv_async() => {
                debug!("web output: {:?}", item);
                entries.push(item);
            }
        }
        if content_response.is_empty() {
            debug!("content_response is empty");
            break;
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
