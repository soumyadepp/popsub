//! The `popsub users` subcommands: managing accounts on a running server
//! through the admin requests, with the SDK.
//!
//! The command logs in as an admin. Its password comes from `--password`,
//! then the `POPSUB_PASSWORD` environment variable, then a prompt. A new
//! account's password is never taken from the command line, where other
//! users of the machine could see it: it comes from `POPSUB_NEW_PASSWORD`
//! or a prompt that doesn't echo.

use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use popsub_sdk::{
    Access, Client, ClientConfig, Credentials, ReconnectPolicy, RoleKind, RoleSpec, TopicGrant,
    UserInfo,
};

/// The environment variable read for the admin's password.
const PASSWORD_ENV: &str = "POPSUB_PASSWORD";

/// The environment variable read for a new account password.
const NEW_PASSWORD_ENV: &str = "POPSUB_NEW_PASSWORD";

/// Where to connect and who to log in as.
#[derive(Args, Debug)]
pub struct Connection {
    /// WebSocket URL of the server
    #[arg(long, global = true, default_value = "ws://127.0.0.1:8080")]
    url: String,
    /// Admin account to log in as
    #[arg(long, global = true, default_value = "admin")]
    username: String,
    /// Admin password; prefer POPSUB_PASSWORD or the prompt, since arguments are visible to other users
    #[arg(long, global = true)]
    password: Option<String>,
    /// PEM certificate to trust for wss://, such as a self-signed one
    #[arg(long, global = true)]
    ca_cert: Option<std::path::PathBuf>,
}

/// What an account may do, on the command line.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RoleArg {
    /// Every topic and the admin requests
    Admin,
    /// The topics its grants allow
    User,
    /// Subscribe only, to the topics its grants allow
    Readonly,
}

/// What a grant allows, on the command line.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AccessArg {
    /// Subscribing only
    Subscribe,
    /// Publishing only
    Publish,
    /// Both
    Both,
}

/// One `users` action.
#[derive(Subcommand, Debug)]
pub enum UsersCommand {
    /// List every account
    List,
    /// Show one account
    Show {
        /// The account
        name: String,
    },
    /// Create an account; its password comes from POPSUB_NEW_PASSWORD or a prompt
    Add {
        /// The account
        name: String,
        /// What it may do
        #[arg(long, value_enum, default_value = "user")]
        role: RoleArg,
        /// A topic grant, PATTERN or PATTERN:ACCESS (subscribe, publish or both); repeatable
        #[arg(long = "topic", value_name = "PATTERN[:ACCESS]")]
        topics: Vec<String>,
    },
    /// Grant an account a topic pattern, replacing its grant for exactly that pattern
    Grant {
        /// The account
        name: String,
        /// The topic pattern, with + for one level and a trailing # for the rest
        pattern: String,
        /// What it allows
        #[arg(long, value_enum, default_value = "both")]
        access: AccessArg,
    },
    /// Remove one of an account's grants and end the subscriptions it no longer allows
    Revoke {
        /// The account
        name: String,
        /// The pattern, exactly as granted
        pattern: String,
    },
    /// Replace an account's role and grants
    Role {
        /// The account
        name: String,
        /// The new role
        #[arg(value_enum)]
        role: RoleArg,
        /// A topic grant, PATTERN or PATTERN:ACCESS; repeatable
        #[arg(long = "topic", value_name = "PATTERN[:ACCESS]")]
        topics: Vec<String>,
    },
    /// Set an account's password, from POPSUB_NEW_PASSWORD or a prompt
    Passwd {
        /// The account
        name: String,
    },
    /// Disable an account and close its connections
    Disable {
        /// The account
        name: String,
    },
    /// Enable a disabled account
    Enable {
        /// The account
        name: String,
    },
    /// Delete an account and close its connections
    Delete {
        /// The account
        name: String,
    },
}

