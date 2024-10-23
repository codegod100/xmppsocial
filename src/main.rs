use std::{env, str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use dotenv::dotenv;
use flume::{Receiver, Sender};
use serde::Deserialize;
use tera::{Context, Tera};
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tokio_xmpp::{
    jid::BareJid,
    minidom::Element,
    parsers::iq::{Iq, IqType},
    Client, Event, Stanza,
};
use tracing::debug;
use ulid::Ulid;
use xmppsocial::{convert_to_entries, Connection, XMLEntry};

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let jid = env::var("JID").expect("JID is not set");
    let password = env::var("PASSWORD").expect("PASSWORD is not set");

    let connection = Arc::new(Mutex::new(Connection::new(jid, password).await?));
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/post", post(publish_handler))
        .with_state(connection);
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
    State(connection): State<Arc<Mutex<Connection>>>,
    Form(input): Form<Input>,
) -> Result<Redirect, AppError> {
    let connection = connection.lock().await;
    let jid = connection.jid.clone();
    let content = input.message;
    let entry = XMLEntry::quick_post(&content, &jid.clone().to_string());
    let stanza = content_stanza(jid.clone(), &entry).await?;
    let mut client = connection.client.lock().await;
    client.send_stanza(stanza).await?;
    Ok(Redirect::to("/"))
}

#[axum::debug_handler]
async fn index_handler(
    State(connection): State<Arc<Mutex<Connection>>>,
) -> Result<Html<String>, AppError> {
    let mut connection = connection.lock().await;
    let roster = connection.get_roster().await?;
    let mut entries = Vec::new();
    for item in roster.items {
        let jid = item.jid.to_string();
        let items = connection.get_items(&jid, "urn:xmpp:microblog:0").await?;
        debug!("{items:?}");
        let items = match items {
            Some(items) => items,
            None => continue,
        };
        let _entries = convert_to_entries(items)?;
        entries.extend(_entries);
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
