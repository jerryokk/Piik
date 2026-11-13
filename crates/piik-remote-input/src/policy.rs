//! Host authorization policy and its prepared changes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::manager::{Inner, SessionId, Shared};

const MAX_PASSWORD_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Ask,
    Room,
    Password,
}

impl Mode {
    pub fn parse(mode: &str) -> Option<Self> {
        Some(match mode {
            "off" => Self::Off,
            "ask" => Self::Ask,
            "room" => Self::Room,
            "password" => Self::Password,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ask => "ask",
            Self::Room => "room",
            Self::Password => "password",
        }
    }
}

fn validate_policy(mode: &str, password: &str) -> Result<Mode, Error> {
    let mode = Mode::parse(mode).ok_or(Error::InvalidPolicy)?;
    // Password mode requires a password; every other mode forbids one.
    if password.len() > MAX_PASSWORD_BYTES || (mode == Mode::Password) == password.is_empty() {
        return Err(Error::InvalidPolicy);
    }
    Ok(mode)
}

/// One Host intent from dispatch through preparation. It never owns the
/// lifetime of an installed capture stream. Dropping it cancels the intent
/// if it is still current.
pub struct PolicyChange {
    shared: Arc<Shared>,
    session: SessionId,
    id: u64,
    token: CancellationToken,
    mode: Mode,
    password: [u8; 32],
    co_play: bool,
}

impl PolicyChange {
    pub(crate) fn parts(&self) -> (&Arc<Shared>, SessionId, u64) {
        (&self.shared, self.session, self.id)
    }

    /// Cancelled when a newer Host intent, source change, pause or close
    /// retires this change.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Commits the prepared policy if its source and intent are still current.
    pub fn apply(&self) -> Result<(), Error> {
        if self.token.is_cancelled() {
            return Err(Error::Canceled);
        }
        let mut inner = self.shared.lock();
        let current = inner
            .sessions
            .get(&self.session)
            .and_then(|s| s.policy_change.as_ref())
            .is_some_and(|(id, _)| *id == self.id);
        if inner.closed
            || inner.session_closed(self.session)
            || !current
            || self.token.is_cancelled()
            || (self.mode != Mode::Off && !inner.available(self.session))
        {
            return Err(Error::Unavailable);
        }
        if self.mode != Mode::Off {
            inner.ensure_stop_shortcut()?;
        }
        if self.co_play && inner.gamepads.is_none() {
            inner.gamepads = (inner.open_gamepads)();
        }
        inner.cancel_policy(self.session);
        inner.apply_policy(self.session, self.mode, self.password, self.co_play);
        Ok(())
    }
}

impl Drop for PolicyChange {
    fn drop(&mut self) {
        let mut inner = self.shared.lock();
        let current = inner
            .sessions
            .get(&self.session)
            .and_then(|s| s.policy_change.as_ref())
            .is_some_and(|(id, _)| *id == self.id);
        if current {
            inner.cancel_policy(self.session);
        }
    }
}

pub(crate) fn begin_policy(
    shared: &Arc<Shared>,
    session: SessionId,
    parent: &CancellationToken,
    mode: &str,
    password: &str,
    co_play: bool,
) -> Result<PolicyChange, Error> {
    let mode = validate_policy(mode, password)?;
    let mut inner = shared.lock();
    if inner.closed
        || inner.session_closed(session)
        || parent.is_cancelled()
        || (mode != Mode::Off && !inner.available(session))
    {
        return Err(Error::Unavailable);
    }
    inner.cancel_policy(session);
    let id = inner.next_id();
    let token = parent.child_token();
    let password = if mode == Mode::Password {
        Sha256::digest(password.as_bytes()).into()
    } else {
        [0; 32]
    };
    if let Some(s) = inner.sessions.get_mut(&session) {
        s.policy_change = Some((id, token.clone()));
    }
    Ok(PolicyChange {
        shared: shared.clone(),
        session,
        id,
        token,
        mode,
        password,
        co_play: co_play && mode != Mode::Off,
    })
}

impl Inner {
    /// Cancellation invalidates preparation before it can commit a newer
    /// permission, even if an OS dialog itself cannot be dismissed.
    pub fn cancel_policy(&mut self, session: SessionId) {
        if let Some((_, token)) = self
            .sessions
            .get_mut(&session)
            .and_then(|s| s.policy_change.take())
        {
            token.cancel();
        }
    }

    /// A policy change retires all previous requests and grants, so an old
    /// approval cannot acquire the newly configured permission.
    pub fn set_policy(&mut self, session: SessionId, mode: Mode, password: [u8; 32]) {
        self.cancel_policy(session);
        let co_play = self.co_play(session);
        self.apply_policy(session, mode, password, co_play);
    }

    pub fn apply_policy(
        &mut self,
        session: SessionId,
        mode: Mode,
        password: [u8; 32],
        co_play: bool,
    ) {
        if let Some(s) = self.sessions.get_mut(&session) {
            s.mode = mode;
            s.password = password;
        }
        if self.owner == Some(session) {
            self.release();
        }
        if let Some(s) = self.sessions.get_mut(&session) {
            s.co_play = co_play;
        }
        self.release_idle_shortcut();
        for key in self.session_keys(session) {
            self.revoke(&key);
        }
        self.notify(session);
    }

    /// One password attempt per second for this source, shared across
    /// endpoints and route changes. The verifier is never logged or persisted.
    /// Returns the rejection reason.
    pub fn authorize(&mut self, session: SessionId, password: &str) -> Option<&'static str> {
        let s = self.sessions.get_mut(&session)?;
        if s.mode != Mode::Password {
            return None;
        }
        let now = Instant::now();
        if s.password_retry.is_some_and(|retry| now < retry) {
            return Some("retry");
        }
        s.password_retry = Some(now + Duration::from_secs(1));
        let digest: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        if password.len() > MAX_PASSWORD_BYTES || !bool::from(digest.ct_eq(&s.password)) {
            return Some("password");
        }
        None
    }

    /// Concurrent control changes the current source's grant policy,
    /// independently of its mode and password. Previous consent is retired.
    pub fn set_simultaneous(&mut self, session: SessionId, enabled: bool) -> Result<(), Error> {
        let usable = self
            .sessions
            .get(&session)
            .is_some_and(|s| s.co_play && s.mode != Mode::Off);
        if self.closed || self.session_closed(session) || !usable || !self.available(session) {
            return Err(Error::Unavailable);
        }
        let s = self.sessions.get_mut(&session).expect("open session");
        if s.simultaneous == enabled {
            return Ok(());
        }
        s.simultaneous = enabled;
        let (mode, password, co_play) = (s.mode, s.password, s.co_play);
        self.cancel_policy(session);
        self.apply_policy(session, mode, password, co_play);
        Ok(())
    }
}
