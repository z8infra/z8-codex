//! In-memory state for an account login that is waiting for a second factor.
//!
//! The temporary token returned by the password step never crosses the Tauri
//! boundary and is never persisted.  This registry also owns the operation
//! generation, so starting another login or cancelling the pending login
//! invalidates the old challenge before it can commit a session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use codex_plus_core::z8_account::{AccountOperation, TwoFactorChallenge};

pub(crate) const TWO_FACTOR_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PendingLoginStatus {
    pub(crate) pending_two_factor: bool,
    pub(crate) pending_email: Option<String>,
}

pub(crate) struct PendingLoginHandle {
    pub(crate) id: u64,
    pub(crate) challenge: TwoFactorChallenge,
    pub(crate) operation: Arc<AccountOperation>,
}

struct PendingLogin {
    id: u64,
    challenge: Option<TwoFactorChallenge>,
    email: String,
    operation: Arc<AccountOperation>,
    created_at: Instant,
    in_flight: bool,
}

#[derive(Default)]
pub(crate) struct PendingLoginRegistry {
    next_id: u64,
    pending: Option<PendingLogin>,
}

impl PendingLoginRegistry {
    pub(crate) fn begin(
        &mut self,
        challenge: TwoFactorChallenge,
        operation: Arc<AccountOperation>,
        now: Instant,
    ) -> PendingLoginStatus {
        let id = self.allocate_id();
        self.replace(PendingLogin {
            id,
            email: challenge.email.clone(),
            challenge: Some(challenge),
            operation,
            created_at: now,
            in_flight: false,
        });
        self.status(now)
    }

    pub(crate) fn status(&mut self, now: Instant) -> PendingLoginStatus {
        self.expire_if_needed(now);
        self.pending
            .as_ref()
            .map(|pending| PendingLoginStatus {
                pending_two_factor: true,
                pending_email: Some(pending.email.clone()),
            })
            .unwrap_or_default()
    }

    pub(crate) fn handle(&mut self, now: Instant) -> Option<PendingLoginHandle> {
        self.expire_if_needed(now);
        let pending = self.pending.as_mut()?;
        if pending.in_flight {
            return None;
        }
        let challenge = pending.challenge.as_ref()?.clone();
        pending.in_flight = true;
        Some(PendingLoginHandle {
            id: pending.id,
            challenge,
            operation: Arc::clone(&pending.operation),
        })
    }

    pub(crate) fn release(&mut self, id: u64) {
        if let Some(pending) = self.pending.as_mut().filter(|pending| pending.id == id) {
            pending.in_flight = false;
        }
    }

    pub(crate) fn finish(&mut self, id: u64) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            self.pending = None;
        }
    }

    pub(crate) fn cancel(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        pending.operation.cancel();
        true
    }

    fn replace(&mut self, next: PendingLogin) {
        if let Some(previous) = self.pending.take() {
            previous.operation.cancel();
        }
        self.pending = Some(next);
    }

    fn allocate_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    fn expire_if_needed(&mut self, now: Instant) {
        let expired = self.pending.as_ref().is_some_and(|pending| {
            now.checked_duration_since(pending.created_at)
                .is_some_and(|age| age >= TWO_FACTOR_TTL)
        });
        if expired {
            let _ = self.cancel();
        }
    }

    #[cfg(test)]
    fn begin_for_test(&mut self, email: &str, operation: Arc<AccountOperation>, now: Instant) {
        let id = self.allocate_id();
        self.replace(PendingLogin {
            id,
            challenge: None,
            email: email.to_string(),
            operation,
            created_at: now,
            in_flight: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_plus_core::z8_account::AccountSessionStore;

    #[test]
    fn pending_status_only_contains_safe_metadata() {
        let sessions = AccountSessionStore::new();
        let operation = Arc::new(sessions.begin());
        let now = Instant::now();
        let mut registry = PendingLoginRegistry::default();
        registry.begin_for_test("user@example.com", operation, now);

        assert_eq!(
            registry.status(now),
            PendingLoginStatus {
                pending_two_factor: true,
                pending_email: Some("user@example.com".to_string()),
            }
        );
    }

    #[test]
    fn cancel_invalidates_pending_operation() {
        let sessions = AccountSessionStore::new();
        let operation = Arc::new(sessions.begin());
        let cancellation = operation.cancellation();
        let now = Instant::now();
        let mut registry = PendingLoginRegistry::default();
        registry.begin_for_test("user@example.com", operation, now);

        assert!(registry.cancel());
        assert!(cancellation.is_cancelled());
        assert_eq!(registry.status(now), PendingLoginStatus::default());
    }

    #[test]
    fn newer_login_replaces_and_invalidates_old_challenge() {
        let sessions = AccountSessionStore::new();
        let old_operation = Arc::new(sessions.begin());
        let old_cancellation = old_operation.cancellation();
        let new_operation = Arc::new(sessions.begin());
        let now = Instant::now();
        let mut registry = PendingLoginRegistry::default();
        registry.begin_for_test("old@example.com", old_operation, now);
        registry.begin_for_test("new@example.com", new_operation, now);

        assert!(old_cancellation.is_cancelled());
        assert_eq!(
            registry.status(now).pending_email.as_deref(),
            Some("new@example.com")
        );
    }

    #[test]
    fn expired_challenge_is_cancelled_and_removed() {
        let sessions = AccountSessionStore::new();
        let operation = Arc::new(sessions.begin());
        let cancellation = operation.cancellation();
        let created = Instant::now();
        let expired = created + TWO_FACTOR_TTL;
        let mut registry = PendingLoginRegistry::default();
        registry.begin_for_test("user@example.com", operation, created);

        assert_eq!(registry.status(expired), PendingLoginStatus::default());
        assert!(cancellation.is_cancelled());
    }
}
