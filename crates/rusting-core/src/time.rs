use std::time::Duration;
use std::{error::Error, fmt};

use bevy_ecs::prelude::Resource;

/// Frame and simulation timing visible to systems.
#[derive(Resource, Clone, Copy, Debug)]
pub struct FrameTime {
    /// Number of successful application updates since the app started.
    pub frame: u64,
    /// Real time since the previous frame, even while paused.
    pub real_delta: Duration,
    /// Game time since the previous frame after pause and time scale.
    /// Inside a fixed-schedule execution it is [`Self::fixed_delta`].
    pub delta: Duration,
    /// Total game time, without time spent paused.
    pub elapsed: Duration,
    /// Time simulated by one fixed-schedule execution.
    pub fixed_delta: Duration,
    /// Number of completed fixed-schedule executions.
    pub fixed_tick: u64,
}

impl Default for FrameTime {
    fn default() -> Self {
        Self {
            frame: 0,
            real_delta: Duration::ZERO,
            delta: Duration::ZERO,
            elapsed: Duration::ZERO,
            fixed_delta: Duration::from_secs_f64(1.0 / 60.0),
            fixed_tick: 0,
        }
    }
}

impl FrameTime {
    /// Returns [`Self::delta`] in seconds: the frame's game time, or the
    /// fixed step inside a fixed-schedule execution such as a game's
    /// `tick` function.
    #[must_use]
    pub fn delta_seconds(&self) -> f32 {
        self.delta.as_secs_f32()
    }

    /// Returns total running game time as an easy-to-use `f32` value.
    #[must_use]
    pub fn elapsed_seconds(&self) -> f32 {
        self.elapsed.as_secs_f32()
    }
}

/// Pause, stepping, scale, and fixed-schedule catch-up controls.
#[derive(Resource, Clone, Debug)]
pub struct TimeControl {
    /// Whether variable time and ordinary fixed-schedule executions are paused.
    pub paused: bool,
    /// Multiplier applied to real time while the simulation is running.
    pub time_scale: f64,
    /// Time simulated by one fixed-schedule execution.
    pub fixed_delta: Duration,
    /// Maximum number of accumulated fixed-schedule executions run per application update.
    pub max_fixed_steps: u32,
    accumulator: Duration,
    pending_steps: u32,
}

impl Default for TimeControl {
    fn default() -> Self {
        Self {
            paused: false,
            time_scale: 1.0,
            fixed_delta: Duration::from_secs_f64(1.0 / 60.0),
            max_fixed_steps: 8,
            accumulator: Duration::ZERO,
            pending_steps: 0,
        }
    }
}

impl TimeControl {
    /// Pauses variable time and ordinary fixed-schedule executions.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Resumes variable time and ordinary fixed-schedule executions.
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Time accumulated toward the next fixed step: how far real time has
    /// run past the last completed tick.
    #[must_use]
    pub fn accumulator(&self) -> Duration {
        self.accumulator
    }

    /// Queues one fixed-schedule execution to be consumed while paused.
    pub fn step(&mut self) {
        self.pending_steps = self.pending_steps.saturating_add(1);
    }
}

/// Errors produced while advancing simulation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeAdvanceError {
    /// The configured fixed timestep is zero.
    InvalidFixedDelta,
}

impl fmt::Display for TimeAdvanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFixedDelta => {
                formatter.write_str("fixed delta must be greater than zero")
            }
        }
    }
}

impl Error for TimeAdvanceError {}

