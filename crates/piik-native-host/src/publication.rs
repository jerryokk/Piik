//! SFU publications of one Host share: at most two, identified by publication
//! generation and connection.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use futures_util::future::BoxFuture;
#[cfg(test)]
use piik_media_edge::PcState;
use piik_media_edge::{EdgeEvent, Publication};
use piik_native_control::media::{
    self as control, MediaError, MediaEventKind, MediaResult, PublicationQualitySample,
};
use piik_native_control::wire::IceCandidateInit as WireCandidate;

use crate::bridge::{self, EdgeFact};
use crate::session::{InputLeg, OwnedPublication, PublicationKey, Session, owns_publication};

const MAX_PUBLICATIONS: usize = 2;

/// One publication and the media descriptor control reads synchronously.
/// The descriptor is refreshed after every change the Host makes and with
/// each quality sample.
pub struct HostPublication {
    publication: Publication,
    media: Mutex<control::PublicationMedia>,
}

impl HostPublication {
    pub(crate) async fn refresh_media(&self) {
        if let Some(media) = self.publication.media().await {
            *self.media.lock().unwrap_or_else(|error| error.into_inner()) =
                bridge::publication_media(media);
        }
    }

    pub(crate) async fn quality_sample(&self, now: SystemTime) -> Option<PublicationQualitySample> {
        self.publication
            .quality_sample(now)
            .await
            .map(bridge::publication_quality_sample)
    }

    pub(crate) async fn close(&self) {
        self.publication.close().await;
    }
}

impl control::Publication for HostPublication {
    fn media(&self) -> control::PublicationMedia {
        self.media
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn set_answer<'a>(&'a self, sdp: &'a str) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(async move {
            self.publication
                .set_answer(sdp)
                .await
                .map_err(bridge::media_error)
        })
    }

    fn add_remote_candidate(
        &self,
        candidate: Option<WireCandidate>,
    ) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async move {
            self.publication
                .add_remote_candidate(candidate.map(bridge::edge_candidate))
                .await
                .map_err(bridge::media_error)
        })
    }

    fn set_active_count(&self, count: usize) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async move {
            self.publication
                .set_active_count(count)
                .await
                .map_err(bridge::media_error)?;
            self.refresh_media().await;
            Ok(())
        })
    }
}

fn unavailable() -> MediaError {
    MediaError::failed("native publication is unavailable")
}

impl Session {
    pub async fn prepare_publication(
        &self,
        generation: &str,
        connection_id: &str,
        ice_servers: Vec<piik_media_edge::IceServerUrls>,
    ) -> MediaResult<(String, control::PublicationMedia)> {
        let inner = &self.inner;
        let key: PublicationKey = (generation.to_owned(), connection_id.to_owned());
        let (source, audio) = {
            let state = inner.lock();
            if state.closed
                || state.publications.len() >= MAX_PUBLICATIONS
                || state.publications.contains_key(&key)
            {
                return Err(unavailable());
            }
            let Some(source) = state.source.clone() else {
                return Err(unavailable());
            };
            (source, state.audio_source.clone())
        };
        let token = Arc::new(());
        let weak = self.weak();
        let owns = {
            let (weak, key, token) = (weak.clone(), key.clone(), Arc::downgrade(&token));
            Arc::new(move || owns_publication(&weak, &key, &token))
        };
        let input = InputLeg::default();
        let sink: piik_media_edge::EventSink = {
            let (weak, generation, connection_id) = (
                weak.clone(),
                generation.to_owned(),
                connection_id.to_owned(),
            );
            Arc::new(move |event: EdgeEvent| {
                let Some(inner) = weak.upgrade() else { return };
                let fact = bridge::edge_fact(&event);
                let owns = owns.clone();
                match fact {
                    Some(EdgeFact::Candidate(candidate, current)) => inner.emit(
                        MediaEventKind::PublicationCandidate {
                            publication_generation: generation.clone(),
                            connection_id: connection_id.clone(),
                            candidate,
                        },
                        Some(Arc::new(move || owns() && current())),
                    ),
                    Some(EdgeFact::State(state, watch)) => inner.emit(
                        MediaEventKind::PublicationState {
                            publication_generation: generation.clone(),
                            connection_id: connection_id.clone(),
                            state: state.as_str().to_owned(),
                        },
                        Some(Arc::new(move || owns() && watch.is(state))),
                    ),
                    // Publications report no selected path.
                    Some(EdgeFact::Path(..)) => {}
                    None => {
                        if let Some(session) = &inner.remote_input {
                            input.handle(session, &connection_id, event);
                        }
                    }
                }
            })
        };
        let publication = inner
            .engine
            .new_publication(
                &source,
                piik_media_edge::EdgeOptions {
                    connection_id: connection_id.to_owned(),
                    ice_servers,
                    audio,
                    local: false,
                    punched: false,
                    events: Some(sink),
                },
            )
            .await
            .map_err(bridge::media_error)?;
        let host = Arc::new(HostPublication {
            publication,
            media: Mutex::new(control::PublicationMedia::default()),
        });
        let installed = {
            let mut state = inner.lock();
            let free = !state.closed
                && state.publications.len() < MAX_PUBLICATIONS
                && !state.publications.contains_key(&key);
            if free {
                state.publications.insert(
                    key,
                    OwnedPublication {
                        token,
                        publication: host.clone(),
                    },
                );
            }
            free
        };
        if !installed {
            host.close().await;
            return Err(MediaError::failed("native publication changed"));
        }
        if inner.remote_input.is_some() {
            let _ = host
                .publication
                .open_data_channel(piik_remote_input::CHANNEL_LABEL)
                .await;
        }
        let offer = match host.publication.create_offer().await {
            Ok(offer) => offer,
            Err(error) => {
                self.close_publication(generation, connection_id).await;
                return Err(bridge::media_error(error));
            }
        };
        host.refresh_media().await;
        Ok((offer, control::Publication::media(host.as_ref())))
    }

    pub fn publication(
        &self,
        generation: &str,
        connection_id: &str,
    ) -> MediaResult<Arc<HostPublication>> {
        let state = self.inner.lock();
        let key = (generation.to_owned(), connection_id.to_owned());
        match state.publications.get(&key) {
            Some(owned) if !state.closed => Ok(owned.publication.clone()),
            _ => Err(MediaError::failed("native publication identity changed")),
        }
    }

    pub async fn close_publication(&self, generation: &str, connection_id: &str) {
        let key = (generation.to_owned(), connection_id.to_owned());
        let owned = self.inner.lock().publications.remove(&key);
        if let Some(owned) = owned {
            owned.publication.close().await;
        }
    }

    #[cfg(test)]
    pub(crate) fn publication_state(
        &self,
        generation: &str,
        connection_id: &str,
    ) -> Option<PcState> {
        let key = (generation.to_owned(), connection_id.to_owned());
        let state = self.inner.lock();
        state
            .publications
            .get(&key)
            .map(|owned| owned.publication.publication.state())
    }
}
