//! Full world snapshots, used to seek in replays. A [`WorldSnapshot`]
//! copies every entity with its components (resources are entities in
//! bevy 0.19) and the entity allocator's state. Restoring it into a new
//! [`App`](super::App) built the same way gives back the same entity ids, so
//! the simulation continues with the same per-tick hashes.
//!
//! Each component type must be registered with
//! [`App::register_snapshot_component`](super::App::register_snapshot_component)
//! (it copies with `Clone`) or skipped with
//! [`App::ignore_in_snapshots`](super::App::ignore_in_snapshots). A snapshot
//! of a world holding any other type fails and names it.

use std::any::TypeId;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use bevy_ecs::change_detection::{DetectChangesMut, Tick, MAX_CHANGE_AGE};
use bevy_ecs::component::{Component, ComponentId, Mutable};
use bevy_ecs::entity::{Entity, EntityAllocator, EntityGeneration};
use bevy_ecs::resource::IsResource;
use bevy_ecs::world::{EntityRef, EntityWorldMut, World};

use super::*;
use crate::Transform;

type Capture = fn(&EntityRef) -> Box<dyn StoredComponent>;

/// Which component types a snapshot copies and which it skips.
#[derive(Default)]
pub(super) struct SnapshotTypes {
    capture: HashMap<TypeId, Capture>,
    ignored: HashSet<TypeId>,
}

impl SnapshotTypes {
    pub(super) fn register<T>(&mut self)
    where
        T: Component<Mutability = Mutable> + Clone,
    {
        self.capture.insert(TypeId::of::<T>(), capture::<T>);
    }

    pub(super) fn ignore<T: 'static>(&mut self) {
        self.ignored.insert(TypeId::of::<T>());
    }
}

trait StoredComponent: Send + Sync {
    fn component_type(&self) -> TypeId;
    /// Inserts or overwrites the component, dated `tick`.
    fn write(&self, entity: &mut EntityWorldMut, tick: Tick);
}

struct Stored<T>(T);

