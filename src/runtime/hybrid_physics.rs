//! CPU-side types shared with the hybrid GPU physics bridge.
//!
//! This module does not create Vulkan buffers. It defines stable body IDs,
//! programmable conditions, and the exact event format used for readback.
//!
//! # Latency
//!
//! GPU physics never blocks a frame. The renderer reads events and body
//! state only after the frame that produced them has finished on the GPU,
//! so they normally reach gameplay one to three rendered frames after the
//! fixed tick they describe; [`GpuPhysicsEvent::tick`] and
//! [`GpuStateMirror::tick`] record that tick. Gameplay that needs an answer
//! in the same tick (a jump check, a hit-scan) must use
//! [`SimulationClass::Cpu`](super::SimulationClass) bodies instead.
//!
//! A dynamic GPU body that reads its state back couples with CPU
//! simulation through that state. CPU solvers meet it at its
//! [`GpuStateMirror`] pose carried forward by its velocity over the state's
//! age, up to [`MAX_COUPLED_STATE_AGE`] ticks; older state is not coupled.
//! Fluids, granular media, water, soft bodies and cloth send what they do
//! to it back as a [`GpuBodyCommand::Impulse`] that applies before the next
//! GPU tick, so the body feels them one tick later. CPU rigid bodies meet it
//! as a kinematic body, and the GPU meets them as colliders from the last
//! CPU step, so each side answers the other's last known state.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use bevy_ecs::change_detection::DetectChanges;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::lifecycle::RemovedComponents;
use bevy_ecs::prelude::{
    Changed, Commands, Has, IntoScheduleConfigs, Query, Ref, Res, ResMut,
    Resource, With, Without, World,
};
use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

use super::{App, AppError, EventQueue, Plugin, ScheduleStage};
use crate::Transform;

/// Maximum number of simple instructions accepted for one condition.
///
/// A fixed limit prevents one object from creating an unexpectedly expensive
/// condition program. Custom compute code remains available for larger logic.
pub const MAX_GPU_CONDITION_INSTRUCTIONS: usize = 64;

/// Stable identity shared by ECS, GPU buffers, commands, and GPU events.
///
/// `slot` locates a table entry. `generation` changes whenever that slot is
/// reused, so a delayed event cannot accidentally target a newer body.
#[repr(C)]
#[derive(
    Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Pod, Zeroable,
)]
pub struct PhysicsId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug)]
struct PhysicsSlot {
    generation: u32,
    entity: Option<Entity>,
}

/// Owns the stable mapping between ECS entities and GPU physics IDs.
#[derive(Resource, Clone, Debug, Default)]
pub struct PhysicsIdRegistry {
    slots: Vec<PhysicsSlot>,
    free_slots: Vec<u32>,
    entity_ids: HashMap<Entity, PhysicsId>,
}

impl PhysicsIdRegistry {
    /// Feeds slot generations, owners, and the free list, which decide the
    /// ids later bodies get.
    pub(crate) fn hash_state(&self, hasher: &mut super::StateHasher) {
        for slot in &self.slots {
            hasher.word(u64::from(slot.generation));
            match slot.entity {
                Some(entity) => hasher.entity(entity),
                None => hasher.word(u64::MAX - 1),
            }
        }
        for slot in &self.free_slots {
            hasher.word(u64::from(*slot));
        }
    }

    /// Returns the existing ID or creates a new stable ID for an entity.
    pub fn assign(&mut self, entity: Entity) -> PhysicsId {
        if let Some(id) = self.entity_ids.get(&entity) {
            return *id;
        }

        let id = if let Some(slot) = self.free_slots.pop() {
            let entry = &mut self.slots[slot as usize];
            entry.entity = Some(entity);
            PhysicsId {
                slot,
                generation: entry.generation,
            }
        } else {
            let slot = self.slots.len() as u32;
            let generation = 1;
            self.slots.push(PhysicsSlot {
                generation,
                entity: Some(entity),
            });
            PhysicsId { slot, generation }
        };

        self.entity_ids.insert(entity, id);
        id
    }

    /// Releases an entity ID and invalidates all delayed events using it.
    ///
    /// A slot whose generation reaches `u32::MAX` is retired, not reused:
    /// wrapping back to 1 would let ids from its first use resolve again.
    pub fn release(&mut self, entity: Entity) -> Option<PhysicsId> {
        let old_id = self.entity_ids.remove(&entity)?;
        let entry = &mut self.slots[old_id.slot as usize];
        entry.entity = None;
        if entry.generation < u32::MAX {
            entry.generation += 1;
            self.free_slots.push(old_id.slot);
        }
        Some(old_id)
    }

    /// Resolves an ID only when both its slot and generation still match.
    #[must_use]
    pub fn resolve(&self, id: PhysicsId) -> Option<Entity> {
        let entry = self.slots.get(id.slot as usize)?;
        (entry.generation == id.generation)
            .then_some(entry.entity)
            .flatten()
    }

    /// Returns the GPU physics ID currently assigned to an ECS entity.
    #[must_use]
    pub fn id_for(&self, entity: Entity) -> Option<PhysicsId> {
        self.entity_ids.get(&entity).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entity_ids.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entity_ids.is_empty()
    }
}

/// Numeric name used by shaders when they emit a registered gameplay event.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Hash, Serialize,
)]
pub struct GpuEventId(pub u32);

/// Assigns compact IDs to event names used by Rust and custom shaders.
#[derive(Resource, Clone, Debug, Default)]
pub struct GpuEventRegistry {
    names: Vec<String>,
    ids: HashMap<String, GpuEventId>,
}

impl GpuEventRegistry {
    /// Registers a name once and returns the same ID on later calls.
    pub fn register(&mut self, name: impl Into<String>) -> GpuEventId {
        let name = name.into();
        if let Some(id) = self.ids.get(&name) {
            return *id;
        }
        // Zero means "no event" inside GPU buffers.
        let id = GpuEventId((self.names.len() as u32).saturating_add(1));
        self.names.push(name.clone());
        self.ids.insert(name, id);
        id
    }

    #[must_use]
    pub fn id(&self, name: &str) -> Option<GpuEventId> {
        self.ids.get(name).copied()
    }

    #[must_use]
    pub fn name(&self, id: GpuEventId) -> Option<&str> {
        let index = id.0.checked_sub(1)? as usize;
        self.names.get(index).map(String::as_str)
    }
}

/// Physics value that a built-in condition reads on the GPU, through
/// `GpuCondition::field(GpuStateField::PositionX)` and the like.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum GpuStateField {
    /// World position in meters.
    PositionX,
    PositionY,
    PositionZ,
    /// Velocity in meters per second.
    VelocityX,
    VelocityY,
    VelocityZ,
    /// Spin in radians per second.
    AngularVelocityX,
    AngularVelocityY,
    AngularVelocityZ,
    /// The transform's scale.
    ScaleX,
    ScaleY,
    ScaleZ,
    /// Mass in kilograms.
    Mass,
    GravityScale,
    /// Length of the velocity, in meters per second.
    Speed,
    /// Custom value 0 to 3.
    Custom(u8),
}

impl GpuStateField {
    fn gpu_code(self) -> u32 {
        match self {
            Self::PositionX => 0,
            Self::PositionY => 1,
            Self::PositionZ => 2,
            Self::VelocityX => 3,
            Self::VelocityY => 4,
            Self::VelocityZ => 5,
            Self::AngularVelocityX => 6,
            Self::AngularVelocityY => 7,
            Self::AngularVelocityZ => 8,
            Self::ScaleX => 9,
            Self::ScaleY => 10,
            Self::ScaleZ => 11,
            Self::Mass => 12,
            Self::GravityScale => 13,
            Self::Speed => 14,
            Self::Custom(index) => 0x100 + u32::from(index),
        }
    }
}

/// Comparison used by one leaf in a GPU condition.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum GpuComparison {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Equal,
    NotEqual,
}

