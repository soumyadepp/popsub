//! The `popsub topics` subcommands: listing, describing and purging topics
//! on a running server through the admin requests, with the SDK. They
//! connect the same way as `popsub users`.

use clap::Subcommand;
use popsub_sdk::TopicInfo;

use crate::users::{Connection, connect};

/// One `topics` action.
#[derive(Subcommand, Debug)]
pub enum TopicsCommand {
    /// List every topic that has messages or ever had
    List,
    /// Show one topic
    Show {
        /// The topic
        name: String,
    },
    /// Remove every message a topic holds; its numbering carries on
    Purge {
        /// The topic
        name: String,
    },
}

/// Runs one `topics` action against a server.
///
/// Args:
///     connection: Where to connect and who to log in as.
///     command: The action.
///
/// Raises:
///     popsub_sdk::Error: When connecting fails or the server refuses the request.
///     String: When the password can't be read.
pub async fn run(
    connection: Connection,
    command: TopicsCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = connect(connection).await?;
    let admin = client.admin();
    let outcome = match command {
        TopicsCommand::List => admin.list_topics().await.map(|topics| {
            println!(
                "{:<40} {:>10} {:>10} {:>10} {:>11}",
                "NAME", "FIRST", "LAST", "MESSAGES", "SUBSCRIBERS"
            );
            for topic in &topics {
                println!(
                    "{:<40} {:>10} {:>10} {:>10} {:>11}",
                    topic.name, topic.first_seq, topic.last_seq, topic.messages, topic.subscribers
                );
            }
        }),
        TopicsCommand::Show { name } => admin.get_topic(name).await.map(|topic| describe(&topic)),
        TopicsCommand::Purge { name } => admin.purge_topic(name).await.map(|topic| {
            println!("Purged {}", topic.name);
            describe(&topic);
        }),
    };
    client.close();
    Ok(outcome?)
}

/// Prints one topic.
///
/// Args:
///     topic: The topic.
fn describe(topic: &TopicInfo) {
    println!("Name:        {}", topic.name);
    println!("Messages:    {}", topic.messages);
    println!("First seq:   {}", topic.first_seq);
    println!("Last seq:    {}", topic.last_seq);
    println!("Subscribers: {}", topic.subscribers);
}
