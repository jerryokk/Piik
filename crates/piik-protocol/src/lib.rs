//! The Rust owner of the Piik wire contract.
//!
//! Decoding mirrors the zod schemas in `src/shared/protocol.ts`: objects are
//! strict, absent and null are distinguished where the schema does, integers
//! must be safe and integral, and string bounds count UTF-16 code units.
//! Encoding emits the TypeScript key order and omission rules. The crate
//! performs no I/O.
//!
//! Entry points: [`decode_client_message`], [`decode_server_message`],
//! [`encode_server_message`], [`encode_client_message`], the HTTP
//! `decode_*_request` functions, and the generic [`decode`] / [`encode`] over
//! [`FromWire`] / [`ToWire`]. Every decode failure is a [`DecodeError`].

#![forbid(unsafe_code)]

mod audio_presence;
mod client;
mod co_play_presence;
pub mod codec;
mod constants;
mod helpers;
mod http;
mod interactions;
mod punch;
mod relay_volunteer;
mod remote_input;
mod scalars;
mod server;
mod sfu;
mod types;
mod viewer_route;
mod voice;

pub use client::*;
pub use co_play_presence::*;
pub use codec::{DecodeError, FromWire, Int, Nullable, Num, ToWire, decode, encode};
pub use constants::*;
pub use helpers::*;
pub use http::*;
pub use interactions::{
    INTERACTION_INTERVAL_MS, InteractionPayload, InteractionRejection, InteractionSender,
    MAX_CHAT_CODE_POINTS, ROOM_REACTION_IDS, RoomInteractionMessage, SendRoomInteractionMessage,
    valid_chat_text,
};
pub use punch::*;
pub use relay_volunteer::*;
pub use remote_input::*;
pub use scalars::{
    DisplayName, is_js_whitespace, normalize_display_name, utf16_length, valid_display_name,
    valid_opaque_id, valid_room_code, valid_stun_url, valid_token, valid_viewer_grant,
    valid_viewer_password,
};
pub use server::*;
pub use sfu::*;
pub use types::*;
pub use viewer_route::*;
pub use voice::*;
