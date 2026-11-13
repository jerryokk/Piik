//! `routeDiagnosticSnapshotSchema`: the privacy-safe route snapshot.

use std::collections::HashSet;

use serde_json::Value;

use crate::codec::{FromWire, Int, Obj, Result, ToWire, in_range, invalid, object, wire_enum};
use crate::constants::{MAX_SAFE_INTEGER, MAX_VIEWERS_PER_ROOM_LIMIT};

const MAX_VIEWERS: Int = MAX_VIEWERS_PER_ROOM_LIMIT as Int;

wire_enum! {
    /// `routeDemandReasonSchema`.
    pub enum RouteDemandReason {
        Join = "join",
        EdgeUnavailable = "edge-unavailable",
        ParentDeparted = "parent-departed",
        CapacityReduction = "capacity-reduction",
        SfuBootstrap = "sfu-bootstrap",
        DirectConvergence = "direct-convergence",
        QualityConvergence = "quality-convergence",
        RootConvergence = "root-convergence",
        PublicationAttach = "publication-attach",
    }
}

wire_enum! {
    /// `routeDiagnosticFinalRouteSchema`.
    pub enum RouteDiagnosticFinalRoute {
        Direct = "direct",
        Sfu = "sfu",
        Waiting = "waiting",
        Failed = "failed",
    }
}

wire_enum! {
    /// `routeDiagnosticBucketSchema`.
    pub enum RouteDiagnosticBucket {
        None = "none",
        Stale = "stale",
        EndpointCapacity = "endpoint-capacity",
        SfuAdmission = "sfu-admission",
        CandidateFailed = "candidate-failed",
        FirstFrameTimeout = "first-frame-timeout",
        OperationDeadline = "operation-deadline",
        Aborted = "aborted",
    }
}

wire_enum! {
    /// `routeDiagnosticStageSchema`.
    pub enum RouteDiagnosticStage {
        Admission = "admission",
        FirstFrame = "first-frame",
        QualityProof = "quality-proof",
    }
}

/// `routeDiagnosticParentSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteDiagnosticParent {
    None,
    Host,
    Sfu,
    /// A viewer parent named by its diagnostic ordinal.
    Viewer {
        ordinal: Int,
    },
}

impl FromWire for RouteDiagnosticParent {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["kind", "ordinal"])?;
        let kind = f.string("kind")?;
        let ordinal = f.int("ordinal")?;
        f.require(&["kind"])?;
        let parent = match kind.as_str() {
            "none" => Self::None,
            "host" => Self::Host,
            "sfu" => Self::Sfu,
            "viewer" => {
                f.require(&["ordinal"])?;
                if !in_range(ordinal, 1, MAX_VIEWERS) {
                    return Err(invalid("route diagnostic parent ordinal is out of range"));
                }
                return Ok(Self::Viewer { ordinal });
            }
            _ => {
                return Err(invalid(format!(
                    "unknown route diagnostic parent kind {kind:?}"
                )));
            }
        };
        if f.has("ordinal") {
            return Err(invalid(format!(
                "route diagnostic parent {kind:?} must not carry ordinal"
            )));
        }
        Ok(parent)
    }
}

impl ToWire for RouteDiagnosticParent {
    fn to_wire(&self) -> Value {
        match self {
            Self::None => Obj::new().put("kind", "none"),
            Self::Host => Obj::new().put("kind", "host"),
            Self::Sfu => Obj::new().put("kind", "sfu"),
            Self::Viewer { ordinal } => Obj::new().put("kind", "viewer").put("ordinal", ordinal),
        }
        .build()
    }
}

/// `routeDiagnosticQualitySchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouteDiagnosticQuality {
    pub eligible_windows: Int,
    pub eligible_duration_ms: Int,
    pub freeze_windows: Int,
    pub freeze_count: Int,
    pub freeze_duration_ms: Int,
    pub pause_count: Int,
    pub pause_duration_ms: Int,
}

const QUALITY_KEYS: [&str; 7] = [
    "eligibleWindows",
    "eligibleDurationMs",
    "freezeWindows",
    "freezeCount",
    "freezeDurationMs",
    "pauseCount",
    "pauseDurationMs",
];

