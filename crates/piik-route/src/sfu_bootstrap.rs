//! The bounded SFU-bootstrap intent for a full Host without a publication.

use crate::candidates::endpoint_transition_rank;
use crate::controller::{Operation, SfuBootstrapCarrier, SfuBootstrapIntent, compare_participant};
use crate::opportunities::consume_opportunity;
use crate::{
    CandidatePlan, CandidateTuple, Controller, DemandReason, EndpointTransition, FinalRoute,
    PublicationPlan, RejectionBucket, TransitionKind, Transport, UpstreamKind,
};

impl Controller {
    fn peek_sfu_bootstrap_carrier(&mut self) -> Option<SfuBootstrapCarrier> {
        let intent = self.current_sfu_bootstrap_intent()?;
        let carrier_peer_id = self
            .safe_sfu_bootstrap_carriers(&intent.demand_peer_id)
            .into_iter()
            .next()?;
        Some(SfuBootstrapCarrier {
            demand_peer_id: intent.demand_peer_id,
            carrier_peer_id,
        })
    }

    pub(crate) fn has_sfu_bootstrap_work(&mut self) -> bool {
        if self.current_sfu_bootstrap_intent().is_some() {
            return true;
        }
        self.available_viewers(true).into_iter().any(|peer_id| {
            let viewer = self.participant(&peer_id).unwrap();
            viewer.availability_exhausted
                && self.sfu_bootstrap_needed(&peer_id)
                && (!self.safe_sfu_bootstrap_carriers(&peer_id).is_empty()
                    || !viewer.bootstrap_failure_reported)
        })
    }

    pub(crate) fn take_sfu_bootstrap_carrier(
        &mut self,
        now_ms: i64,
        failed_peer_ids: &mut Vec<String>,
    ) -> Option<SfuBootstrapCarrier> {
        if !self.sfu_bootstrap_globally_needed() {
            return None;
        }
        if let Some(carrier) = self.peek_sfu_bootstrap_carrier() {
            return Some(carrier);
        }
        self.sfu_bootstrap_intent = None;
        self.report_sfu_bootstrap_failures(now_ms, failed_peer_ids);
        None
    }

    /// Revalidates the sticky intent on every access and otherwise re-derives
    /// it from the first exhausted viewer in available-viewer order.
    fn current_sfu_bootstrap_intent(&mut self) -> Option<SfuBootstrapIntent> {
        if let Some(current) = self.sfu_bootstrap_intent.clone() {
            let demand_ok = self
                .participant(&current.demand_peer_id)
                .is_some_and(|demand| demand.session_is(&current.demand_session_id));
            if self.sfu_bootstrap_needed(&current.demand_peer_id)
                && demand_ok
                && !self
                    .safe_sfu_bootstrap_carriers(&current.demand_peer_id)
                    .is_empty()
            {
                return Some(current);
            }
            self.sfu_bootstrap_intent = None;
        }
        for peer_id in self.available_viewers(true) {
            let viewer = self.participant(&peer_id).unwrap();
            if viewer.availability_exhausted
                && self.sfu_bootstrap_needed(&peer_id)
                && !self.safe_sfu_bootstrap_carriers(&peer_id).is_empty()
            {
                if viewer.session_id.is_empty() {
                    return None;
                }
                let created = SfuBootstrapIntent {
                    demand_peer_id: peer_id,
                    demand_session_id: viewer.session_id.clone(),
                };
                self.sfu_bootstrap_intent = Some(created.clone());
                return Some(created);
            }
        }
        None
    }

    /// Checked first in validation so a bootstrap operation evaporates with
    /// its intent.
    pub(crate) fn bootstrap_operation_owned(&self, op: &Operation) -> bool {
        if op.reason != DemandReason::SfuBootstrap {
            return true;
        }
        let Some(intent) = &self.sfu_bootstrap_intent else {
            return false;
        };
        if intent.demand_peer_id != op.demand_peer_id
            || intent.demand_session_id != op.demand_session_id
            || !self.sfu_bootstrap_needed(&op.demand_peer_id)
        {
            return false;
        }
        self.safe_sfu_bootstrap_carriers(&op.demand_peer_id)
            .contains(&op.child_peer_id)
    }

