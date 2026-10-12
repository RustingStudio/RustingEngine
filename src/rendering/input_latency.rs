//! Input-to-photon latency as far as the engine can see it.
//!
//! A sample runs from the oldest press not yet drawn to the moment the
//! frame that first saw it is handed to the presentation engine, plus that
//! frame's GPU time. The display's scan-out and the monitor's own delay are
//! not included; measuring those needs a camera or a photodiode.

use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct InputLatency {
    pending: Option<Instant>,
    /// Latencies since the last [`InputLatency::take_samples`], in ms.
    samples: Vec<f64>,
}

impl InputLatency {
    /// Notes a key or button press. Later presses before the next frame
    /// share the oldest one's sample.
    pub fn record_input(&mut self, at: Instant) {
        self.pending.get_or_insert(at);
    }

    /// Closes the pending sample: the frame that read the press was
    /// presented at `at` and took `gpu` on the GPU.
    pub fn frame_presented(&mut self, at: Instant, gpu: Duration) {
        if let Some(input) = self.pending.take() {
            let latency = at.saturating_duration_since(input) + gpu;
            self.samples.push(latency.as_secs_f64() * 1000.0);
        }
    }

    pub fn take_samples(&mut self) -> Vec<f64> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_spans_the_oldest_press_to_its_presented_frame() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut latency = InputLatency::default();
        // A frame with no press makes no sample.
        latency.frame_presented(start, ms(2));
        latency.record_input(start + ms(1));
        latency.record_input(start + ms(4));
        latency.frame_presented(start + ms(10), ms(3));
        // The next frame has nothing pending.
        latency.frame_presented(start + ms(20), ms(3));
        latency.record_input(start + ms(25));
        latency.frame_presented(start + ms(30), ms(1));
        assert_eq!(latency.take_samples(), [12.0, 6.0]);
        assert!(latency.take_samples().is_empty());
    }
}