impl GpuComparison {
    fn gpu_code(self) -> u32 {
        match self {
            Self::Less => 0,
            Self::LessOrEqual => 1,
            Self::Greater => 2,
            Self::GreaterOrEqual => 3,
            Self::Equal => 4,
            Self::NotEqual => 5,
        }
    }
}

/// One node in a condition tree authored through normal Rust code.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
enum GpuConditionNode {
    Compare {
        field: GpuStateField,
        comparison: GpuComparison,
        value: f32,
    },
    Range {
        field: GpuStateField,
        minimum: f32,
        maximum: f32,
    },
    Colliding,
    Sleeping,
    TimerElapsed(f32),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Not(Box<Self>),
}

/// A condition that can be compiled into data evaluated by a compute shader.
///
/// This deliberately stays a Rust builder instead of introducing another
/// scripting language. Truly arbitrary logic can use the custom shader ABI.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GpuCondition {
    node: GpuConditionNode,
}

impl GpuCondition {
    /// Starts a comparison on any [`GpuStateField`]:
    /// `GpuCondition::field(GpuStateField::PositionX).inside(-1.0, 1.0)`.
    #[must_use]
    pub fn field(field: GpuStateField) -> GpuFieldCondition {
        GpuFieldCondition { field }
    }

    /// Starts a comparison on the world Y position, in meters.
    #[must_use]
    pub fn position_y() -> GpuFieldCondition {
        Self::field(GpuStateField::PositionY)
    }

    /// Starts a comparison on the Y velocity, in meters per second.
    #[must_use]
    pub fn velocity_y() -> GpuFieldCondition {
        Self::field(GpuStateField::VelocityY)
    }

    /// Starts a comparison on custom value `index` (0 to 3), as set by
    /// `GpuBodyCommand::SetCustomValues` or a condition shader.
    #[must_use]
    pub fn custom(index: u8) -> GpuFieldCondition {
        Self::field(GpuStateField::Custom(index))
    }

    /// True while the body touches another body or a collider.
    #[must_use]
    pub fn colliding() -> Self {
        Self {
            node: GpuConditionNode::Colliding,
        }
    }

    /// True while the body sleeps: it touched something and stayed slower
    /// than 0.05 m/s and 0.05 rad/s for 20 ticks. A push wakes it.
    #[must_use]
    pub fn sleeping() -> Self {
        Self {
            node: GpuConditionNode::Sleeping,
        }
    }

    /// True once `seconds` of simulated time have passed since physics
    /// started.
    #[must_use]
    pub fn timer_elapsed(seconds: f32) -> Self {
        Self {
            node: GpuConditionNode::TimerElapsed(seconds.max(0.0)),
        }
    }

    /// True when both conditions are.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self {
            node: GpuConditionNode::And(
                Box::new(self.node),
                Box::new(other.node),
            ),
        }
    }

    /// True when either condition is.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self {
            node: GpuConditionNode::Or(
                Box::new(self.node),
                Box::new(other.node),
            ),
        }
    }

    /// True when `self` is false; `!condition` does the same.
    #[must_use]
    pub fn inverted(self) -> Self {
        Self {
            node: GpuConditionNode::Not(Box::new(self.node)),
        }
    }

    /// Converts the tree into postfix instructions consumed by the GPU.
    pub fn compile(
        &self,
    ) -> Result<Vec<GpuConditionInstruction>, ConditionError> {
        let mut instructions = Vec::new();
        compile_node(&self.node, &mut instructions)?;
        if instructions.len() > MAX_GPU_CONDITION_INSTRUCTIONS {
            return Err(ConditionError::TooManyInstructions {
                count: instructions.len(),
                maximum: MAX_GPU_CONDITION_INSTRUCTIONS,
            });
        }
        Ok(instructions)
    }
}

impl std::ops::BitAnd for GpuCondition {
    type Output = Self;

    fn bitand(self, other: Self) -> Self::Output {
        self.and(other)
    }
}

impl std::ops::BitOr for GpuCondition {
    type Output = Self;

    fn bitor(self, other: Self) -> Self::Output {
        self.or(other)
    }
}

impl std::ops::Not for GpuCondition {
    type Output = Self;

    fn not(self) -> Self::Output {
        self.inverted()
    }
}

/// Starts a comparison against one GPU physics value.
#[derive(Clone, Copy, Debug)]
pub struct GpuFieldCondition {
    field: GpuStateField,
}

impl GpuFieldCondition {
    fn compare(self, comparison: GpuComparison, value: f32) -> GpuCondition {
        GpuCondition {
            node: GpuConditionNode::Compare {
                field: self.field,
                comparison,
                value,
            },
        }
    }

    /// True while the value is below `value`.
    #[must_use]
    pub fn less_than(self, value: f32) -> GpuCondition {
        self.compare(GpuComparison::Less, value)
    }

    /// True while the value is at most `value`.
    #[must_use]
    pub fn less_or_equal(self, value: f32) -> GpuCondition {
        self.compare(GpuComparison::LessOrEqual, value)
    }

    /// True while the value is above `value`.
    #[must_use]
    pub fn greater_than(self, value: f32) -> GpuCondition {
        self.compare(GpuComparison::Greater, value)
    }

    /// True while the value is at least `value`.
    #[must_use]
    pub fn greater_or_equal(self, value: f32) -> GpuCondition {
        self.compare(GpuComparison::GreaterOrEqual, value)
    }

    /// True while the value equals `value` exactly.
    #[must_use]
    pub fn equal_to(self, value: f32) -> GpuCondition {
        self.compare(GpuComparison::Equal, value)
    }

    /// True while the value differs from `value`.
    #[must_use]
    pub fn not_equal_to(self, value: f32) -> GpuCondition {
        self.compare(GpuComparison::NotEqual, value)
    }

    /// True while the value is in `minimum..=maximum`; the bounds may come
    /// in either order.
    #[must_use]
    pub fn inside(self, minimum: f32, maximum: f32) -> GpuCondition {
        GpuCondition {
            node: GpuConditionNode::Range {
                field: self.field,
                minimum: minimum.min(maximum),
                maximum: minimum.max(maximum),
            },
        }
    }
}

/// Operation codes shared with the condition compute shader.
mod condition_opcode {
    pub const COMPARE: u32 = 1;
    pub const RANGE: u32 = 2;
    pub const COLLIDING: u32 = 3;
    pub const SLEEPING: u32 = 4;
    pub const TIMER_ELAPSED: u32 = 5;
    pub const AND: u32 = 16;
    pub const OR: u32 = 17;
    pub const NOT: u32 = 18;
}

/// Fixed-size instruction stored in a GPU condition buffer.
///
/// The four integer words describe the operation. The four float words hold
/// thresholds or future parameters. Keeping this at 32 bytes makes its GLSL
/// `std430` layout simple and predictable.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuConditionInstruction {
    pub opcode: u32,
    pub operand: u32,
    pub flags: u32,
    pub reserved: u32,
    pub values: [f32; 4],
}