    fn sfu_bootstrap_needed(&self, demand_peer_id: &str) -> bool {
        if !self.sfu_bootstrap_globally_needed()
            || !self.allows_incoming(demand_peer_id, UpstreamKind::Sfu)
        {
            return false;
        }
        self.participant(demand_peer_id).is_some_and(|demand| {
            !demand.session_id.is_empty()
                && (!self.usable_route(demand_peer_id) || self.voice_route_missing(demand_peer_id))
                && self.sfu_opportunity_available(demand_peer_id)
        })
    }

    fn sfu_opportunity_available(&self, demand_peer_id: &str) -> bool {
        self.candidate_opportunity_available(
            demand_peer_id,
            &CandidatePlan {
                tuple: CandidateTuple::sfu(PublicationPlan::Reuse),
                endpoint_transition: EndpointTransition {
                    kind: TransitionKind::None,
                    producer_peer_id: None,
                    retire: None,
                },
            },
        )
    }

    fn sfu_bootstrap_globally_needed(&self) -> bool {
        self.sfu_enabled && self.host_publication.is_none() && !self.host_has_publication_slot()
    }

    fn safe_sfu_bootstrap_carriers(&self, demand_peer_id: &str) -> Vec<String> {
        if !self.sfu_bootstrap_needed(demand_peer_id) {
            return Vec::new();
        }
        self.sfu_bootstrap_carriers()
    }

    /// The Host's usable direct children that can create a publication,
    /// newest first.
    fn sfu_bootstrap_carriers(&self) -> Vec<String> {
        let tuple = CandidateTuple::sfu(PublicationPlan::Create);
        let mut carriers: Vec<String> = self
            .children_of(&self.host_peer_id)
            .into_iter()
            .filter(|peer_id| {
                let current_ok = self
                    .participant(peer_id)
                    .is_some_and(|current| !current.session_id.is_empty());
                let edge_ok = self.edge(peer_id).is_some_and(|edge| {
                    edge.kind == UpstreamKind::Peer
                        && edge.transport == Transport::Direct
                        && edge.usable
                        && edge.physical_active
                });
                current_ok
                    && edge_ok
                    && self.plan_candidate(peer_id, &tuple).is_some_and(|plan| {
                        self.candidate_valid(peer_id, &plan, None)
                            && plan.endpoint_transition.kind != TransitionKind::BoundedGap
                            && self.sfu_bootstrap_opportunity_available(peer_id, &plan)
                    })
            })
            .collect();
        carriers.sort_by(|left, right| {
            compare_participant(
                self.participant(right).unwrap(),
                self.participant(left).unwrap(),
            )
        });
        carriers
    }

    fn sfu_bootstrap_opportunity_available(
        &self,
        carrier_peer_id: &str,
        plan: &CandidatePlan,
    ) -> bool {
        self.consumed_sfu_bootstrap_opportunities
            .get(&self.sfu_bootstrap_opportunity_base(carrier_peer_id, plan))
            .is_none_or(|consumed| endpoint_transition_rank(&plan.endpoint_transition) < *consumed)
    }

    fn sfu_bootstrap_opportunity_base(
        &self,
        carrier_peer_id: &str,
        plan: &CandidatePlan,
    ) -> String {
        format!(
            "{carrier_peer_id}\0{}",
            self.candidate_opportunity_base(carrier_peer_id, plan)
        )
    }

