//! Authentication and route-update envelope decoding.

use serde_json::Value;

use crate::codec::{Int, Result, discriminator, ensure, in_range, invalid, object};
use crate::constants::{
    DEFAULT_ROUTE_POLICY, MAX_ENDPOINT_MEDIA_COPY_CAPACITY, MAX_VIEWERS_PER_ROOM_LIMIT,
    SIGNALING_PROTOCOL,
};
use crate::scalars::valid_opaque_id;
use crate::server::{AuthenticatedMessage, MEDIA_MODE, ServerMessage, require_opaque, revision};
use crate::types::Role;

pub(crate) fn decode_authenticated(value: &Value) -> Result<AuthenticatedMessage> {
    const BASE: [&str; 19] = [
        "type",
        "protocol",
        "peerId",
        "maxViewers",
        "endpointMediaCopyCapacity",
        "hostOnline",
        "hostPaused",
        "connectionId",
        "iceConfig",
        "routePolicy",
        "codeEntryPolicy",
        "viewerAuthorizationGeneration",
        "mediaMode",
        "shareGeneration",
        "routeRevision",
        "routeAssignment",
        "qualitySettings",
        "role",
        "viewerPasswordEnabled",
    ];
    let role = discriminator(value, "role")?;
    let host = match role {
        "host" => true,
        "viewer" => false,
        other => return Err(invalid(format!("unknown authenticated role {other:?}"))),
    };
    let keys = if host {
        &BASE[..]
    } else {
        &BASE[..BASE.len() - 1]
    };
    let f = object(value, keys)?;
    if host {
        f.require(&["viewerPasswordEnabled"])?;
    }
    f.require(&[
        "type",
        "protocol",
        "peerId",
        "maxViewers",
        "endpointMediaCopyCapacity",
        "hostOnline",
        "iceConfig",
        "codeEntryPolicy",
        "viewerAuthorizationGeneration",
        "mediaMode",
        "routeRevision",
        "routeAssignment",
        "qualitySettings",
        "role",
    ])?;
    f.require_nullable(&["connectionId", "shareGeneration"])?;
    f.optional(&["hostPaused", "routePolicy"])?;
    ensure(
        f.string("protocol")? == SIGNALING_PROTOCOL,
        "protocol is not the current signaling generation",
    )?;
    let peer_id = require_opaque(&f, "peerId", "peerId is not an opaque id")?;
    let max_viewers = f.int("maxViewers")?;
    ensure(
        in_range(max_viewers, 1, MAX_VIEWERS_PER_ROOM_LIMIT as Int),
        "maxViewers is out of range",
    )?;
    let endpoint_media_copy_capacity = f.int("endpointMediaCopyCapacity")?;
    ensure(
        in_range(
            endpoint_media_copy_capacity,
            1,
            MAX_ENDPOINT_MEDIA_COPY_CAPACITY,
        ),
        "endpointMediaCopyCapacity is out of range",
    )?;
    let connection_id: Option<String> = f.opt("connectionId")?;
    ensure(
        connection_id.as_deref().is_none_or(valid_opaque_id),
        "connectionId is not an opaque id",
    )?;
    let code_entry_policy = f.value("codeEntryPolicy")?;
    let viewer_authorization_generation = require_opaque(
        &f,
        "viewerAuthorizationGeneration",
        "viewerAuthorizationGeneration is not an opaque id",
    )?;
    ensure(
        f.string("mediaMode")? == MEDIA_MODE,
        "mediaMode must be peer-assisted",
    )?;
    let share_generation: Option<String> = f.opt("shareGeneration")?;
    ensure(
        share_generation.as_deref().is_none_or(valid_opaque_id),
        "shareGeneration is not an opaque id",
    )?;
    Ok(AuthenticatedMessage {
        peer_id,
        max_viewers,
        endpoint_media_copy_capacity,
        host_online: f.boolean("hostOnline")?,
        host_paused: f.opt("hostPaused")?,
        connection_id,
        ice_config: f.value("iceConfig")?,
        route_policy: f.opt("routePolicy")?.unwrap_or(DEFAULT_ROUTE_POLICY),
        code_entry_policy,
        viewer_authorization_generation,
        share_generation,
        route_revision: revision(&f, "routeRevision")?,
        route_assignment: f.value("routeAssignment")?,
        quality_settings: f.value("qualitySettings")?,
        role: if host { Role::Host } else { Role::Viewer },
        viewer_password_enabled: if host {
            Some(f.boolean("viewerPasswordEnabled")?)
        } else {
            None
        },
    })
}

pub(crate) fn decode_route_update(value: &Value) -> Result<ServerMessage> {
    match discriminator(value, "phase")? {
        "prepare" => {
            let keys = ["type", "revision", "phase", "assignment", "candidate"];
            let f = object(value, &keys)?;
            f.require(&keys)?;
            Ok(ServerMessage::RouteUpdatePrepare {
                assignment: f.value("assignment")?,
                candidate: f.value("candidate")?,
                revision: revision(&f, "revision")?,
            })
        }
        "active" => {
            let keys = ["type", "revision", "phase", "assignment"];
            let f = object(value, &keys)?;
            f.require(&keys)?;
            Ok(ServerMessage::RouteUpdateActive {
                assignment: f.value("assignment")?,
                revision: revision(&f, "revision")?,
            })
        }
        other => Err(invalid(format!("unknown route-update phase {other:?}"))),
    }
}