/// Runs one `users` action against a server.
///
/// Args:
///     connection: Where to connect and who to log in as.
///     command: The action.
///
/// Raises:
///     popsub_sdk::Error: When connecting fails or the server refuses the request.
///     String: When a password can't be read, a `--topic` is malformed, or
///         the `--ca-cert` file can't be read.
pub async fn run(
    connection: Connection,
    command: UsersCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let password = match connection.password {
        Some(password) => password,
        None => secret(
            PASSWORD_ENV,
            &format!("Password for {}: ", connection.username),
            false,
        )?,
    };
    let config = crate::trust(
        ClientConfig::new(
            connection.url.as_str(),
            Credentials::password(connection.username.as_str(), password),
        )
        .reconnect(ReconnectPolicy {
            max_attempts: Some(0),
            ..ReconnectPolicy::default()
        })
        .request_timeout(Duration::from_secs(10)),
        connection.ca_cert.as_deref(),
    )?;
    let client = Client::connect(config).await?;
    let outcome = act(&client, command).await;
    client.close();
    outcome
}

/// Carries out one action on a connected client and prints the result.
///
/// Args:
///     client: The admin's client.
///     command: The action.
///
/// Raises:
///     popsub_sdk::Error: When the server refuses the request.
///     String: When a password can't be read, or a `--topic` is malformed.
async fn act(client: &Client, command: UsersCommand) -> Result<(), Box<dyn std::error::Error>> {
    let admin = client.admin();
    match command {
        UsersCommand::List => {
            let users = admin.list_users().await?;
            println!("{:<24} {:<9} {:<8} TOPICS", "NAME", "ROLE", "ENABLED");
            for user in &users {
                println!(
                    "{:<24} {:<9} {:<8} {}",
                    user.username,
                    role_name(user.role.kind),
                    if user.enabled { "yes" } else { "no" },
                    grants(&user.role)
                );
            }
        }
        UsersCommand::Show { name } => describe(&admin.get_user(name).await?),
        UsersCommand::Add { name, role, topics } => {
            let role = role_spec(role, &topics)?;
            let password = new_password(&name)?;
            describe(&admin.create_user(name, password, role).await?);
        }
        UsersCommand::Grant {
            name,
            pattern,
            access,
        } => describe(&admin.grant(name, pattern, access_of(access)).await?),
        UsersCommand::Revoke { name, pattern } => describe(&admin.revoke(name, pattern).await?),
        UsersCommand::Role { name, role, topics } => {
            describe(&admin.set_role(name, role_spec(role, &topics)?).await?)
        }
        UsersCommand::Passwd { name } => {
            let password = new_password(&name)?;
            admin.set_password(name.as_str(), password).await?;
            println!("Password changed for {name}");
        }
        UsersCommand::Disable { name } => describe(&admin.set_enabled(name, false).await?),
        UsersCommand::Enable { name } => describe(&admin.set_enabled(name, true).await?),
        UsersCommand::Delete { name } => {
            admin.delete_user(name.as_str()).await?;
            println!("Deleted {name}");
        }
    }
    Ok(())
}

/// Builds a role from the command line.
///
/// Args:
///     role: The kind of account.
///     topics: The `--topic` values. Without an access suffix a grant
///         allows both for a user and subscribing for a readonly account.
///
/// Returns:
///     The role.
///
/// Raises:
///     String: When grants are given for an admin.
fn role_spec(role: RoleArg, topics: &[String]) -> Result<RoleSpec, String> {
    let (kind, default) = match role {
        RoleArg::Admin if !topics.is_empty() => {
            return Err("admins can use every topic; drop the --topic options".to_string());
        }
        RoleArg::Admin => return Ok(RoleSpec::admin()),
        RoleArg::User => (RoleKind::User, Access::Both),
        RoleArg::Readonly => (RoleKind::Readonly, Access::Subscribe),
    };
    Ok(RoleSpec {
        kind,
        topics: topics.iter().map(|t| parse_grant(t, default)).collect(),
    })
}

/// Splits `PATTERN:ACCESS` into a grant. The suffix only counts when it is
/// an access name, so a pattern containing `:` still works on its own.
///
/// Args:
///     value: The `--topic` value.
///     default: The access when there is no suffix.
///
/// Returns:
///     The grant.
fn parse_grant(value: &str, default: Access) -> TopicGrant {
    if let Some((pattern, suffix)) = value.rsplit_once(':') {
        let access = match suffix {
            "subscribe" => Some(Access::Subscribe),
            "publish" => Some(Access::Publish),
            "both" => Some(Access::Both),
            _ => None,
        };
        if let Some(access) = access {
            return TopicGrant::new(pattern, access);
        }
    }
    TopicGrant::new(value, default)
}

