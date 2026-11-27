//! The `popsub backup` command: asks a running server to back itself up,
//! through the `backup` admin request, and prints where the backup went.

use std::time::Duration;

use clap::Args;

use crate::users::{Connection, connect_waiting};

/// Options for `popsub backup`.
#[derive(Args, Debug)]
pub struct BackupArgs {
    /// The backup's directory name under the server's storage.backup_path; defaults to the time in UTC
    #[arg(long)]
    pub name: Option<String>,
    /// How long to wait for the server to finish copying, in seconds
    #[arg(long, default_value_t = 3600)]
    pub timeout_secs: u64,
}

/// Asks the server for a backup and prints what it wrote.
///
/// Args:
///     connection: Where to connect and who to log in as.
///     args: The backup's name and how long to wait.
///
/// Raises:
///     popsub_sdk::Error: When connecting fails or the server refuses or
///         fails the backup.
///     String: When the password can't be read.
pub async fn run(
    connection: Connection,
    args: BackupArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = connect_waiting(connection, Duration::from_secs(args.timeout_secs.max(1))).await?;
    let outcome = client.admin().backup(args.name).await;
    client.close();
    let backup = outcome?;
    println!("Backed up to {}", backup.path);
    println!("Topics:   {}", backup.topics);
    println!("Messages: {}", backup.records);
    println!("Bytes:    {}", backup.bytes);
    println!(
        "Accounts: {}",
        if backup.users {
            "included"
        } else {
            "not included (they aren't kept in users.json)"
        }
    );
    Ok(())
}
