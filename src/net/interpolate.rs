//! Smooth presentation of remote objects between host updates.
//!
//! Host state arrives a few times a second and late by a varying amount.
//! [`Interpolation`] keeps the transforms a [`Replica`](super::replicate::Replica)
//! received with the time each one is from, and shows every object where
//! it was `delay` ago, blending between the two updates around that time.
//! A delay of a bit more than two update intervals hides one lost update.
//!
//! A game that sends its own snapshots keeps a [`Track`] of `(tick,
//! position)` samples per remote object instead, with no replica.

use super::predict::Predicted;
use super::replicate::NetTransform;
use crate::Transform;
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::f32::consts::{PI, TAU};
use std::time::Duration;

/// Most updates kept per object.
const MAX_SAMPLES: usize = 64;

/// Recorded transforms and the delay to present them with.
#[derive(Clone, Debug)]
pub struct Interpolation {
    delay: Duration,
    samples: BTreeMap<Entity, VecDeque<(Duration, Transform)>>,
}

impl Interpolation {
    /// Shows objects `delay` behind the newest update. 100 ms suits
    /// updates at 20 Hz or faster.
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            samples: BTreeMap::new(),
        }
    }

    /// How far behind the newest update objects are shown.
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// Changes the delay. Updates older than the old delay are already
    /// dropped, so a longer delay holds objects until enough new updates
    /// arrive.
    pub fn set_delay(&mut self, delay: Duration) {
        self.delay = delay;
    }

    /// Records every [`NetTransform`] as the state at `time`: the host
    /// time of the update just applied (such as its tick times the fixed
    /// step), or the time it arrived. Call it once per applied update.
    /// Skips the [`Predicted`] hero, whose `Transform` the game sets.
    pub fn record(&mut self, world: &mut World, time: Duration) {
        let mut query = world
            .query_filtered::<(Entity, &NetTransform), Without<Predicted>>();
        let mut alive = BTreeSet::new();
        for (entity, net) in query.iter(world) {
            let samples = self.samples.entry(entity).or_default();
            if samples.back().is_some_and(|(last, _)| *last >= time) {
                samples.pop_back(); // the same update again, or a rewind
            }
            if samples.len() == MAX_SAMPLES {
                samples.pop_front();
            }
            samples.push_back((time, net.0));
            alive.insert(entity);
        }
        // A host can make a client track MAX_OBJECTS objects; keep this
        // n log n, not a scan per object.
        self.samples.retain(|entity, _| alive.contains(entity));
    }

    /// Sets each recorded object's `Transform` to where it was at
    /// `now - delay`, in the same clock as [`record`](Self::record).
    /// Before the first update it holds the oldest; past the newest, it
    /// holds the newest rather than guessing ahead.
    pub fn present(&mut self, world: &mut World, now: Duration) {
        let at = now.saturating_sub(self.delay);
        for (entity, samples) in &mut self.samples {
            // Keep one update at or before `at` to blend from.
            while samples.get(1).is_some_and(|(time, _)| *time <= at) {
                samples.pop_front();
            }
            let Some(&(from_time, from)) = samples.front() else {
                continue;
            };
            let transform = match samples.get(1) {
                Some(&(to_time, to)) if at > from_time => {
                    let t = (at - from_time).as_secs_f32()
                        / (to_time - from_time).as_secs_f32();
                    blend(&from, &to, t)
                }
                _ => from,
            };
            if let Ok(mut entity) = world.get_entity_mut(*entity) {
                entity.insert(transform);
            }
        }
    }
}

/// Positions of one remote object at host ticks, for a game that sends its
/// own snapshots instead of using a [`Replica`](super::replicate::Replica).
/// Push each `(tick, position)` as it arrives, in any order, and draw the
/// object at [`Track::at`] a delay behind the newest tick.
#[derive(Clone, Debug, Default)]
pub struct Track {
    samples: VecDeque<(u64, [f32; 3])>,
}

impl Track {
    /// A track with no samples.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `position` at host `tick`. A late sample goes in tick
    /// order, and one for a tick already held replaces it. The newest 64
    /// ticks are kept.
    pub fn push(&mut self, tick: u64, position: [f32; 3]) {
        match self.samples.binary_search_by_key(&tick, |(tick, _)| *tick) {
            Ok(index) => self.samples[index].1 = position,
            Err(0) if self.samples.len() == MAX_SAMPLES => {}
            Err(index) => {
                self.samples.insert(index, (tick, position));
                if self.samples.len() > MAX_SAMPLES {
                    self.samples.pop_front();
                }
            }
        }
    }

    /// The newest tick pushed, or `None` before the first sample.
    pub fn newest(&self) -> Option<u64> {
        self.samples.back().map(|(tick, _)| *tick)
    }

    /// The position at `tick`, which may fall between whole ticks (such as
    /// the newest tick less 2.5): blended between the samples around it.
    /// Before the oldest sample it holds the oldest; past the newest, it
    /// holds the newest rather than guessing ahead. `None` when empty.
    pub fn at(&self, tick: f64) -> Option<[f32; 3]> {
        let after = self.samples.partition_point(|(at, _)| *at as f64 <= tick);
        let Some(&(to_tick, to)) = self.samples.get(after) else {
            return self.samples.back().map(|(_, position)| *position);
        };
        let Some(&(from_tick, from)) = after
            .checked_sub(1)
            .and_then(|before| self.samples.get(before))
        else {
            return Some(to);
        };
        let t =
            ((tick - from_tick as f64) / (to_tick - from_tick) as f64) as f32;
        Some(std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t))
    }
}