/// Converts the command-line access to the protocol's.
///
/// Args:
///     access: The command-line value.
///
/// Returns:
///     The protocol value.
fn access_of(access: AccessArg) -> Access {
    match access {
        AccessArg::Subscribe => Access::Subscribe,
        AccessArg::Publish => Access::Publish,
        AccessArg::Both => Access::Both,
    }
}

/// The name a role kind is shown with.
///
/// Args:
///     kind: The kind.
///
/// Returns:
///     Its name on the wire.
fn role_name(kind: RoleKind) -> &'static str {
    match kind {
        RoleKind::Admin => "admin",
        RoleKind::User => "user",
        RoleKind::Readonly => "readonly",
    }
}

/// Formats a role's grants on one line.
///
/// Args:
///     role: The role.
///
/// Returns:
///     The grants as `pattern:access`, comma separated; `*` for an admin and
///     `-` for none.
fn grants(role: &RoleSpec) -> String {
    if role.kind == RoleKind::Admin {
        return "*".to_string();
    }
    if role.topics.is_empty() {
        return "-".to_string();
    }
    role.topics
        .iter()
        .map(|g| {
            let access = match g.access {
                Access::Subscribe => "subscribe",
                Access::Publish => "publish",
                Access::Both => "both",
            };
            format!("{}:{access}", g.pattern)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Prints one account.
///
/// Args:
///     user: The account.
fn describe(user: &UserInfo) {
    println!("Name:    {}", user.username);
    println!("Role:    {}", role_name(user.role.kind));
    println!("Enabled: {}", if user.enabled { "yes" } else { "no" });
    println!("Topics:  {}", grants(&user.role));
}

/// Reads the password for an account being created or changed.
///
/// Args:
///     name: The account, for the prompt.
///
/// Returns:
///     The password.
///
/// Raises:
///     String: When it can't be read, or the two prompted copies differ.
fn new_password(name: &str) -> Result<String, String> {
    secret(
        NEW_PASSWORD_ENV,
        &format!("New password for {name}: "),
        true,
    )
}

/// Reads a secret from an environment variable, or else from a prompt that
/// doesn't echo.
///
/// Args:
///     env: The environment variable to try first.
///     prompt: The prompt.
///     confirm: Whether to ask twice and compare.
///
/// Returns:
///     The secret.
///
/// Raises:
///     String: When the prompt can't be read, or the two copies differ.
fn secret(env: &str, prompt: &str, confirm: bool) -> Result<String, String> {
    if let Ok(value) = std::env::var(env)
        && !value.is_empty()
    {
        return Ok(value);
    }
    let read = |prompt: &str| {
        rpassword::prompt_password(prompt).map_err(|e| format!("can't read the password: {e}"))
    };
    let first = read(prompt)?;
    if confirm && read("Again: ")? != first {
        return Err("the passwords don't match".to_string());
    }
    Ok(first)
}

/// Unit tests for parsing grants and roles from the command line.
#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that an access suffix is split off only when it names an access.
    #[test]
    fn grants_parse_with_and_without_an_access() {
        assert_eq!(
            parse_grant("orders/#:publish", Access::Both),
            TopicGrant::new("orders/#", Access::Publish)
        );
        assert_eq!(
            parse_grant("news/+", Access::Subscribe),
            TopicGrant::new("news/+", Access::Subscribe)
        );
        assert_eq!(
            parse_grant("a:b", Access::Both),
            TopicGrant::new("a:b", Access::Both)
        );
    }

    /// Checks role defaults and that admins can't take grants.
    #[test]
    fn roles_build_from_arguments() {
        let readonly = role_spec(RoleArg::Readonly, &["metrics/#".to_string()]).expect("role");
        assert_eq!(readonly, RoleSpec::readonly(["metrics/#"]));
        assert!(role_spec(RoleArg::Admin, &["x".to_string()]).is_err());
        assert_eq!(grants(&RoleSpec::admin()), "*");
        assert_eq!(grants(&RoleSpec::user(Vec::new())), "-");
    }
}
