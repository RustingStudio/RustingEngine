//! Reflection: descriptions of component, resource, and asset types that
//! scene files, the Inspector, `rusting schema`, and field paths share.
//!
//! [`reflect!`](crate::reflect!) writes the [`Reflect`] impl of a type from a
//! list of its fields with editor hints. It also emits a check that
//! destructures the type with exactly those fields, and matches an enum on
//! exactly those variants, so adding, renaming, or removing a field without
//! updating the description does not compile:
//!
//! ```
//! use rusting_engine::reflect::{Reflect, TypeInfo};
//!
//! #[derive(Default, serde::Serialize, serde::Deserialize)]
//! struct Health {
//!     current: f32,
//!     max: f32,
//!     #[serde(skip)]
//!     flashing: bool,
//! }
//!
//! rusting_engine::reflect! {
//!     struct Health {
//!         current: f32 { min: 0.0, doc: "hit points left" },
//!         max: f32 { min: 1.0 },
//!         #[skip] flashing: bool,
//!     }
//! }
//!
//! let TypeInfo::Struct(info) = Health::type_info() else { unreachable!() };
//! assert_eq!(info.fields.len(), 2);
//! ```
//!
//! Serialization stays serde. Reflection adds what serde does not know:
//! field lists and hints, rejection of saved fields the type no longer has,
//! versioned [`FieldMigration`]s, and the scene form of references (an
//! [`Entity`] saves as its target's `SceneId`, a [`Handle`] as
//! `{"$asset": path}`).
//!
//! Field and variant names must match serde's: `#[serde(rename)]` and
//! `rename_all` are not supported. Mark `#[serde(skip)]` fields `#[skip]`.
//! Hints are `unit: "m/s"`, `min: 0.0`, `max: 1.0` (floats), `doc: "..."`,
//! and `color: true` for `[f32; 3]` or `[f32; 4]` colors.

mod builtin;

use std::collections::{BTreeMap, HashMap};
use std::fmt::{Debug, Display, Formatter};
use std::path::{Path, PathBuf};

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Resource, World};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::assets::{
    AssetError, AssetServer, DataAsset, Handle, MeshAsset, TextureAsset,
};
use crate::runtime::SceneId;

/// A type with a reflected description. Implement it with
/// [`reflect!`](crate::reflect!).
pub trait Reflect: 'static {
    fn type_info() -> TypeInfo;
}

/// The shape of a reflected type, in the form serde writes it to JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeInfo {
    Bool,
    Int {
        name: &'static str,
        min: i128,
        max: i128,
    },
    Float,
    String,
    /// A file path, saved as a string.
    Path,
    /// Fixed-length array.
    Array(Box<TypeInfo>, usize),
    List(Box<TypeInfo>),
    /// String-keyed map, saved as a JSON object.
    Map(Box<TypeInfo>),
    /// `null` or the inner value.
    Option(Box<TypeInfo>),
    Struct(StructInfo),
    Enum(EnumInfo),
    /// Saved as the target's `SceneId`.
    Entity,
    /// Saved as `{"$asset": path}`.
    Handle(AssetKind),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructInfo {
    pub name: &'static str,
    pub fields: Vec<FieldInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldInfo {
    pub name: &'static str,
    pub ty: TypeInfo,
    pub hints: Hints,
}

/// Editor and documentation hints of one field.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hints {
    pub unit: &'static str,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub doc: &'static str,
    /// Edit as a color: `[f32; 3]` RGB or `[f32; 4]` RGBA.
    pub color: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumInfo {
    pub name: &'static str,
    pub variants: Vec<VariantInfo>,
}

impl EnumInfo {
    #[must_use]
    pub fn variant(&self, name: &str) -> Option<&VariantInfo> {
        self.variants.iter().find(|variant| variant.name == name)
    }

    /// True when every variant is a unit variant, saved as its name.
    #[must_use]
    pub fn is_unit_only(&self) -> bool {
        self.variants
            .iter()
            .all(|variant| variant.fields == VariantFields::Unit)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantInfo {
    pub name: &'static str,
    pub fields: VariantFields,
}

/// Unit variants save as `"Name"`, the others as `{"Name": payload}`.
#[derive(Clone, Debug, PartialEq)]
pub enum VariantFields {
    Unit,
    /// One item saves as the item itself, more as an array.
    Tuple(Vec<TypeInfo>),
    Struct(Vec<FieldInfo>),
}

/// An asset type that handles can point at, with how to find a handle's
/// path and load a path.
#[derive(Clone, Copy)]
pub struct AssetKind {
    pub name: &'static str,
    path: fn(&AssetServer, u64) -> Option<PathBuf>,
    load: fn(&mut AssetServer, &Path) -> Result<u64, AssetError>,
    paths: fn(&AssetServer) -> Vec<PathBuf>,
    /// Set for data assets, whose values can also live inside a scene.
    pub embedded: Option<EmbeddedKind>,
}

/// How a data asset handle saves a value that has no file of its own: as
/// `{"$data": value}` in the scene or file that refers to it.
#[derive(Clone, Copy)]
pub struct EmbeddedKind {
    /// The data asset type's description.
    pub info: fn() -> TypeInfo,
    /// The type's `Default` value in scene form.
    pub default: fn() -> Value,
    embed: fn(&AssetServer, u64) -> Result<Value, ReflectProblem>,
    insert: fn(&mut AssetServer, Value) -> Result<u64, ReflectProblem>,
}

impl AssetKind {
    /// Loads the asset at `path`, or finds it when it is already loaded.
    pub fn load(
        &self,
        assets: &mut AssetServer,
        path: &Path,
    ) -> Result<u64, AssetError> {
        (self.load)(assets, path)
    }

    /// Path of the loaded asset `key` points at, if it came from a file.
    #[must_use]
    pub fn path_of(&self, assets: &AssetServer, key: u64) -> Option<PathBuf> {
        (self.path)(assets, key)
    }

    /// Paths of every loaded asset of this kind, sorted.
    #[must_use]
    pub fn loaded_paths(&self, assets: &AssetServer) -> Vec<PathBuf> {
        let mut paths = (self.paths)(assets);
        paths.sort();
        paths
    }
}

impl Debug for AssetKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name)
    }
}