impl RouteDiagnosticQuality {
    fn values(&self) -> [Int; 7] {
        [
            self.eligible_windows,
            self.eligible_duration_ms,
            self.freeze_windows,
            self.freeze_count,
            self.freeze_duration_ms,
            self.pause_count,
            self.pause_duration_ms,
        ]
    }
}

impl FromWire for RouteDiagnosticQuality {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &QUALITY_KEYS)?;
        let quality = Self {
            eligible_windows: f.int("eligibleWindows")?,
            eligible_duration_ms: f.int("eligibleDurationMs")?,
            freeze_windows: f.int("freezeWindows")?,
            freeze_count: f.int("freezeCount")?,
            freeze_duration_ms: f.int("freezeDurationMs")?,
            pause_count: f.int("pauseCount")?,
            pause_duration_ms: f.int("pauseDurationMs")?,
        };
        f.require(&QUALITY_KEYS)?;
        if quality
            .values()
            .iter()
            .any(|value| !in_range(*value, 0, MAX_SAFE_INTEGER))
        {
            return Err(invalid("route diagnostic quality value is out of range"));
        }
        Ok(quality)
    }
}

impl ToWire for RouteDiagnosticQuality {
    fn to_wire(&self) -> Value {
        QUALITY_KEYS
            .iter()
            .zip(self.values())
            .fold(Obj::new(), |out, (key, value)| out.put(key, &value))
            .build()
    }
}

/// `routeDiagnosticChildSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDiagnosticChild {
    pub ordinal: Int,
    pub parent: RouteDiagnosticParent,
    pub effective_capacity: Int,
    pub child_count: Int,
    pub demand_age_ms: Option<Int>,
    pub queue_wait_ms: Option<Int>,
    pub candidate_start_ms: Option<Int>,
    pub first_decoded_frame_ms: Option<Int>,
    pub final_ms: Option<Int>,
    pub final_route: RouteDiagnosticFinalRoute,
    pub rejection_bucket: RouteDiagnosticBucket,
    pub quality: Option<RouteDiagnosticQuality>,
}

impl FromWire for RouteDiagnosticChild {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "ordinal",
                "parent",
                "effectiveCapacity",
                "childCount",
                "demandAgeMs",
                "queueWaitMs",
                "candidateStartMs",
                "firstDecodedFrameMs",
                "finalMs",
                "finalRoute",
                "rejectionBucket",
                "quality",
            ],
        )?;
        f.require(&[
            "ordinal",
            "parent",
            "effectiveCapacity",
            "childCount",
            "finalRoute",
            "rejectionBucket",
        ])?;
        f.require_nullable(&[
            "demandAgeMs",
            "queueWaitMs",
            "candidateStartMs",
            "firstDecodedFrameMs",
            "finalMs",
            "quality",
        ])?;
        let child = Self {
            ordinal: f.int("ordinal")?,
            parent: f.value("parent")?,
            effective_capacity: f.int("effectiveCapacity")?,
            child_count: f.int("childCount")?,
            demand_age_ms: f.opt("demandAgeMs")?,
            queue_wait_ms: f.opt("queueWaitMs")?,
            candidate_start_ms: f.opt("candidateStartMs")?,
            first_decoded_frame_ms: f.opt("firstDecodedFrameMs")?,
            final_ms: f.opt("finalMs")?,
            final_route: f.value("finalRoute")?,
            rejection_bucket: f.value("rejectionBucket")?,
            quality: f.opt("quality")?,
        };
        if !in_range(child.ordinal, 1, MAX_VIEWERS) {
            return Err(invalid("route diagnostic ordinal is out of range"));
        }
        if !in_range(child.effective_capacity, 0, MAX_VIEWERS)
            || !in_range(child.child_count, 0, MAX_VIEWERS)
        {
            return Err(invalid("route diagnostic capacity is out of range"));
        }
        if [
            child.demand_age_ms,
            child.queue_wait_ms,
            child.candidate_start_ms,
            child.first_decoded_frame_ms,
            child.final_ms,
        ]
        .into_iter()
        .flatten()
        .any(|duration| !in_range(duration, 0, MAX_SAFE_INTEGER))
        {
            return Err(invalid("route diagnostic duration is out of range"));
        }
        Ok(child)
    }
}