fn compile_node(
    node: &GpuConditionNode,
    output: &mut Vec<GpuConditionInstruction>,
) -> Result<(), ConditionError> {
    let instruction = match node {
        GpuConditionNode::Compare {
            field,
            comparison,
            value,
        } => GpuConditionInstruction {
            opcode: condition_opcode::COMPARE,
            operand: field.gpu_code(),
            flags: comparison.gpu_code(),
            values: [*value, 0.0, 0.0, 0.0],
            ..Default::default()
        },
        GpuConditionNode::Range {
            field,
            minimum,
            maximum,
        } => GpuConditionInstruction {
            opcode: condition_opcode::RANGE,
            operand: field.gpu_code(),
            values: [*minimum, *maximum, 0.0, 0.0],
            ..Default::default()
        },
        GpuConditionNode::Colliding => GpuConditionInstruction {
            opcode: condition_opcode::COLLIDING,
            ..Default::default()
        },
        GpuConditionNode::Sleeping => GpuConditionInstruction {
            opcode: condition_opcode::SLEEPING,
            ..Default::default()
        },
        GpuConditionNode::TimerElapsed(seconds) => GpuConditionInstruction {
            opcode: condition_opcode::TIMER_ELAPSED,
            values: [*seconds, 0.0, 0.0, 0.0],
            ..Default::default()
        },
        GpuConditionNode::And(left, right) => {
            compile_node(left, output)?;
            compile_node(right, output)?;
            GpuConditionInstruction {
                opcode: condition_opcode::AND,
                ..Default::default()
            }
        }
        GpuConditionNode::Or(left, right) => {
            compile_node(left, output)?;
            compile_node(right, output)?;
            GpuConditionInstruction {
                opcode: condition_opcode::OR,
                ..Default::default()
            }
        }
        GpuConditionNode::Not(inner) => {
            compile_node(inner, output)?;
            GpuConditionInstruction {
                opcode: condition_opcode::NOT,
                ..Default::default()
            }
        }
    };
    output.push(instruction);
    if output.len() > MAX_GPU_CONDITION_INSTRUCTIONS {
        return Err(ConditionError::TooManyInstructions {
            count: output.len(),
            maximum: MAX_GPU_CONDITION_INSTRUCTIONS,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionError {
    TooManyInstructions { count: usize, maximum: usize },
}

impl std::fmt::Display for ConditionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyInstructions { count, maximum } => write!(
                formatter,
                "GPU condition has {count} instructions; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for ConditionError {}

/// Decides when a true/false condition produces an event.
#[repr(u32)]
#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize,
)]
pub enum GpuEventMode {
    #[default]
    OnEnter = 0,
    OnExit = 1,
    WhileTrue = 2,
    Once = 3,
}

/// Selects the four floats copied into an event without another readback.
#[repr(u32)]
#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize,
)]
pub enum GpuEventPayload {
    #[default]
    None = 0,
    Position = 1,
    Velocity = 2,
    AngularVelocity = 3,
    Contact = 4,
    Custom = 5,
}

/// One authored rule attached to one body or a prepared body group.
#[derive(
    Component, Clone, Debug, Default, Deserialize, PartialEq, Serialize,
)]
pub struct GpuPhysicsWatch {
    pub rules: Vec<GpuPhysicsRule>,
}

/// GPU rules assigned to reusable object classes.
///
/// One rule can target 10,000 objects without also changing unrelated GPU
/// bodies. Objects may belong to more than one class.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct GpuPhysicsClassWatches {
    /// Rules stored under their authored class name.
    pub classes: BTreeMap<String, Vec<GpuPhysicsRule>>,
}

impl GpuPhysicsClassWatches {
    /// Adds one rule to a class without registering it twice.
    pub fn add(&mut self, class: impl Into<String>, rule: GpuPhysicsRule) {
        let class = class.into();
        let class = class.trim();
        if class.is_empty() {
            return;
        }
        let rules = self.classes.entry(class.to_owned()).or_default();
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
}

/// Configures which event is emitted when a GPU condition changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GpuPhysicsRule {
    pub event: String,
    pub condition: GpuCondition,
    pub mode: GpuEventMode,
    pub payload: GpuEventPayload,
    pub cooldown_seconds: f32,
}

impl GpuPhysicsRule {
    #[must_use]
    pub fn new(event: impl Into<String>, condition: GpuCondition) -> Self {
        Self {
            event: event.into(),
            condition,
            mode: GpuEventMode::OnEnter,
            payload: GpuEventPayload::None,
            cooldown_seconds: 0.0,
        }
    }

    #[must_use]
    pub fn mode(mut self, mode: GpuEventMode) -> Self {
        self.mode = mode;
        self
    }

    #[must_use]
    pub fn payload(mut self, payload: GpuEventPayload) -> Self {
        self.payload = payload;
        self
    }

    #[must_use]
    pub fn cooldown(mut self, seconds: f32) -> Self {
        self.cooldown_seconds = seconds.max(0.0);
        self
    }
}

/// Exact event bytes copied from a GPU readback buffer.
///
/// The tick is split into two words because 64-bit shader integers are not a
/// baseline feature on low-end Vulkan devices.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct RawGpuPhysicsEvent {
    pub body_slot: u32,
    pub body_generation: u32,
    pub event_id: u32,
    pub flags: u32,
    pub tick_low: u32,
    pub tick_high: u32,
    pub payload_kind: u32,
    pub reserved: u32,
    pub payload: [f32; 4],
}

impl RawGpuPhysicsEvent {
    #[must_use]
    pub fn physics_id(self) -> PhysicsId {
        PhysicsId {
            slot: self.body_slot,
            generation: self.body_generation,
        }
    }

    #[must_use]
    pub fn tick(self) -> u64 {
        u64::from(self.tick_low) | (u64::from(self.tick_high) << 32)
    }
}

/// One change CPU gameplay asks the GPU simulation to make to a body.
///
/// Spawning, despawning, solver changes, and watch-rule changes need no
/// command: edit the ECS components and the next extraction rebuilds the
/// GPU tables while keeping every surviving body's simulated state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuBodyCommand {
    /// Moves the body; its velocity is kept.
    Teleport(Transform),
    SetVelocity {
        linear: [f32; 3],
        angular: [f32; 3],
    },
    /// Instant change of momentum, in newton-seconds. Dynamic bodies only.
    Impulse([f32; 3]),
    /// Force in newtons applied for one fixed tick. Dynamic bodies only.
    Force([f32; 3]),
    /// Replaces the four custom values that conditions and shaders read.
    SetCustomValues([f32; 4]),
    /// Reads the body's full state back once, as a [`GpuStateMirror`], even
    /// when its [`PhysicsSyncMode`] does not read state back.
    ReadState,
}

/// Commands waiting for the next render extraction, in submission order.
///
/// A command applies before the GPU simulates the tick after the one that
/// was completed when it was pushed: one pushed during fixed tick `n` (when
/// `FrameTime::fixed_tick` reads `n - 1`), or in `Update` right after tick
/// `n - 1`, applies before GPU tick `n`. That holds however many ticks a
/// frame batches. Commands apply in order per body; the renderer rejects
/// commands whose body generation is stale.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct GpuPhysicsCommands {
    pub commands: Vec<(PhysicsId, GpuBodyCommand)>,
    /// GPU tick each command applies before, parallel to `commands`. The
    /// engine fills it after every fixed tick and at extraction; a command
    /// without an entry applies before the next GPU tick.
    pub apply_ticks: Vec<u64>,
    /// Reads every GPU body's full state back once after the next step.
    /// Meant for debugging, save states, and tests: it copies the whole
    /// simulation, so keep it off the per-frame gameplay path.
    pub read_all_states: bool,
    /// Discards the simulated GPU state and restarts every body from its
    /// authored ECS components, with rule edge and cooldown state cleared.
    /// This is Stop for a play session: the authored scene is the snapshot,
    /// so no state is read back. Commands in the same batch apply after the
    /// reset, which lets [`Self::restore`] resume a saved mid-play state.
    pub reset_to_authored: bool,
}

impl GpuPhysicsCommands {
    pub fn push(&mut self, body: PhysicsId, command: GpuBodyCommand) {
        self.commands.push((body, command));
    }

    /// Puts a body back into a state read earlier, for example one captured
    /// with [`Self::read_all_states`] when Play was paused.
    pub fn restore(&mut self, body: PhysicsId, state: &GpuStateMirror) {
        self.push(body, GpuBodyCommand::Teleport(state.transform));
        self.push(
            body,
            GpuBodyCommand::SetVelocity {
                linear: state.linear_velocity,
                angular: state.angular_velocity,
            },
        );
        if let Some(values) = state.custom_values {
            self.push(body, GpuBodyCommand::SetCustomValues(values));
        }
    }
}