impl PartialEq for AssetKind {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

/// Asset types a reflected [`Handle`] can point at.
pub trait ReflectAsset: Sized + 'static {
    const KIND: AssetKind;
}

fn texture_path(assets: &AssetServer, key: u64) -> Option<PathBuf> {
    assets
        .textures
        .path(Handle::from_key(key))
        .map(Path::to_path_buf)
}

fn load_texture(
    assets: &mut AssetServer,
    path: &Path,
) -> Result<u64, AssetError> {
    assets.load_texture(path).map(Handle::key)
}

fn mesh_path(assets: &AssetServer, key: u64) -> Option<PathBuf> {
    assets
        .meshes
        .path(Handle::from_key(key))
        .map(Path::to_path_buf)
}

fn load_mesh(assets: &mut AssetServer, path: &Path) -> Result<u64, AssetError> {
    assets.load_mesh(path).map(Handle::key)
}

fn texture_paths(assets: &AssetServer) -> Vec<PathBuf> {
    assets
        .textures
        .paths()
        .map(|(_, path)| path.to_path_buf())
        .collect()
}

fn mesh_paths(assets: &AssetServer) -> Vec<PathBuf> {
    assets
        .meshes
        .paths()
        .map(|(_, path)| path.to_path_buf())
        .collect()
}

impl ReflectAsset for TextureAsset {
    const KIND: AssetKind = AssetKind {
        name: "texture",
        path: texture_path,
        load: load_texture,
        paths: texture_paths,
        embedded: None,
    };
}

impl ReflectAsset for MeshAsset {
    const KIND: AssetKind = AssetKind {
        name: "mesh",
        path: mesh_path,
        load: load_mesh,
        paths: mesh_paths,
        embedded: None,
    };
}

fn data_path<T: DataAsset>(assets: &AssetServer, key: u64) -> Option<PathBuf> {
    assets
        .data
        .store::<T>()?
        .path(Handle::from_key(key))
        .map(Path::to_path_buf)
}

fn load_data<T: DataAsset>(
    assets: &mut AssetServer,
    path: &Path,
) -> Result<u64, AssetError> {
    assets.load_data::<T>(path).map(Handle::key)
}

fn data_paths<T: DataAsset>(assets: &AssetServer) -> Vec<PathBuf> {
    assets.data.store::<T>().map_or_else(Vec::new, |store| {
        store.paths().map(|(_, path)| path.to_path_buf()).collect()
    })
}

impl<T: DataAsset> ReflectAsset for T {
    const KIND: AssetKind = AssetKind {
        name: T::NAME,
        path: data_path::<T>,
        load: load_data::<T>,
        paths: data_paths::<T>,
        embedded: Some(EmbeddedKind {
            info: T::type_info,
            default: default_data::<T>,
            embed: embed_data::<T>,
            insert: insert_data::<T>,
        }),
    };
}

pub(crate) fn default_data<T: DataAsset>() -> Value {
    serde_json::to_value(T::default()).unwrap_or(Value::Null)
}

fn embed_data<T: DataAsset>(
    assets: &AssetServer,
    key: u64,
) -> Result<Value, ReflectProblem> {
    assets.embedded_data::<T>(Handle::from_key(key))
}

fn insert_data<T: DataAsset>(
    assets: &mut AssetServer,
    value: Value,
) -> Result<u64, ReflectProblem> {
    assets.insert_embedded_data::<T>(value).map(Handle::key)
}