    /// Consumes the carrier's attempt and returns the failed demand peer only
    /// once no carrier is left.
    pub(crate) fn complete_sfu_bootstrap_carrier(
        &mut self,
        carrier_peer_id: &str,
        now_ms: i64,
        bucket: RejectionBucket,
        attempted_plan: Option<CandidatePlan>,
    ) -> String {
        let Some(intent) = self.sfu_bootstrap_intent.clone() else {
            return String::new();
        };
        let plan = attempted_plan.or_else(|| {
            self.plan_candidate(
                carrier_peer_id,
                &CandidateTuple::sfu(PublicationPlan::Create),
            )
        });
        if let Some(plan) = plan {
            let base = self.sfu_bootstrap_opportunity_base(carrier_peer_id, &plan);
            consume_opportunity(
                &mut self.consumed_sfu_bootstrap_opportunities,
                base,
                endpoint_transition_rank(&plan.endpoint_transition),
            );
            self.consume_candidate_opportunity(carrier_peer_id, &plan);
        }
        if !self
            .safe_sfu_bootstrap_carriers(&intent.demand_peer_id)
            .is_empty()
        {
            return String::new();
        }
        if let Some(demand) = self.participant_mut(&intent.demand_peer_id) {
            demand.availability_exhausted = true;
        }
        self.finish_timing(
            &intent.demand_peer_id,
            now_ms,
            FinalRoute::Failed,
            bucket,
            false,
        );
        if let Some(demand) = self.participant_mut(&intent.demand_peer_id) {
            demand.bootstrap_failure_reported = true;
        }
        self.sfu_bootstrap_intent = None;
        intent.demand_peer_id
    }

    /// Follows available-viewer order.
    fn report_sfu_bootstrap_failures(&mut self, now_ms: i64, failed_peer_ids: &mut Vec<String>) {
        for peer_id in self.available_viewers(true) {
            let demand = self.participant(&peer_id).unwrap();
            if !demand.availability_exhausted
                || !self.sfu_bootstrap_needed(&peer_id)
                || self.usable_route(&peer_id)
                || demand.bootstrap_failure_reported
            {
                continue;
            }
            self.participant_mut(&peer_id)
                .unwrap()
                .bootstrap_failure_reported = true;
            self.finish_timing(
                &peer_id,
                now_ms,
                FinalRoute::Failed,
                RejectionBucket::CandidateFailed,
                false,
            );
            failed_peer_ids.push(peer_id);
        }
    }

    pub(crate) fn remember_unavailable_sfu_bootstrap(&mut self, demand_peer_id: &str) {
        if !self.sfu_bootstrap_needed(demand_peer_id) {
            return;
        }
        let Some(demand) = self.participant_mut(demand_peer_id) else {
            return;
        };
        if demand.session_id.is_empty() {
            return;
        }
        demand.bootstrap_failure_reported = true;
        self.sfu_bootstrap_intent = None;
    }

    pub(crate) fn stage_sfu_bootstrap(&mut self) {
        let demand_peer_id = self.op().demand_peer_id.clone();
        let Some(demand) = self.participant(&demand_peer_id) else {
            return;
        };
        if demand.session_id.is_empty() || !self.sfu_bootstrap_needed(&demand_peer_id) {
            return;
        }
        if self.safe_sfu_bootstrap_carriers(&demand_peer_id).is_empty() {
            return;
        }
        let session_id = demand.session_id.clone();
        if self.sfu_bootstrap_intent.as_ref().is_some_and(|current| {
            current.demand_peer_id == demand_peer_id && current.demand_session_id == session_id
        }) {
            return;
        }
        self.sfu_bootstrap_intent = Some(SfuBootstrapIntent {
            demand_peer_id,
            demand_session_id: session_id,
        });
    }

    pub(crate) fn clear_sfu_bootstrap_for_demand(&mut self, demand_peer_id: &str) {
        if let Some(demand) = self.participant_mut(demand_peer_id) {
            demand.sfu_first_at_next_route = false;
        }
        if self
            .sfu_bootstrap_intent
            .as_ref()
            .is_some_and(|intent| intent.demand_peer_id == demand_peer_id)
        {
            self.sfu_bootstrap_intent = None;
        }
    }

    pub(crate) fn bootstrap_candidate_available(&self, demand_peer_id: &str) -> bool {
        !self.safe_sfu_bootstrap_carriers(demand_peer_id).is_empty()
    }
}
