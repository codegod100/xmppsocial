use std::env;

use anyhow::Result;
use dotenv::dotenv;
use tracing::debug;
use xmppsocial::Connection;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dotenv().ok();

    let jid = env::var("JID").expect("JID is not set");
    let password = env::var("PASSWORD").expect("PASSWORD is not set");

    let mut connection = Connection::new(jid, password).await?;
    let items = connection
        .get_items("nandi@conversations.im", "urn:xmpp:microblog:0")
        .await?;
    debug!("{items:?}");

    let roster = connection.get_roster().await?;
    debug!("{roster:?}");

    for item in roster.items {
        let jid = item.jid.to_string();
        let items = connection.get_items(&jid, "urn:xmpp:microblog:0").await?;
        debug!("{items:?}");
    }

    Ok(())
}
