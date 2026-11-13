//! Keyframe-request throttling per spatial layer: a forced request (source
//! replacement, new subscriber) always passes and restarts the interval.

use std::time::{Duration, Instant};

use crate::layer::MAX_SPATIAL_LAYERS;

/// Default screen-share keyframe-request throttle for each quality.
pub const DEFAULT_PLI_THROTTLE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct PliThrottle {
    interval: Duration,
    last: [Option<Instant>; MAX_SPATIAL_LAYERS],
}

impl Default for PliThrottle {
    fn default() -> Self {
        Self::new(DEFAULT_PLI_THROTTLE)
    }
}

impl PliThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: [None; MAX_SPATIAL_LAYERS],
        }
    }

    /// Returns whether the owner should send a keyframe request for `layer` now.
    pub fn request(&mut self, layer: usize, force: bool, now: Instant) -> bool {
        let Some(last) = self.last.get_mut(layer) else {
            return false;
        };
        if !force && last.is_some_and(|sent| now.saturating_duration_since(sent) < self.interval) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// A keyframe arrived; the next request need not wait for the interval.
    pub fn keyframe_received(&mut self, layer: usize) {
        if let Some(last) = self.last.get_mut(layer) {
            *last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_each_layer_independently_and_forced_requests_pass() {
        let start = Instant::now();
        let mut throttle = PliThrottle::new(Duration::from_millis(500));
        assert!(throttle.request(0, false, start));
        assert!(!throttle.request(0, false, start + Duration::from_millis(100)));
        assert!(throttle.request(1, false, start + Duration::from_millis(100)));
        assert!(throttle.request(0, true, start + Duration::from_millis(200)));
        assert!(!throttle.request(0, false, start + Duration::from_millis(600)));
        assert!(throttle.request(0, false, start + Duration::from_millis(700)));
        throttle.keyframe_received(0);
        assert!(throttle.request(0, false, start + Duration::from_millis(701)));
        assert!(!throttle.request(MAX_SPATIAL_LAYERS, true, start));
    }
}
