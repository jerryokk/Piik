//! HTTP room requests and responses.

use serde_json::Value;

use crate::codec::{
    DecodeError, Nullable, Obj, ToWire, discriminator, ensure, invalid, object, parse,
};
use crate::scalars::{valid_room_code, valid_viewer_password};
use crate::types::CodeEntryPolicy;

/// `createRoomRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRoomRequest {
    pub code_entry_policy: CodeEntryPolicy,
    pub room_password: Nullable<String>,
    pub preferred_room_id: Option<String>,
}

fn room_password(value: &Nullable<String>) -> Result<(), DecodeError> {
    ensure(
        value
            .value()
            .is_none_or(|password| valid_viewer_password(password)),
        "roomPassword is malformed",
    )
}

/// Parses `createRoomRequestSchema`.
pub fn decode_create_room_request(data: &[u8]) -> Result<CreateRoomRequest, DecodeError> {
    let value = parse(data)?;
    let f = object(
        &value,
        &["codeEntryPolicy", "roomPassword", "preferredRoomId"],
    )?;
    let room_password_value = f.nullable("roomPassword")?;
    let preferred_room_id = f.string("preferredRoomId")?;
    f.require(&["codeEntryPolicy"])?;
    f.optional(&["preferredRoomId"])?;
    let request = CreateRoomRequest {
        code_entry_policy: f.value("codeEntryPolicy")?,
        room_password: room_password_value,
        preferred_room_id: f.has("preferredRoomId").then_some(preferred_room_id),
    };
    room_password(&request.room_password)?;
    ensure(
        request
            .preferred_room_id
            .as_deref()
            .is_none_or(valid_room_code),
        "preferredRoomId is not a room code",
    )?;
    Ok(request)
}

/// `replaceRoomRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceRoomRequest {
    pub code_entry_policy: CodeEntryPolicy,
    pub room_password: Nullable<String>,
}

/// Parses `replaceRoomRequestSchema`.
pub fn decode_replace_room_request(data: &[u8]) -> Result<ReplaceRoomRequest, DecodeError> {
    let value = parse(data)?;
    let f = object(&value, &["codeEntryPolicy", "roomPassword"])?;
    let room_password_value = f.nullable("roomPassword")?;
    f.require(&["codeEntryPolicy"])?;
    let request = ReplaceRoomRequest {
        code_entry_policy: f.value("codeEntryPolicy")?,
        room_password: room_password_value,
    };
    room_password(&request.room_password)?;
    Ok(request)
}

/// One member of `roomAccessUpdateRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomAccessUpdateRequest {
    /// `set-code-entry-policy`.
    SetCodeEntryPolicy { policy: CodeEntryPolicy },
    /// `rotate-viewer-grant`.
    RotateViewerGrant,
    /// `revoke-viewer-grant`.
    RevokeViewerGrant,
    /// `set-viewer-password`; `None` clears the password.
    SetViewerPassword { password: Option<String> },
}

/// Parses `roomAccessUpdateRequestSchema`.
pub fn decode_room_access_update_request(
    data: &[u8],
) -> Result<RoomAccessUpdateRequest, DecodeError> {
    let value = parse(data)?;
    match discriminator(&value, "action")? {
        "set-code-entry-policy" => {
            let f = object(&value, &["action", "policy"])?;
            f.require(&["action", "policy"])?;
            Ok(RoomAccessUpdateRequest::SetCodeEntryPolicy {
                policy: f.value("policy")?,
            })
        }
        "rotate-viewer-grant" => {
            object(&value, &["action"])?.require(&["action"])?;
            Ok(RoomAccessUpdateRequest::RotateViewerGrant)
        }
        "revoke-viewer-grant" => {
            object(&value, &["action"])?.require(&["action"])?;
            Ok(RoomAccessUpdateRequest::RevokeViewerGrant)
        }
        "set-viewer-password" => {
            let f = object(&value, &["action", "password"])?;
            let password: Option<String> = f.opt("password")?;
            f.require(&["action"])?;
            f.require_nullable(&["password"])?;
            ensure(
                password.as_deref().is_none_or(valid_viewer_password),
                "password is malformed",
            )?;
            Ok(RoomAccessUpdateRequest::SetViewerPassword { password })
        }
        other => Err(invalid(format!("unknown room access action {other:?}"))),
    }
}

/// `createRoomResponseSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRoomResponse {
    pub room_id: String,
    pub host_token: String,
    pub invite_url: String,
    pub code_entry_policy: CodeEntryPolicy,
}

impl ToWire for CreateRoomResponse {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("roomId", &self.room_id)
            .put("hostToken", &self.host_token)
            .put("inviteUrl", &self.invite_url)
            .put("codeEntryPolicy", &self.code_entry_policy)
            .build()
    }
}

/// One member of `roomAccessUpdateResponseSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomAccessUpdateResponse {
    /// `code-entry-policy-updated`.
    CodeEntryPolicyUpdated {
        code_entry_policy: CodeEntryPolicy,
        viewer_password_enabled: bool,
    },
    /// `viewer-grant-updated`; `invite_url` is `None` for the explicit null
    /// a revoke returns.
    ViewerGrantUpdated {
        viewer_authorization_generation: String,
        invite_url: Option<String>,
    },
    /// `viewer-password-updated`.
    ViewerPasswordUpdated { enabled: bool },
}

impl ToWire for RoomAccessUpdateResponse {
    fn to_wire(&self) -> Value {
        match self {
            Self::CodeEntryPolicyUpdated {
                code_entry_policy,
                viewer_password_enabled,
            } => Obj::message("code-entry-policy-updated")
                .put("codeEntryPolicy", code_entry_policy)
                .put("viewerPasswordEnabled", viewer_password_enabled),
            Self::ViewerGrantUpdated {
                viewer_authorization_generation,
                invite_url,
            } => Obj::message("viewer-grant-updated")
                .put(
                    "viewerAuthorizationGeneration",
                    viewer_authorization_generation,
                )
                .put("inviteUrl", invite_url),
            Self::ViewerPasswordUpdated { enabled } => {
                Obj::message("viewer-password-updated").put("enabled", enabled)
            }
        }
        .build()
    }
}
