//! Incoming Viewer packets: request, release and authorized input dispatch.

use super::*;

impl Inner {
    /// Caller holds the lock from delivery validation through the input action.
    pub fn receive_locked(&mut self, key: &Key, plain: &[u8]) {
        if plain.len() > MAX_INPUT_BYTES || !self.connections.contains_key(key) {
            return;
        }
        let Ok(event) = serde_json::from_slice::<InputEvent>(plain) else {
            return;
        };
        self.diagnostics.parsed_events += 1;
        if let Some(family) = self.input_family(&event.kind) {
            family.parsed += 1;
        }
        let session = key.0;
        match event.kind.as_str() {
            "sync" => self.send_state(key),
            "request" => {
                let c = &self.connections[key];
                let mode = self.mode(session);
                if !self.available(session)
                    || mode == Mode::Off
                    || c.requested
                    || !c.grant.is_empty()
                {
                    return;
                }
                if hex::decode(&event.intent).map_or(true, |intent| intent.len() != 16) {
                    return;
                }
                let c = self.connections.get_mut(key).expect("known connection");
                c.request_id = random_hex(16);
                c.intent = event.intent;
                if mode != Mode::Ask {
                    if let Some(reason) = self.authorize(session, &event.password) {
                        self.reply(key, "ready", reason);
                        return;
                    }
                    // Automatic permission never displaces another controller,
                    // including a controller of the App's other native session.
                    if !self.can_join(key) {
                        self.reply(key, "ready", "busy");
                        return;
                    }
                    let _ = self.grant(key);
                    return;
                }
                self.connections
                    .get_mut(key)
                    .expect("known connection")
                    .requested = true;
                self.send(key, "pending");
                self.notify(session);
            }
            "release" => {
                let c = self.connections.get_mut(key).expect("known connection");
                if event.intent != c.intent || (!c.requested && c.grant.is_empty()) {
                    return;
                }
                c.requested = false;
                self.release_connection(key);
                self.send(key, "ready");
                self.notify(session);
            }
            _ => self.receive_input(key, event),
        }
    }

    fn receive_input(&mut self, key: &Key, event: InputEvent) {
        let session = key.0;
        let c = &self.connections[key];
        if c.grant.is_empty() || event.grant != c.grant || !self.available(session) {
            return;
        }
        if let Some(family) = self.input_family(&event.kind) {
            family.authorized += 1;
        }
        if matches!(
            event.kind.as_str(),
            "gamepad" | "gamepad-reset" | "gamepad-stop" | "gamepad-retry"
        ) {
            self.receive_gamepad(key, &event);
            return;
        }
        if event.gamepad.is_some() {
            return;
        }
        if event.kind == "reset" {
            self.receive_gamepad(key, &InputEvent::of("gamepad-reset"));
            let failed = self.clear_held(key).is_err();
            self.note_input_result(key, failed);
            return;
        }
        if event.kind == "text"
            && self.co_play(session)
            && self.single_controller(session).as_ref() != Some(key)
        {
            // Committed text resets the sink and would release other players.
            return;
        }
        let (bounds, focused) = self.refresh_active();
        if !focused {
            return;
        }
        if let Some(family) = self.input_family(&event.kind) {
            family.target_ready += 1;
        }
        let result = if event.kind == "text" {
            apply_text(
                &mut ControllerText { inner: self, key },
                bounds,
                &event.text,
            )
        } else {
            self.apply_held(key, bounds, &event)
        };
        if let Err(error) = &result
            && let Some(c) = self.connections.get(key)
        {
            let source_kind = self
                .sessions
                .get(&session)
                .map(|s| s.target.kind.clone())
                .unwrap_or_default();
            tracing::debug!(
                event = "apply-failed",
                connectionId = %piik_diagnostics::id(&c.edge_id),
                inputGeneration = %piik_diagnostics::id(&c.epoch),
                operation = %event.kind,
                sourceKind = %source_kind,
                targetReady = focused && bounds.width > 0 && bounds.height > 0,
                error = %error,
                "remote-input"
            );
        }
        self.note_input_result(key, result.is_err());
    }
}