/// Turns a handle key into its scene form: `{"$asset": path}` for an
/// asset loaded from a file, `{"$data": value}` for a data asset without
/// one.
pub fn handle_to_scene(
    kind: &AssetKind,
    assets: &AssetServer,
    value: &mut Value,
) -> Result<(), ReflectProblem> {
    let key = value
        .as_u64()
        .ok_or(ReflectProblem::WrongKind("a handle key"))?;
    *value = match (kind.path_of(assets, key), kind.embedded) {
        (Some(path), _) => json!({ ASSET_KEY: path }),
        (None, Some(embedded)) => {
            json!({ DATA_KEY: (embedded.embed)(assets, key)? })
        }
        (None, None) => return Err(ReflectProblem::UnsavedAsset),
    };
    Ok(())
}

/// Turns a handle's scene form back into a key. A file loads once and is
/// shared; every `{"$data": value}` becomes its own copy.
pub fn handle_from_scene(
    kind: &AssetKind,
    assets: &mut AssetServer,
    value: &mut Value,
) -> Result<(), ReflectProblem> {
    let key = if let Some(path) = value.get(ASSET_KEY).and_then(Value::as_str) {
        kind.load(assets, Path::new(path))
            .map_err(|error| ReflectProblem::Asset(error.to_string()))?
    } else if let (Some(data), Some(embedded)) =
        (value.get_mut(DATA_KEY), kind.embedded)
    {
        (embedded.insert)(assets, data.take())?
    } else {
        return Err(ReflectProblem::WrongKind(HANDLE_FORM));
    };
    *value = key.into();
    Ok(())
}

impl<T: ReflectAsset> Reflect for Handle<T> {
    fn type_info() -> TypeInfo {
        TypeInfo::Handle(T::KIND)
    }
}

impl Reflect for Entity {
    fn type_info() -> TypeInfo {
        TypeInfo::Entity
    }
}

macro_rules! reflect_leaf {
    ($($ty:ty => $info:expr),* $(,)?) => {
        $(impl Reflect for $ty {
            fn type_info() -> TypeInfo {
                $info
            }
        })*
    };
}

macro_rules! reflect_int {
    ($($ty:ty),*) => {
        $(impl Reflect for $ty {
            fn type_info() -> TypeInfo {
                TypeInfo::Int {
                    name: stringify!($ty),
                    min: <$ty>::MIN as i128,
                    max: <$ty>::MAX as i128,
                }
            }
        })*
    };
}

reflect_leaf! {
    bool => TypeInfo::Bool,
    f32 => TypeInfo::Float,
    f64 => TypeInfo::Float,
    String => TypeInfo::String,
    PathBuf => TypeInfo::Path,
}
reflect_int!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

impl<T: Reflect, const N: usize> Reflect for [T; N] {
    fn type_info() -> TypeInfo {
        TypeInfo::Array(Box::new(T::type_info()), N)
    }
}

impl<T: Reflect> Reflect for Vec<T> {
    fn type_info() -> TypeInfo {
        TypeInfo::List(Box::new(T::type_info()))
    }
}

impl<T: Reflect> Reflect for Option<T> {
    fn type_info() -> TypeInfo {
        TypeInfo::Option(Box::new(T::type_info()))
    }
}

impl<T: Reflect> Reflect for BTreeMap<String, T> {
    fn type_info() -> TypeInfo {
        TypeInfo::Map(Box::new(T::type_info()))
    }
}

impl<T: Reflect, S: 'static> Reflect for HashMap<String, T, S> {
    fn type_info() -> TypeInfo {
        TypeInfo::Map(Box::new(T::type_info()))
    }
}

/// Converts a hint value written in [`reflect!`](crate::reflect!) into its
/// [`Hints`] field.
pub trait IntoHint<T> {
    fn into_hint(self) -> T;
}

impl IntoHint<&'static str> for &'static str {
    fn into_hint(self) -> &'static str {
        self
    }
}

impl IntoHint<Option<f64>> for f64 {
    fn into_hint(self) -> Option<f64> {
        Some(self)
    }
}

impl IntoHint<bool> for bool {
    fn into_hint(self) -> bool {
        self
    }
}

