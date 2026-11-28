//! The limit on how many topics one account may create, checked on every
//! publish.
//!
//! A topic belongs to the account whose publish created it, and the broker
//! keeps the counts (`SharedBroker::claim_topic`). This module decides what a
//! publish gets: through, `topic_limit_reached`, or `storage_unavailable`
//! when the counts can't be read. Admins aren't limited.

use popsub_broker::TopicClaim;
use popsub_protocol::ErrorCode;
use popsub_utils::pii::pii;
use tracing::{error, info};

use super::Session;

impl Session {
    /// Decides whether a publish to a topic is within the account's topic
    /// limit, counting the topic against the account when the publish
    /// creates it (`SharedBroker::claim_topic`).
    ///
    /// The last topic found to exist is remembered, so a connection
    /// publishing to one topic doesn't take the count's lock for every
    /// message. Admins aren't limited: the account is only read when the
    /// limit is reached, to tell an admin from anyone else.
    ///
    /// Args:
    ///     username: The account this connection authenticated as.
    ///     topic: The topic to publish to.
    ///
    /// Returns:
    ///     Nothing when the publish may go ahead.
    ///
    /// Raises:
    ///     (ErrorCode, String): `topic_limit_reached` when the topic is new
    ///         and the account is at its limit, or `storage_unavailable` when
    ///         the topic counts can't be read from storage.
    pub(super) async fn may_create(
        &self,
        username: &str,
        topic: &str,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        if self.auth_state().claimed_topic.as_deref() == Some(topic) {
            return Ok(());
        }
        let unavailable = |e: popsub_storage::StorageError| {
            error!(client_id = %self.client_id, topic, %e, "reading topic counts failed");
            (
                ErrorCode::StorageUnavailable,
                "the topic counts couldn't be read; try again".to_string(),
            )
        };
        let broker = &self.server.broker;
        let mut claim = broker
            .claim_topic(topic, username, self.server.max_topics_per_user)
            .await
            .map_err(unavailable)?;
        if let TopicClaim::LimitReached { limit } = claim {
            let is_admin = matches!(
                self.server.auth.get_user(username).await,
                Ok(Some(user)) if user.enabled && user.is_admin()
            );
            if !is_admin {
                return Err((
                    ErrorCode::TopicLimitReached,
                    format!(
                        "this account has created {limit} topics, the most it may; an admin can raise limits.max_topics_per_user"
                    ),
                ));
            }
            claim = broker
                .claim_topic(topic, username, None)
                .await
                .map_err(unavailable)?;
        }
        if claim == TopicClaim::Created {
            info!(client_id = %self.client_id, username = %pii(username), topic, "topic created");
        }
        self.auth_state().claimed_topic = Some(topic.to_string());
        Ok(())
    }
}
