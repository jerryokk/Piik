//! Viewer password derivation behind a bounded FIFO admission gate.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use piik_protocol::valid_viewer_password;

use crate::error::{Error, ErrorCode, invalid};
use crate::store::{RandomSource, RoomRef};

pub(crate) const VIEWER_PASSWORD_SALT_BYTES: usize = 16;
pub(crate) const VIEWER_PASSWORD_VERIFIER_BYTES: usize = 32;
pub(crate) const VIEWER_PASSWORD_MATERIAL_BYTES: usize =
    VIEWER_PASSWORD_SALT_BYTES + VIEWER_PASSWORD_VERIFIER_BYTES;

/// log2 of scrypt N = 16 384, with r = 8 and p = 1.
const VIEWER_PASSWORD_SCRYPT_LOG_N: u8 = 14;
const VIEWER_PASSWORD_SCRYPT_R: u32 = 8;
const VIEWER_PASSWORD_SCRYPT_P: u32 = 1;

// Scrypt at N=16384 costs ~16 MiB per derivation, so two concurrent
// derivations bound the memory and CPU a room burst can claim, and sixteen
// waiters bound the queue before the next caller is told ROOM_BUSY.
const GATE_ACTIVE_LIMIT: usize = 2;
const GATE_PENDING_LIMIT: usize = 16;

/// The result of an optional password derivation handed unchanged to
/// [`crate::Store::create_room`], [`crate::Store::replace_room`] or
/// [`crate::Store::set_viewer_password`]: `Ok(None)` when no password was
/// given, `Ok(Some(salt || verifier))` after a derivation, or its error.
pub type DerivedMaterial = Result<Option<Vec<u8>>, Error>;

/// The locked first half of password admission: the material to compare with
/// (48 zero bytes for a missing room or a room without a private password) and
/// the room reference to recheck afterwards.
#[derive(Debug, Clone)]
pub struct PasswordChallenge {
    pub(crate) expected_material: Vec<u8>,
    pub(crate) room: Option<RoomRef>,
}

impl PasswordChallenge {
    /// The salt to derive against.
    pub fn salt(&self) -> &[u8] {
        &self.expected_material[..VIEWER_PASSWORD_SALT_BYTES]
    }
}

/// Runs scrypt outside the store lock. It is shared by every room of one
/// store, so the gate bounds work across the whole store. Calls block while
/// queued and while deriving; async callers use a blocking thread.
pub struct PasswordDeriver {
    random: RandomSource,
    gate: Gate,
}

impl PasswordDeriver {
    pub(crate) fn new(random: RandomSource) -> Self {
        Self {
            random,
            gate: Gate::new(GATE_ACTIVE_LIMIT, GATE_PENDING_LIMIT),
        }
    }

    /// Validates the password, draws a fresh salt and returns salt||verifier.
    /// Pass the result (wrapped in `Some`) to the commit unchanged so it can
    /// recheck authority before reporting busy/cancelled admission. `None`
    /// for `may_start` permits derivation once the gate admits it.
    pub fn derive_viewer_password_material(
        &self,
        password: &str,
        may_start: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<u8>, Error> {
        if !valid_viewer_password(password) {
            return Err(ErrorCode::InvalidToken.into());
        }
        let mut material = (self.random)(VIEWER_PASSWORD_SALT_BYTES);
        if material.len() != VIEWER_PASSWORD_SALT_BYTES {
            return Err(invalid("Viewer password salt source must return 16 bytes"));
        }
        let verifier = self.derive(password, &material, may_start)?;
        material.extend_from_slice(&verifier);
        Ok(material)
    }

    /// Derives the verifier for the challenge salt. Gate failures return
    /// before the room commit; cancelled admission is an invalid token.
    pub fn derive_viewer_password(
        &self,
        password: &str,
        salt: &[u8],
        may_connect: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<u8>, Error> {
        if !valid_viewer_password(password) {
            return Err(ErrorCode::InvalidToken.into());
        }
        match self.derive(password, salt, may_connect) {
            Err(Error::DerivationCancelled) => Err(ErrorCode::InvalidToken.into()),
            result => result,
        }
    }

    fn derive(
        &self,
        password: &str,
        salt: &[u8],
        may_start: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<u8>, Error> {
        self.gate.run(may_start, || {
            let params = scrypt::Params::new(
                VIEWER_PASSWORD_SCRYPT_LOG_N,
                VIEWER_PASSWORD_SCRYPT_R,
                VIEWER_PASSWORD_SCRYPT_P,
            )
            .map_err(|_| invalid("Viewer password scrypt parameters are invalid"))?;
            let mut verifier = vec![0; VIEWER_PASSWORD_VERIFIER_BYTES];
            scrypt::scrypt(password.as_bytes(), salt, &params, &mut verifier)
                .map_err(|_| invalid("Viewer password verifier length is invalid"))?;
            Ok(verifier)
        })
    }

    /// Reports (active, pending) gate counters for tests.
    #[cfg(test)]
    pub(crate) fn gate_state(&self) -> (usize, usize) {
        let state = self.gate.lock();
        (state.active, state.pending)
    }
}

/// Admits at most `limit` concurrent derivations and at most `pending_limit`
/// waiters, first in first out. A saturated gate answers ROOM_BUSY.
struct Gate {
    state: Mutex<GateState>,
    wake: Condvar,
    limit: usize,
    pending_limit: usize,
}

struct GateState {
    active: usize,
    pending: usize,
    /// The ticket the next queued caller receives.
    next_ticket: u64,
    /// Tickets below this value have been handed an active slot.
    admitted: u64,
}

impl Gate {
    fn new(limit: usize, pending_limit: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                active: 0,
                pending: 0,
                next_ticket: 0,
                admitted: 0,
            }),
            wake: Condvar::new(),
            limit,
            pending_limit,
        }
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        // The counters stay consistent across a panicking task because
        // release runs from a drop guard.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(
        &self,
        may_start: Option<&dyn Fn() -> bool>,
        task: impl FnOnce() -> Result<Vec<u8>, Error>,
    ) -> Result<Vec<u8>, Error> {
        if !self.acquire() {
            return Err(ErrorCode::RoomBusy.into());
        }
        let _slot = Slot(self);
        if may_start.is_some_and(|may_start| !may_start()) {
            return Err(Error::DerivationCancelled);
        }
        task()
    }

    fn acquire(&self) -> bool {
        let mut state = self.lock();
        if state.active < self.limit {
            state.active += 1;
            return true;
        }
        if state.pending >= self.pending_limit {
            return false;
        }
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.pending += 1;
        while ticket >= state.admitted {
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        true
    }

    /// Transfers the active slot to the first waiter, keeping new callers
    /// behind already queued derivations.
    fn release(&self) {
        let mut state = self.lock();
        if state.pending > 0 {
            state.pending -= 1;
            state.admitted += 1;
            drop(state);
            self.wake.notify_all();
            return;
        }
        state.active -= 1;
    }
}

struct Slot<'a>(&'a Gate);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Shared handle type the store hands to callers.
pub(crate) type SharedDeriver = Arc<PasswordDeriver>;