/// Gives every command pushed since the last call the tick after the
/// current `FrameTime::fixed_tick`; see [`GpuPhysicsCommands`].
pub(super) fn stamp_gpu_commands(world: &mut World) {
    let tick = world.resource::<super::FrameTime>().fixed_tick + 1;
    if let Some(mut commands) = world.get_resource_mut::<GpuPhysicsCommands>() {
        let length = commands.commands.len();
        if commands.apply_ticks.len() != length {
            commands.apply_ticks.resize(length, tick);
        }
    }
}

/// Requests a one-shot snapshot of every GPU body in an object class.
///
/// Each member's state arrives together, one to three frames later, as a
/// [`GpuStateMirror`] carrying the same tick. Returns how many bodies were
/// requested. Use it when gameplay needs more than events for a group,
/// without paying [`PhysicsSyncMode::SelectedState`] every tick.
pub fn request_gpu_class_snapshot(world: &mut World, class: &str) -> usize {
    let members = world
        .query::<(&PhysicsId, &super::ObjectClasses)>()
        .iter(world)
        .filter(|(_, classes)| classes.contains(class))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    let mut commands = world.resource_mut::<GpuPhysicsCommands>();
    for &id in &members {
        commands.push(id, GpuBodyCommand::ReadState);
    }
    members.len()
}

/// Version of the GPU physics ABI in `src/shaders/physics_abi.glsl`: the body
/// state, command, and event layouts, the push constants, the bindings, and
/// `emit_event`. GLSL sees it as `RUSTING_PHYSICS_ABI_VERSION`. It changes
/// whenever a custom shader written for the old layout would break.
pub const GPU_PHYSICS_ABI_VERSION: u32 = 1;

/// Custom GLSL that runs over every GPU body after each fixed step.
///
/// `glsl` must define `void condition(inout PhysicsState body)`. It sees the
/// ABI in `src/shaders/physics_abi.glsl` (the `PhysicsState` layout, the `pc`
/// push constants with `dt`, `elapsed`, and the tick) and reports with the
/// same `emit_event(body, event_id, payload_kind, payload)` as built-in rules.
/// `EVENTS[i]` is the [`GpuEventId`] of `events[i]`, registered by name, so
/// Rust receives these as ordinary [`GpuPhysicsEvent`]s. Changes to `body`
/// are written back, so a hook can also steer the simulation.
///
/// The renderer compiles the source at runtime with `glslc` (from the Vulkan
/// SDK or shaderc; set `RUSTING_GLSLC` to use another path). A shader that
/// fails to compile is skipped and its error kept; see
/// `SceneRenderer::condition_shader_errors`. The event buffer reserves
/// `events_per_body` events per body, tick, and shader (at most 262144 per
/// frame in all); more are counted as lost and reported as
/// [`GpuPhysicsEventsLost`].
///
/// `params` reach GLSL as `condition_params.values[i]` (a `vec4` each).
/// Change them every frame to steer the shader; unlike `glsl`, a change does
/// not recompile. Reading past the end is undefined, so check
/// `condition_params.values.length()`.
///
/// See `rusting docs show guide/gpu-condition-shaders` for the whole ABI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuConditionShader {
    /// Event names the shader emits, by index.
    pub events: Vec<String>,
    /// GLSL body of the condition function.
    pub glsl: String,
    /// Values the shader reads as `condition_params.values[i]`.
    pub params: Vec<[f32; 4]>,
    /// Most events one body emits per tick from this shader; 0 counts as 1.
    pub events_per_body: u32,
}

/// Custom condition shaders, dispatched in list order after the built-in step.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct GpuConditionShaders(pub Vec<GpuConditionShader>);

impl GpuConditionShader {
    /// Registers the events and returns the GLSL the renderer compiles after
    /// the shared ABI: an `EVENTS` table followed by the user's code.
    #[must_use]
    pub fn resolve(&self, registry: &mut GpuEventRegistry) -> String {
        let ids = self
            .events
            .iter()
            .map(|name| format!("{}u", registry.register(name.clone()).0))
            .collect::<Vec<_>>();
        let table = if ids.is_empty() {
            String::new()
        } else {
            format!(
                "const uint EVENTS[{}] = uint[]({});\n",
                ids.len(),
                ids.join(", ")
            )
        };
        format!("{table}#line 1\n{}", self.glsl)
    }
}

/// Sent when GPU events were lost because the per-frame event buffer reached
/// its memory budget. `WhileTrue` rules fire again on the next tick, but
/// `OnEnter`/`OnExit` edges are gone; resynchronize from body state
/// ([`PhysicsSyncMode::SelectedState`]) if gameplay depends on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPhysicsEventsLost {
    pub count: u64,
}

/// Safe Rust event delivered after a raw GPU event resolves to a live entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuPhysicsEvent {
    pub entity: Entity,
    pub physics_id: PhysicsId,
    pub event_id: GpuEventId,
    pub tick: u64,
    pub flags: u32,
    pub payload_kind: u32,
    pub payload: [f32; 4],
}

/// Reports how many readback events were accepted or rejected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuEventRouteReport {
    pub delivered: usize,
    pub stale: usize,
    pub unknown_event: usize,
}

/// How much GPU simulation data one body sends back to the CPU.
///
/// Synchronization is a cost chosen per body. It is never a hidden copy of
/// the whole scene. Bodies without this component use `Events`.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    PartialEq,
    Eq,
    Hash,
    Serialize,
)]
pub enum PhysicsSyncMode {
    /// Nothing returns: watch rules are not evaluated for this body.
    None,
    /// Only the events of the body's watch rules return.
    #[default]
    Events,
    /// Events, plus pose and velocities in [`GpuStateMirror`] every physics
    /// frame.
    SelectedState,
    /// Like `SelectedState`, plus the body's custom values.
    FullState,
}

impl PhysicsSyncMode {
    /// Bytes copied back per body and fixed tick when state is read back.
    /// The renderer asserts this matches its packed body layout.
    pub const STATE_READBACK_BYTES: u64 = 144;

    #[must_use]
    pub fn reads_back_state(self) -> bool {
        matches!(self, Self::SelectedState | Self::FullState)
    }
}

/// GPU state of one body, copied back after its frame finished.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuStateSample {
    pub physics_id: PhysicsId,
    /// Fixed tick the GPU had simulated when the copy was taken.
    pub tick: u64,
    pub transform: Transform,
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    /// Present only for [`PhysicsSyncMode::FullState`] bodies.
    pub custom_values: Option<[f32; 4]>,
}

/// Newest GPU state read back for a `SelectedState` or `FullState` body.
///
/// The authored `Transform` stays as it is: writing the delayed GPU pose into
/// it would count as an edit and restart the body from that older pose. Read
/// the simulated pose here, and check its age before trusting it for
/// same-tick gameplay decisions.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct GpuStateMirror {
    /// Fixed tick the GPU had simulated when this state was produced.
    pub tick: u64,
    pub transform: Transform,
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub custom_values: Option<[f32; 4]>,
}

impl GpuStateMirror {
    /// Fixed ticks between this state and `current_tick`; GPU state usually
    /// reaches the CPU one to three frames late.
    #[must_use]
    pub fn age_ticks(&self, current_tick: u64) -> u64 {
        current_tick.saturating_sub(self.tick)
    }
}

/// Oldest read-back GPU state, in fixed ticks, that CPU fluids and water
/// still couple with. Readback normally runs one to three frames behind; a
/// body whose state is older (paused readback, a stalled GPU) is left out
/// rather than pushed from a pose that may be far from where it is.
pub const MAX_COUPLED_STATE_AGE: u64 = 8;

/// A point that keeps GPU bodies near it simulated at full rate under
/// [`GpuSimulationLod`], at its world position. Put it on
/// players or other gameplay points, not on the render camera, so every
/// peer steps the same bodies at the same rate.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct SimulationObserver;

