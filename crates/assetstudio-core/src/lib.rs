//! Native Rust implementation of `AssetStudio`'s format parsing core.
//!
//! The rewrite is intentionally being built from the bottom up. This crate does
//! not call the existing .NET `NativeAOT` adapter; every exported module here is a
//! native Rust implementation.

pub mod acl;
pub mod animation_clip;
pub mod animation_component;
pub mod animation_graph;
pub mod animator_controller;
pub mod audio;
pub mod avatar;
pub mod bundle;
pub mod compression;
pub mod cubism_moc;
pub mod endian;
pub mod error;
pub mod export;
pub mod extraction;
pub mod fbx_ascii;
pub mod fbx_binary;
pub mod fbx_binary_scene;
mod fbx_scene_ascii;
pub mod file_type;
mod fsb_vorbis;
pub mod image_export;
pub mod json;
pub mod legacy_bundle;
pub mod live2d;
pub mod live2d_clip_motion;
pub mod live2d_motion;
mod live2d_number;
pub mod live2d_package;
pub mod live2d_physics;
pub mod live2d_schema;
pub mod loader;
mod managed_number;
pub mod material;
pub mod mesh;
pub mod model_animation;
pub mod model_export;
pub mod model_ir;
pub mod mono_schema;
pub mod monobehaviour;
pub mod obj_scene;
pub mod object_name;
pub mod packed_bits;
pub mod project_settings;
pub mod renderer;
pub mod scene;
pub mod scene_hierarchy;
pub mod scene_textures;
pub mod serialized;
pub mod serialized_file;
pub mod shader;
pub mod simple_assets;
pub mod source;
pub mod sprite;
pub mod sprite_atlas;
pub mod studio;
pub mod texture;
pub mod texture_array;
pub mod type_tree;
pub mod type_tree_dump;
pub mod unity_cn;
pub mod unity_version;
/// Vendored third-party decoders; see the module documentation for why.
mod vendor;
pub mod web_file;

pub use error::{Error, Result};
