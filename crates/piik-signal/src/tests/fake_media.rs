//! Deterministic physical-media effects.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::runtime::lock;
use piik_sfu::runtime::{
    DeferredWork, IceCandidateInit, MediaError, PublicationMedia, ResourceFence, Runtime, SdpType,
    SessionDescription, SubscriptionFence, VoiceConnectionPlan, VoiceEventSink, VoiceRuntime,
    VoiceSignal,
};

/// A gate that blocks subscription closes until opened.
#[derive(Default)]
pub(crate) struct Barrier {
    open: Mutex<bool>,
    ready: Condvar,
}

impl Barrier {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn open(&self) {
        *lock(&self.open) = true;
        self.ready.notify_all();
    }

    pub(crate) fn wait(&self) {
        let mut open = lock(&self.open);
        while !*open {
            open = self
                .ready
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// `true` once opened within `timeout`.
    pub(crate) fn wait_timeout(&self, timeout: std::time::Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut open = lock(&self.open);
        while !*open {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            open = self
                .ready
                .wait_timeout(open, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

pub(crate) type ClosePublicationOverride =
    Arc<dyn Fn(&ResourceFence) -> Result<(), MediaError> + Send + Sync>;

#[derive(Default)]
struct State {
    publications: HashMap<ResourceFence, String>,
    created: Vec<ResourceFence>,
    deleted: Vec<ResourceFence>,
    drained: Vec<SubscriptionFence>,
    drain_attempts: Vec<SubscriptionFence>,
    fail_delete: bool,
    drain_barrier: Option<Arc<Barrier>>,
    delete_calls: usize,
    close_publication: Option<ClosePublicationOverride>,
    retire: bool,
    voice_enabled: bool,
    /// Offers with this SDP fail like an unparsable description.
    reject_offer_sdp: Option<String>,
    /// Candidates with this text are rejected as malformed.
    reject_candidate: Option<String>,
}

/// Records every physical effect and answers with fixed SDP.
#[derive(Default)]
pub(crate) struct FakeMedia {
    state: Mutex<State>,
}

impl FakeMedia {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A runtime that also provides room voice.
    pub(crate) fn with_voice() -> Arc<Self> {
        let media = Self::default();
        lock(&media.state).voice_enabled = true;
        Arc::new(media)
    }

    /// Supports detaching one video facet.
    pub(crate) fn set_retire(&self, retire: bool) {
        lock(&self.state).retire = retire;
    }

    /// Rejects offers carrying `sdp` and candidates carrying `candidate`.
    pub(crate) fn set_rejections(&self, sdp: Option<&str>, candidate: Option<&str>) {
        let mut state = lock(&self.state);
        state.reject_offer_sdp = sdp.map(str::to_owned);
        state.reject_candidate = candidate.map(str::to_owned);
    }

    fn check_candidate(&self, candidate: &IceCandidateInit) -> Result<(), MediaError> {
        if lock(&self.state).reject_candidate.as_deref() == Some(candidate.candidate.as_str()) {
            return Err(MediaError::CandidateRejected(
                "malformed candidate".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn set_fail_delete(&self, fail: bool) {
        lock(&self.state).fail_delete = fail;
    }

    pub(crate) fn set_subscription_drain_barrier(&self, barrier: Option<Arc<Barrier>>) {
        lock(&self.state).drain_barrier = barrier;
    }

    pub(crate) fn set_close_publication(&self, close: Option<ClosePublicationOverride>) {
        lock(&self.state).close_publication = close;
    }

    pub(crate) fn created(&self) -> Vec<ResourceFence> {
        lock(&self.state).created.clone()
    }

    pub(crate) fn deleted(&self) -> Vec<ResourceFence> {
        lock(&self.state).deleted.clone()
    }

    pub(crate) fn drained_subscriptions(&self) -> Vec<SubscriptionFence> {
        lock(&self.state).drained.clone()
    }

    pub(crate) fn subscription_drain_attempts(&self) -> Vec<SubscriptionFence> {
        lock(&self.state).drain_attempts.clone()
    }

    pub(crate) fn delete_calls(&self) -> usize {
        lock(&self.state).delete_calls
    }

    fn close_publication_inner(&self, fence: &ResourceFence) -> Result<(), MediaError> {
        let mut state = lock(&self.state);
        if state.fail_delete {
            return Err(MediaError::failed("test media close failed"));
        }
        state.publications.remove(fence);
        state.deleted.push(fence.clone());
        Ok(())
    }
}

fn description(sdp_type: SdpType, sdp: &str) -> SessionDescription {
    SessionDescription {
        sdp_type,
        sdp: sdp.to_owned(),
    }
}

impl Runtime for FakeMedia {
    fn has_publication(&self, fence: &ResourceFence, connection_id: &str) -> bool {
        !connection_id.is_empty()
            && lock(&self.state)
                .publications
                .get(fence)
                .is_some_and(|id| id == connection_id)
    }

    fn prepare_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        _: &PublicationMedia,
    ) -> Result<(), MediaError> {
        let mut state = lock(&self.state);
        state
            .publications
            .insert(fence.clone(), connection_id.to_owned());
        state.created.push(fence.clone());
        Ok(())
    }

    fn update_publication(
        &self,
        _: &ResourceFence,
        _: &str,
        _: &PublicationMedia,
    ) -> Result<(), MediaError> {
        Ok(())
    }

    fn publication_demand(&self, _: &ResourceFence, _: &str) -> Result<i64, MediaError> {
        Ok(1)
    }

    fn accept_publisher_offer(
        &self,
        _: &ResourceFence,
        _: &str,
        offer: SessionDescription,
    ) -> Result<SessionDescription, MediaError> {
        if lock(&self.state).reject_offer_sdp.as_deref() == Some(offer.sdp.as_str()) {
            return Err(MediaError::failed("invalid publisher offer"));
        }
        Ok(description(SdpType::Answer, "answer"))
    }

    fn add_publisher_ice(
        &self,
        _: &ResourceFence,
        _: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError> {
        self.check_candidate(&candidate)
    }

    fn prepare_subscriber(
        &self,
        _: Instant,
        _: &SubscriptionFence,
        _: &str,
    ) -> Result<SessionDescription, MediaError> {
        Ok(description(SdpType::Offer, "offer"))
    }

    fn apply_subscriber_answer(
        &self,
        _: &SubscriptionFence,
        _: &str,
        _: SessionDescription,
    ) -> Result<(), MediaError> {
        Ok(())
    }

    fn add_subscriber_ice(
        &self,
        _: &SubscriptionFence,
        _: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError> {
        self.check_candidate(&candidate)
    }

    fn restart_subscriber(
        &self,
        _: &SubscriptionFence,
        _: &str,
    ) -> Result<SessionDescription, MediaError> {
        Ok(description(SdpType::Offer, "restart"))
    }

    fn close_subscription(&self, fence: &SubscriptionFence, _: &str) -> Result<(), MediaError> {
        let barrier = {
            let mut state = lock(&self.state);
            state.drain_attempts.push(fence.clone());
            state.drain_barrier.clone()
        };
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        lock(&self.state).drained.push(fence.clone());
        Ok(())
    }

    fn close_publication(&self, fence: &ResourceFence) -> Result<(), MediaError> {
        let close = {
            let mut state = lock(&self.state);
            state.delete_calls += 1;
            state.close_publication.clone()
        };
        match close {
            Some(close) => close(fence),
            None => self.close_publication_inner(fence),
        }
    }

    fn supports_retire_publication(&self) -> bool {
        lock(&self.state).retire
    }

    /// Detaches the exact handle at once (it stops being visible) and returns
    /// its physical close, which never touches a replacement.
    fn retire_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Option<DeferredWork> {
        let mut state = lock(&self.state);
        if state
            .publications
            .get(fence)
            .is_none_or(|id| id != connection_id)
        {
            return None;
        }
        state.publications.remove(fence);
        Some(Box::new(|| Ok(())))
    }

    fn voice(&self) -> Option<&dyn VoiceRuntime> {
        lock(&self.state)
            .voice_enabled
            .then_some(self as &dyn VoiceRuntime)
    }
}

impl FakeMedia {
    /// The default close used by tests that override `close_publication`.
    pub(crate) fn real_close_publication(&self, fence: &ResourceFence) -> Result<(), MediaError> {
        self.close_publication_inner(fence)
    }

    /// Closes only the exact physical identity, as a late retired close does.
    pub(crate) fn close_exact(&self, fence: &ResourceFence, connection_id: &str) {
        let mut state = lock(&self.state);
        if state
            .publications
            .get(fence)
            .is_some_and(|id| id == connection_id)
        {
            state.publications.remove(fence);
        }
    }
}

/// Delegates to [`FakeMedia`] and lets a test hold chosen effects.
pub(crate) struct HookedMedia {
    pub inner: Arc<FakeMedia>,
    /// Held before creating this connection's publication.
    pub hold_create: Option<(String, Arc<Barrier>, Arc<Barrier>)>,
    /// Held before answering this connection's offer.
    pub hold_accept: Option<(String, Arc<Barrier>, Arc<Barrier>)>,
    /// Held before running a retired publication's physical close.
    pub hold_retire: Option<(Arc<Barrier>, Arc<Barrier>)>,
    /// The last retired identity.
    pub retired: Mutex<Option<(ResourceFence, String)>>,
    /// A test-controlled room-voice runtime.
    pub voice: Option<Arc<dyn VoiceRuntime>>,
}

impl HookedMedia {
    pub(crate) fn new(inner: Arc<FakeMedia>) -> Self {
        Self {
            inner,
            hold_create: None,
            hold_accept: None,
            hold_retire: None,
            retired: Mutex::new(None),
            voice: None,
        }
    }
}

fn hold(gate: &Option<(String, Arc<Barrier>, Arc<Barrier>)>, connection_id: &str) {
    if let Some((held, entered, release)) = gate
        && held == connection_id
    {
        entered.open();
        release.wait();
    }
}

impl Runtime for HookedMedia {
    fn has_publication(&self, fence: &ResourceFence, connection_id: &str) -> bool {
        self.inner.has_publication(fence, connection_id)
    }
    fn prepare_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        media: &PublicationMedia,
    ) -> Result<(), MediaError> {
        hold(&self.hold_create, connection_id);
        self.inner.prepare_publication(fence, connection_id, media)
    }
    fn update_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        media: &PublicationMedia,
    ) -> Result<(), MediaError> {
        self.inner.update_publication(fence, connection_id, media)
    }
    fn publication_demand(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Result<i64, MediaError> {
        self.inner.publication_demand(fence, connection_id)
    }
    fn accept_publisher_offer(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        offer: SessionDescription,
    ) -> Result<SessionDescription, MediaError> {
        hold(&self.hold_accept, connection_id);
        self.inner
            .accept_publisher_offer(fence, connection_id, offer)
    }
    fn add_publisher_ice(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError> {
        self.inner
            .add_publisher_ice(fence, connection_id, candidate)
    }
    fn prepare_subscriber(
        &self,
        deadline: Instant,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<SessionDescription, MediaError> {
        self.inner
            .prepare_subscriber(deadline, fence, connection_id)
    }
    fn apply_subscriber_answer(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
        answer: SessionDescription,
    ) -> Result<(), MediaError> {
        self.inner
            .apply_subscriber_answer(fence, connection_id, answer)
    }
    fn add_subscriber_ice(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError> {
        self.inner
            .add_subscriber_ice(fence, connection_id, candidate)
    }
    fn restart_subscriber(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<SessionDescription, MediaError> {
        self.inner.restart_subscriber(fence, connection_id)
    }
    fn close_subscription(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<(), MediaError> {
        self.inner.close_subscription(fence, connection_id)
    }
    fn close_publication(&self, fence: &ResourceFence) -> Result<(), MediaError> {
        self.inner.close_publication(fence)
    }
    fn supports_retire_publication(&self) -> bool {
        true
    }
    fn retire_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Option<DeferredWork> {
        let work = self.inner.retire_publication(fence, connection_id)?;
        *lock(&self.retired) = Some((fence.clone(), connection_id.to_owned()));
        let Some((started, release)) = self.hold_retire.clone() else {
            return Some(work);
        };
        Some(Box::new(move || {
            started.open();
            release.wait();
            work()
        }))
    }
    fn voice(&self) -> Option<&dyn VoiceRuntime> {
        self.voice.as_deref()
    }
}

impl VoiceRuntime for FakeMedia {
    /// Commits plans with no SDP work, as `sfutest.FakeMedia` does.
    fn configure_voice(
        &self,
        _: VoiceConnectionPlan,
        _: VoiceEventSink,
    ) -> Result<Option<DeferredWork>, MediaError> {
        Ok(None)
    }

    fn signal_voice(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: VoiceSignal,
    ) -> Result<(), MediaError> {
        Ok(())
    }

    fn close_voice(&self, _: &str, _: &str, _: &str, _: &str) -> Option<DeferredWork> {
        None
    }
}