/// Level of detail for GPU simulation. With this resource and at least one
/// [`SimulationObserver`], a dynamic GPU body steps every tick within
/// `full_radius` of its nearest observer, takes one step of
/// `reduced_stride` ticks once every `reduced_stride` ticks out to
/// `reduced_radius`, and freezes beyond that, keeping its state until an
/// observer comes back in range. A body that does not step on a tick holds
/// still in contacts like a fixed one. Without the resource or without
/// observers every body steps every tick.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct GpuSimulationLod {
    /// Meters.
    pub full_radius: f32,
    /// Meters; `f32::INFINITY` never freezes.
    pub reduced_radius: f32,
    /// Ticks per reduced step; below 1 counts as 1.
    pub reduced_stride: u32,
}

impl Default for GpuSimulationLod {
    fn default() -> Self {
        Self {
            full_radius: 64.0,
            reduced_radius: 256.0,
            reduced_stride: 4,
        }
    }
}

/// Device budget for GPU bodies. With this resource and at least one
/// [`SimulationObserver`], at most `max_resident` GPU bodies (each a
/// [`GpuBodyState`] in device memory) stay on the GPU: the nearest to an
/// observer, ties broken by [`PhysicsId::slot`]. The rest stream out: each
/// takes its last read-back state into its `Transform` and `RigidBody`,
/// gains [`StreamedOut`] and leaves the GPU; it streams back in from that
/// state when it ranks inside the budget again. A body without read-back
/// state (`PhysicsSyncMode::None`) comes back from wherever its `Transform`
/// last was. Decided each fixed tick, from observer world positions as of
/// the last frame.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuStreamingBudget {
    pub max_resident: usize,
}

/// A GPU body that [`GpuStreamingBudget`] moved out of device memory. It is
/// not simulated and is drawn at its `Transform`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamedOut;

/// Applies [`GpuStreamingBudget`].
/// ponytail: no hysteresis, so two bodies trading places at the budget's edge
/// stream every tick; add a margin if that shows. The saved state is the
/// last read-back one, up to a few ticks old (exact for a body frozen by
/// [`GpuSimulationLod`]).
#[allow(clippy::type_complexity)]
fn stream_gpu_bodies(
    mut commands: Commands,
    budget: Option<Res<GpuStreamingBudget>>,
    observers: Query<
        (&Transform, Option<&super::GlobalTransform>),
        With<SimulationObserver>,
    >,
    mut bodies: Query<
        (
            Entity,
            &PhysicsId,
            &super::PhysicsBody,
            &mut Transform,
            Option<&mut super::RigidBody>,
            Option<&GpuStateMirror>,
            Has<StreamedOut>,
        ),
        Without<SimulationObserver>,
    >,
) {
    let observers: Vec<[f32; 3]> = observers
        .iter()
        .map(|(transform, global)| {
            global.map_or(transform.position, |global| {
                let [x, y, z, _] = global.matrix[3];
                [x, y, z]
            })
        })
        .collect();
    let keep = match budget {
        Some(budget) if !observers.is_empty() => budget.max_resident,
        _ => usize::MAX,
    };
    let mut ranked: Vec<_> = bodies
        .iter()
        .filter(|(_, _, body, ..)| body.uses_gpu())
        .map(|(entity, id, _, transform, _, mirror, out)| {
            let at = match mirror {
                Some(mirror) if !out => mirror.transform.position,
                _ => transform.position,
            };
            let nearest = observers
                .iter()
                .map(|o| (0..3).map(|i| (at[i] - o[i]).powi(2)).sum::<f32>())
                .fold(f32::INFINITY, f32::min);
            (nearest, id.slot, entity)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (rank, (_, _, entity)) in ranked.into_iter().enumerate() {
        let Ok((_, _, _, mut transform, rigid, mirror, out)) =
            bodies.get_mut(entity)
        else {
            continue;
        };
        let resident = rank < keep;
        if resident && out {
            commands.entity(entity).remove::<StreamedOut>();
        } else if !resident && !out {
            if let Some(mirror) = mirror {
                *transform = mirror.transform;
                if let Some(mut rigid) = rigid {
                    rigid.linear_velocity = mirror.linear_velocity;
                    rigid.angular_velocity = mirror.angular_velocity;
                }
            }
            commands
                .entity(entity)
                .insert(StreamedOut)
                .remove::<GpuStateMirror>();
        }
    }
}

impl GpuSimulationLod {
    /// Ticks of time a body `distance_squared` from its nearest observer
    /// steps over on `tick`: 1 at full rate, `reduced_stride` on its reduced
    /// ticks, 0 while it waits or is frozen. `slot` is its
    /// [`PhysicsId::slot`], which spreads reduced bodies over the stride.
    /// The reference for `lod_steps` in `physics_lod.glsl`.
    #[must_use]
    pub fn steps(&self, distance_squared: f32, tick: u64, slot: u32) -> u32 {
        let stride = self.reduced_stride.max(1);
        if distance_squared <= self.full_radius * self.full_radius {
            1
        } else if distance_squared > self.reduced_radius * self.reduced_radius {
            0
        } else if (tick as u32).wrapping_add(slot).is_multiple_of(stride) {
            stride
        } else {
            0
        }
    }
}

/// Pose and linear velocity CPU coupling uses for a GPU body during the
/// tick after `fixed_tick`: the read-back pose carried forward by its
/// velocity over the state's age. `None` past [`MAX_COUPLED_STATE_AGE`].
pub(crate) fn coupled_gpu_state(
    mirror: &GpuStateMirror,
    fixed_tick: u64,
    dt: f32,
) -> Option<(Transform, [f32; 3])> {
    let age = mirror.age_ticks(fixed_tick);
    if age > MAX_COUPLED_STATE_AGE {
        return None;
    }
    let mut pose = mirror.transform;
    for axis in 0..3 {
        pose.position[axis] += mirror.linear_velocity[axis] * age as f32 * dt;
    }
    Some((pose, mirror.linear_velocity))
}

/// Lets the engine choose CPU or GPU simulation for this body.
///
/// [`allocate_auto_simulation`] decides once, writes the choice into
/// `PhysicsBody::simulation`, and records it in `decision` so tools can show
/// why. Remove this component to pick the class by hand again; set
/// `decision` to `None` to ask for a new decision. A body that is `None` or
/// `Static` when it is decided stays that way.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct AutoSimulation {
    /// Filled in by the engine; not saved.
    #[serde(skip)]
    pub decision: Option<AllocationDecision>,
}

/// The class an [`AutoSimulation`] body got and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocationDecision {
    pub class: super::SimulationClass,
    pub reason: AllocationReason,
}