fn blend(from: &Transform, to: &Transform, t: f32) -> Transform {
    let lerp = |a: [f32; 3], b: [f32; 3], angle: bool| {
        std::array::from_fn(|i| {
            let mut d = b[i] - a[i];
            if angle {
                // The short way around.
                d = (d + PI).rem_euclid(TAU) - PI;
            }
            a[i] + d * t
        })
    };
    let mut out = *to;
    out.position = lerp(from.position, to.position, false);
    out.rotation = lerp(from.rotation, to.rotation, true);
    out.scale = lerp(from.scale, to.scale, false);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::replicate::{Replica, Replicated, Replication, TRANSFORM};
    use crate::net::HOST;
    use crate::runtime::{SceneComponentRegistry, SceneId};

    #[test]
    fn remote_objects_show_where_they_were_one_delay_ago() {
        let mut host = World::new();
        let mut client = World::new();
        client.insert_resource(SceneComponentRegistry::default());
        host.insert_resource(SceneComponentRegistry::default());
        let mut replication = Replication::new();
        replication.component(TRANSFORM);
        let mut replica = Replica::new(&replication).interpolated();
        let mut smooth = Interpolation::new(Duration::from_millis(100));
        let raft = host
            .spawn((Replicated, SceneId::new(), Transform::new([0.0; 3])))
            .id();
        let ms = Duration::from_millis;

        // Updates every 50 ms, moving 1 m each.
        for i in 0..4u8 {
            host.get_mut::<Transform>(raft).unwrap().position[0] = f32::from(i);
            host.get_mut::<Transform>(raft).unwrap().rotation[1] =
                if i % 2 == 0 { 3.1 } else { -3.1 };
            if let Some(delta) = replication.delta(&mut host).unwrap() {
                replica.apply(&mut client, HOST, &delta).unwrap().unwrap();
            }
            smooth.record(&mut client, ms(50 * u64::from(i)));
        }
        let shown = |client: &mut World| {
            *client
                .query_filtered::<&Transform, With<Replicated>>()
                .single(client)
                .unwrap()
        };
        let net = |client: &mut World| {
            client.query::<&NetTransform>().single(client).unwrap().0
        };
        assert_eq!(net(&mut client).position[0], 3.0, "the host's latest");
        // The first update went to Transform too.
        smooth.present(&mut client, ms(50));
        assert_eq!(shown(&mut client).position[0], 0.0, "held before 0 ms");
        smooth.present(&mut client, ms(125));
        let halfway = shown(&mut client);
        assert!((halfway.position[0] - 0.5).abs() < 1e-5);
        assert!(
            halfway.rotation[1].abs() > 3.1,
            "turns the short way past pi, not through zero"
        );
        smooth.present(&mut client, ms(500));
        assert_eq!(shown(&mut client).position[0], 3.0, "held at the newest");

        // A longer delay shows an older state.
        smooth.set_delay(ms(475));
        smooth.present(&mut client, ms(500));
        assert_eq!(smooth.delay(), ms(475));
        assert_eq!(shown(&mut client).position[0], 3.0, "never goes back");
    }

    #[test]
    fn a_track_blends_plain_tick_samples_that_arrive_out_of_order() {
        let mut track = Track::new();
        assert_eq!(track.at(5.0), None);
        track.push(20, [2.0, 0.0, 0.0]);
        track.push(10, [1.0, 0.0, 0.0]); // late over UDP
        track.push(30, [9.0, 0.0, 0.0]);
        track.push(30, [3.0, 0.0, 6.0]); // the same tick again
        assert_eq!(track.newest(), Some(30));
        assert_eq!(track.at(5.0), Some([1.0, 0.0, 0.0]), "holds the oldest");
        assert_eq!(track.at(15.0), Some([1.5, 0.0, 0.0]));
        assert_eq!(track.at(27.5), Some([2.75, 0.0, 4.5]));
        assert_eq!(track.at(99.0), Some([3.0, 0.0, 6.0]), "no guessing");

        for tick in 31..200 {
            track.push(tick, [0.0; 3]);
        }
        assert_eq!(track.samples.len(), MAX_SAMPLES);
        track.push(5, [7.0; 3]);
        assert_eq!(track.samples.front().unwrap().0, 200 - 64, "too old");
    }

    #[test]
    fn the_predicted_hero_is_not_interpolated() {
        let mut world = World::new();
        let at = |x| Transform::new([x, 0.0, 0.0]);
        let hero = world.spawn((NetTransform(at(1.0)), at(9.0))).id();
        let other = world.spawn((NetTransform(at(1.0)), at(9.0))).id();
        crate::net::predict::predict_only(&mut world, other);
        crate::net::predict::predict_only(&mut world, hero);
        assert!(world.get::<Predicted>(other).is_none(), "only one hero");
        let mut smooth = Interpolation::new(Duration::ZERO);
        smooth.record(&mut world, Duration::ZERO);
        smooth.present(&mut world, Duration::ZERO);
        assert_eq!(world.get::<Transform>(hero).unwrap().position[0], 9.0);
        assert_eq!(world.get::<Transform>(other).unwrap().position[0], 1.0);
    }
}
