//! Endpoint identity decoding for Host and Viewer authentication.

use serde_json::Value;

use crate::client::{
    AuthenticateHostMessage, AuthenticateViewerMessage, ClientMessage, current_protocol,
    optional_opaque, require_opaque,
};
use crate::codec::{Result, discriminator, ensure, invalid, object};
use crate::constants::DEFAULT_ROUTE_POLICY;
use crate::scalars::{valid_room_code, valid_token, valid_viewer_grant, valid_viewer_password};

pub(crate) fn decode_authenticate(value: &Value) -> Result<ClientMessage> {
    let role = discriminator(value, "role")?;
    match role {
        "host" => {
            let f = object(
                value,
                &[
                    "type",
                    "protocol",
                    "roomId",
                    "role",
                    "token",
                    "clientId",
                    "shareGeneration",
                    "sharingPaused",
                    "qualitySettings",
                    "routePolicy",
                    "displayName",
                ],
            )?;
            let sharing_paused = f.opt("sharingPaused")?;
            let quality_settings = f.opt("qualitySettings")?;
            let route_policy = f.opt("routePolicy")?;
            let display_name = f.opt("displayName")?;
            f.require(&["type", "protocol", "roomId", "role", "token", "clientId"])?;
            f.optional(&[
                "shareGeneration",
                "sharingPaused",
                "qualitySettings",
                "routePolicy",
                "displayName",
            ])?;
            current_protocol(&f)?;
            let room_id = f.string("roomId")?;
            ensure(valid_room_code(&room_id), "roomId is not a room code")?;
            let token = f.string("token")?;
            ensure(valid_token(&token), "token is malformed")?;
            let client_id = require_opaque(&f, "clientId", "clientId is not an opaque id")?;
            let share_generation =
                optional_opaque(&f, "shareGeneration", "shareGeneration is not an opaque id")?;
            Ok(ClientMessage::AuthenticateHost(Box::new(
                AuthenticateHostMessage {
                    room_id,
                    token,
                    client_id,
                    share_generation,
                    sharing_paused,
                    quality_settings,
                    route_policy: route_policy.unwrap_or(DEFAULT_ROUTE_POLICY),
                    display_name,
                },
            )))
        }
        "viewer" => {
            let f = object(
                value,
                &[
                    "type",
                    "protocol",
                    "roomId",
                    "role",
                    "clientId",
                    "viewerGrant",
                    "viewerPassword",
                    "displayName",
                    "routePreference",
                ],
            )?;
            let display_name = f.opt("displayName")?;
            let route_preference = f.opt("routePreference")?;
            let viewer_grant = f.string("viewerGrant")?;
            let viewer_password = f.string("viewerPassword")?;
            f.require(&["type", "protocol", "roomId", "role", "clientId"])?;
            f.optional(&[
                "viewerGrant",
                "viewerPassword",
                "displayName",
                "routePreference",
            ])?;
            current_protocol(&f)?;
            let room_id = f.string("roomId")?;
            ensure(valid_room_code(&room_id), "roomId is not a room code")?;
            let client_id = require_opaque(&f, "clientId", "clientId is not an opaque id")?;
            ensure(
                !f.has("viewerGrant") || valid_viewer_grant(&viewer_grant),
                "viewerGrant is malformed",
            )?;
            ensure(
                !f.has("viewerPassword") || valid_viewer_password(&viewer_password),
                "viewerPassword is malformed",
            )?;
            Ok(ClientMessage::AuthenticateViewer(Box::new(
                AuthenticateViewerMessage {
                    room_id,
                    client_id,
                    viewer_grant: f.has("viewerGrant").then_some(viewer_grant),
                    viewer_password: f.has("viewerPassword").then_some(viewer_password),
                    display_name,
                    route_preference,
                },
            )))
        }
        other => Err(invalid(format!("unknown authenticate role {other:?}"))),
    }
}
