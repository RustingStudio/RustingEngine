//! Engine integration for reusable named input actions.

use bevy_ecs::prelude::{Component, Resource, World};
use serde::{Deserialize, Deserializer, Serialize};

pub use rusting_core::input::{ActionMap, InputBinding};

use super::{KeyCode, MouseButton, PadButton};

pub(super) fn install(world: &mut World) {
    world.insert_resource(ActionMap::default());
}

/// Binds a named action to keys and mouse buttons from scene data, so game
/// code and scenarios can use an action no Rust code bound. Inputs are winit
/// key names (`KeyF`, `Space`, `ArrowUp`, `Digit1`) or `MouseLeft`,
/// `MouseRight` and `MouseMiddle`, or `Pad` plus a [`PadButton`] name
/// (`PadSouth`, `PadStart`, `PadDpadUp`, `PadLeftStickUp`). Removing or editing the component updates
/// the bindings.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct InputAction {
    pub action: String,
    #[serde(deserialize_with = "input_names")]
    pub inputs: Vec<String>,
}

/// The binding an input name stands for.
pub fn parse_input(name: &str) -> Result<InputBinding, String> {
    let mouse = match name {
        "MouseLeft" => Some(MouseButton::Left),
        "MouseRight" => Some(MouseButton::Right),
        "MouseMiddle" => Some(MouseButton::Middle),
        _ => None,
    };
    if let Some(button) = mouse {
        return Ok(InputBinding::Mouse(button));
    }
    if let Some(pad) = name.strip_prefix("Pad") {
        return serde_json::from_value::<PadButton>(serde_json::Value::from(pad))
            .map(InputBinding::Pad)
            .map_err(|_| {
                format!(
                    "unknown gamepad input `{name}`: use PadSouth, PadEast, PadWest, \
                     PadNorth, PadLeftBumper, PadRightBumper, PadLeftTrigger, \
                     PadRightTrigger, PadSelect, PadStart, PadLeftStick, PadRightStick, \
                     PadDpadUp/Down/Left/Right or PadLeftStickUp/Down/Left/Right \
                     (and RightStick)"
                )
            });
    }
    serde_json::from_value::<KeyCode>(serde_json::Value::from(name))
        .map(InputBinding::Key)
        .map_err(|_| {
            format!(
                "unknown input `{name}`: use a key name such as KeyF, Space \
                 or ArrowUp, MouseLeft, MouseRight or MouseMiddle, or a gamepad \
                 input such as PadSouth"
            )
        })
}

fn input_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    let names = Vec::<String>::deserialize(deserializer)?;
    for name in &names {
        parse_input(name).map_err(serde::de::Error::custom)?;
    }
    Ok(names)
}

/// What the scene's [`InputAction`]s bound last, so a change can undo it.
/// A resource, so the schedule and scenario presses share one record.
#[derive(Resource, Clone, Default)]
pub(super) struct SceneBindings {
    declared: Vec<(String, String)>,
    bound: Vec<(String, InputBinding)>,
}

/// Makes the [`ActionMap`] match the scene's [`InputAction`]s. Runs each
/// frame but only does work when the declared inputs changed: it drops the
/// bindings it added before, so an edited, removed or replaced component
/// stops firing. Bindings game code added itself stay.
pub fn bind_input_actions(world: &mut World) {
    let mut scene =
        world.remove_resource::<SceneBindings>().unwrap_or_default();
    sync_bindings(world, &mut scene);
    world.insert_resource(scene);
}

fn sync_bindings(world: &mut World, scene: &mut SceneBindings) {
    let mut query = world.query::<&InputAction>();
    let mut declared: Vec<(String, String)> = query
        .iter(world)
        .flat_map(|action| {
            action
                .inputs
                .iter()
                .map(|name| (action.action.clone(), name.clone()))
        })
        .collect();
    declared.sort();
    if scene.declared != declared {
        let mut map = world.resource_mut::<ActionMap>();
        for (action, binding) in scene.bound.drain(..) {
            map.unbind(&action, binding);
        }
        for (action, name) in &declared {
            let Ok(binding) = parse_input(name) else {
                continue;
            };
            if !map.bindings(action).contains(&binding) {
                map.bind(action.clone(), binding);
                scene.bound.push((action.clone(), binding));
            }
        }
        scene.declared = declared;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_actions_follow_their_components_and_keep_code_bindings() {
        assert_eq!(parse_input("KeyF"), Ok(InputBinding::Key(KeyCode::KeyF)));
        assert_eq!(
            parse_input("MouseRight"),
            Ok(InputBinding::Mouse(MouseButton::Right))
        );
        assert_eq!(
            parse_input("PadSouth"),
            Ok(InputBinding::Pad(PadButton::South))
        );
        assert!(parse_input("PadNope").unwrap_err().contains("gamepad"));
        assert!(parse_input("KeyQQ").unwrap_err().contains("KeyQQ"));

        let mut world = World::new();
        install(&mut world);
        // Game code bound F to fire itself; the scene must leave it alone.
        world
            .resource_mut::<ActionMap>()
            .bind("fire", InputBinding::Key(KeyCode::KeyF));
        let action = |inputs: &[&str]| InputAction {
            action: "fire".into(),
            inputs: inputs.iter().map(|&name| name.to_owned()).collect(),
        };
        let entity = world.spawn(action(&["KeyF", "MouseLeft"])).id();
        world.spawn(action(&["MouseLeft"]));
        bind_input_actions(&mut world);
        let fire = |world: &World| {
            world.resource::<ActionMap>().bindings("fire").to_vec()
        };
        assert_eq!(
            fire(&world),
            [
                InputBinding::Key(KeyCode::KeyF),
                InputBinding::Mouse(MouseButton::Left)
            ],
            "each binding once"
        );

        world.entity_mut(entity).insert(action(&["PadSouth"]));
        bind_input_actions(&mut world);
        assert_eq!(
            fire(&world),
            [
                InputBinding::Key(KeyCode::KeyF),
                InputBinding::Mouse(MouseButton::Left),
                InputBinding::Pad(PadButton::South)
            ]
        );

        let all: Vec<_> = world
            .query_filtered::<bevy_ecs::entity::Entity, bevy_ecs::query::With<InputAction>>()
            .iter(&world)
            .collect();
        for entity in all {
            world.despawn(entity);
        }
        bind_input_actions(&mut world);
        assert_eq!(
            fire(&world),
            [InputBinding::Key(KeyCode::KeyF)],
            "only the game code's binding stays"
        );
    }
}
