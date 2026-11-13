//! Event-driven Host-root capacity convergence.

use crate::controller::{Operation, compare_participant};
use crate::debug::route_debug;
use crate::{
    CandidatePlan, CandidateTuple, Controller, DemandReason, TransitionKind, UpstreamKind,
};

pub(crate) struct RootConvergencePick {
    pub(crate) child_peer_id: String,
    pub(crate) plan: CandidatePlan,
}

struct RootConvergence {
    donor_peer_id: String,
    child_peer_id: String,
    plan: CandidatePlan,
}

impl Controller {
    /// Called from commit whenever a child newly became a direct Host child.
    pub(crate) fn stage_root_convergence(&mut self, root_peer_id: &str) {
        if !self.quality_convergence_enabled {
            return;
        }
        let Some(convergence) = self.root_convergence_plan(root_peer_id) else {
            return;
        };
        self.root_convergence_root_peer_id = root_peer_id.to_owned();
        route_debug!(self, "root-convergence-staged",
            "root" => self.debug_peer(root_peer_id),
            "donor" => self.debug_peer(&convergence.donor_peer_id),
            "child" => self.debug_peer(&convergence.child_peer_id));
    }

    /// Consumes the intent by clearing it.
    pub(crate) fn take_root_convergence_intent(&mut self) -> Option<RootConvergencePick> {
        let root_peer_id = std::mem::take(&mut self.root_convergence_root_peer_id);
        if root_peer_id.is_empty() || !self.quality_convergence_enabled {
            return None;
        }
        let convergence = self.root_convergence_plan(&root_peer_id)?;
        Some(RootConvergencePick {
            child_peer_id: convergence.child_peer_id,
            plan: convergence.plan,
        })
    }

    /// The busiest other Host root donates its newest active direct child to
    /// the fresh root.
    fn root_convergence_plan(&self, root_peer_id: &str) -> Option<RootConvergence> {
        let root = self.participant(root_peer_id)?;
        if root.session_id.is_empty()
            || root.departure_confirmed
            || !self.is_active_host_root(root_peer_id)
            || !self.active_direct_children(root_peer_id).is_empty()
        {
            return None;
        }
        let mut donors: Vec<String> = self
            .children_of(&self.host_peer_id)
            .into_iter()
            .filter(|peer_id| {
                peer_id != root_peer_id
                    && self.is_active_host_root(peer_id)
                    && self.active_direct_children(peer_id).len() >= 2
            })
            .collect();
        donors.sort_by(|left, right| {
            self.active_direct_children(right)
                .len()
                .cmp(&self.active_direct_children(left).len())
                .then_with(|| {
                    compare_participant(
                        self.participant(left).unwrap(),
                        self.participant(right).unwrap(),
                    )
                })
        });
        let donor_peer_id = donors.into_iter().next()?;
        let mut children = self.active_direct_children(&donor_peer_id);
        children.sort_by(|left, right| {
            compare_participant(
                self.participant(right).unwrap(),
                self.participant(left).unwrap(),
            )
        });
        let child_peer_id = children.into_iter().next()?;
        let plan = self.plan_candidate(&child_peer_id, &CandidateTuple::peer(root_peer_id))?;
        if plan.endpoint_transition.kind != TransitionKind::BoundedGap
            && self.candidate_valid(&child_peer_id, &plan, None)
        {
            return Some(RootConvergence {
                donor_peer_id,
                child_peer_id,
                plan,
            });
        }
        None
    }

    pub(crate) fn root_convergence_operation_still_eligible(&self, op: &Operation) -> bool {
        if op.reason != DemandReason::RootConvergence {
            return true;
        }
        let (Some(tuple), Some(current)) = (op.current_tuple(), self.edge(&op.child_peer_id))
        else {
            return false;
        };
        tuple.kind == UpstreamKind::Peer
            && current.kind == UpstreamKind::Peer
            && current.usable
            && current.physical_active
            && current.parent_peer_id != tuple.parent_peer_id
            && self.is_active_host_root(&tuple.parent_peer_id)
            && self.is_active_host_root(&current.parent_peer_id)
            && self
                .active_direct_children(&tuple.parent_peer_id)
                .is_empty()
            && self.active_direct_children(&current.parent_peer_id).len() >= 2
    }

    fn is_active_host_root(&self, peer_id: &str) -> bool {
        self.edge(peer_id).is_some_and(|edge| {
            edge.kind == UpstreamKind::Peer
                && edge.parent_peer_id == self.host_peer_id
                && edge.usable
                && edge.physical_active
        })
    }

    fn active_direct_children(&self, parent_peer_id: &str) -> Vec<String> {
        self.children_of(parent_peer_id)
            .into_iter()
            .filter(|child| {
                self.edge(child).is_some_and(|edge| {
                    edge.kind == UpstreamKind::Peer && edge.usable && edge.physical_active
                })
            })
            .collect()
    }
}
