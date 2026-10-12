use std::sync::Arc;
use std::time::{Duration, Instant};

use vulkano::device::Queue;
use vulkano::swapchain::{PresentMode, Surface};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::Window;

use crate::runtime::RenderSettings;

/// Selects the fastest available non-VSync mode, while retaining Vulkan's
/// guaranteed FIFO fallback on devices that expose no optional present mode.
#[must_use]
pub fn select_present_mode(
    queue: &Arc<Queue>,
    surface: &Arc<Surface>,
    vsync: bool,
) -> PresentMode {
    let Ok(modes) = queue
        .device()
        .physical_device()
        .surface_present_modes(surface, Default::default())
    else {
        return PresentMode::Fifo;
    };
    pick_present_mode(&modes, vsync)
}

/// The present mode [`select_present_mode`] takes from the modes a surface
/// supports: FIFO first with VSync, otherwise Immediate, then Mailbox, and
/// FIFO, which every surface has, when no preferred mode is there.
#[must_use]
pub fn pick_present_mode(modes: &[PresentMode], vsync: bool) -> PresentMode {
    let preferences = if vsync {
        [
            PresentMode::Fifo,
            PresentMode::Mailbox,
            PresentMode::Immediate,
        ]
    } else {
        [
            PresentMode::Immediate,
            PresentMode::Mailbox,
            PresentMode::Fifo,
        ]
    };
    preferences
        .into_iter()
        .find(|mode| modes.contains(mode))
        .unwrap_or(PresentMode::Fifo)
}

/// Winit-side optional FPS limiter. Unlimited mode continuously polls; limited
/// mode sleeps through `ControlFlow::WaitUntil` without blocking render work.
pub struct FramePacer {
    next_frame: Instant,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self {
            next_frame: Instant::now(),
        }
    }
}

impl FramePacer {
    pub fn request_next_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Window,
        settings: &RenderSettings,
    ) {
        if !settings.limit_fps {
            event_loop.set_control_flow(ControlFlow::Poll);
            self.next_frame = Instant::now();
            window.request_redraw();
            return;
        }

        let interval =
            Duration::from_secs_f64(1.0 / f64::from(settings.max_fps.max(1)));
        match self.due(Instant::now(), interval) {
            Ok(()) => window.request_redraw(),
            Err(deadline) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
        }
    }

    /// Whether a frame is due at `now`, or else the instant to wait for.
    /// Deadlines step by whole intervals from the last one, so a late wake
    /// or a slow frame does not push every later frame back. A stall longer
    /// than one interval restarts the schedule instead of bursting frames
    /// to catch up.
    fn due(&mut self, now: Instant, interval: Duration) -> Result<(), Instant> {
        if now < self.next_frame {
            return Err(self.next_frame);
        }
        self.next_frame += interval;
        if self.next_frame <= now {
            self.next_frame = now + interval;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_mode_falls_back_to_what_the_surface_supports() {
        let all = [
            PresentMode::Fifo,
            PresentMode::Mailbox,
            PresentMode::Immediate,
        ];
        assert_eq!(pick_present_mode(&all, true), PresentMode::Fifo);
        assert_eq!(pick_present_mode(&all, false), PresentMode::Immediate);
        // No Immediate: tear-free Mailbox is the next fastest.
        let no_immediate = [PresentMode::Fifo, PresentMode::Mailbox];
        assert_eq!(
            pick_present_mode(&no_immediate, false),
            PresentMode::Mailbox
        );
        // FIFO only, as many mobile and Wayland surfaces offer.
        assert_eq!(
            pick_present_mode(&[PresentMode::Fifo], false),
            PresentMode::Fifo
        );
        // A surface that reports no mode still gets FIFO.
        assert_eq!(pick_present_mode(&[], false), PresentMode::Fifo);
        // A VSync request on a surface without FIFO in its list.
        assert_eq!(
            pick_present_mode(&[PresentMode::Mailbox], true),
            PresentMode::Mailbox
        );
    }

    #[test]
    fn the_limiter_holds_its_rate_through_late_wakes_and_uneven_frames() {
        let interval = Duration::from_millis(10);
        let start = Instant::now();
        let mut pacer = FramePacer { next_frame: start };
        let mut now = start;
        let mut frames = Vec::new();
        for frame in 0..200u64 {
            // The OS wakes the loop 0.5 ms after the deadline it asked for.
            while let Err(deadline) = pacer.due(now, interval) {
                now = deadline + Duration::from_micros(500);
            }
            frames.push(now - start);
            // CPU and GPU work from 1 to 9.1 ms, and one 35 ms hitch.
            now += if frame == 100 {
                Duration::from_millis(35)
            } else {
                Duration::from_micros(1000 + frame * 37 % 8100)
            };
        }
        // Before the hitch, frame k starts 0.5 ms after k intervals.
        for (k, at) in frames[1..=100].iter().enumerate() {
            let k = k as u32 + 1;
            assert_eq!(*at, interval * k + Duration::from_micros(500), "{k}");
        }
        // The hitch restarts the schedule: no burst of catch-up frames.
        let gaps: Vec<_> = frames.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|gap| *gap >= interval), "{gaps:?}");
        assert_eq!(gaps[100], Duration::from_millis(35));
        assert!(gaps[102..].iter().all(|gap| *gap == interval), "{gaps:?}");
    }
}
