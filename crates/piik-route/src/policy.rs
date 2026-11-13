//! Per-share route policy over the retained room controller.

use piik_protocol::{Role, RoutePolicy, ViewerRoutePreference};

use crate::{Controller, RejectionBucket, ResourceRef};

impl Controller {
    /// Applies the server-validated policy for a new share to the retained
    /// room controller. Existing routes stay physical while the ordinary
    /// operation proves their replacement; disallowed routes cannot attach
    /// video.
    pub fn set_route_policy(&mut self, policy: RoutePolicy, now_ms: i64) -> Vec<ResourceRef> {
        let sfu_enabled = !policy.peer_only;
        if self.sfu_enabled == sfu_enabled
            && self.quality_convergence_enabled == policy.topology_optimization
            && self.nat_prediction_enabled == policy.nat_prediction
        {
            return Vec::new();
        }
        let released = self.abort_operation(Some(now_ms), RejectionBucket::Aborted);
        self.sfu_enabled = sfu_enabled;
        self.quality_convergence_enabled = policy.topology_optimization;
        self.nat_prediction_enabled = policy.nat_prediction;
        for peer_id in self.participants.keys() {
            let current = self.participant(&peer_id).unwrap();
            if current.role != Role::Viewer {
                continue;
            }
            let allowed = self.allows_viewer_route_preference(current.route_preference);
            let current = self.participant_mut(&peer_id).unwrap();
            if !allowed {
                current.route_preference = Some(ViewerRoutePreference::Auto);
            }
            current.consumed_candidate_opportunities.clear();
            current.availability_exhausted = false;
            current.bootstrap_failure_reported = false;
            current.sfu_first_at_next_route = false;
        }
        self.direct_continuations.clear();
        self.sfu_bootstrap_intent = None;
        self.consumed_sfu_bootstrap_opportunities.clear();
        self.root_convergence_root_peer_id.clear();
        self.quality_observations.clear();
        self.quality_baselines_pending.clear();
        self.sender_quality_observations.clear();
        self.sender_quality_baselines.clear();
        self.sender_quality_regeneration_blocks.clear();
        self.sfu_publisher_quality_observation = None;
        self.touch_facts();
        released
    }
}
