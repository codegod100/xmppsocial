use std::{cell::RefCell, env, rc::Rc, str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use dotenv::dotenv;
use tokio::{sync::Mutex, task, time::sleep};
use tokio_xmpp::{connect, jid::BareJid, Client};
use tracing::debug;
use xmppsocial::{match_event, Connection, Signal, XMLEntry};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let jid = env::var("JID").expect("JID is not set");
    let jid = BareJid::from_str(&jid.clone())?;
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let client = Client::new(jid.clone(), password);
    {
        let mut connection = Connection::new(client).await;
        let items = connection
            .get_items("nandi@conversations.im", "urn:xmpp:microblog:0")
            .await?;
        debug!("{items:?}");
        debug!("done");
    }
    Ok(())
}
