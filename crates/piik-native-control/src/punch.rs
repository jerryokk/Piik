//! Connection preparation (UDP hole punching) commands.
//!
//! Response workers are bounded as well as socket work: one punch slot,
//! independent of capture mutations, is held through response delivery and
//! released before its ACK is written. close-punch remains synchronous.

use std::sync::Arc;

use piik_loopback::ControlEvent;
use serde_json::json;

use crate::handle::prediction_value;
use crate::media::{PunchPreparation, punch_target};
use crate::session::{Fail, Inner, Request, SlotKind, fail};
use crate::wire::{
    ConnectionRequest, MAX_EDGE_CAPACITY, PreparePunchRequest, RunPunchRequest, decode_strict,
    ice_servers, valid_identities, valid_identity,
};

impl Inner {
    pub(crate) async fn handle_punch(
        self: &Arc<Self>,
        request: &Request,
        payload: &[u8],
    ) -> Result<Option<ControlEvent>, Fail> {
        match request.kind.as_str() {
            "prepare-punch" => self.prepare_punch(request, payload).await,
            "run-punch" => self.run_punch(request, payload),
            _ => {
                let close: ConnectionRequest = decode_strict(payload)
                    .filter(|close: &ConnectionRequest| {
                        valid_identities(&[&close.share_id, &close.connection_id])
                    })
                    .ok_or(Fail::Violation("native connection retirement is invalid"))?;
                if let Some(media) = self.current_media(&close.share_id) {
                    media.close_punch(&close.connection_id);
                }
                Ok(Some(request.ack("punch-closed")))
            }
        }
    }

    async fn prepare_punch(
        self: &Arc<Self>,
        request: &Request,
        payload: &[u8],
    ) -> Result<Option<ControlEvent>, Fail> {
        const INVALID: &str = "native connection preparation is invalid";
        let prepare: PreparePunchRequest =
            decode_strict(payload).ok_or(Fail::Violation(INVALID))?;
        let relayed = !prepare.source_connection_id.is_empty();
        if !valid_identities(&[&prepare.share_id, &prepare.connection_id])
            || (relayed && !valid_identity(&prepare.source_connection_id))
            || prepare
                .edge_capacity
                .is_some_and(|capacity| !(1..=MAX_EDGE_CAPACITY).contains(&capacity) || relayed)
        {
            return Err(Fail::Violation(INVALID));
        }
        if hex::decode(&prepare.secret).map_or(true, |key| key.len() != 32) {
            return Err(Fail::Violation(
                "native connection preparation key is invalid",
            ));
        }
        let servers = ice_servers(&prepare.ice_servers).map_err(Fail::Violation)?;
        let slot = {
            let mut state = self.lock();
            self.begin(&mut state, SlotKind::Punch)
        }
        .ok_or_else(|| fail("connection preparation is unavailable or already active"))?;
        let preparation = self.begin_preparation(&prepare, servers).await;
        let preparation = match preparation {
            Ok(preparation) => preparation,
            Err(error) => {
                self.abandon(SlotKind::Punch, &slot);
                return Err(error);
            }
        };
        // Reserved before launching work, so a subsequent close cannot be
        // undone by a late worker.
        let inner = self.clone();
        let request = request.clone();
        tokio::spawn(async move {
            let result = preparation.prepare().await.map(|prediction| {
                let mut value = request.response("punch-prepared");
                value["prediction"] = prediction_value(prediction.as_ref());
                value
            });
            let value = request.complete(result);
            inner.finish(SlotKind::Punch, slot, value).await;
        });
        Ok(None)
    }

    async fn begin_preparation(
        &self,
        prepare: &PreparePunchRequest,
        servers: Vec<Vec<String>>,
    ) -> Result<Arc<dyn PunchPreparation>, Fail> {
        if let Some(capacity) = prepare.edge_capacity {
            // Only a receiving request may establish a Viewer role. A late
            // sender request must not replace its retired source with a Viewer.
            let capacity = usize::try_from(capacity).unwrap_or_default();
            let viewer = self.ensure_viewer(&prepare.share_id, capacity).await?;
            return Ok(viewer.begin_punch(&prepare.connection_id, "", servers, &prepare.secret)?);
        }
        if prepare.source_connection_id.is_empty() {
            if let Some(host) = self.current(&prepare.share_id) {
                return Ok(host.begin_punch(&prepare.connection_id, servers, &prepare.secret)?);
            }
        } else if let Some(viewer) = self.current_viewer(&prepare.share_id) {
            return Ok(viewer.begin_punch(
                &prepare.connection_id,
                &prepare.source_connection_id,
                servers,
                &prepare.secret,
            )?);
        }
        Err(fail("native media source does not exist"))
    }

    fn run_punch(
        self: &Arc<Self>,
        request: &Request,
        payload: &[u8],
    ) -> Result<Option<ControlEvent>, Fail> {
        let run: RunPunchRequest = decode_strict(payload)
            .filter(|run: &RunPunchRequest| valid_identities(&[&run.share_id, &run.connection_id]))
            .ok_or(Fail::Violation("native connection probe is invalid"))?;
        let prediction = run
            .prediction
            .unwrap_or_else(|| piik_protocol::PunchPrediction {
                address: String::new(),
                port: 0,
                step: 0,
            });
        punch_target(&prediction)?;
        let media = self
            .current_media(&run.share_id)
            .ok_or_else(|| fail("native media session does not exist"))?;
        let preparation = media
            .punch(&run.connection_id)
            .ok_or_else(|| fail("connection preparation was retired"))?;
        let slot = {
            let mut state = self.lock();
            self.begin(&mut state, SlotKind::Punch)
        }
        .ok_or_else(|| fail("connection preparation is unavailable or already active"))?;
        let inner = self.clone();
        let request = request.clone();
        tokio::spawn(async move {
            let connected = preparation.run(prediction).await;
            let mut value = request.response("punch-complete");
            value["connected"] = json!(connected);
            inner.finish(SlotKind::Punch, slot, value).await;
        });
        Ok(None)
    }
}
