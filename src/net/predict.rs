//! Client-side prediction and reconciliation for the objects a player
//! steers.
//!
//! The client applies each tick's input at once instead of waiting a round
//! trip, keeps the inputs the host has not confirmed, and sends them. The
//! host applies them and replies with its state and the last input tick it
//! applied. [`Prediction::reconcile`] then takes the host's state and
//! replays the inputs still unconfirmed on top of it, so a correct
//! prediction changes nothing and a wrong one is fixed without a jump back
//! in time.

use bevy_ecs::prelude::*;
use std::collections::VecDeque;
use std::time::Duration;

/// Marks the one object this client predicts: its local hero.
/// [`Interpolation`](super::interpolate::Interpolation) leaves it alone, so
/// the game draws it from its prediction. Everything else, other players
/// and anything the host simulates, such as terrain destruction, is shown
/// from host state: predicting it needs a simulation proven to match the
/// host's bit for bit.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Predicted;

/// Makes `hero` the only [`Predicted`] object in `world`.
pub fn predict_only(world: &mut World, hero: Entity) {
    let others: Vec<Entity> = world
        .query_filtered::<Entity, With<Predicted>>()
        .iter(world)
        .filter(|entity| *entity != hero)
        .collect();
    for entity in others {
        world.entity_mut(entity).remove::<Predicted>();
    }
    world.entity_mut(hero).insert(Predicted);
}

/// Unconfirmed inputs kept at most: 4 seconds at 60 ticks. Older ones are
/// dropped, so a host that stops answering cannot grow memory.
pub const MAX_PENDING: usize = 240;

/// The inputs one predicted object has sent but the host has not applied.
#[derive(Clone, Debug)]
pub struct Prediction<I> {
    pending: VecDeque<(u64, I)>,
}

impl<I> Default for Prediction<I> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }
}

impl<I> Prediction<I> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the input for `tick`. Apply it to the object yourself and
    /// send it, with its tick, to the host. Ticks must rise.
    pub fn push(&mut self, tick: u64, input: I) {
        if self.pending.len() == MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back((tick, input));
    }

    /// Takes the host's `state` after it applied inputs up to `acked_tick`,
    /// forgets those inputs, and returns `state` with the rest replayed by
    /// `step`, in tick order. `step` must be the same code the host runs
    /// for one input.
    pub fn reconcile<S>(
        &mut self,
        acked_tick: u64,
        mut state: S,
        mut step: impl FnMut(&mut S, &I),
    ) -> S {
        while self
            .pending
            .front()
            .is_some_and(|(tick, _)| *tick <= acked_tick)
        {
            self.pending.pop_front();
        }
        for (_, input) in &self.pending {
            step(&mut state, input);
        }
        state
    }

    /// Inputs not yet confirmed, oldest first.
    pub fn pending(&self) -> impl Iterator<Item = &(u64, I)> {
        self.pending.iter()
    }
}

/// Hides a reconcile's correction from the player without touching the
/// simulation. It keeps only a drawing offset: the predicted state stays
/// exactly what [`Prediction::reconcile`] returned, and the offset fades
/// out over time.
#[derive(Clone, Copy, Debug)]
pub struct Smoothing {
    offset: [f32; 3],
    half_life: Duration,
    snap: f32,
}

impl Smoothing {
    /// Halves the shown error every `half_life`. A correction longer than
    /// `snap` metres (a teleport or respawn) shows at once instead.
    pub fn new(half_life: Duration, snap: f32) -> Self {
        Self {
            offset: [0.0; 3],
            half_life,
            snap,
        }
    }

    /// Records a correction: the position was drawn at `shown`, and the
    /// reconciled simulation now puts it at `corrected`.
    pub fn correct(&mut self, shown: [f32; 3], corrected: [f32; 3]) {
        let offset: [f32; 3] = std::array::from_fn(|i| shown[i] - corrected[i]);
        let length = offset.iter().map(|x| x * x).sum::<f32>().sqrt();
        self.offset = if length > self.snap { [0.0; 3] } else { offset };
    }

    /// Where to draw `simulated` after `elapsed` more time. Feed only
    /// rendering from it, never the simulation.
    pub fn shown(
        &mut self,
        simulated: [f32; 3],
        elapsed: Duration,
    ) -> [f32; 3] {
        if !self.half_life.is_zero() {
            let decay = 0.5f32
                .powf(elapsed.as_secs_f32() / self.half_life.as_secs_f32());
            self.offset = self.offset.map(|x| x * decay);
        } else {
            self.offset = [0.0; 3];
        }
        std::array::from_fn(|i| simulated[i] + self.offset[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_replays_unconfirmed_inputs_on_the_host_state() {
        fn step(x: &mut f32, input: &f32) {
            *x += input;
        }
        let mut prediction = Prediction::new();
        let mut client = 0.0;
        for tick in 1..=5 {
            prediction.push(tick, 1.0);
            step(&mut client, &1.0);
        }
        // The host agrees up to tick 3: the prediction stands.
        assert_eq!(prediction.reconcile(3, 3.0, step), client);
        assert_eq!(prediction.pending().count(), 2);
        // The host was pushed back at tick 4: replay tick 5 on its state.
        assert_eq!(prediction.reconcile(4, 2.0, step), 3.0);
        assert_eq!(
            prediction.pending().map(|(t, _)| *t).collect::<Vec<_>>(),
            [5]
        );
        // A stale reply replays nothing it already forgot.
        assert_eq!(prediction.reconcile(2, 2.0, step), 3.0);

        for tick in 6..6 + MAX_PENDING as u64 + 10 {
            prediction.push(tick, 0.0);
        }
        assert_eq!(prediction.pending().count(), MAX_PENDING);
    }

    #[test]
    fn smoothing_fades_a_correction_without_touching_the_simulation() {
        let mut prediction = Prediction::new();
        let mut position = [0.0f32; 3];
        let step = |p: &mut [f32; 3], dx: &f32| p[0] += dx;
        for tick in 1..=3 {
            prediction.push(tick, 1.0);
            step(&mut position, &1.0);
        }
        let mut smoothing = Smoothing::new(Duration::from_millis(100), 2.0);
        let shown = smoothing.shown(position, Duration::ZERO);
        // The host was 0.5 m behind: the simulation jumps, the drawing
        // does not.
        position = prediction.reconcile(1, [0.5, 0.0, 0.0], step);
        assert_eq!(position, [2.5, 0.0, 0.0], "simulation takes it whole");
        smoothing.correct(shown, position);
        assert_eq!(smoothing.shown(position, Duration::ZERO), shown);
        let later = smoothing.shown(position, Duration::from_millis(100));
        assert!((later[0] - 2.75).abs() < 1e-5, "half the error left");
        assert_eq!(position, [2.5, 0.0, 0.0], "never fed back");
        // A teleport snaps.
        smoothing.correct(position, [50.0, 0.0, 0.0]);
        assert_eq!(smoothing.shown([50.0, 0.0, 0.0], Duration::ZERO)[0], 50.0);
    }
}