impl AllocationDecision {
    /// One sentence saying why the body got its class, with the
    /// thresholds from `policy` filled in.
    pub fn explain(&self, policy: &AutoAllocationPolicy) -> String {
        let class = match self.class {
            super::SimulationClass::None => "no simulation",
            super::SimulationClass::Static => "static",
            super::SimulationClass::Cpu => "the CPU",
            super::SimulationClass::Gpu => "the GPU",
        };
        let why = match self.reason {
            AllocationReason::NotDynamic => {
                return format!("Kept as {class}: the body is not dynamic.");
            }
            AllocationReason::NoGpuBackend => {
                "no GPU physics backend is connected".to_owned()
            }
            AllocationReason::CustomSolver => {
                "custom solvers run only on the GPU".to_owned()
            }
            AllocationReason::Kinematic => {
                "gameplay moves kinematic bodies on the CPU".to_owned()
            }
            AllocationReason::CpuOnlyCollider => {
                "sensors and mesh, polygon and chain colliders exist only \
                 in the CPU solver"
                    .to_owned()
            }
            AllocationReason::ReadsStateEveryTick => {
                "reading its full state back every tick costs more than \
                 simulating it on the CPU"
                    .to_owned()
            }
            AllocationReason::FewBodies => format!(
                "fewer than {} flexible auto bodies",
                policy.gpu_min_bodies
            ),
            AllocationReason::ManyBodies => format!(
                "at least {} flexible auto bodies",
                policy.gpu_min_bodies
            ),
            AllocationReason::CpuOverBudget => format!(
                "CPU physics took over {:?} last frame",
                policy.cpu_physics_budget
            ),
        };
        format!("On {class}: {why}.")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationReason {
    /// The body was `None` or `Static`; nothing to allocate.
    NotDynamic,
    /// No GPU physics backend is connected.
    NoGpuBackend,
    /// Custom solvers run only on the GPU.
    CustomSolver,
    /// Gameplay moves kinematic bodies every frame on the CPU.
    Kinematic,
    /// Sensors and mesh colliders exist only in the CPU solver.
    CpuOnlyCollider,
    /// Reading the full state back every tick costs more than simulating
    /// the body on the CPU.
    ReadsStateEveryTick,
    /// Fewer flexible bodies than [`AutoAllocationPolicy::gpu_min_bodies`].
    FewBodies,
    /// At least [`AutoAllocationPolicy::gpu_min_bodies`] flexible bodies.
    ManyBodies,
    /// The last measured CPU physics time was over budget.
    CpuOverBudget,
}

/// Thresholds used by [`allocate_auto_simulation`].
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct AutoAllocationPolicy {
    /// Flexible `AutoSimulation` bodies at which they all go to the GPU.
    pub gpu_min_bodies: usize,
    /// CPU physics time per frame above which new bodies go to the GPU.
    pub cpu_physics_budget: std::time::Duration,
}

impl Default for AutoAllocationPolicy {
    fn default() -> Self {
        Self {
            gpu_min_bodies: 256,
            cpu_physics_budget: std::time::Duration::from_millis(4),
        }
    }
}

/// A class this body must have regardless of the scene, if any.
fn required_class(
    body: &super::PhysicsBody,
    rigid: Option<&super::RigidBody>,
    collider: Option<&super::Collider>,
    sync: Option<&PhysicsSyncMode>,
    gpu: bool,
) -> Option<AllocationDecision> {
    use super::SimulationClass::{Cpu, Gpu};
    let decide = |class, reason| Some(AllocationDecision { class, reason });
    if !body.participates_in_dynamic_simulation() {
        return decide(body.simulation, AllocationReason::NotDynamic);
    }
    if !gpu {
        return decide(Cpu, AllocationReason::NoGpuBackend);
    }
    if body.solver == super::PhysicsSolver::Custom {
        return decide(Gpu, AllocationReason::CustomSolver);
    }
    if rigid.is_some_and(|rigid| rigid.kind == super::RigidBodyKind::Kinematic)
    {
        return decide(Cpu, AllocationReason::Kinematic);
    }
    if collider.is_some_and(|collider| {
        collider.sensor
            || matches!(
                collider.shape,
                super::ColliderShape::ConvexMesh
                    | super::ColliderShape::TriangleMesh
                    | super::ColliderShape::Heightfield
                    | super::ColliderShape::Polygon
                    | super::ColliderShape::Chain
            )
    }) {
        return decide(Cpu, AllocationReason::CpuOnlyCollider);
    }
    if sync.is_some_and(|sync| sync.reads_back_state()) {
        return decide(Cpu, AllocationReason::ReadsStateEveryTick);
    }
    None
}

/// Gives every undecided [`AutoSimulation`] body a class.
///
/// Hard requirements come first (see [`AllocationReason`]); the other
/// "flexible" bodies go to the GPU when there are at least
/// `gpu_min_bodies` of them or the last frame's CPU physics was over
/// budget, else to the CPU.
// ponytail: decisions stick; a decided body never migrates between CPU and
// GPU, because that needs a live state handoff. Add one if scenes grow
// past the threshold long after they start.
pub fn allocate_auto_simulation(world: &mut World) {
    use super::SimulationClass::{Cpu, Gpu};
    let policy = world
        .get_resource::<AutoAllocationPolicy>()
        .copied()
        .unwrap_or_default();
    let gpu = world
        .get_resource::<super::PhysicsBackendStatus>()
        .is_some_and(|status| status.gpu_dynamic_available);
    let over_budget = world
        .get_resource::<super::CpuFrameTimings>()
        .is_some_and(|timings| timings.physics > policy.cpu_physics_budget);
    let mut query = world.query::<(
        Entity,
        &AutoSimulation,
        &super::PhysicsBody,
        Option<&super::RigidBody>,
        Option<&super::Collider>,
        Option<&PhysicsSyncMode>,
    )>();
    let mut flexible = 0;
    let mut pending = Vec::new();
    for (entity, auto, body, rigid, collider, sync) in query.iter(world) {
        let required = if auto.decision.is_none() {
            // Judge the authored class, not a previous decision.
            required_class(body, rigid, collider, sync, gpu)
        } else {
            auto.decision.filter(|decision| {
                decision.reason != AllocationReason::FewBodies
                    && decision.reason != AllocationReason::ManyBodies
                    && decision.reason != AllocationReason::CpuOverBudget
            })
        };
        flexible += usize::from(required.is_none());
        if auto.decision.is_none() {
            pending.push((entity, required));
        }
    }
    for (entity, required) in pending {
        let decision = required.unwrap_or(if over_budget {
            AllocationDecision {
                class: Gpu,
                reason: AllocationReason::CpuOverBudget,
            }
        } else if flexible >= policy.gpu_min_bodies {
            AllocationDecision {
                class: Gpu,
                reason: AllocationReason::ManyBodies,
            }
        } else {
            AllocationDecision {
                class: Cpu,
                reason: AllocationReason::FewBodies,
            }
        });
        let mut entity = world.entity_mut(entity);
        if let Some(mut auto) = entity.get_mut::<AutoSimulation>() {
            auto.decision = Some(decision);
        }
        if let Some(mut body) = entity.get_mut::<super::PhysicsBody>() {
            if body.simulation != decision.class {
                body.simulation = decision.class;
            }
        }
    }
}

/// Keeps a CPU collider standing in for this GPU body, so CPU queries and
/// CPU bodies can find it without reading the whole body back.
///
/// The proxy is a separate `Static` entity marked [`GpuProxyOf`]. The body's
/// `place_on` event (with a `Position` payload) creates or moves it and
/// `remove_on` removes it; removing this component or the body removes it
/// too. Its pose is as old as the event (usually one to three frames) and
/// reaches CPU queries on the next fixed step. GPU bodies never collide with
/// proxies.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct GpuQueryProxy {
    pub collider: super::Collider,
    pub layers: super::CollisionLayers,
    pub place_on: GpuEventId,
    pub remove_on: Option<GpuEventId>,
}

/// Marks the CPU proxy entity of a GPU body; see [`GpuQueryProxy`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuProxyOf(pub Entity);

/// Places, moves and removes [`GpuQueryProxy`] proxies from this frame's
/// GPU events, in their delivery order.
fn sync_gpu_query_proxies(world: &mut World) {
    let mut proxies = world
        .query::<(Entity, &GpuProxyOf)>()
        .iter(world)
        .map(|(proxy, owner)| (owner.0, proxy))
        .collect::<HashMap<_, _>>();
    proxies.retain(|owner, proxy| {
        let alive = world.get::<GpuQueryProxy>(*owner).is_some();
        if !alive {
            world.despawn(*proxy);
        }
        alive
    });
    let events = world
        .resource::<EventQueue<GpuPhysicsEvent>>()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    for event in events {
        let Some(settings) = world.get::<GpuQueryProxy>(event.entity).copied()
        else {
            continue;
        };
        if Some(event.event_id) == settings.remove_on {
            if let Some(proxy) = proxies.remove(&event.entity) {
                world.despawn(proxy);
            }
            continue;
        }
        if event.event_id != settings.place_on
            || event.payload_kind != GpuEventPayload::Position as u32
        {
            continue;
        }
        // The body's authored rotation and scale, at the reported position.
        let mut transform = world
            .get::<Transform>(event.entity)
            .copied()
            .unwrap_or_default();
        transform.position =
            [event.payload[0], event.payload[1], event.payload[2]];
        let parts = (
            transform,
            settings.collider,
            settings.layers,
            super::PhysicsBody {
                simulation: super::SimulationClass::Static,
                ..super::PhysicsBody::default()
            },
            GpuProxyOf(event.entity),
        );
        match proxies.get(&event.entity) {
            Some(&proxy) => {
                world.entity_mut(proxy).insert(parts);
            }
            None => {
                let proxy = world.spawn(parts).id();
                proxies.insert(event.entity, proxy);
            }
        }
    }
}

/// Writes read-back GPU state into [`GpuStateMirror`] on live entities.
///
/// Samples for removed bodies (stale generation) are counted and ignored, and
/// an older sample never replaces a newer one.
pub fn apply_gpu_state_samples(
    world: &mut World,
    samples: &[GpuStateSample],
) -> GpuEventRouteReport {
    let mut report = GpuEventRouteReport::default();
    for sample in samples {
        let entity = world
            .resource::<PhysicsIdRegistry>()
            .resolve(sample.physics_id);
        let Some(mut entity) =
            entity.and_then(|entity| world.get_entity_mut(entity).ok())
        else {
            report.stale += 1;
            continue;
        };
        if entity.contains::<StreamedOut>()
            || entity
                .get::<GpuStateMirror>()
                .is_some_and(|mirror| mirror.tick > sample.tick)
        {
            continue;
        }
        entity.insert(GpuStateMirror {
            tick: sample.tick,
            transform: sample.transform,
            linear_velocity: sample.linear_velocity,
            angular_velocity: sample.angular_velocity,
            custom_values: sample.custom_values,
        });
        report.delivered += 1;
    }
    report
}

/// One compiled rule copied into the renderer-facing physics snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedGpuPhysicsRule {
    pub event_id: GpuEventId,
    pub instructions: Vec<GpuConditionInstruction>,
    pub mode: GpuEventMode,
    pub payload: GpuEventPayload,
    pub cooldown_seconds: f32,
}