/// Writes [`Reflect`] for a struct, a one-field tuple struct, or an enum.
/// See the [module docs](mod@crate::reflect).
#[macro_export]
macro_rules! reflect {
    (struct $name:ident { $($body:tt)* }) => {
        impl $crate::reflect::Reflect for $name {
            fn type_info() -> $crate::reflect::TypeInfo {
                $crate::reflect::TypeInfo::Struct($crate::reflect::StructInfo {
                    name: ::std::stringify!($name),
                    fields: $crate::reflect!(@fields $($body)*),
                })
            }
        }
        const _: () = {
            #[allow(dead_code)]
            fn check(value: &$name) {
                $crate::reflect!(@check_fields [$name] value; $($body)*);
            }
        };
    };
    (struct $name:ident ( $inner:ty )) => {
        impl $crate::reflect::Reflect for $name {
            fn type_info() -> $crate::reflect::TypeInfo {
                <$inner as $crate::reflect::Reflect>::type_info()
            }
        }
        const _: () = {
            #[allow(dead_code)]
            fn check() {
                let _: fn($inner) -> $name = $name;
            }
        };
    };
    (enum $name:ident {
        $( $variant:ident $( ( $($tuple:ty),* $(,)? ) )? $( { $($body:tt)* } )? ),* $(,)?
    }) => {
        impl $crate::reflect::Reflect for $name {
            fn type_info() -> $crate::reflect::TypeInfo {
                $crate::reflect::TypeInfo::Enum($crate::reflect::EnumInfo {
                    name: ::std::stringify!($name),
                    variants: ::std::vec![$(
                        $crate::reflect::VariantInfo {
                            name: ::std::stringify!($variant),
                            fields: $crate::reflect!(
                                @variant [$( $($tuple),* )?] [$( $($body)* )?]
                            ),
                        }
                    ),*],
                })
            }
        }
        const _: () = {
            #[allow(dead_code)]
            fn check(value: &$name) {
                match value {
                    $( $name::$variant { .. } => {} )*
                }
                $( $crate::reflect!(
                    @check_variant $name, $variant, value,
                    [$( $($tuple),* )?] [$( $($body)* )?]
                ); )*
            }
        };
    };

    (@fields $(
        $(#[$attr:ident])? $field:ident : $ty:ty
        $({ $($hint:ident : $value:expr),* $(,)? })?
    ),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut fields = ::std::vec::Vec::new();
        $( $crate::reflect!(
            @field fields, [$($attr)?], $field, $ty, { $($($hint : $value),*)? }
        ); )*
        fields
    }};
    (@field $fields:ident, [skip], $field:ident, $ty:ty,
        { $($hint:ident : $value:expr),* }) => {};
    (@field $fields:ident, [], $field:ident, $ty:ty,
        { $($hint:ident : $value:expr),* }) => {
        $fields.push($crate::reflect::FieldInfo {
            name: ::std::stringify!($field),
            ty: <$ty as $crate::reflect::Reflect>::type_info(),
            hints: {
                #[allow(unused_mut)]
                let mut hints = $crate::reflect::Hints::default();
                $( hints.$hint = $crate::reflect::IntoHint::into_hint($value); )*
                hints
            },
        });
    };

    (@variant [] []) => { $crate::reflect::VariantFields::Unit };
    (@variant [$($tuple:ty),+] []) => {
        $crate::reflect::VariantFields::Tuple(::std::vec![
            $( <$tuple as $crate::reflect::Reflect>::type_info() ),+
        ])
    };
    (@variant [] [$($body:tt)+]) => {
        $crate::reflect::VariantFields::Struct($crate::reflect!(@fields $($body)+))
    };

    (@check_variant $name:ident, $variant:ident, $value:ident, [] []) => {};
    (@check_variant $name:ident, $variant:ident, $value:ident,
        [$($tuple:ty),+] []) => {
        let _: fn($($tuple),+) -> $name = $name::$variant;
    };
    (@check_variant $name:ident, $variant:ident, $value:ident,
        [] [$($body:tt)+]) => {
        $crate::reflect!(@check_fields [$name::$variant] $value; $($body)+);
    };

    (@check_fields [$($path:tt)+] $value:ident; $(
        $(#[$attr:ident])? $field:ident : $ty:ty
        $({ $($hint:ident : $hint_value:expr),* $(,)? })?
    ),* $(,)?) => {
        #[allow(irrefutable_let_patterns)]
        if let $($path)+ { $($field),* } = $value {
            $( let _: &$ty = $field; )*
        }
    };
}

/// Key of a saved component's version when it has migrations.
pub const VERSION_KEY: &str = "$version";
/// Key of a saved asset reference: `{"$asset": path}`.
pub const ASSET_KEY: &str = "$asset";
/// Key of a data asset saved inside the scene or file that refers to it:
/// `{"$data": value}`.
pub const DATA_KEY: &str = "$data";
const HANDLE_FORM: &str = "{\"$asset\": path} or {\"$data\": value}";

/// One step in the history of a reflected component. Each registered step
/// raises the component's version by one; values saved at an older version
/// run the steps after it when they load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldMigration {
    /// A top-level field was renamed.
    Rename { from: String, to: String },
    /// A top-level field was removed; saved values of it are dropped.
    Remove(String),
}

impl FieldMigration {
    #[must_use]
    pub fn rename(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::Rename {
            from: from.into(),
            to: to.into(),
        }
    }