/// Advances frame and fixed-schedule bookkeeping by one application update.
///
/// Catch-up work is capped by [`TimeControl::max_fixed_steps`]. Remaining
/// accumulated time is retained for later updates, up to one more capped
/// update's worth; older backlog is dropped. A queued step is
/// consumed only while paused and advances elapsed time by exactly one fixed
/// timestep without consuming the accumulator.
///
/// # Errors
///
/// Returns [`TimeAdvanceError::InvalidFixedDelta`] when `control.fixed_delta`
/// is zero, before mutating either the supplied [`TimeControl`] or
/// [`FrameTime`].
///
/// # Panics
///
/// After fixed-delta validation, when the simulation is not paused, this may
/// panic if `control.time_scale` is positive and non-finite, or if scaling
/// `real_delta` by the non-negative time scale cannot be represented by
/// [`Duration::mul_f64`]. Negative and `NaN` time scales are clamped to zero
/// and do not panic.
pub fn advance(
    control: &mut TimeControl,
    time: &mut FrameTime,
    real_delta: Duration,
) -> Result<u32, TimeAdvanceError> {
    if control.fixed_delta.is_zero() {
        return Err(TimeAdvanceError::InvalidFixedDelta);
    }

    let scaled_delta = if control.paused {
        Duration::ZERO
    } else {
        real_delta.mul_f64(control.time_scale.max(0.0))
    };
    control.accumulator = control.accumulator.saturating_add(scaled_delta);

    let mut fixed_steps = 0;
    let fixed_delta = control.fixed_delta;
    while control.accumulator >= fixed_delta
        && fixed_steps < control.max_fixed_steps
        && !control.paused
    {
        control.accumulator -= fixed_delta;
        fixed_steps += 1;
    }
    if !control.paused && control.max_fixed_steps > 0 {
        // Keep at most one more capped update of backlog, so one long stall
        // does not make many later frames run the maximum number of steps.
        control.accumulator = control
            .accumulator
            .min(fixed_delta.saturating_mul(control.max_fixed_steps));
    }
    let stepped_while_paused = control.paused && control.pending_steps > 0;
    if stepped_while_paused {
        control.pending_steps -= 1;
        fixed_steps = 1;
    }
    let elapsed_delta = if stepped_while_paused {
        fixed_delta
    } else {
        scaled_delta
    };

    time.frame = time.frame.saturating_add(1);
    time.real_delta = real_delta;
    time.delta = scaled_delta;
    time.elapsed = time.elapsed.saturating_add(elapsed_delta);
    time.fixed_delta = fixed_delta;
    time.fixed_tick = time.fixed_tick.saturating_add(u64::from(fixed_steps));
    Ok(fixed_steps)
}

/// Seed for gameplay randomness. Draw values with [`RandomSeed::value`],
/// indexed by the fixed tick and a stream from [`RandomSeed::stream`], so a
/// run repeats exactly for the same seed. Scenario tests set the seed they
/// record. Simulation code takes all its randomness from here, never from a
/// global or thread-local generator.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RandomSeed(pub u64);

impl RandomSeed {
    /// Uniform 64-bit value for `stream` at fixed `tick`. The same seed,
    /// tick and stream always give the same value.
    #[must_use]
    pub fn value(self, tick: u64, stream: u64) -> u64 {
        splitmix64(self.0 ^ splitmix64(tick ^ splitmix64(stream)))
    }