/// GPU-owned body data prepared without exposing Vulkan buffers to ECS.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedGpuPhysicsBody {
    pub entity: Entity,
    pub physics_id: PhysicsId,
    pub transform: Transform,
    pub rigid_body: super::RigidBody,
    /// Solver selected for this GPU body, including the Space attractor mode.
    pub solver: super::PhysicsSolver,
    /// Collides against [`super::GpuCollider`]s unless the solver is
    /// `NoCollision`. Full, Simplified and Space bodies also collide with
    /// each other.
    pub collider: Option<(super::Collider, super::CollisionLayers)>,
    /// Solver hook file of a `PhysicsSolver::Custom` body; see
    /// [`custom_solver_id`]. `None` for every other solver.
    pub custom_shader: Option<String>,
    pub rules: Vec<ExtractedGpuPhysicsRule>,
    pub sync: PhysicsSyncMode,
}

/// Identifies a custom solver file on the GPU. The native shader stores it
/// in `properties.w` of every body that uses the file, and the solver's
/// wrapper runs `solve(body)` only for those bodies. 24 bits so the id stays
/// exact as an `f32`.
// ponytail: FNV-1a hash; two paths could collide (1 in 16M). Hand out
// indices from a registry if a project ever has many solver files.
#[must_use]
pub fn custom_solver_id(path: &str) -> u32 {
    let hash = path.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    (hash & 0x00ff_ffff).max(1)
}

/// Wraps a custom solver file as a condition shader that runs its
/// `void solve(inout PhysicsState body)` for the bodies that selected it.
/// The built-in step still applies commands and watch rules to those bodies
/// but leaves their motion to `solve`.
#[must_use]
pub fn custom_solver_source(path: &str, glsl: &str) -> String {
    format!(
        "const uint SOLVER_ID = {}u;\n#line 1\n{glsl}\n\
         void condition(inout PhysicsState body) {{\n\
         if (uint(body.properties.w) == SOLVER_ID) solve(body);\n}}\n",
        custom_solver_id(path)
    )
}

/// Returns a cheap signature of every GPU body's extracted inputs.
///
/// GPU-owned swarms keep their state after setup, so their complete CPU
/// descriptions do not need to be cloned and compared on every render frame.
/// Watch rules and class lists are covered by their change ticks rather than
/// by hashing their contents.
// ponytail: a tick bumps on any mutable access, even without a real edit; the
// full extraction that follows still compares contents before a rebuild.
pub(super) fn simple_gpu_physics_signature(world: &mut World) -> u64 {
    let mut hasher = super::FastHasher::default();
    world
        .resource_ref::<GpuPhysicsClassWatches>()
        .last_changed()
        .get()
        .hash(&mut hasher);
    let mut count = 0_u64;
    let mut query = world.query_filtered::<(
        Entity,
        &PhysicsId,
        &Transform,
        &super::PhysicsBody,
        Option<&super::RigidBody>,
        Option<Ref<super::ObjectClasses>>,
        Option<Ref<GpuPhysicsWatch>>,
        Option<&PhysicsSyncMode>,
        Option<Ref<super::Collider>>,
        Option<Ref<super::CollisionLayers>>,
    ), Without<StreamedOut>>();
    for (
        entity,
        id,
        transform,
        body,
        rigid_body,
        classes,
        watch,
        sync,
        collider,
        layers,
    ) in query.iter(world)
    {
        if !body.uses_gpu() {
            continue;
        }
        count += 1;
        sync.copied().unwrap_or_default().hash(&mut hasher);
        classes
            .map(|classes| classes.last_changed().get())
            .hash(&mut hasher);
        watch
            .map(|watch| watch.last_changed().get())
            .hash(&mut hasher);
        collider
            .map(|collider| collider.last_changed().get())
            .hash(&mut hasher);
        layers
            .map(|layers| layers.last_changed().get())
            .hash(&mut hasher);
        entity.to_bits().hash(&mut hasher);
        id.hash(&mut hasher);
        for value in transform
            .position
            .into_iter()
            .chain(transform.rotation)
            .chain(transform.scale)
        {
            value.to_bits().hash(&mut hasher);
        }
        let solver = match body.solver {
            super::PhysicsSolver::Full => 0_u8,
            super::PhysicsSolver::Simplified => 1,
            super::PhysicsSolver::NoCollision => 2,
            super::PhysicsSolver::Custom => 3,
            super::PhysicsSolver::Space => 4,
        };
        solver.hash(&mut hasher);
        body.custom_shader.hash(&mut hasher);
        let rigid_body = rigid_body.copied().unwrap_or_default();
        (rigid_body.kind as u8).hash(&mut hasher);
        rigid_body.mass.to_bits().hash(&mut hasher);
        rigid_body.gravity_scale.to_bits().hash(&mut hasher);
        for value in rigid_body
            .linear_velocity
            .into_iter()
            .chain(rigid_body.angular_velocity)
        {
            value.to_bits().hash(&mut hasher);
        }
    }
    count.hash(&mut hasher);
    hasher.finish()
}