    #[must_use]
    pub fn remove(field: impl Into<String>) -> Self {
        Self::Remove(field.into())
    }
}

/// What is wrong at a [`ReflectError`]'s path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReflectProblem {
    /// A saved field the type does not have.
    UnknownField,
    /// A saved enum variant the type does not have.
    UnknownVariant(String, Vec<&'static str>),
    /// Saved by a build with more migrations than this one.
    NewerVersion,
    /// A handle to an asset that was not loaded from a file.
    UnsavedAsset,
    /// The asset a saved reference names failed to load.
    Asset(String),
    /// A value of the wrong kind; names the expected one.
    WrongKind(&'static str),
    /// The path names nothing in the type.
    NoSuchPath,
    /// A migration on a component that is not a struct.
    NotAStruct,
}

/// A reflected component value that cannot be read or written, with the
/// saved and current version so a tool can tell which migration is missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectError {
    pub component: String,
    /// Version the value was saved at; the current one for runtime edits.
    pub saved_version: u32,
    pub current_version: u32,
    /// JSON pointer into the component's scene form.
    pub path: String,
    pub problem: ReflectProblem,
    /// The scene object holding the component, when loading a scene.
    pub object: Option<Uuid>,
}

impl Display for ReflectError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let path = if self.path.is_empty() {
            "/"
        } else {
            &self.path
        };
        write!(
            formatter,
            "component `{}` at `{path}` (saved version {}, current {}): ",
            self.component, self.saved_version, self.current_version
        )?;
        Display::fmt(&self.problem, formatter)
    }
}

impl Display for ReflectProblem {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ReflectProblem::UnknownField => formatter.write_str(
                "the type has no such field; register a rename or remove \
                 migration instead of dropping the saved value",
            ),
            ReflectProblem::UnknownVariant(name, expected) => write!(
                formatter,
                "the type has no variant `{name}`; expected one of: {}",
                expected.join(", ")
            ),
            ReflectProblem::NewerVersion => formatter
                .write_str("saved by a newer build with more migrations"),
            ReflectProblem::UnsavedAsset => formatter.write_str(
                "references an asset that was not loaded from a file",
            ),
            ReflectProblem::Asset(message) => {
                write!(formatter, "asset failed to load: {message}")
            }
            ReflectProblem::WrongKind(expected) => {
                write!(formatter, "expected {expected}")
            }
            ReflectProblem::NoSuchPath => {
                formatter.write_str("the type has nothing at this path")
            }
            ReflectProblem::NotAStruct => formatter
                .write_str("migrations need a component saved as a struct"),
        }
    }
}

impl std::error::Error for ReflectError {}

/// A problem and the JSON pointer it was found at.
pub type Located = (String, ReflectProblem);

impl TypeInfo {
    /// True when values hold an [`Entity`] or a [`Handle`] somewhere, which
    /// saves in a different form than serde writes.
    #[must_use]
    pub fn has_references(&self) -> bool {
        match self {
            Self::Entity | Self::Handle(_) => true,
            Self::Array(item, _)
            | Self::List(item)
            | Self::Map(item)
            | Self::Option(item) => item.has_references(),
            Self::Struct(info) => {
                info.fields.iter().any(|field| field.ty.has_references())
            }
            Self::Enum(info) => {
                info.variants.iter().any(|variant| match &variant.fields {
                    VariantFields::Unit => false,
                    VariantFields::Tuple(items) => {
                        items.iter().any(Self::has_references)
                    }
                    VariantFields::Struct(fields) => {
                        fields.iter().any(|field| field.ty.has_references())
                    }
                })
            }
            _ => false,
        }
    }

    /// A plain value of this type, for new list items and variant switches:
    /// false, zero, empty, `null`, or the first variant.
    #[must_use]
    pub fn zero_value(&self) -> Value {
        match self {
            Self::Bool => Value::Bool(false),
            Self::Int { .. } => json!(0),
            Self::Float => json!(0.0),
            Self::String | Self::Path => json!(""),
            Self::Array(item, len) => {
                Value::Array(vec![item.zero_value(); *len])
            }
            Self::List(_) => json!([]),
            Self::Map(_) => json!({}),
            Self::Option(_) | Self::Entity | Self::Handle(_) => Value::Null,
            Self::Struct(info) => fields_zero(&info.fields),
            Self::Enum(info) => {
                info.variants.first().map_or(Value::Null, variant_zero)
            }
        }
    }

