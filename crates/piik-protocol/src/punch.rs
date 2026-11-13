//! Connection preparation (UDP hole punching) signals.

use std::net::Ipv4Addr;

use serde_json::Value;

use crate::codec::{FromWire, Obj, Result, ToWire, invalid, object, plain_int};

/// `punchPredictionSchema`: one IPv4 mapping observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PunchPrediction {
    pub address: String,
    pub port: i64,
    pub step: i64,
}

impl FromWire for PunchPrediction {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["address", "port", "step"])?;
        let prediction = Self {
            address: f.string("address")?,
            port: f.get("port").map_or(Ok(0), plain_int)?,
            step: f.get("step").map_or(Ok(0), plain_int)?,
        };
        f.require(&["address", "port", "step"])?;
        let Self {
            address,
            port,
            step,
        } = &prediction;
        // Ipv4Addr rejects leading zeros and IPv6 forms, like netip's Is4.
        if address.parse::<Ipv4Addr>().is_err()
            || !(1024..=65535).contains(port)
            || !(0..=16).contains(step)
            || port + 4 * step > 65535
        {
            return Err(invalid("invalid mapping observation"));
        }
        Ok(prediction)
    }
}

impl ToWire for PunchPrediction {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("address", &self.address)
            .put("port", &self.port)
            .put("step", &self.step)
            .build()
    }
}

/// `punchSignalSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PunchSignal {
    /// `{ phase: "prepare", secret }`: 32 bytes of lowercase hex.
    Prepare { secret: String },
    /// `{ phase: "ready", prediction? }`.
    Ready { prediction: Option<PunchPrediction> },
    /// `{ phase: "start", prediction }`.
    Start { prediction: PunchPrediction },
    /// `{ phase: "complete", connected }`.
    Complete { connected: bool },
}

/// Exactly `bytes` bytes of lowercase hexadecimal.
pub(crate) fn canonical_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl FromWire for PunchSignal {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["phase", "secret", "prediction", "connected"])?;
        let phase = f.string("phase")?;
        let secret = f.string("secret")?;
        let prediction = f.opt::<PunchPrediction>("prediction")?;
        let connected = f.opt::<bool>("connected")?;
        f.require(&["phase"])?;
        let signal = match (phase.as_str(), f.len(), prediction, connected) {
            ("prepare", 2, _, _) if canonical_hex(&secret, 32) => Some(Self::Prepare { secret }),
            ("ready", 1, _, _) => Some(Self::Ready { prediction: None }),
            ("ready", 2, Some(prediction), _) => Some(Self::Ready {
                prediction: Some(prediction),
            }),
            ("start", 2, Some(prediction), _) => Some(Self::Start { prediction }),
            ("complete", 2, _, Some(connected)) => Some(Self::Complete { connected }),
            _ => None,
        };
        signal.ok_or_else(|| invalid("invalid connection preparation phase"))
    }
}

impl ToWire for PunchSignal {
    fn to_wire(&self) -> Value {
        match self {
            Self::Prepare { secret } => Obj::new().put("phase", "prepare").put("secret", secret),
            Self::Ready { prediction } => Obj::new()
                .put("phase", "ready")
                .put_some("prediction", prediction),
            Self::Start { prediction } => Obj::new()
                .put("phase", "start")
                .put("prediction", prediction),
            Self::Complete { connected } => Obj::new()
                .put("phase", "complete")
                .put("connected", connected),
        }
        .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode, encode};
    use crate::types::SignalPayload;

    #[test]
    fn punch_signal_strict_bounded_round_trip() {
        let prediction = r#"{"address":"198.51.100.1","port":40000,"step":1}"#;
        let secret = "42".repeat(32);
        for body in [
            format!(r#"{{"phase":"prepare","secret":"{secret}"}}"#),
            r#"{"phase":"ready"}"#.to_owned(),
            format!(r#"{{"phase":"ready","prediction":{prediction}}}"#),
            format!(r#"{{"phase":"start","prediction":{prediction}}}"#),
            r#"{"phase":"start","prediction":{"address":"198.51.100.1","port":65535,"step":0}}"#
                .to_owned(),
            r#"{"phase":"complete","connected":false}"#.to_owned(),
            r#"{"phase":"complete","connected":true}"#.to_owned(),
        ] {
            let raw =
                format!(r#"{{"kind":"punch","connectionId":"connection_123456","punch":{body}}}"#);
            let payload: SignalPayload = decode(raw.as_bytes()).expect(&body);
            assert_eq!(String::from_utf8(encode(&payload)).unwrap(), raw);
        }
        for body in [
            r#"{"phase":"prepare","secret":"bad"}"#.to_owned(),
            r#"{"phase":"prepare","secret":null}"#.to_owned(),
            r#"{"phase":"prepare","secret":"4242"}"#.to_owned(),
            format!(r#"{{"phase":"prepare","secret":"{}"}}"#, "AB".repeat(32)),
            r#"{"phase":"ready","prediction":null}"#.to_owned(),
            r#"{"phase":"ready","connected":true}"#.to_owned(),
            r#"{"phase":"start"}"#.to_owned(),
            r#"{"phase":"complete"}"#.to_owned(),
            r#"{"phase":"complete","connected":null}"#.to_owned(),
            format!(r#"{{"phase":"ready","prediction":{prediction},"secret":"unused"}}"#),
            r#"{"phase":"start","prediction":{"address":"::1","port":40000,"step":1}}"#.to_owned(),
            r#"{"phase":"start","prediction":{"address":"198.51.100.1","port":65535,"step":1}}"#
                .to_owned(),
            r#"{"phase":"start","prediction":{"address":"198.51.100.1","port":40000,"step":17}}"#
                .to_owned(),
            r#"{"phase":"start","prediction":{"address":"198.51.100.1","port":40000,"step":-1}}"#
                .to_owned(),
            r#"{"phase":"start","prediction":{"address":"198.51.100.01","port":40000,"step":1}}"#
                .to_owned(),
            r#"{"phase":"start","prediction":{"address":"198.51.100.1","port":40000.0,"step":1}}"#
                .to_owned(),
        ] {
            assert!(
                decode::<PunchSignal>(body.as_bytes()).is_err(),
                "accepted {body}"
            );
        }
    }
}