/// Collects GPU bodies and compiles their authored rules for rendering.
pub(super) fn extract_gpu_physics_bodies(
    world: &mut World,
) -> Vec<ExtractedGpuPhysicsBody> {
    let class_watches = world.resource::<GpuPhysicsClassWatches>().clone();
    let needs_class_lookup = !class_watches.classes.is_empty();
    let raw = {
        let mut query = world.query_filtered::<(
            Entity,
            &PhysicsId,
            &Transform,
            &super::PhysicsBody,
            Option<&super::RigidBody>,
            Option<&super::ObjectClasses>,
            Option<&GpuPhysicsWatch>,
            Option<&PhysicsSyncMode>,
            Option<&super::Collider>,
            Option<&super::CollisionLayers>,
        ), Without<StreamedOut>>();
        query
            .iter(world)
            .filter(|(_, _, _, body, ..)| body.uses_gpu())
            .map(
                |(
                    entity,
                    id,
                    transform,
                    body,
                    rigid_body,
                    classes,
                    watch,
                    sync,
                    collider,
                    layers,
                )| {
                    let sync = sync.copied().unwrap_or_default();
                    let watched = sync != PhysicsSyncMode::None;
                    (
                        entity,
                        *id,
                        *transform,
                        body.solver,
                        (body.solver == super::PhysicsSolver::Custom)
                            .then(|| body.custom_shader.clone())
                            .flatten(),
                        rigid_body.copied().unwrap_or_default(),
                        (needs_class_lookup && watched)
                            .then(|| classes.cloned().unwrap_or_default()),
                        watch.filter(|_| watched).cloned().unwrap_or_default(),
                        sync,
                        collider.map(|collider| {
                            (*collider, layers.copied().unwrap_or_default())
                        }),
                    )
                },
            )
            .collect::<Vec<_>>()
    };

    let mut extracted = Vec::with_capacity(raw.len());
    world.resource_scope(
        |_, mut events: bevy_ecs::prelude::Mut<GpuEventRegistry>| {
            for (
                entity,
                physics_id,
                transform,
                solver,
                custom_shader,
                rigid_body,
                classes,
                watch,
                sync,
                collider,
            ) in raw
            {
                let mut authored_rules = watch.rules;
                if let Some(classes) = classes {
                    for class in classes.names {
                        if let Some(class_rules) =
                            class_watches.classes.get(&class)
                        {
                            for rule in class_rules {
                                if !authored_rules.contains(rule) {
                                    authored_rules.push(rule.clone());
                                }
                            }
                        }
                    }
                }
                let rules = authored_rules
                    .into_iter()
                    .filter_map(|rule| {
                        let instructions = rule.condition.compile().ok()?;
                        Some(ExtractedGpuPhysicsRule {
                            event_id: events.register(rule.event),
                            instructions,
                            mode: rule.mode,
                            payload: rule.payload,
                            cooldown_seconds: rule.cooldown_seconds,
                        })
                    })
                    .collect();
                extracted.push(ExtractedGpuPhysicsBody {
                    entity,
                    physics_id,
                    transform,
                    rigid_body,
                    solver,
                    collider,
                    custom_shader,
                    rules,
                    sync,
                });
            }
        },
    );
    extracted.sort_by_key(|body| body.physics_id.slot);
    extracted
}

/// Converts raw readback bytes into frame-bounded Rust gameplay events.
///
/// Delayed events with stale body generations are counted and ignored. This
/// is normal when an object is deleted while older GPU work is still running.
pub fn route_gpu_physics_events(
    world: &mut World,
    raw_events: &[RawGpuPhysicsEvent],
) -> GpuEventRouteReport {
    let resolved = {
        let ids = world.resource::<PhysicsIdRegistry>();
        raw_events
            .iter()
            .map(|raw| (*raw, ids.resolve(raw.physics_id())))
            .collect::<Vec<_>>()
    };
    let known_events = world.resource::<GpuEventRegistry>();
    let mut routed = Vec::with_capacity(resolved.len());
    let mut report = GpuEventRouteReport::default();
    for (raw, entity) in resolved {
        let Some(entity) = entity else {
            report.stale += 1;
            continue;
        };
        let event_id = GpuEventId(raw.event_id);
        if known_events.name(event_id).is_none() {
            report.unknown_event += 1;
            continue;
        }
        routed.push(GpuPhysicsEvent {
            entity,
            physics_id: raw.physics_id(),
            event_id,
            tick: raw.tick(),
            flags: raw.flags,
            payload_kind: raw.payload_kind,
            payload: raw.payload,
        });
        report.delivered += 1;
    }
    // The GPU appends events with an atomic counter, so their buffer order
    // changes from run to run. Deliver them in a fixed order instead.
    routed.sort_by_key(|event| {
        (
            event.tick,
            event.physics_id.slot,
            event.physics_id.generation,
            event.event_id.0,
            event.flags,
        )
    });
    let mut events = world.resource_mut::<EventQueue<GpuPhysicsEvent>>();
    for event in routed {
        events.send(event);
    }
    report
}

/// Installs CPU-side resources needed by the asynchronous physics bridge.
#[derive(Clone, Copy, Debug, Default)]
pub struct HybridPhysicsPlugin;

impl Plugin for HybridPhysicsPlugin {
    fn build(&self, app: &mut App) -> Result<(), AppError> {
        if !app.world().contains_resource::<PhysicsIdRegistry>() {
            app.insert_resource(PhysicsIdRegistry::default());
        }
        if !app.world().contains_resource::<GpuEventRegistry>() {
            app.insert_resource(GpuEventRegistry::default());
        }
        if !app.world().contains_resource::<GpuPhysicsClassWatches>() {
            app.insert_resource(GpuPhysicsClassWatches::default());
        }
        if !app.world().contains_resource::<GpuPhysicsCommands>() {
            app.insert_resource(GpuPhysicsCommands::default());
        }
        if !app.world().contains_resource::<GpuConditionShaders>() {
            app.insert_resource(GpuConditionShaders::default());
        }
        if !app.world().contains_resource::<AutoAllocationPolicy>() {
            app.insert_resource(AutoAllocationPolicy::default());
        }
        app.add_event::<GpuPhysicsEvent>()
            .add_event::<GpuPhysicsEventsLost>()
            .add_systems(
                ScheduleStage::PostUpdate,
                (allocate_auto_simulation, maintain_gpu_physics_ids).chain(),
            )
            .add_systems(ScheduleStage::Update, sync_gpu_query_proxies)
            .add_systems(ScheduleStage::FixedUpdate, stream_gpu_bodies);
        Ok(())
    }
}

/// Adds IDs only when a physics body changes and releases removed IDs.
///
/// GPU simulation does not change the ECS body component every render frame,
/// so a stable swarm costs no CPU work here after its setup frame.
fn maintain_gpu_physics_ids(
    mut commands: Commands,
    changed_bodies: Query<
        (Entity, &super::PhysicsBody),
        Changed<super::PhysicsBody>,
    >,
    mut removed_bodies: RemovedComponents<super::PhysicsBody>,
    mut registry: ResMut<PhysicsIdRegistry>,
) {
    // Removal messages also cover despawned entities, which have no
    // component left to remove.
    for entity in removed_bodies.read() {
        if registry.release(entity).is_none() {
            continue;
        }
        if let Ok(mut entity) = commands.get_entity(entity) {
            entity.try_remove::<PhysicsId>();
        }
    }

    for (entity, body) in &changed_bodies {
        if body.uses_gpu() {
            let id = registry.assign(entity);
            commands.entity(entity).insert(id);
        } else {
            registry.release(entity);
            commands.entity(entity).remove::<PhysicsId>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_at_the_last_generation_is_retired_not_wrapped() {
        let first = Entity::from_raw_u32(1).unwrap();
        let second = Entity::from_raw_u32(2).unwrap();
        let mut ids = PhysicsIdRegistry::default();
        let oldest = ids.assign(first);
        ids.release(first);
        ids.slots[0].generation = u32::MAX - 1;
        let last = ids.assign(first);
        assert_eq!(last.generation, u32::MAX - 1);
        ids.release(first);
        let final_use = ids.assign(first);
        assert_eq!((final_use.slot, final_use.generation), (0, u32::MAX));
        ids.release(first);

        // Wrapping would hand slot 0 out again at generation 1.
        let fresh = ids.assign(second);
        assert_eq!(fresh.slot, 1);
        for id in [oldest, last, final_use] {
            assert_eq!(ids.resolve(id), None, "{id:?}");
        }
        assert_eq!(ids.resolve(fresh), Some(second));
    }
}