    /// What a value of this type looks like, for error messages.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Bool => "a boolean",
            Self::Int { .. } => "an integer",
            Self::Float => "a number",
            Self::String => "a string",
            Self::Path => "a path string",
            Self::Array(..) => "an array of fixed length",
            Self::List(_) => "an array",
            Self::Map(_) => "an object",
            Self::Option(_) => "null or a value",
            Self::Struct(_) => "an object with the type's fields",
            Self::Enum(_) => "a variant name or {\"Variant\": payload}",
            Self::Entity => "an object ID or null",
            Self::Handle(_) => HANDLE_FORM,
        }
    }

    /// Shallow check that `value` has this type's scene form.
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Bool => value.is_boolean(),
            Self::Int { .. } => value.is_i64() || value.is_u64(),
            Self::Float => value.is_number(),
            Self::String | Self::Path => value.is_string(),
            Self::Array(_, len) => {
                value.as_array().is_some_and(|items| items.len() == *len)
            }
            Self::List(_) => value.is_array(),
            Self::Map(_) | Self::Struct(_) => value.is_object(),
            Self::Option(inner) => value.is_null() || inner.accepts(value),
            Self::Enum(_) => value.is_string() || value.is_object(),
            Self::Entity => {
                value.is_null()
                    || value
                        .as_str()
                        .is_some_and(|text| text.parse::<Uuid>().is_ok())
            }
            Self::Handle(kind) => {
                value.get(ASSET_KEY).is_some_and(Value::is_string)
                    || kind.embedded.is_some() && value.get(DATA_KEY).is_some()
            }
        }
    }

    /// The type at a JSON pointer into this type's scene form. Enum
    /// payloads are addressed by variant name, map values by any key, and
    /// an `Option` by its inner type's paths.
    #[must_use]
    pub fn at(&self, pointer: &str) -> Option<&TypeInfo> {
        if pointer.is_empty() {
            return Some(self);
        }
        if let Self::Option(inner) = self {
            return inner.at(pointer);
        }
        let (segment, rest) = split_first(pointer)?;
        match self {
            Self::Struct(info) => field_type(&info.fields, &segment)?.at(rest),
            Self::Map(item) => item.at(rest),
            Self::List(item) => {
                segment.parse::<usize>().ok()?;
                item.at(rest)
            }
            Self::Array(item, len) => {
                (segment.parse::<usize>().ok()? < *len).then_some(())?;
                item.at(rest)
            }
            Self::Enum(info) => match &info.variant(&segment)?.fields {
                VariantFields::Unit => None,
                VariantFields::Tuple(items) if items.len() == 1 => {
                    items[0].at(rest)
                }
                VariantFields::Tuple(items) => {
                    let (index, rest) = split_first(rest)?;
                    items.get(index.parse::<usize>().ok()?)?.at(rest)
                }
                VariantFields::Struct(fields) => {
                    let (name, rest) = split_first(rest)?;
                    field_type(fields, &name)?.at(rest)
                }
            },
            _ => None,
        }
    }
}

fn split_first(pointer: &str) -> Option<(String, &str)> {
    let rest = pointer.strip_prefix('/')?;
    let (segment, rest) = match rest.find('/') {
        Some(end) => (&rest[..end], &rest[end..]),
        None => (rest, ""),
    };
    Some((segment.replace("~1", "/").replace("~0", "~"), rest))
}

fn field_type<'a>(fields: &'a [FieldInfo], name: &str) -> Option<&'a TypeInfo> {
    fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| &field.ty)
}

fn fields_zero(fields: &[FieldInfo]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|field| (field.name.to_owned(), field.ty.zero_value()))
            .collect(),
    )
}