impl ToWire for RouteDiagnosticChild {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("ordinal", &self.ordinal)
            .put("parent", &self.parent)
            .put("effectiveCapacity", &self.effective_capacity)
            .put("childCount", &self.child_count)
            .put("demandAgeMs", &self.demand_age_ms)
            .put("queueWaitMs", &self.queue_wait_ms)
            .put("candidateStartMs", &self.candidate_start_ms)
            .put("firstDecodedFrameMs", &self.first_decoded_frame_ms)
            .put("finalMs", &self.final_ms)
            .put("finalRoute", &self.final_route)
            .put("rejectionBucket", &self.rejection_bucket)
            .put("quality", &self.quality)
            .build()
    }
}

/// `routeDiagnosticOperationSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDiagnosticOperation {
    pub child_ordinal: Int,
    pub reason: RouteDemandReason,
    pub stage: RouteDiagnosticStage,
    pub cursor: Int,
    pub candidate_count: Int,
}

impl FromWire for RouteDiagnosticOperation {
    fn from_wire(value: &Value) -> Result<Self> {
        let keys = [
            "childOrdinal",
            "reason",
            "stage",
            "cursor",
            "candidateCount",
        ];
        let f = object(value, &keys)?;
        f.require(&keys)?;
        let operation = Self {
            child_ordinal: f.int("childOrdinal")?,
            reason: f.value("reason")?,
            stage: f.value("stage")?,
            cursor: f.int("cursor")?,
            candidate_count: f.int("candidateCount")?,
        };
        if !in_range(operation.child_ordinal, 1, MAX_VIEWERS) {
            return Err(invalid("route diagnostic childOrdinal is out of range"));
        }
        if !in_range(operation.cursor, 0, MAX_VIEWERS) {
            return Err(invalid("route diagnostic cursor is out of range"));
        }
        if !in_range(operation.candidate_count, 1, MAX_VIEWERS + 1) {
            return Err(invalid("route diagnostic candidateCount is out of range"));
        }
        Ok(operation)
    }
}

impl ToWire for RouteDiagnosticOperation {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("childOrdinal", &self.child_ordinal)
            .put("reason", &self.reason)
            .put("stage", &self.stage)
            .put("cursor", &self.cursor)
            .put("candidateCount", &self.candidate_count)
            .build()
    }
}

/// `routeDiagnosticSnapshotSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RouteDiagnosticSnapshot {
    pub children: Vec<RouteDiagnosticChild>,
    pub operation: Option<RouteDiagnosticOperation>,
}

impl FromWire for RouteDiagnosticSnapshot {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["children", "operation"])?;
        f.require(&["children"])?;
        f.require_nullable(&["operation"])?;
        let snapshot = Self {
            children: f.or_default("children")?,
            operation: f.opt("operation")?,
        };
        if snapshot.children.len() > MAX_VIEWERS_PER_ROOM_LIMIT {
            return Err(invalid("route diagnostic snapshot holds too many children"));
        }
        // superRefine: unique ordinals, resolvable viewer parents, and an
        // operation that names a present child with a cursor inside its
        // candidates.
        let ordinals: HashSet<Int> = snapshot
            .children
            .iter()
            .map(|child| child.ordinal)
            .collect();
        if ordinals.len() != snapshot.children.len() {
            return Err(invalid("Route diagnostic ordinals must be unique"));
        }
        for child in &snapshot.children {
            if let RouteDiagnosticParent::Viewer { ordinal } = child.parent
                && (!ordinals.contains(&ordinal) || ordinal == child.ordinal)
            {
                return Err(invalid("Route diagnostic parent ordinal is invalid"));
            }
        }
        if let Some(operation) = &snapshot.operation
            && (!ordinals.contains(&operation.child_ordinal)
                || operation.cursor >= operation.candidate_count)
        {
            return Err(invalid("Route diagnostic operation is invalid"));
        }
        Ok(snapshot)
    }
}

impl ToWire for RouteDiagnosticSnapshot {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("children", &self.children)
            .put("operation", &self.operation)
            .build()
    }
}
