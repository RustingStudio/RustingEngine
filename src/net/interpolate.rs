//! Smooth presentation of remote objects between host updates.
//!
//! Host state arrives a few times a second and late by a varying amount.
//! [`Interpolation`] keeps the transforms a [`Replica`](super::replicate::Replica)
//! received with the time each one is from, and shows every object where
//! it was `delay` ago, blending between the two updates around that time.
//! A delay of a bit more than two update intervals hides one lost update.

use super::replicate::NetTransform;
use crate::Transform;
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, VecDeque};
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
    pub fn record(&mut self, world: &mut World, time: Duration) {
        let mut query = world.query::<(Entity, &NetTransform)>();
        let mut alive = Vec::new();
        for (entity, net) in query.iter(world) {
            let samples = self.samples.entry(entity).or_default();
            if samples.back().is_some_and(|(last, _)| *last >= time) {
                samples.pop_back(); // the same update again, or a rewind
            }
            if samples.len() == MAX_SAMPLES {
                samples.pop_front();
            }
            samples.push_back((time, net.0));
            alive.push(entity);
        }
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
}