/// The saved form of `variant` with zero payload.
#[must_use]
pub fn variant_zero(variant: &VariantInfo) -> Value {
    let payload = match &variant.fields {
        VariantFields::Unit => return json!(variant.name),
        VariantFields::Tuple(items) if items.len() == 1 => {
            items[0].zero_value()
        }
        VariantFields::Tuple(items) => {
            Value::Array(items.iter().map(TypeInfo::zero_value).collect())
        }
        VariantFields::Struct(fields) => fields_zero(fields),
    };
    json!({ variant.name: payload })
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Walks `value` as `info`: rejects object keys and variants the type does
/// not know, and calls `visit` on every non-null entity and handle.
pub fn walk(
    info: &TypeInfo,
    value: &mut Value,
    visit: &mut dyn FnMut(&TypeInfo, &mut Value) -> Result<(), ReflectProblem>,
) -> Result<(), Located> {
    walk_at(info, value, &mut String::new(), visit)
}

fn walk_at(
    info: &TypeInfo,
    value: &mut Value,
    pointer: &mut String,
    visit: &mut dyn FnMut(&TypeInfo, &mut Value) -> Result<(), ReflectProblem>,
) -> Result<(), Located> {
    let located = |pointer: &String, problem| (pointer.clone(), problem);
    match (info, value) {
        // Entities are visited even when null: a dangling reference saves
        // as null and loads as `Entity::PLACEHOLDER`.
        (TypeInfo::Entity, value) => {
            visit(info, value).map_err(|problem| located(pointer, problem))
        }
        (TypeInfo::Handle(_), value) if !value.is_null() => {
            visit(info, value).map_err(|problem| located(pointer, problem))
        }
        (TypeInfo::Option(inner), value) if !value.is_null() => {
            walk_at(inner, value, pointer, visit)?;
            if **inner == TypeInfo::Entity && *value == placeholder() {
                *value = Value::Null;
            }
            Ok(())
        }
        (TypeInfo::Struct(info), Value::Object(map)) => {
            walk_fields(&info.fields, map, pointer, visit)
        }
        (
            TypeInfo::Array(item, _) | TypeInfo::List(item),
            Value::Array(items),
        ) => {
            for (index, value) in items.iter_mut().enumerate() {
                let length = pointer.len();
                pointer.push_str(&format!("/{index}"));
                walk_at(item, value, pointer, visit)?;
                pointer.truncate(length);
            }
            Ok(())
        }
        (TypeInfo::Map(item), Value::Object(map)) => {
            for (key, value) in map.iter_mut() {
                let length = pointer.len();
                pointer.push('/');
                pointer.push_str(&escape(key));
                walk_at(item, value, pointer, visit)?;
                pointer.truncate(length);
            }
            Ok(())
        }
        (TypeInfo::Enum(info), Value::String(name)) => {
            if info.variant(name).is_some() {
                Ok(())
            } else {
                Err(located(
                    pointer,
                    ReflectProblem::UnknownVariant(
                        name.clone(),
                        info.variants.iter().map(|v| v.name).collect(),
                    ),
                ))
            }
        }
        (TypeInfo::Enum(info), Value::Object(map)) if map.len() == 1 => {
            let (name, payload) = map.iter_mut().next().expect("one entry");
            let Some(variant) = info.variant(name) else {
                return Err(located(
                    pointer,
                    ReflectProblem::UnknownVariant(
                        name.clone(),
                        info.variants.iter().map(|v| v.name).collect(),
                    ),
                ));
            };
            let length = pointer.len();
            pointer.push('/');
            pointer.push_str(&escape(name));
            match (&variant.fields, payload) {
                (VariantFields::Struct(fields), Value::Object(map)) => {
                    walk_fields(fields, map, pointer, visit)?;
                }
                (VariantFields::Tuple(items), payload) if items.len() == 1 => {
                    walk_at(&items[0], payload, pointer, visit)?;
                }
                (VariantFields::Tuple(items), Value::Array(values)) => {
                    for (index, (item, value)) in
                        items.iter().zip(values.iter_mut()).enumerate()
                    {
                        let length = pointer.len();
                        pointer.push_str(&format!("/{index}"));
                        walk_at(item, value, pointer, visit)?;
                        pointer.truncate(length);
                    }
                }
                _ => {}
            }
            pointer.truncate(length);
            Ok(())
        }
        // Serde reports values of the wrong kind with its own message.
        _ => Ok(()),
    }
}

fn walk_fields(
    fields: &[FieldInfo],
    map: &mut serde_json::Map<String, Value>,
    pointer: &mut String,
    visit: &mut dyn FnMut(&TypeInfo, &mut Value) -> Result<(), ReflectProblem>,
) -> Result<(), Located> {
    for (key, value) in map.iter_mut() {
        let length = pointer.len();
        pointer.push('/');
        pointer.push_str(&escape(key));
        let Some(field) = fields.iter().find(|field| field.name == key) else {
            return Err((pointer.clone(), ReflectProblem::UnknownField));
        };
        walk_at(&field.ty, value, pointer, visit)?;
        pointer.truncate(length);
    }
    Ok(())
}

/// Removes the saved version from `value`: 0 when it has none.
pub fn take_version(value: &mut Value) -> Result<u32, Located> {
    match value
        .as_object_mut()
        .and_then(|map| map.remove(VERSION_KEY))
    {
        Some(version) => version
            .as_u64()
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| {
                (
                    format!("/{}", escape(VERSION_KEY)),
                    ReflectProblem::WrongKind("a version number"),
                )
            }),
        None => Ok(0),
    }
}

/// Runs the migrations after `saved` on a value saved at that version.
pub fn migrate(
    value: &mut Value,
    saved: u32,
    migrations: &[FieldMigration],
) -> Result<(), Located> {
    let pending = migrations
        .get(saved as usize..)
        .ok_or((String::new(), ReflectProblem::NewerVersion))?;
    if let Value::Object(map) = value {
        for migration in pending {
            match migration {
                FieldMigration::Rename { from, to } => {
                    if let Some(moved) = map.remove(from) {
                        map.insert(to.clone(), moved);
                    }
                }
                FieldMigration::Remove(field) => {
                    map.remove(field);
                }
            }
        }
    }
    Ok(())
}

