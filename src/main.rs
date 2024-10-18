use std::{env, str::FromStr, time::Duration};

use anyhow::Result;
use axum::{
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use dotenv::dotenv;
use flume::{Receiver, Sender};
use tera::{Context, Tera};
use tokio_stream::StreamExt;
use tokio_xmpp::{
    jid::BareJid,
    minidom::Element,
    parsers::{
        iq::{Iq, IqType},
        roster::Roster,
    },
    Client,
};
use tracing::{debug, error};
use ulid::Ulid;
use xmppsocial::{match_event, Entry};

async fn command_loop(
    http_request: Sender<String>,
    http_response: Receiver<String>,
    content_request: Sender<Entry>,
    roster_response: Receiver<()>,
) -> Result<()> {
    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let mut client = Client::new(jid.clone(), password);

    loop {
        tokio::select! {
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
    tokio::spawn(run_server(
        http_request.clone(),
        content_response,
        roster_request,
    ));
    command_loop(
        http_request,
        http_response,
        content_request,
        roster_response,
    )
    .await?;

    Ok(())
}

async fn run_server(
    http_request: Sender<String>,
    content_response: Receiver<Entry>,
    roster_request: Sender<()>,
) -> Result<()> {
    let app = Router::new().route(
        "/",
        get(|| handler(http_request, content_response, roster_request)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    debug!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
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