impl<T> StoredComponent for Stored<T>
where
    T: Component<Mutability = Mutable> + Clone,
{
    fn component_type(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn write(&self, entity: &mut EntityWorldMut, tick: Tick) {
        if let Some(mut current) = entity.get_mut::<T>() {
            *current = self.0.clone();
        } else {
            entity.insert(self.0.clone());
        }
        let mut current = entity.get_mut::<T>().expect("written above");
        current.set_last_added(tick);
        current.set_last_changed(tick);
    }
}

fn capture<T>(entity: &EntityRef) -> Box<dyn StoredComponent>
where
    T: Component<Mutability = Mutable> + Clone,
{
    let value = entity.get::<T>().expect("the archetype holds it");
    Box::new(Stored(value.clone()))
}

/// A copy of a whole world; see the module docs.
pub struct WorldSnapshot {
    pub(super) tick: u64,
    pub(super) startup_complete: bool,
    /// Spawned entities in ascending order.
    pub(super) entities: Vec<EntitySnapshot>,
    pub(super) allocator: AllocatorState,
}

impl WorldSnapshot {
    /// Fixed ticks completed when the snapshot was taken.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }
}

pub(super) struct EntitySnapshot {
    pub(super) entity: Entity,
    components: Vec<Box<dyn StoredComponent>>,
    /// Components kept from the app restored into.
    ignored: Vec<TypeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// Component or resource types with no snapshot registration.
    Unregistered(Vec<String>),
    /// The app restored into was not built like the snapshotted one.
    Incompatible(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unregistered(types) => write!(
                formatter,
                "cannot snapshot the world: register these types with \
                 App::register_snapshot_component or \
                 App::ignore_in_snapshots: {}",
                types.join(", ")
            ),
            Self::Incompatible(message) => {
                write!(formatter, "cannot restore the snapshot: {message}")
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

pub(super) fn capture_world(
    world: &mut World,
    types: &SnapshotTypes,
) -> Result<Vec<EntitySnapshot>, SnapshotError> {
    let resource_marker = TypeId::of::<IsResource>();
    let mut missing = BTreeSet::new();
    let mut entities = Vec::new();
    for archetype in world.archetypes().iter() {
        if archetype.is_empty() {
            continue;
        }
        let mut captures = Vec::new();
        let mut ignored = Vec::new();
        for id in archetype.components() {
            let info = world.components().get_info(*id).expect("registered");
            match info.type_id() {
                // Inserting the resource inserts its marker.
                Some(type_id) if type_id == resource_marker => {}
                Some(type_id) if types.ignored.contains(&type_id) => {
                    ignored.push(type_id);
                }
                Some(type_id) if types.capture.contains_key(&type_id) => {
                    captures.push(types.capture[&type_id]);
                }
                _ => {
                    missing.insert(info.name().to_string());
                }
            }
        }
        for archetype_entity in archetype.entities() {
            let entity = world.entity(archetype_entity.id());
            entities.push(EntitySnapshot {
                entity: entity.id(),
                components: captures
                    .iter()
                    .map(|capture| capture(&entity))
                    .collect(),
                ignored: ignored.clone(),
            });
        }
    }
    if !missing.is_empty() {
        return Err(SnapshotError::Unregistered(missing.into_iter().collect()));
    }
    entities.sort_unstable_by_key(|saved| saved.entity);
    Ok(entities)
}

/// Size of the free list bevy keeps per allocator before moving it to the
/// shared one (`remote_allocator::Allocator::local_free`, bevy 0.19).
const LOCAL_FREE_CAPACITY: usize = 128;

/// Everything that decides which ids the entity allocator hands out next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AllocatorState {
    /// Bevy's shared free list, next allocation first.
    pub(super) shared: Vec<Entity>,
    /// Entities freed since the last move to the shared list, oldest first.
    /// Allocation cannot reach them until 128 pile up.
    pub(super) local: Vec<Entity>,
    /// First index never allocated.
    pub(super) fresh: u32,
}

impl AllocatorState {
    fn free(&self) -> impl Iterator<Item = Entity> + '_ {
        self.shared.iter().chain(&self.local).copied()
    }
}

// ponytail: an allocated but never spawned index with generation 0 is taken
// for a fresh one. Bevy only frees such indices if code frees them by hand.
fn is_fresh(world: &World, entity: Entity) -> bool {
    entity.generation() == EntityGeneration::FIRST
        && !world.entities().is_index_spawned(entity.index())
}

/// Reads the allocator's state. Bevy offers no way to look at it, so this
/// allocates and frees until it has seen everything, which scrambles the
/// allocator: follow with [`write_allocator`].
pub(super) fn read_allocator(world: &mut World) -> AllocatorState {
    let mut shared = Vec::new();
    let first_fresh = loop {
        let entity = world.entity_allocator().alloc();
        if is_fresh(world, entity) {
            break entity;
        }
        shared.push(entity);
    };
    // Freeing one more than the local list holds makes bevy move the local
    // list to the shared one, below whatever of ours moves with it.
    let mut ours: Vec<Entity> =
        shared.iter().copied().chain([first_fresh]).collect();
    while ours.len() <= LOCAL_FREE_CAPACITY {
        ours.push(world.entity_allocator().alloc());
    }
    for &entity in &ours {
        world.entity_allocator_mut().free(entity);
    }
    let ours: HashSet<Entity> = ours.into_iter().collect();
    let mut local = Vec::new();
    loop {
        let entity = world.entity_allocator().alloc();
        if ours.contains(&entity) {
            continue;
        }
        if is_fresh(world, entity) {
            break;
        }
        local.push(entity);
    }
    local.reverse();
    AllocatorState {
        shared,
        local,
        fresh: first_fresh.index_u32(),
    }
}

/// Replaces the world's allocator with one in `state`.
pub(super) fn write_allocator(world: &mut World, state: &AllocatorState) {
    let allocator = world.entity_allocator_mut();
    *allocator = EntityAllocator::default();
    // A free list longer than the local one goes straight to the shared
    // list. Fresh indices at its bottom pad it: they come out last, in the
    // order they would anyway.
    let padding =
        (LOCAL_FREE_CAPACITY + 1).saturating_sub(state.shared.len()) as u32;
    drop(allocator.alloc_many(state.fresh + padding));
    let bottom_first: Vec<Entity> = (state.fresh..state.fresh + padding)
        .rev()
        .map(|index| Entity::from_raw_u32(index).expect("valid index"))
        .chain(state.shared.iter().rev().copied())
        .collect();
    allocator.free_many(&bottom_first);
    allocator.free_many(&state.local);
}

fn spawned_entities(world: &World) -> Vec<Entity> {
    world
        .archetypes()
        .iter()
        .flat_map(|archetype| archetype.entities().iter().map(|e| e.id()))
        .collect()
}

fn type_name(world: &World, type_id: TypeId) -> String {
    world
        .components()
        .get_id(type_id)
        .and_then(|id| world.components().get_name(id))
        .map_or_else(|| format!("{type_id:?}"), |name| name.to_string())
}

/// Makes `world` equal to `snapshot`. The world should come from the same
/// setup as the snapshotted one and be no further along, since entity
/// generations cannot go back. On error the world is left half restored.
pub(super) fn restore_world(
    world: &mut World,
    snapshot: &WorldSnapshot,
) -> Result<(), SnapshotError> {
    let incompatible =
        |message: String| Err(SnapshotError::Incompatible(message));
    let wanted: HashSet<Entity> =
        snapshot.entities.iter().map(|saved| saved.entity).collect();
    let existing = spawned_entities(world);
    // Checked first: despawning a resource entity breaks its resource.
    for &entity in &existing {
        if !wanted.contains(&entity)
            && world.entity(entity).contains::<IsResource>()
        {
            return incompatible(format!(
                "resource entity {entity} is not in the snapshot"
            ));
        }
    }
    let current = read_allocator(world);
    if current.fresh > snapshot.allocator.fresh {
        write_allocator(world, &current);
        return incompatible(format!(
            "the app has used {} entity indices, the snapshot {}",
            current.fresh, snapshot.allocator.fresh
        ));
    }

    let mut kept = HashSet::new();
    for entity in existing {
        if wanted.contains(&entity) {
            kept.insert(entity);
        } else {
            world.despawn_no_free(entity).expect("spawned");
        }
    }
    // Bring each index to the generation the snapshot has.
    let targets = snapshot
        .entities
        .iter()
        .map(|saved| (saved.entity, true))
        .filter(|(entity, _)| !kept.contains(entity))
        .chain(snapshot.allocator.free().map(|entity| (entity, false)));
    for (target, spawn) in targets {
        if world.entities().is_index_spawned(target.index()) {
            return incompatible(format!("entity {target} is already in use"));
        }
        let mut current = world.entities().resolve_from_index(target.index());
        // ponytail: one spawn and despawn per missing generation, since
        // bevy only raises generations that way; fine for replay churn.
        while current != target {
            let behind = target
                .generation()
                .to_bits()
                .wrapping_sub(current.generation().to_bits());
            if behind > u32::MAX / 2 {
                return incompatible(format!(
                    "entity {current} is newer than the snapshot's {target}"
                ));
            }
            world.spawn_empty_at(current).expect("not spawned");
            current = world.despawn_no_free(current).expect("just spawned");
        }
        if spawn {
            world.spawn_empty_at(target).expect("not spawned");
        }
    }
    write_allocator(world, &snapshot.allocator);

    // Anything newer than `MAX_CHANGE_AGE` counts as changed for systems
    // that have not run yet; this makes restored values look old.
    let tick =
        Tick::new(world.change_tick().get().wrapping_sub(MAX_CHANGE_AGE));
    let resource_marker = TypeId::of::<IsResource>();
    for saved in &snapshot.entities {
        if kept.contains(&saved.entity) {
            let keep: HashSet<TypeId> = saved
                .components
                .iter()
                .map(|component| component.component_type())
                .chain(saved.ignored.iter().copied())
                .chain([resource_marker])
                .collect();
            let entity = world.entity(saved.entity);
            let present: Vec<(ComponentId, Option<TypeId>)> = entity
                .archetype()
                .components()
                .iter()
                .map(|&id| {
                    let info =
                        world.components().get_info(id).expect("registered");
                    (id, info.type_id())
                })
                .collect();
            for &ignored in &saved.ignored {
                if !present.iter().any(|(_, type_id)| *type_id == Some(ignored))
                {
                    return incompatible(format!(
                        "{} has no {}, which snapshots do not copy",
                        saved.entity,
                        type_name(world, ignored)
                    ));
                }
            }
            let mut entity = world.entity_mut(saved.entity);
            for (id, type_id) in present {
                if !type_id.is_some_and(|type_id| keep.contains(&type_id)) {
                    entity.remove_by_id(id);
                }
            }
        } else if let Some(&ignored) = saved.ignored.first() {
            return incompatible(format!(
                "{} needs a {}, which snapshots do not copy",
                saved.entity,
                type_name(world, ignored)
            ));
        }
        let mut entity = world.entity_mut(saved.entity);
        for component in &saved.components {
            component.write(&mut entity, tick);
        }
    }

    world.flush();
    // Drop the removals the restore itself caused.
    world.clear_trackers();
    world.clear_trackers();
    Ok(())
}

/// The engine's own types. Game plugins register theirs.
pub(super) fn register_engine_types(types: &mut SnapshotTypes) {
    // Simulation state and anything gameplay reads.
    types.register::<AmbientLight>();
    types.register::<BurstEmitter>();
    types.register::<BurstParticle>();
    types.register::<super::ParticleEmitter>();
    types.register::<super::ParticleSystem>();
    types.register::<super::Animation>();
    types.register::<super::Skin>();
    types.register::<super::Morph>();
    types.register::<super::Ik>();
    types.register::<super::IkPose>();
    types.register::<super::Ragdoll>();
    types.register::<super::RagdollState>();
    types.register::<super::RagdollPart>();
    types.register::<super::Rope>();
    types.register::<super::RopeState>();
    types.register::<super::RopeBead>();
    types.register::<super::SkinnedMesh>();
    types.register::<super::AnimationPlayer>();
    types.register::<Camera>();
    types.register::<FluidBlock>();
    types.register::<super::WaterBody>();
    types.register::<super::GravityVolume>();
    types.register::<super::Vehicle>();
    types.register::<super::ForceField>();
    types.register::<super::AxisLock>();
    types.register::<super::Heightfield>();
    types.register::<super::Polygon2d>();
    types.register::<super::MeshSurfaces>();
    types.register::<super::PhysicsMaterial>();
    types.register::<super::WaterMesh>();
    types.register::<super::FluidParticle>();
    types.register::<super::fluid::OwnedSurface>();
    types.register::<super::FluidVolume>();
    types.register::<super::SoftBodyVolume>();
    types.register::<super::ClothVolume>();
    types.register::<Children>();
    types.register::<Collider>();
    types.register::<CollisionLayers>();
    types.register::<Counter>();
    types.register::<super::ObjectState>();
    types.register::<super::Health>();
    types.register::<super::LootTable>();
    types.register::<super::Dialogue>();
    types.register::<DirectionalLight>();
    types.register::<GlobalTransform>();
    types.register::<GpuProxyOf>();
    types.register::<GpuQueryProxy>();
    types.register::<GpuStateMirror>();
    types.register::<HudElement>();
    types.register::<Joint>();
    types.register::<Articulation>();
    types.register::<MeshRenderer>();
    types.register::<Name>();
    types.register::<ObjectClasses>();
    types.register::<Parent>();
    types.register::<PhysicsBody>();
    types.register::<PhysicsId>();
    types.register::<Pickup>();
    types.register::<PlatformerController>();
    types.register::<PlayerController>();
    types.register::<PointLight>();
    types.register::<RenderBounds>();
    types.register::<RigidBody>();
    types.register::<SceneBackground>();
    types.register::<super::EnvironmentMap>();
    types.register::<super::LightCookie>();
    types.register::<super::ReflectionProbe>();
    types.register::<super::Fog>();
    types.register::<super::ColorGrading>();
    types.register::<super::ColorLut>();
    types.register::<super::PostVolume>();
    types.register::<super::FogVolume>();
    types.register::<super::CameraScreen>();
    types.register::<super::Bloom>();
    types.register::<super::AutoExposure>();
    types.register::<super::DepthOfField>();
    types.register::<super::MotionBlur>();
    types.register::<super::AmbientOcclusion>();
    types.register::<SceneId>();
    types.register::<SkyLight>();
    types.register::<Sleeping>();
    types.register::<super::SpawnOrder>();
    types.register::<SoundCue>();
    types.register::<super::ReverbZone>();
    types.register::<super::SlideSound>();
    types.register::<super::CameraShake>();
    types.register::<super::Squash>();
    types.register::<super::Flash>();
    types.register::<super::SpawnGrid>();
    types.register::<SpotLight>();
    types.register::<TileMap>();
    types.register::<TileOf>();
    types.register::<ToneMapping>();
    types.register::<Transform>();
    types.register::<Tween>();
    types.register::<UnregisteredComponents>();
    types.register::<Visibility>();

    types.register::<ActionMap>();
    types.register::<super::actions::SceneBindings>();
    types.register::<AutoAllocationPolicy>();
    types.register::<DeterminismMode>();
    types.register::<DeterminismSupport>();
    types.register::<EventQueue<ClickEvent>>();
    types.register::<EventQueue<CollisionEvent>>();
    types.register::<EventQueue<JointBroken>>();
    types.register::<EventQueue<FellOut>>();
    types.register::<EventQueue<GpuPhysicsEvent>>();
    types.register::<EventQueue<GpuPhysicsEventsLost>>();
    types.register::<EventQueue<HudButtonPressed>>();
    types.register::<EventQueue<super::AnimationEvent>>();
    types.register::<EventQueue<SoundEvent>>();
    types.register::<AudioQueue>();
    types.register::<ExitState>();
    types.register::<FrameTime>();
    types.register::<GpuConditionShaders>();
    types.register::<GpuEventRegistry>();
    types.register::<GpuPhysicsClassWatches>();
    types.register::<GpuPhysicsCommands>();
    types.register::<HierarchyDiagnostics>();
    types.register::<PhysicsBackendStatus>();
    types.register::<PhysicsIdRegistry>();
    types.register::<PhysicsSettings>();
    types.register::<super::NextSpawnOrder>();
    types.register::<PhysicsWorld>();
    types.register::<rusting_core::hierarchy::PropagationFingerprint>();
    types.register::<RandomSeed>();
    types.register::<super::Connections>();
    types.register::<RenderCameraOverride>();
    types.register::<RenderSettings>();
    types.register::<RuntimeInput>();
    types.register::<StateHashes>();
    types.register::<TimeControl>();

    // Fixed after setup, rebuilt every frame, or not simulation state.
    types.ignore::<bevy_ecs::entity_disabling::DefaultQueryFilters>();
    types.ignore::<bevy_ecs::schedule::Schedules>();
    types.ignore::<crate::assets::AssetServer>();
    types.ignore::<crate::assets::DataAssetTypes>();
    types.ignore::<CpuFrameTimings>();
    types.ignore::<super::HitStop>();
    types.ignore::<RenderWorld>();
    types.ignore::<SceneComponentRegistry>();
    types.ignore::<super::signals::SignalHandlers>();
    // Rebuilt after every restore.
    types.ignore::<super::ClassIndex>();
    types.ignore::<bevy_ecs::observer::Observer>();
    #[cfg(feature = "ui")]
    types.ignore::<RuntimeUi>();
}