fn placeholder() -> Value {
    Entity::PLACEHOLDER.to_bits().into()
}

/// Turns entities and handles in serde's output into their scene form. A
/// reference to an entity that is gone or has no `SceneId` saves as null.
pub fn to_scene_form(
    info: &TypeInfo,
    value: &mut Value,
    world: &World,
) -> Result<(), Located> {
    walk(info, value, &mut |info, value| match info {
        TypeInfo::Entity => {
            *value = value
                .as_u64()
                .and_then(Entity::try_from_bits)
                .and_then(|entity| world.get::<SceneId>(entity))
                .map_or(Value::Null, |id| Value::String(id.0.to_string()));
            Ok(())
        }
        TypeInfo::Handle(kind) => {
            let assets = world
                .get_resource::<AssetServer>()
                .ok_or(ReflectProblem::UnsavedAsset)?;
            handle_to_scene(kind, assets, value)
        }
        _ => Ok(()),
    })
}

/// Turns scene-form entities and handles back into what serde reads,
/// loading referenced assets. `ids` maps scene IDs to entities; without it,
/// the world's `SceneId`s are searched. Null and IDs no entity has load as
/// `Entity::PLACEHOLDER`, or `None` inside an `Option`, so deleting an
/// object leaves references to it dangling instead of failing the scene.
pub fn from_scene_form(
    info: &TypeInfo,
    value: &mut Value,
    world: &mut World,
    ids: Option<&HashMap<Uuid, Entity>>,
) -> Result<(), Located> {
    // ponytail: builds the whole ID map for one edit; keep a map resource
    // if entity references in large scenes are edited often.
    let mut searched: Option<HashMap<Uuid, Entity>> = None;
    walk(info, value, &mut |info, value| match info {
        TypeInfo::Entity => {
            if value.is_null() {
                *value = placeholder();
                return Ok(());
            }
            let id = value
                .as_str()
                .and_then(|text| text.parse::<Uuid>().ok())
                .ok_or(ReflectProblem::WrongKind("an object ID"))?;
            let ids = match ids {
                Some(ids) => ids,
                None => searched.get_or_insert_with(|| {
                    let mut query = world.query::<(Entity, &SceneId)>();
                    query
                        .iter(world)
                        .map(|(entity, id)| (id.0, entity))
                        .collect()
                }),
            };
            let entity = ids.get(&id).copied().unwrap_or(Entity::PLACEHOLDER);
            *value = entity.to_bits().into();
            Ok(())
        }
        TypeInfo::Handle(kind) => {
            let mut assets = world
                .get_resource_mut::<AssetServer>()
                .ok_or(ReflectProblem::Asset("no AssetServer".into()))?;
            handle_from_scene(kind, &mut assets, value)
        }
        _ => Ok(()),
    })
}

/// Applies `convert` to every `{"$asset": path}` in a saved component.
pub fn map_asset_paths<E>(
    value: &mut Value,
    convert: &mut impl FnMut(&Path) -> Result<PathBuf, E>,
) -> Result<(), E> {
    match value {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(Value::String(path)) = map.get_mut(ASSET_KEY) {
                    let converted = convert(Path::new(path.as_str()))?;
                    *path = crate::runtime::portable_path(&converted);
                    return Ok(());
                }
            }
            map.values_mut()
                .try_for_each(|value| map_asset_paths(value, convert))
        }
        Value::Array(items) => items
            .iter_mut()
            .try_for_each(|value| map_asset_paths(value, convert)),
        _ => Ok(()),
    }
}

/// The engine's reflected resource and asset types, listed by
/// `rusting schema`. Components are in `SceneComponentRegistry`, which also
/// stores them.
pub struct TypeRegistry {
    resources: BTreeMap<String, TypeInfo>,
    assets: BTreeMap<String, TypeInfo>,
}

impl Default for TypeRegistry {
    fn default() -> Self {
        let mut registry = Self {
            resources: BTreeMap::new(),
            assets: BTreeMap::new(),
        };
        builtin::register(&mut registry);
        registry
    }
}

impl TypeRegistry {
    pub fn register_resource<T: Resource + Reflect>(
        &mut self,
        name: impl Into<String>,
    ) {
        self.resources.insert(name.into(), T::type_info());
    }

    pub fn register_asset<T: Reflect>(&mut self, name: impl Into<String>) {
        self.assets.insert(name.into(), T::type_info());
    }

    pub fn resources(&self) -> impl Iterator<Item = (&str, &TypeInfo)> {
        self.resources
            .iter()
            .map(|(name, info)| (name.as_str(), info))
    }

    pub fn assets(&self) -> impl Iterator<Item = (&str, &TypeInfo)> {
        self.assets.iter().map(|(name, info)| (name.as_str(), info))
    }
}

#[cfg(test)]
mod tests;