    /// Stream `key` of the subsystem called `subsystem`, such as
    /// `"bursts"`. The name is hashed in, so two subsystems that pick the
    /// same keys still draw independent values.
    #[must_use]
    pub fn stream(subsystem: &str, key: u64) -> u64 {
        // FNV-1a over the name.
        let name =
            subsystem
                .bytes()
                .fold(0xCBF2_9CE4_8422_2325_u64, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
                });
        splitmix64(name ^ splitmix64(key))
    }

    /// Uniform value in `[0, 1)` for `stream` at fixed `tick`.
    #[must_use]
    pub fn unit(self, tick: u64, stream: u64) -> f32 {
        (self.value(tick, stream) >> 40) as f32 / (1_u64 << 24) as f32
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy_ecs::prelude::Resource;

    use super::{
        advance, FrameTime, RandomSeed, TimeAdvanceError, TimeControl,
    };

    #[test]
    fn default_starts_at_frame_zero_with_sixty_hz_fixed_delta() {
        let time = FrameTime::default();

        assert_eq!(time.frame, 0);
        assert_eq!(time.real_delta, Duration::ZERO);
        assert_eq!(time.delta, Duration::ZERO);
        assert_eq!(time.elapsed, Duration::ZERO);
        assert_eq!(time.fixed_delta, Duration::from_secs_f64(1.0 / 60.0));
        assert_eq!(time.fixed_tick, 0);
    }

    #[test]
    fn duration_accessors_return_seconds() {
        let time = FrameTime {
            delta: Duration::from_millis(125),
            elapsed: Duration::from_millis(2_500),
            ..FrameTime::default()
        };

        assert_eq!(time.delta_seconds(), 0.125);
        assert_eq!(time.elapsed_seconds(), 2.5);
    }

    #[test]
    fn frame_time_is_an_ecs_resource() {
        fn assert_resource<T: Resource>() {}

        assert_resource::<FrameTime>();
    }

    #[test]
    fn frame_time_is_clone_and_copy() {
        fn assert_clone_copy<T: Clone + Copy>() {}

        assert_clone_copy::<FrameTime>();
    }

    #[test]
    fn time_control_defaults_and_public_traits_are_stable() {
        fn assert_resource_clone_debug<
            T: Resource + Clone + std::fmt::Debug,
        >() {
        }
        fn assert_error_traits<T: std::error::Error + Clone + Copy + Eq>() {}

        assert_resource_clone_debug::<TimeControl>();
        assert_error_traits::<TimeAdvanceError>();

        let control = TimeControl::default();
        assert!(!control.paused);
        assert_eq!(control.time_scale, 1.0);
        assert_eq!(control.fixed_delta, Duration::from_secs_f64(1.0 / 60.0));
        assert_eq!(control.max_fixed_steps, 8);
        assert_eq!(control.accumulator, Duration::ZERO);
        assert_eq!(control.pending_steps, 0);
        assert_eq!(
            TimeAdvanceError::InvalidFixedDelta.to_string(),
            "fixed delta must be greater than zero"
        );
    }

    #[test]
    fn normal_scaled_advance_updates_all_bookkeeping() {
        let mut control = TimeControl {
            time_scale: 0.5,
            fixed_delta: Duration::from_millis(10),
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_millis(25)),
            Ok(1)
        );
        assert_eq!(control.accumulator, Duration::from_micros(2_500));
        assert_eq!(time.frame, 1);
        assert_eq!(time.real_delta, Duration::from_millis(25));
        assert_eq!(time.delta, Duration::from_micros(12_500));
        assert_eq!(time.elapsed, Duration::from_micros(12_500));
        assert_eq!(time.fixed_delta, Duration::from_millis(10));
        assert_eq!(time.fixed_tick, 1);
    }

    #[test]
    fn catch_up_cap_retains_backlog_for_later_frames() {
        let mut control = TimeControl {
            fixed_delta: Duration::from_millis(10),
            max_fixed_steps: 2,
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_millis(35)),
            Ok(2)
        );
        assert_eq!(control.accumulator, Duration::from_millis(15));
        assert_eq!(advance(&mut control, &mut time, Duration::ZERO), Ok(1));
        assert_eq!(control.accumulator, Duration::from_millis(5));
        assert_eq!(time.fixed_tick, 3);
        assert_eq!(time.elapsed, Duration::from_millis(35));
    }

    #[test]
    fn pause_drains_one_queued_step_per_update_and_resume_restores_time() {
        let mut control = TimeControl {
            fixed_delta: Duration::from_millis(10),
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();
        control.pause();
        control.step();
        control.step();

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_secs(1)),
            Ok(1)
        );
        assert_eq!(time.delta, Duration::ZERO);
        assert_eq!(time.elapsed, Duration::from_millis(10));
        assert_eq!(control.pending_steps, 1);
        assert_eq!(advance(&mut control, &mut time, Duration::ZERO), Ok(1));
        assert_eq!(advance(&mut control, &mut time, Duration::ZERO), Ok(0));

        control.resume();
        assert_eq!(
            advance(&mut control, &mut time, Duration::from_millis(10)),
            Ok(1)
        );
        assert_eq!(time.elapsed, Duration::from_millis(30));
        assert_eq!(time.fixed_tick, 3);
    }

    #[test]
    fn steps_queued_while_running_persist_until_paused() {
        let mut control = TimeControl {
            fixed_delta: Duration::from_millis(10),
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();
        control.step();

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_millis(10)),
            Ok(1)
        );
        assert_eq!(control.pending_steps, 1);
        control.pause();
        assert_eq!(advance(&mut control, &mut time, Duration::ZERO), Ok(1));
        assert_eq!(control.pending_steps, 0);
    }

    #[test]
    fn negative_and_nan_time_scales_clamp_to_zero() {
        for scale in [-1.0, f64::NAN] {
            let mut control = TimeControl {
                time_scale: scale,
                fixed_delta: Duration::from_millis(10),
                ..TimeControl::default()
            };
            let mut time = FrameTime::default();

            assert_eq!(
                advance(&mut control, &mut time, Duration::from_secs(1)),
                Ok(0)
            );
            assert_eq!(control.accumulator, Duration::ZERO);
            assert_eq!(time.delta, Duration::ZERO);
            assert_eq!(time.elapsed, Duration::ZERO);
        }
    }

    #[test]
    #[should_panic]
    fn positive_infinite_time_scale_panics_while_running() {
        let mut control = TimeControl {
            time_scale: f64::INFINITY,
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();

        let _ = advance(&mut control, &mut time, Duration::from_secs(1));
    }

    #[test]
    fn long_stall_backlog_is_bounded_to_one_capped_update() {
        let mut control = TimeControl {
            fixed_delta: Duration::from_millis(10),
            max_fixed_steps: 2,
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_secs(5)),
            Ok(2)
        );
        assert_eq!(control.accumulator, Duration::from_millis(20));
        assert_eq!(advance(&mut control, &mut time, Duration::ZERO), Ok(2));
        assert_eq!(advance(&mut control, &mut time, Duration::ZERO), Ok(0));
    }

    #[test]
    fn zero_max_fixed_steps_retains_the_entire_backlog() {
        let mut control = TimeControl {
            fixed_delta: Duration::from_millis(10),
            max_fixed_steps: 0,
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_millis(25)),
            Ok(0)
        );
        assert_eq!(control.accumulator, Duration::from_millis(25));
        assert_eq!(time.elapsed, Duration::from_millis(25));
        assert_eq!(time.fixed_tick, 0);
    }

    #[test]
    fn zero_fixed_delta_errors_before_mutating_either_resource() {
        let mut control = TimeControl {
            paused: true,
            time_scale: 2.0,
            fixed_delta: Duration::ZERO,
            max_fixed_steps: 3,
            accumulator: Duration::from_millis(7),
            pending_steps: 2,
        };
        let mut time = FrameTime {
            frame: 4,
            real_delta: Duration::from_millis(1),
            delta: Duration::from_millis(2),
            elapsed: Duration::from_millis(3),
            fixed_delta: Duration::from_millis(4),
            fixed_tick: 5,
        };

        assert_eq!(
            advance(&mut control, &mut time, Duration::from_secs(1)),
            Err(TimeAdvanceError::InvalidFixedDelta)
        );
        assert!(control.paused);
        assert_eq!(control.time_scale, 2.0);
        assert_eq!(control.fixed_delta, Duration::ZERO);
        assert_eq!(control.max_fixed_steps, 3);
        assert_eq!(control.accumulator, Duration::from_millis(7));
        assert_eq!(control.pending_steps, 2);
        assert_eq!(time.frame, 4);
        assert_eq!(time.real_delta, Duration::from_millis(1));
        assert_eq!(time.delta, Duration::from_millis(2));
        assert_eq!(time.elapsed, Duration::from_millis(3));
        assert_eq!(time.fixed_delta, Duration::from_millis(4));
        assert_eq!(time.fixed_tick, 5);
    }

    #[test]
    fn paused_updates_and_steps_preserve_accumulated_partial_time() {
        let mut control = TimeControl {
            fixed_delta: Duration::from_millis(10),
            ..TimeControl::default()
        };
        let mut time = FrameTime::default();
        advance(&mut control, &mut time, Duration::from_millis(6)).unwrap();
        control.pause();
        advance(&mut control, &mut time, Duration::from_secs(1)).unwrap();
        control.step();
        advance(&mut control, &mut time, Duration::ZERO).unwrap();
        assert_eq!(control.accumulator, Duration::from_millis(6));

        control.resume();
        assert_eq!(
            advance(&mut control, &mut time, Duration::from_millis(4)),
            Ok(1)
        );
        assert_eq!(control.accumulator, Duration::ZERO);
    }

    #[test]
    fn counters_durations_and_step_queue_saturate() {
        let mut control = TimeControl {
            paused: true,
            fixed_delta: Duration::from_nanos(1),
            accumulator: Duration::MAX,
            pending_steps: u32::MAX,
            ..TimeControl::default()
        };
        control.step();
        assert_eq!(control.pending_steps, u32::MAX);

        let mut time = FrameTime {
            frame: u64::MAX,
            elapsed: Duration::MAX,
            fixed_tick: u64::MAX,
            ..FrameTime::default()
        };
        assert_eq!(advance(&mut control, &mut time, Duration::MAX), Ok(1));
        assert_eq!(control.accumulator, Duration::MAX);
        assert_eq!(control.pending_steps, u32::MAX - 1);
        assert_eq!(time.frame, u64::MAX);
        assert_eq!(time.elapsed, Duration::MAX);
        assert_eq!(time.fixed_tick, u64::MAX);
    }

    #[test]
    fn random_seed_repeats_per_seed_tick_and_stream() {
        let seed = RandomSeed(42);
        assert_eq!(seed.value(3, 1), RandomSeed(42).value(3, 1));
        assert_ne!(seed.value(3, 1), seed.value(4, 1));
        assert_ne!(seed.value(3, 1), seed.value(3, 2));
        assert_ne!(seed.value(3, 1), RandomSeed(43).value(3, 1));
        let units: Vec<_> = (0..1000).map(|tick| seed.unit(tick, 0)).collect();
        assert!(units.iter().all(|unit| (0.0..1.0).contains(unit)));
        let mean = units.iter().sum::<f32>() / units.len() as f32;
        assert!((0.45..0.55).contains(&mean), "{mean}");
    }

    #[test]
    fn subsystem_streams_are_independent_and_fixed() {
        let bursts = RandomSeed::stream("bursts", 7);
        assert_eq!(bursts, RandomSeed::stream("bursts", 7));
        assert_ne!(bursts, RandomSeed::stream("bursts", 8));
        assert_ne!(bursts, RandomSeed::stream("spawner", 7));
        // Pinned so a replay recorded today draws the same values later.
        assert_eq!(RandomSeed(42).value(3, bursts), 5_142_468_438_960_481_807);
    }
}
