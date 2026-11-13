//! Count-only remote input diagnostics.
//!
//! Incoming events and sink apply calls are separate counts: held-input
//! deduplication can avoid a call, while text and release cleanup can make
//! several. API acceptance is the apply result, never proof of OS delivery.
//! Reset calls are not expanded into inferred per-key/button operations.

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::event::{Bounds, InputEvent};
use crate::manager::Inner;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticFamilyCounters {
    pub parsed: u64,
    pub authorized: u64,
    pub target_ready: u64,
    pub api_attempts: u64,
    pub api_accepted: u64,
    pub api_rejected: u64,
}

/// `received_packets` counts current, known-endpoint payloads, not raw channel
/// traffic. Packet type is unknown before decryption and parsing; those totals
/// include control and gamepad messages, while family counters cover keyboard
/// and mouse actions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCounters {
    pub received_packets: u64,
    pub decrypted_packets: u64,
    pub parsed_events: u64,
    pub mouse: DiagnosticFamilyCounters,
    pub keyboard: DiagnosticFamilyCounters,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSnapshot {
    pub counters: DiagnosticCounters,
    pub active_grants: usize,
    pub suspended_grants: usize,
    pub pending_key_releases: usize,
    pub pending_button_releases: usize,
}

impl Inner {
    pub fn snapshot(&self) -> DiagnosticSnapshot {
        let mut result = DiagnosticSnapshot {
            counters: self.diagnostics,
            pending_key_releases: self.pending_keys.len(),
            pending_button_releases: self.pending_buttons.len(),
            ..DiagnosticSnapshot::default()
        };
        for c in self.connections.values().filter(|c| !c.grant.is_empty()) {
            if c.suspended {
                result.suspended_grants += 1;
            } else {
                result.active_grants += 1;
            }
        }
        result
    }

    pub fn input_family(&mut self, kind: &str) -> Option<&mut DiagnosticFamilyCounters> {
        match kind {
            "move" | "relative" | "button" | "wheel" => Some(&mut self.diagnostics.mouse),
            "key" | "text" => Some(&mut self.diagnostics.keyboard),
            _ => None,
        }
    }

    pub fn apply_input(&mut self, bounds: Bounds, event: &InputEvent) -> Result<(), Error> {
        if let Some(family) = self.input_family(&event.kind) {
            family.api_attempts += 1;
        }
        let result = self.sink.apply(bounds, event);
        if let Some(family) = self.input_family(&event.kind) {
            if result.is_ok() {
                family.api_accepted += 1;
            } else {
                family.api_rejected += 1;
            }
        }
        result
    }
}
