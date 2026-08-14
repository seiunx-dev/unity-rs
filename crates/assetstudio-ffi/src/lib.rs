//! C ABI compatibility layer for `haruki_assetstudio_native.h`.
//!
//! Unsafe operations are confined to this crate. Every exported entry point
//! initializes its response first and catches panics before they can cross the
//! ABI boundary.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::{self, Write};
use std::mem::{align_of, size_of};
use std::path::Path;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use assetstudio_core::loader::AssetCollection;
use assetstudio_core::mesh::{MESH_CLASS_ID, MeshReadLimits, read_mesh, write_mesh_obj};
use assetstudio_core::monobehaviour::{
    MONO_BEHAVIOUR_CLASS_ID, MonoBehaviourReadLimits, read_mono_behaviour_json,
};
use assetstudio_core::object_name::{ObjectNameReadLimits, read_object_name_metadata};
use assetstudio_core::shader::{SHADER_CLASS_ID, ShaderReadLimits, read_shader};
use assetstudio_core::simple_assets::{
    AUDIO_CLIP_CLASS_ID, FONT_CLASS_ID, MOVIE_TEXTURE_CLASS_ID, SimpleAssetReadLimits,
    VIDEO_CLIP_CLASS_ID, read_audio_clip, read_font, read_movie_texture, read_video_clip,
};
use assetstudio_core::sprite::{
    SPRITE_CLASS_ID, SpriteReadLimits, decode_sprite_rgba8, read_sprite,
};
use assetstudio_core::texture::{
    TEXTURE_2D_CLASS_ID, TextureReadLimits, read_texture2d, write_rgba_ir,
    write_rgba_ir_display_order,
};
use assetstudio_core::texture_array::{
    TEXTURE_2D_ARRAY_CLASS_ID, TextureArrayReadLimits, read_texture2d_array,
    write_texture2d_array_rgba_bundle,
};
use libc::{free, malloc};

const ABI_VERSION: i32 = 1;
const SCHEMA_VERSION: i32 = 1;
const LAYOUT_VERSION: i32 = 1;
const CONTEXT_ABI_VERSION: i32 = 1;
const LIMITS_ABI_VERSION: i32 = 1;
const OBJECT_TABLE_ABI_VERSION: i32 = 1;
const OBJECT_TABLE_INTO_ABI_VERSION: i32 = 1;
const OBJECT_READ_ABI_VERSION: i32 = 1;
const OBJECT_READ_BATCH_ABI_VERSION: i32 = 1;
const OBJECT_READ_BATCH_HANDLE_ABI_VERSION: i32 = 1;
const OBJECT_READ_BATCH_INTO_ABI_VERSION: i32 = 1;
const DIRECT_RETRY_ABI_VERSION: i32 = 1;
const TEXTURE_2D_ARRAY_IMAGE_CLASS_ID: i32 = -187;

const OK: i32 = 0;
const NULL_POINTER: i32 = 1;
const INVALID_REQUEST: i32 = 2;
const CONTEXT_NOT_FOUND: i32 = 4;
const CONTEXT_LIMIT: i32 = 5;
const OPERATION_BUSY: i32 = 5;
const ASSET_NOT_FOUND: i32 = 6;
const UNSUPPORTED_KIND: i32 = 7;
const PARTIAL_FAILURE: i32 = 9;
const CONTEXT_BUSY: i32 = 10;
const INTERNAL_ERROR: i32 = 100;

const MAX_UTF8_BYTES: usize = 1024 * 1024;
const MAX_BATCH_COUNT: usize = 4_096;
const MAX_PAGE_LIMIT: usize = 65_536;
const MAX_BATCH_PAYLOAD_BYTES: usize = 512 * 1024 * 1024;
const MAX_OBJECT_TABLE_BUFFER_BYTES: usize = 512 * 1024 * 1024;
const MAX_ACTIVE_CONTEXTS: usize = 4;

// Keep both lookup capability claims disabled until the production index is equivalent for every
// context mode, not merely for the classes with authoritative readers. Remaining blockers are
// documented beside `object_name`: managed load-all skips objects whose class-specific constructor
// fails, Shader 5.5+ uses SerializedShader.m_Name rather than the NamedObject prefix, and .NET's
// full Unicode OrdinalIgnoreCase matching is not identical to Rust's Unicode lowercase mapping.
const SUPPORTS_AUTHORITATIVE_OBJECT_LOOKUP: i32 = 0;

#[repr(C)]
pub struct ContextOpenRequest {
    pub struct_size: i32,
    pub input_path_utf8: *const u8,
    pub input_path_utf8_len: i32,
    pub unity_version_utf8: *const u8,
    pub unity_version_utf8_len: i32,
    pub asset_types_csv_utf8: *const u8,
    pub asset_types_csv_utf8_len: i32,
    pub output_dir_utf8: *const u8,
    pub output_dir_utf8_len: i32,
    pub load_all_assets: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ContextOpenResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub context_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub assets_file_count: i32,
    pub exportable_asset_count: i32,
    pub object_index_count: i32,
    pub has_more_assets: i32,
    pub unity_version_utf8: *mut u8,
    pub unity_version_utf8_len: i32,
    pub buffer: *mut u8,
    pub buffer_len: i64,
    pub duration_ms: i64,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
pub struct ContextCloseRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ContextCloseResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub context_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub duration_ms: i64,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct LimitsResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub limits_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub max_native_utf8_bytes: i32,
    pub max_object_read_batch_count: i32,
    pub max_object_table_page_limit: i32,
    pub max_object_read_batch_payload_bytes: i64,
    pub max_cached_object_read_batch_payload_bytes: i64,
    pub max_active_contexts: i32,
    pub max_concurrent_operations: i32,
    pub supports_multiple_contexts: i32,
    pub supports_concurrent_operations: i32,
    pub legacy_static_engine: i32,
    pub native_console_capture: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct CapabilitiesResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub core_api_version_major: i32,
    pub core_api_version_minor: i32,
    pub context_abi_version: i32,
    pub object_table_abi_version: i32,
    pub object_table_into_abi_version: i32,
    pub object_lookup_abi_version: i32,
    pub object_lookup_into_abi_version: i32,
    pub object_read_abi_version: i32,
    pub object_read_batch_abi_version: i32,
    pub object_read_batch_handle_abi_version: i32,
    pub object_read_batch_into_abi_version: i32,
    pub object_read_batch_by_index_abi_version: i32,
    pub object_read_batch_direct_into_abi_version: i32,
    pub object_read_batch_direct_retry_abi_version: i32,
    pub supports_typed_object_table: i32,
    pub supports_caller_provided_object_table_buffers: i32,
    pub supports_typed_object_lookup: i32,
    pub supports_caller_provided_object_lookup_buffers: i32,
    pub supports_typed_object_read: i32,
    pub supports_typed_object_read_batch: i32,
    pub supports_result_handle: i32,
    pub supports_direct_object_read_retry: i32,
    pub supports_typed_context: i32,
    pub supports_native_dependency_resolver: i32,
    pub supports_abi_layout: i32,
    pub supports_multiple_contexts: i32,
    pub supports_concurrent_operations: i32,
    pub supports_context_lifetime_guards: i32,
    pub native_console_capture: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct AbiLayoutResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub layout_version: i32,
    pub context_open_request: i32,
    pub context_open_response: i32,
    pub context_close_request: i32,
    pub context_close_response: i32,
    pub limits_response: i32,
    pub capabilities_response: i32,
    pub object_list_request: i32,
    pub object_list_into_request_v1: i32,
    pub object_table: i32,
    pub asset_object: i32,
    pub object_read_item_request: i32,
    pub object_read_batch_into_request_v1: i32,
    pub object_read_item_response_v1: i32,
    pub object_read_batch_retry_response_v1: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
pub struct ObjectListRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub offset: i32,
    pub limit: i32,
    pub asset_types_csv_utf8: *const u8,
    pub asset_types_csv_utf8_len: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
pub struct ObjectListIntoRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub offset: i32,
    pub limit: i32,
    pub asset_types_csv_utf8: *const u8,
    pub asset_types_csv_utf8_len: i32,
    pub flags: i32,
    pub reserved: i32,
    pub buffer: *mut u8,
    pub buffer_len: i64,
}

#[repr(C)]
pub struct ObjectLookupRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub lookup_kind: i32,
    pub path_id: i64,
    pub query_utf8: *const u8,
    pub query_utf8_len: i32,
    pub asset_types_csv_utf8: *const u8,
    pub asset_types_csv_utf8_len: i32,
    pub offset: i32,
    pub limit: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
pub struct ObjectLookupIntoRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub lookup_kind: i32,
    pub path_id: i64,
    pub query_utf8: *const u8,
    pub query_utf8_len: i32,
    pub asset_types_csv_utf8: *const u8,
    pub asset_types_csv_utf8_len: i32,
    pub offset: i32,
    pub limit: i32,
    pub flags: i32,
    pub reserved: i32,
    pub buffer: *mut u8,
    pub buffer_len: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AssetObject {
    pub index: i32,
    pub type_id: i32,
    pub path_id: i64,
    pub size: i64,
    pub estimated_payload_capacity: i64,
    pub raw_payload_capacity: i64,
    pub image_payload_capacity: i64,
    pub text_payload_capacity: i64,
    pub payload_capacity_flags: i32,
    pub reserved: i32,
    pub name_offset: i32,
    pub name_len: i32,
    pub container_offset: i32,
    pub container_len: i32,
    pub type_offset: i32,
    pub type_len: i32,
    pub unique_id_offset: i32,
    pub unique_id_len: i32,
    pub source_file_offset: i32,
    pub source_file_len: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ObjectTable {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_table_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub offset: i32,
    pub limit: i32,
    pub next_offset: i32,
    pub has_more: i32,
    pub total_count: i32,
    pub returned_count: i32,
    pub objects: *mut AssetObject,
    pub string_data: *mut u8,
    pub string_data_len: i32,
    pub buffer: *mut u8,
    pub buffer_len: i64,
    pub duration_ms: i64,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
pub struct ObjectReadItemRequest {
    pub path_id: i64,
    pub kind_utf8: *const u8,
    pub kind_utf8_len: i32,
    pub image_format_utf8: *const u8,
    pub image_format_utf8_len: i32,
}

#[repr(C)]
pub struct ObjectReadRequest {
    pub context_id: i64,
    pub path_id: i64,
    pub kind_utf8: *const u8,
    pub kind_utf8_len: i32,
    pub image_format_utf8: *const u8,
    pub image_format_utf8_len: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ObjectReadResponse {
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_read_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub path_id: i64,
    pub type_id: i32,
    pub size: i64,
    pub payload_kind: *mut u8,
    pub payload_kind_len: i32,
    pub suggested_extension: *mut u8,
    pub suggested_extension_len: i32,
    pub payload: *mut u8,
    pub payload_len: i64,
    pub buffer: *mut u8,
    pub buffer_len: i64,
    pub duration_ms: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LegacyObjectReadItemResponse {
    pub index: i32,
    pub status: i32,
    pub error_code: i32,
    pub path_id: i64,
    pub type_id: i32,
    pub size: i64,
    pub payload_offset: i64,
    pub payload_len: i64,
    pub payload_kind_offset: i32,
    pub payload_kind_len: i32,
    pub suggested_extension_offset: i32,
    pub suggested_extension_len: i32,
}

#[repr(C)]
pub struct LegacyObjectReadBatchRequest {
    pub context_id: i64,
    pub items: *const ObjectReadItemRequest,
    pub count: i32,
    pub flags: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct LegacyObjectReadBatchResponse {
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_read_batch_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub requested_count: i32,
    pub returned_count: i32,
    pub failed_count: i32,
    pub items: *mut LegacyObjectReadItemResponse,
    pub string_data: *mut u8,
    pub string_data_len: i32,
    pub items_buffer: *mut u8,
    pub items_buffer_len: i64,
    pub payload: *mut u8,
    pub payload_len: i64,
    pub duration_ms: i64,
}

#[repr(C)]
#[derive(Default)]
pub struct LegacyObjectReadBatchHandleResponse {
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_read_batch_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub requested_count: i32,
    pub returned_count: i32,
    pub failed_count: i32,
    pub items: *mut LegacyObjectReadItemResponse,
    pub string_data: *mut u8,
    pub string_data_len: i32,
    pub items_buffer: *mut u8,
    pub items_buffer_len: i64,
    pub payload: *mut u8,
    pub payload_len: i64,
    pub duration_ms: i64,
    pub object_read_batch_handle_abi_version: i32,
    pub result_handle: i64,
}

#[repr(C)]
pub struct ObjectReadItemByIndexRequest {
    pub object_index: i32,
    pub kind_utf8: *const u8,
    pub kind_utf8_len: i32,
    pub image_format_utf8: *const u8,
    pub image_format_utf8_len: i32,
}

#[repr(C)]
pub struct ObjectReadBatchIntoRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub items: *const ObjectReadItemRequest,
    pub count: i32,
    pub flags: i32,
    pub items_buffer: *mut u8,
    pub items_buffer_len: i64,
    pub payload: *mut u8,
    pub payload_len: i64,
    pub reserved: i32,
}

#[repr(C)]
pub struct ObjectReadBatchRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub items: *const ObjectReadItemRequest,
    pub count: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
pub struct ObjectReadBatchByIndexIntoRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub items: *const ObjectReadItemByIndexRequest,
    pub count: i32,
    pub flags: i32,
    pub reserved: i32,
    pub items_buffer: *mut u8,
    pub items_buffer_len: i64,
    pub payload: *mut u8,
    pub payload_len: i64,
}

#[repr(C)]
pub struct ObjectReadBatchByIndexRequest {
    pub struct_size: i32,
    pub context_id: i64,
    pub items: *const ObjectReadItemByIndexRequest,
    pub count: i32,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ObjectReadItemResponse {
    pub index: i32,
    pub status: i32,
    pub error_code: i32,
    pub path_id: i64,
    pub type_id: i32,
    pub size: i64,
    pub payload_offset: i64,
    pub payload_len: i64,
    pub payload_kind_offset: i32,
    pub payload_kind_len: i32,
    pub suggested_extension_offset: i32,
    pub suggested_extension_len: i32,
    pub error_message_offset: i32,
    pub error_message_len: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ObjectReadBatchSizeResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_read_batch_abi_version: i32,
    pub object_read_batch_into_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub requested_count: i32,
    pub returned_count: i32,
    pub failed_count: i32,
    pub required_items_buffer_len: i64,
    pub required_string_data_len: i32,
    pub required_payload_len: i64,
    pub items_buffer_len: i64,
    pub string_data_len: i32,
    pub payload_len: i64,
    pub duration_ms: i64,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ObjectReadBatchIntoResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_read_batch_abi_version: i32,
    pub object_read_batch_into_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub requested_count: i32,
    pub returned_count: i32,
    pub failed_count: i32,
    pub items: *mut ObjectReadItemResponse,
    pub string_data: *mut u8,
    pub string_data_len: i32,
    pub items_buffer: *mut u8,
    pub items_buffer_len: i64,
    pub payload: *mut u8,
    pub payload_len: i64,
    pub required_items_buffer_len: i64,
    pub required_string_data_len: i32,
    pub required_payload_len: i64,
    pub duration_ms: i64,
    pub flags: i32,
    pub reserved: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct ObjectReadBatchRetryResponse {
    pub struct_size: i32,
    pub abi_version: i32,
    pub schema_version: i32,
    pub object_read_batch_abi_version: i32,
    pub object_read_batch_into_abi_version: i32,
    pub object_read_batch_direct_retry_abi_version: i32,
    pub status: i32,
    pub error_code: i32,
    pub context_id: i64,
    pub requested_count: i32,
    pub returned_count: i32,
    pub failed_count: i32,
    pub items: *mut ObjectReadItemResponse,
    pub string_data: *mut u8,
    pub string_data_len: i32,
    pub items_buffer: *mut u8,
    pub items_buffer_len: i64,
    pub payload: *mut u8,
    pub payload_len: i64,
    pub required_items_buffer_len: i64,
    pub required_string_data_len: i32,
    pub required_payload_len: i64,
    pub duration_ms: i64,
    pub result_handle: i64,
    pub ownership_flags: i32,
    pub flags: i32,
    pub reserved: i32,
}

struct ContextState {
    collection: AssetCollection,
    objects: Vec<ObjectRef>,
    path_id_index: HashMap<i64, usize>,
    requested_asset_types: Vec<String>,
    lifetime: AtomicUsize,
}

const CONTEXT_CLOSING_BIT: usize = 1 << (usize::BITS - 1);

struct ContextOperation<'a> {
    context: &'a ContextState,
}

impl ContextState {
    fn try_acquire(&self) -> Result<ContextOperation<'_>, i32> {
        let mut state = self.lifetime.load(Ordering::Acquire);
        loop {
            if state & CONTEXT_CLOSING_BIT != 0 {
                return Err(OPERATION_BUSY);
            }
            if state == CONTEXT_CLOSING_BIT - 1 {
                return Err(INTERNAL_ERROR);
            }
            match self.lifetime.compare_exchange_weak(
                state,
                state + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(ContextOperation { context: self }),
                Err(current) => state = current,
            }
        }
    }

    fn try_begin_close(&self) -> bool {
        self.lifetime
            .compare_exchange(0, CONTEXT_CLOSING_BIT, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl Drop for ContextOperation<'_> {
    fn drop(&mut self) {
        let previous = self.context.lifetime.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0 && previous & CONTEXT_CLOSING_BIT == 0);
    }
}

#[derive(Clone)]
struct ObjectRef {
    file_index: usize,
    object_index: usize,
    texture_array_layer: Option<u32>,
    path_id: i64,
    class_id: i32,
    size: u64,
    name: String,
    container: String,
    type_name: String,
    unique_id: String,
    source_file: String,
}

struct ResultArena {
    context_id: i64,
    items_buffer: usize,
    payload: usize,
}

static CONTEXTS: OnceLock<RwLock<HashMap<i64, Arc<ContextState>>>> = OnceLock::new();
static RESULTS: OnceLock<Mutex<HashMap<i64, ResultArena>>> = OnceLock::new();
static LEGACY_BUFFERS: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
static NEXT_CONTEXT_ID: AtomicI64 = AtomicI64::new(1);
static NEXT_RESULT_ID: AtomicI64 = AtomicI64::new(1);

fn contexts() -> &'static RwLock<HashMap<i64, Arc<ContextState>>> {
    CONTEXTS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn results() -> &'static Mutex<HashMap<i64, ResultArena>> {
    RESULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn legacy_buffers() -> &'static Mutex<HashSet<usize>> {
    LEGACY_BUFFERS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[unsafe(no_mangle)]
/// # Safety
/// `response` must be null or point to writable storage for the complete ABI response.
pub unsafe extern "C" fn haruki_assetstudio_capabilities_v1(
    response: *mut CapabilitiesResponse,
) -> i32 {
    ffi_boundary(|| unsafe { capabilities(response) })
}

unsafe fn capabilities(response: *mut CapabilitiesResponse) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    *response = CapabilitiesResponse {
        struct_size: size_i32::<CapabilitiesResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        core_api_version_major: 1,
        core_api_version_minor: 0,
        context_abi_version: CONTEXT_ABI_VERSION,
        object_table_abi_version: OBJECT_TABLE_ABI_VERSION,
        object_table_into_abi_version: OBJECT_TABLE_INTO_ABI_VERSION,
        object_lookup_abi_version: 1,
        object_lookup_into_abi_version: 1,
        object_read_abi_version: OBJECT_READ_ABI_VERSION,
        object_read_batch_abi_version: OBJECT_READ_BATCH_ABI_VERSION,
        object_read_batch_handle_abi_version: OBJECT_READ_BATCH_HANDLE_ABI_VERSION,
        object_read_batch_into_abi_version: OBJECT_READ_BATCH_INTO_ABI_VERSION,
        object_read_batch_by_index_abi_version: 1,
        object_read_batch_direct_into_abi_version: 1,
        object_read_batch_direct_retry_abi_version: DIRECT_RETRY_ABI_VERSION,
        supports_typed_object_table: 1,
        supports_caller_provided_object_table_buffers: 1,
        supports_typed_object_lookup: SUPPORTS_AUTHORITATIVE_OBJECT_LOOKUP,
        supports_caller_provided_object_lookup_buffers: SUPPORTS_AUTHORITATIVE_OBJECT_LOOKUP,
        supports_typed_object_read: 1,
        supports_typed_object_read_batch: 1,
        supports_result_handle: 1,
        supports_direct_object_read_retry: 1,
        supports_typed_context: 1,
        supports_abi_layout: 1,
        supports_multiple_contexts: 1,
        supports_concurrent_operations: 1,
        supports_context_lifetime_guards: 1,
        ..CapabilitiesResponse::default()
    };
    OK
}

#[unsafe(no_mangle)]
/// # Safety
/// `response` must be null or point to writable storage for the complete ABI response.
pub unsafe extern "C" fn haruki_assetstudio_abi_layout_v1(response: *mut AbiLayoutResponse) -> i32 {
    ffi_boundary(|| unsafe { abi_layout(response) })
}

unsafe fn abi_layout(response: *mut AbiLayoutResponse) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    *response = AbiLayoutResponse {
        struct_size: size_i32::<AbiLayoutResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        layout_version: LAYOUT_VERSION,
        context_open_request: size_i32::<ContextOpenRequest>(),
        context_open_response: size_i32::<ContextOpenResponse>(),
        context_close_request: size_i32::<ContextCloseRequest>(),
        context_close_response: size_i32::<ContextCloseResponse>(),
        limits_response: size_i32::<LimitsResponse>(),
        capabilities_response: size_i32::<CapabilitiesResponse>(),
        object_list_request: size_i32::<ObjectListRequest>(),
        object_list_into_request_v1: size_i32::<ObjectListIntoRequest>(),
        object_table: size_i32::<ObjectTable>(),
        asset_object: size_i32::<AssetObject>(),
        object_read_item_request: size_i32::<ObjectReadItemRequest>(),
        object_read_batch_into_request_v1: size_i32::<ObjectReadBatchIntoRequest>(),
        object_read_item_response_v1: size_i32::<ObjectReadItemResponse>(),
        object_read_batch_retry_response_v1: size_i32::<ObjectReadBatchRetryResponse>(),
        ..AbiLayoutResponse::default()
    };
    OK
}

#[unsafe(no_mangle)]
/// # Safety
/// `response` must be null or point to writable storage for the complete ABI response.
pub unsafe extern "C" fn haruki_assetstudio_limits_v1(response: *mut LimitsResponse) -> i32 {
    ffi_boundary(|| unsafe {
        let Some(response) = response.as_mut() else {
            return NULL_POINTER;
        };
        *response = LimitsResponse {
            struct_size: size_i32::<LimitsResponse>(),
            abi_version: ABI_VERSION,
            schema_version: SCHEMA_VERSION,
            limits_abi_version: LIMITS_ABI_VERSION,
            max_native_utf8_bytes: i32::try_from(MAX_UTF8_BYTES).unwrap_or(i32::MAX),
            max_object_read_batch_count: i32::try_from(MAX_BATCH_COUNT).unwrap_or(i32::MAX),
            max_object_table_page_limit: i32::try_from(MAX_PAGE_LIMIT).unwrap_or(i32::MAX),
            max_object_read_batch_payload_bytes: i64::try_from(MAX_BATCH_PAYLOAD_BYTES)
                .unwrap_or(i64::MAX),
            max_active_contexts: i32::try_from(MAX_ACTIVE_CONTEXTS).unwrap_or(i32::MAX),
            max_concurrent_operations: i32::try_from(MAX_ACTIVE_CONTEXTS).unwrap_or(i32::MAX),
            supports_multiple_contexts: 1,
            supports_concurrent_operations: 1,
            ..LimitsResponse::default()
        };
        OK
    })
}

#[unsafe(no_mangle)]
/// # Safety
/// Non-null request/response pointers and every pointer/length pair in the request must refer to
/// readable or writable storage for the duration of this call.
pub unsafe extern "C" fn haruki_assetstudio_context_open_v1(
    request: *const ContextOpenRequest,
    response: *mut ContextOpenResponse,
) -> i32 {
    ffi_boundary(|| unsafe { context_open(request, response) })
}

unsafe fn context_open(
    request: *const ContextOpenRequest,
    response: *mut ContextOpenResponse,
) -> i32 {
    let started = Instant::now();
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_open(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_open(response, NULL_POINTER, started);
    };
    if request.struct_size < size_i32::<ContextOpenRequest>()
        || request.flags != 0
        || request.reserved != 0
    {
        return fail_open(response, INVALID_REQUEST, started);
    }
    let input_path =
        match unsafe { read_utf8(request.input_path_utf8, request.input_path_utf8_len) } {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return fail_open(response, INVALID_REQUEST, started),
        };
    let Ok(unity_version) = (unsafe {
        validate_optional_utf8(request.unity_version_utf8, request.unity_version_utf8_len)
    }) else {
        return fail_open(response, INVALID_REQUEST, started);
    };
    let Ok(parsed_asset_types) = (unsafe {
        parse_filters(
            request.asset_types_csv_utf8,
            request.asset_types_csv_utf8_len,
        )
    }) else {
        return fail_open(response, INVALID_REQUEST, started);
    };
    if !parsed_asset_types
        .iter()
        .all(|filter| valid_open_filter(filter))
    {
        return fail_open(response, INVALID_REQUEST, started);
    }
    if unsafe { validate_optional_utf8(request.output_dir_utf8, request.output_dir_utf8_len) }
        .is_err()
        || !matches!(request.load_all_assets, 0 | 1)
    {
        return fail_open(response, INVALID_REQUEST, started);
    }
    if contexts()
        .read()
        .map_or(true, |guard| guard.len() >= MAX_ACTIVE_CONTEXTS)
    {
        return fail_open(response, CONTEXT_LIMIT, started);
    }
    let unity_version_override = if unity_version.trim().is_empty() {
        None
    } else {
        match unity_version.parse() {
            Ok(version) => Some(version),
            Err(_) => return fail_open(response, INVALID_REQUEST, started),
        }
    };
    let options = assetstudio_core::loader::AssetLoadOptions {
        unity_version_override,
        ..assetstudio_core::loader::AssetLoadOptions::default()
    };
    let Ok(collection) = AssetCollection::load_path_with_options(Path::new(&input_path), options)
    else {
        return fail_open(response, INTERNAL_ERROR, started);
    };
    let requested_asset_types = if parsed_asset_types.is_empty() && request.load_all_assets == 0 {
        default_asset_filters()
    } else {
        parsed_asset_types
    };
    let indexed_asset_types = if request.load_all_assets == 0 {
        requested_asset_types.clone()
    } else {
        Vec::new()
    };
    let state = Arc::new(build_context(
        collection,
        requested_asset_types,
        &indexed_asset_types,
    ));
    let context_id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
    let object_count = filtered_indexes(&state, &state.requested_asset_types).len();
    let reported_unity_version = state
        .collection
        .serialized_files
        .first()
        .map_or_else(String::new, |loaded| loaded.file.unity_version.to_string());
    let Ok(mut guard) = contexts().write() else {
        return fail_open(response, INTERNAL_ERROR, started);
    };
    if guard.len() >= MAX_ACTIVE_CONTEXTS {
        return fail_open(response, CONTEXT_LIMIT, started);
    }
    response.context_id = context_id;
    response.assets_file_count =
        i32::try_from(state.collection.serialized_files.len()).unwrap_or(i32::MAX);
    response.exportable_asset_count = i32::try_from(object_count).unwrap_or(i32::MAX);
    response.object_index_count = i32::try_from(state.path_id_index.len()).unwrap_or(i32::MAX);
    response.has_more_assets = i32::from(object_count != 0);
    response.duration_ms = elapsed_ms(started);
    guard.insert(context_id, state);
    drop(guard);
    if !reported_unity_version.is_empty() {
        let buffer = match unsafe { allocate_legacy_buffer(reported_unity_version.as_bytes()) } {
            Ok(buffer) => buffer,
            Err(status) => {
                if let Ok(mut guard) = contexts().write() {
                    guard.remove(&context_id);
                }
                return fail_open(response, status, started);
            }
        };
        response.unity_version_utf8 = buffer;
        response.unity_version_utf8_len =
            i32::try_from(reported_unity_version.len()).unwrap_or(i32::MAX);
        response.buffer = buffer;
        response.buffer_len = i64::try_from(reported_unity_version.len()).unwrap_or(i64::MAX);
    }
    OK
}

#[unsafe(no_mangle)]
/// # Safety
/// `request` and `response` must be null or valid for reads/writes of their complete ABI types.
pub unsafe extern "C" fn haruki_assetstudio_context_close_v1(
    request: *const ContextCloseRequest,
    response: *mut ContextCloseResponse,
) -> i32 {
    ffi_boundary(|| unsafe { context_close(request, response) })
}

unsafe fn context_close(
    request: *const ContextCloseRequest,
    response: *mut ContextCloseResponse,
) -> i32 {
    let started = Instant::now();
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    *response = ContextCloseResponse {
        struct_size: size_i32::<ContextCloseResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        context_abi_version: CONTEXT_ABI_VERSION,
        ..ContextCloseResponse::default()
    };
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_close(response, NULL_POINTER, started);
    };
    response.context_id = request.context_id;
    if request.struct_size < size_i32::<ContextCloseRequest>()
        || request.flags != 0
        || request.reserved != 0
    {
        return fail_close(response, INVALID_REQUEST, started);
    }
    let removed = {
        let Ok(mut guard) = contexts().write() else {
            return fail_close(response, INTERNAL_ERROR, started);
        };
        let Some(context) = guard.get(&request.context_id) else {
            return fail_close(response, CONTEXT_NOT_FOUND, started);
        };
        if !context.try_begin_close() {
            return fail_close(response, CONTEXT_BUSY, started);
        }
        guard.remove(&request.context_id)
    };
    debug_assert!(removed.is_some());
    release_context_results(request.context_id);
    response.duration_ms = elapsed_ms(started);
    OK
}

#[unsafe(no_mangle)]
/// # Safety
/// Non-null request/response pointers and request pointer/length pairs must remain valid for the
/// duration of this call.
pub unsafe extern "C" fn haruki_assetstudio_context_list_objects_size_v1(
    request: *const ObjectListRequest,
    response: *mut ObjectTable,
) -> i32 {
    ffi_boundary(|| unsafe { list_size(request, response) })
}

#[unsafe(no_mangle)]
/// # Safety
/// Non-null request/response pointers and request pointer/length pairs must remain valid for the
/// duration of this call. The returned `buffer` must be released with
/// [`haruki_assetstudio_free_buffer`].
pub unsafe extern "C" fn haruki_assetstudio_context_list_objects_v1(
    request: *const ObjectListRequest,
    response: *mut ObjectTable,
) -> i32 {
    ffi_boundary(|| unsafe { list_owned(request, response) })
}

unsafe fn list_owned(request: *const ObjectListRequest, response: *mut ObjectTable) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    let status = unsafe { list_size(request, response) };
    if status != OK {
        return status;
    }
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_table(response, NULL_POINTER);
    };
    let Ok(filters) = (unsafe {
        parse_filters(
            request.asset_types_csv_utf8,
            request.asset_types_csv_utf8_len,
        )
    }) else {
        return fail_table(response, INVALID_REQUEST);
    };
    let Some(context) = get_context(request.context_id) else {
        return fail_table(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_table(response, status),
    };
    let selected = filtered_indexes(&context, &filters);
    let Ok((_, _, page)) = paginate(&selected, request.offset, request.limit) else {
        return fail_table(response, INVALID_REQUEST);
    };
    let required = usize::try_from(response.buffer_len).unwrap_or(usize::MAX);
    let buffer = match unsafe { allocate_registered_buffer(required) } {
        Ok(buffer) => buffer,
        Err(status) => return fail_table(response, status),
    };
    let status = unsafe {
        write_object_table(
            response,
            &page,
            &context,
            buffer,
            i64::try_from(required).unwrap_or(i64::MAX),
        )
    };
    if status != OK {
        haruki_assetstudio_free_buffer(buffer);
    }
    status
}

unsafe fn list_size(request: *const ObjectListRequest, response: *mut ObjectTable) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_table(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_table(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectListRequest>()
        || request.flags != 0
        || request.reserved != 0
    {
        return fail_table(response, INVALID_REQUEST);
    }
    let Ok(filters) = (unsafe {
        parse_filters(
            request.asset_types_csv_utf8,
            request.asset_types_csv_utf8_len,
        )
    }) else {
        return fail_table(response, INVALID_REQUEST);
    };
    let Some(context) = get_context(request.context_id) else {
        return fail_table(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_table(response, status),
    };
    let selected = filtered_indexes(&context, &filters);
    let Ok((offset, limit, page)) = paginate(&selected, request.offset, request.limit) else {
        return fail_table(response, INVALID_REQUEST);
    };
    if let Err(status) = populate_table_metadata(
        response,
        request.context_id,
        offset,
        limit,
        selected.len(),
        &page,
        &context,
    ) {
        return fail_table(response, status);
    }
    OK
}

#[unsafe(no_mangle)]
/// # Safety
/// All request pointer/length pairs must be readable, `response` writable, and a non-null output
/// buffer must be valid for `buffer_len` bytes for the duration of this call.
pub unsafe extern "C" fn haruki_assetstudio_context_list_objects_into_v1(
    request: *const ObjectListIntoRequest,
    response: *mut ObjectTable,
) -> i32 {
    ffi_boundary(|| unsafe { list_into(request, response) })
}

unsafe fn list_into(request: *const ObjectListIntoRequest, response: *mut ObjectTable) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_table(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_table(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectListIntoRequest>()
        || request.flags != 0
        || request.reserved != 0
    {
        return fail_table(response, INVALID_REQUEST);
    }
    let Ok(filters) = (unsafe {
        parse_filters(
            request.asset_types_csv_utf8,
            request.asset_types_csv_utf8_len,
        )
    }) else {
        return fail_table(response, INVALID_REQUEST);
    };
    let Some(context) = get_context(request.context_id) else {
        return fail_table(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_table(response, status),
    };
    let selected = filtered_indexes(&context, &filters);
    let Ok((offset, limit, page)) = paginate(&selected, request.offset, request.limit) else {
        return fail_table(response, INVALID_REQUEST);
    };
    if let Err(status) = populate_table_metadata(
        response,
        request.context_id,
        offset,
        limit,
        selected.len(),
        &page,
        &context,
    ) {
        return fail_table(response, status);
    }
    unsafe {
        write_object_table(
            response,
            &page,
            &context,
            request.buffer,
            request.buffer_len,
        )
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// Non-null request/response pointers and request pointer/length pairs must remain valid for the
/// duration of this call.
pub unsafe extern "C" fn haruki_assetstudio_context_lookup_objects_size_v1(
    request: *const ObjectLookupRequest,
    response: *mut ObjectTable,
) -> i32 {
    ffi_boundary(|| unsafe { lookup_size(request, response) })
}

#[unsafe(no_mangle)]
/// # Safety
/// Non-null request/response pointers and request pointer/length pairs must remain valid for the
/// duration of this call. The returned `buffer` must be released with
/// [`haruki_assetstudio_free_buffer`].
pub unsafe extern "C" fn haruki_assetstudio_context_lookup_objects_v1(
    request: *const ObjectLookupRequest,
    response: *mut ObjectTable,
) -> i32 {
    ffi_boundary(|| unsafe { lookup_owned(request, response) })
}

unsafe fn lookup_owned(request: *const ObjectLookupRequest, response: *mut ObjectTable) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_table(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_table(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectLookupRequest>() {
        return fail_table(response, INVALID_REQUEST);
    }
    let Some(context) = get_context(request.context_id) else {
        return fail_table(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_table(response, status),
    };
    let (selected, offset, limit, page) = match unsafe { build_lookup(request, &context) } {
        Ok(result) => result,
        Err(status) => return fail_table(response, status),
    };
    if let Err(status) = populate_table_metadata(
        response,
        request.context_id,
        offset,
        limit,
        selected.len(),
        &page,
        &context,
    ) {
        return fail_table(response, status);
    }
    let required = usize::try_from(response.buffer_len).unwrap_or(usize::MAX);
    let buffer = match unsafe { allocate_registered_buffer(required) } {
        Ok(buffer) => buffer,
        Err(status) => return fail_table(response, status),
    };
    let status = unsafe {
        write_object_table(
            response,
            &page,
            &context,
            buffer,
            i64::try_from(required).unwrap_or(i64::MAX),
        )
    };
    if status != OK {
        haruki_assetstudio_free_buffer(buffer);
    }
    status
}

unsafe fn lookup_size(request: *const ObjectLookupRequest, response: *mut ObjectTable) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_table(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_table(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectLookupRequest>() {
        return fail_table(response, INVALID_REQUEST);
    }
    let Some(context) = get_context(request.context_id) else {
        return fail_table(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_table(response, status),
    };
    let result = unsafe { build_lookup(request, &context) };
    match result {
        Ok((selected, offset, limit, page)) => {
            if let Err(status) = populate_table_metadata(
                response,
                request.context_id,
                offset,
                limit,
                selected.len(),
                &page,
                &context,
            ) {
                return fail_table(response, status);
            }
            OK
        }
        Err(status) => fail_table(response, status),
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// All request pointer/length pairs must be readable, `response` writable, and a non-null output
/// buffer must be valid for `buffer_len` bytes for the duration of this call.
pub unsafe extern "C" fn haruki_assetstudio_context_lookup_objects_into_v1(
    request: *const ObjectLookupIntoRequest,
    response: *mut ObjectTable,
) -> i32 {
    ffi_boundary(|| unsafe { lookup_into(request, response) })
}

unsafe fn lookup_into(request: *const ObjectLookupIntoRequest, response: *mut ObjectTable) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_table(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_table(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectLookupIntoRequest>() {
        return fail_table(response, INVALID_REQUEST);
    }
    let base = ObjectLookupRequest {
        struct_size: size_i32::<ObjectLookupRequest>(),
        context_id: request.context_id,
        lookup_kind: request.lookup_kind,
        path_id: request.path_id,
        query_utf8: request.query_utf8,
        query_utf8_len: request.query_utf8_len,
        asset_types_csv_utf8: request.asset_types_csv_utf8,
        asset_types_csv_utf8_len: request.asset_types_csv_utf8_len,
        offset: request.offset,
        limit: request.limit,
        flags: request.flags,
        reserved: request.reserved,
    };
    let Some(context) = get_context(request.context_id) else {
        return fail_table(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_table(response, status),
    };
    match unsafe { build_lookup(&base, &context) } {
        Ok((selected, offset, limit, page)) => {
            if let Err(status) = populate_table_metadata(
                response,
                request.context_id,
                offset,
                limit,
                selected.len(),
                &page,
                &context,
            ) {
                return fail_table(response, status);
            }
            unsafe {
                write_object_table(
                    response,
                    &page,
                    &context,
                    request.buffer,
                    request.buffer_len,
                )
            }
        }
        Err(status) => fail_table(response, status),
    }
}

type LookupBuild = (Vec<usize>, usize, usize, Vec<usize>);

unsafe fn build_lookup(
    request: &ObjectLookupRequest,
    context: &ContextState,
) -> Result<LookupBuild, i32> {
    if request.struct_size < size_i32::<ObjectLookupRequest>() {
        return Err(INVALID_REQUEST);
    }
    let lookup_kind @ 1..=4 = request.lookup_kind else {
        return Err(INVALID_REQUEST);
    };
    // NativeExports uses Encoding.UTF8's replacement fallback for lookup strings. Keep lookup
    // parsing deliberately lossy even though read/export kind strings use strict UTF-8.
    let query = unsafe { parse_native_utf8_lossy(request.query_utf8, request.query_utf8_len) }
        .map_err(|()| INVALID_REQUEST)?;
    if lookup_kind != 1 && query.is_empty() {
        return Err(INVALID_REQUEST);
    }
    let filters = unsafe {
        parse_lookup_filters(
            request.asset_types_csv_utf8,
            request.asset_types_csv_utf8_len,
        )
    }
    .map_err(|()| INVALID_REQUEST)?;
    let limit = normalize_lookup_limit(request.limit).ok_or(INVALID_REQUEST)?;
    let filters = if filters.is_empty() {
        &context.requested_asset_types
    } else {
        &filters
    };
    let contains = request.flags & 1 != 0;
    let selected: Vec<usize> = if lookup_kind == 1 {
        context
            .path_id_index
            .get(&request.path_id)
            .copied()
            .filter(|index| matches_filters(&context.objects[*index], filters))
            .into_iter()
            .collect()
    } else {
        context
            .objects
            .iter()
            .enumerate()
            .filter_map(|(index, object)| {
                if !matches_filters(object, filters) {
                    return None;
                }
                let value = match lookup_kind {
                    2 => &object.name,
                    3 => &object.container,
                    4 => &object.type_name,
                    _ => unreachable!("lookup kind was validated"),
                };
                string_matches(value, &query, contains).then_some(index)
            })
            .collect()
    };
    let offset = usize::try_from(request.offset.max(0)).map_err(|_| INVALID_REQUEST)?;
    let start = offset.min(selected.len());
    let end = start.saturating_add(limit).min(selected.len());
    let page = selected[start..end].to_vec();
    Ok((selected, offset, limit, page))
}

#[unsafe(no_mangle)]
/// # Safety
/// Request string pointer/length pairs must remain readable and `response` must be writable for
/// its complete ABI type. Successful `payload` and `buffer` pointers are independent allocations
/// and must each be released once with [`haruki_assetstudio_free_buffer`].
pub unsafe extern "C" fn haruki_assetstudio_context_read_object_v1(
    request: *const ObjectReadRequest,
    response: *mut ObjectReadResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_legacy_object(request, response) })
}

unsafe fn read_legacy_object(
    request: *const ObjectReadRequest,
    response: *mut ObjectReadResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_legacy_read(response);
    let started = Instant::now();
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_legacy_read(response, NULL_POINTER, started);
    };
    response.context_id = request.context_id;
    response.path_id = request.path_id;
    let Some(context) = get_context(request.context_id) else {
        return fail_legacy_read(response, CONTEXT_NOT_FOUND, started);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_legacy_read(response, status, started),
    };
    let read = build_read(
        &context,
        0,
        context.path_id_index.get(&request.path_id).copied(),
        request.path_id,
        unsafe { read_kind(request.kind_utf8, request.kind_utf8_len) },
        unsafe { read_image_format(request.image_format_utf8, request.image_format_utf8_len) },
        MAX_BATCH_PAYLOAD_BYTES,
    );
    response.type_id = read.type_id;
    response.size = read.size;
    if read.status != OK {
        return fail_legacy_read(response, read.error_code, started);
    }

    let payload = match unsafe { allocate_registered_buffer(read.payload.len()) } {
        Ok(value) => value,
        Err(status) => return fail_legacy_read(response, status, started),
    };
    let Some(string_len) = read
        .payload_kind
        .len()
        .checked_add(read.suggested_extension.len())
    else {
        haruki_assetstudio_free_buffer(payload);
        return fail_legacy_read(response, INTERNAL_ERROR, started);
    };
    let strings = match unsafe { allocate_registered_buffer(string_len) } {
        Ok(value) => value,
        Err(status) => {
            haruki_assetstudio_free_buffer(payload);
            return fail_legacy_read(response, status, started);
        }
    };
    if !read.payload.is_empty() {
        unsafe { ptr::copy_nonoverlapping(read.payload.as_ptr(), payload, read.payload.len()) };
    }
    let mut cursor = 0_usize;
    if !read.payload_kind.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(
                read.payload_kind.as_ptr(),
                strings.add(cursor),
                read.payload_kind.len(),
            );
        };
        response.payload_kind = unsafe { strings.add(cursor) };
        response.payload_kind_len = i32::try_from(read.payload_kind.len()).unwrap_or(i32::MAX);
        cursor += read.payload_kind.len();
    }
    if !read.suggested_extension.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(
                read.suggested_extension.as_ptr(),
                strings.add(cursor),
                read.suggested_extension.len(),
            );
        };
        response.suggested_extension = unsafe { strings.add(cursor) };
        response.suggested_extension_len =
            i32::try_from(read.suggested_extension.len()).unwrap_or(i32::MAX);
    }
    response.status = OK;
    response.error_code = OK;
    response.payload = payload;
    response.payload_len = i64::try_from(read.payload.len()).unwrap_or(i64::MAX);
    response.buffer = strings;
    response.buffer_len = i64::try_from(string_len).unwrap_or(i64::MAX);
    response.duration_ms = elapsed_ms(started);
    OK
}

#[unsafe(no_mangle)]
/// # Safety
/// The request item array and its string pointer/length pairs must remain readable and `response`
/// must be writable. Successful `items_buffer` and `payload` pointers are independent allocations
/// and must each be released once with [`haruki_assetstudio_free_buffer`].
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_v1(
    request: *const LegacyObjectReadBatchRequest,
    response: *mut LegacyObjectReadBatchResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_legacy_batch(request, response) })
}

unsafe fn read_legacy_batch(
    request: *const LegacyObjectReadBatchRequest,
    response: *mut LegacyObjectReadBatchResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_legacy_batch(response);
    let started = Instant::now();
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_legacy_batch(response, NULL_POINTER, started);
    };
    response.context_id = request.context_id;
    response.requested_count = request.count.max(0);
    let packed = match unsafe { execute_legacy_batch(request, LegacyOwnership::Buffers) } {
        Ok(value) => value,
        Err(status) => return fail_legacy_batch(response, status, started),
    };
    populate_legacy_batch(response, &packed);
    response.duration_ms = elapsed_ms(started);
    packed.status
}

#[unsafe(no_mangle)]
/// # Safety
/// The request item array and its string pointer/length pairs must remain readable and `response`
/// must be writable. When non-zero, `result_handle` owns all returned buffers and must be released
/// once with [`haruki_assetstudio_result_free`].
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_handle_v1(
    request: *const LegacyObjectReadBatchRequest,
    response: *mut LegacyObjectReadBatchHandleResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_legacy_batch_handle(request, response) })
}

unsafe fn read_legacy_batch_handle(
    request: *const LegacyObjectReadBatchRequest,
    response: *mut LegacyObjectReadBatchHandleResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_legacy_batch_handle(response);
    let started = Instant::now();
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_legacy_batch_handle(response, NULL_POINTER, started);
    };
    response.context_id = request.context_id;
    response.requested_count = request.count.max(0);
    let packed = match unsafe { execute_legacy_batch(request, LegacyOwnership::Handle) } {
        Ok(value) => value,
        Err(status) => return fail_legacy_batch_handle(response, status, started),
    };
    populate_legacy_batch_handle(response, &packed);
    response.duration_ms = elapsed_ms(started);
    packed.status
}

#[unsafe(no_mangle)]
/// # Safety
/// All request arrays and pointer/length pairs must be readable, `response` writable, and caller
/// output buffers (when provided) valid for their declared lengths during this call.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_direct_retry_v1(
    request: *const ObjectReadBatchIntoRequest,
    response: *mut ObjectReadBatchRetryResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_by_path_retry(request, response) })
}

unsafe fn read_by_path_retry(
    request: *const ObjectReadBatchIntoRequest,
    response: *mut ObjectReadBatchRetryResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_retry(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_retry(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectReadBatchIntoRequest>()
        || request.flags != 0
        || request.reserved != 0
        || request.items_buffer_len < 0
        || request.payload_len < 0
    {
        return fail_retry(response, INVALID_REQUEST);
    }
    response.context_id = request.context_id;
    let Some(context) = get_context(request.context_id) else {
        return fail_retry(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_retry(response, status),
    };
    let items = match unsafe { request_slice(request.items, request.count, MAX_BATCH_COUNT) } {
        Ok(value) => value,
        Err(status) => return fail_retry(response, status),
    };
    let reads = unsafe { build_reads_by_path(&context, items) };
    unsafe {
        finish_retry(
            request.context_id,
            reads,
            request.items_buffer,
            request.items_buffer_len,
            request.payload,
            request.payload_len,
            response,
        )
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// All request arrays and pointer/length pairs must be readable, `response` writable, and caller
/// output buffers (when provided) valid for their declared lengths during this call.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_by_index_direct_retry_v1(
    request: *const ObjectReadBatchByIndexIntoRequest,
    response: *mut ObjectReadBatchRetryResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_by_index_retry(request, response) })
}

#[unsafe(no_mangle)]
/// # Safety
/// Request pointer/length pairs must be readable and `response` must be writable for its complete
/// ABI type.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_size_v1(
    request: *const ObjectReadBatchRequest,
    response: *mut ObjectReadBatchSizeResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_size_by_path(request, response) })
}

unsafe fn read_size_by_path(
    request: *const ObjectReadBatchRequest,
    response: *mut ObjectReadBatchSizeResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_size(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_size(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectReadBatchRequest>()
        || request.flags != 0
        || request.reserved != 0
    {
        return fail_size(response, INVALID_REQUEST);
    }
    let Some(context) = get_context(request.context_id) else {
        return fail_size(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_size(response, status),
    };
    let into = ObjectReadBatchIntoRequest {
        struct_size: size_i32::<ObjectReadBatchIntoRequest>(),
        context_id: request.context_id,
        items: request.items,
        count: request.count,
        flags: request.flags,
        items_buffer: ptr::null_mut(),
        items_buffer_len: 0,
        payload: ptr::null_mut(),
        payload_len: 0,
        reserved: request.reserved,
    };
    let mut retry = ObjectReadBatchRetryResponse::default();
    let status = unsafe { read_by_path_retry(&raw const into, &raw mut retry) };
    finish_size_from_retry(response, &retry);
    if !release_retry_handle(&retry) {
        return fail_size(response, INTERNAL_ERROR);
    }
    status
}

#[unsafe(no_mangle)]
/// # Safety
/// Request arrays and pointer/length pairs must be readable, `response` writable, and caller
/// buffers valid for their declared lengths.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_into_v1(
    request: *const ObjectReadBatchIntoRequest,
    response: *mut ObjectReadBatchIntoResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_into_by_path(request, response) })
}

#[unsafe(no_mangle)]
/// # Safety
/// The same requirements as [`haruki_assetstudio_context_read_objects_into_v1`] apply.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_direct_into_v1(
    request: *const ObjectReadBatchIntoRequest,
    response: *mut ObjectReadBatchIntoResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_into_by_path(request, response) })
}

unsafe fn read_into_by_path(
    request: *const ObjectReadBatchIntoRequest,
    response: *mut ObjectReadBatchIntoResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_into(response);
    let Some(request_ref) = (unsafe { request.as_ref() }) else {
        return fail_into(response, NULL_POINTER);
    };
    let Some(context) = get_context(request_ref.context_id) else {
        return fail_into(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_into(response, status),
    };
    let mut retry = ObjectReadBatchRetryResponse::default();
    let status = unsafe { read_by_path_retry(request, &raw mut retry) };
    finish_into_from_retry(
        response,
        &retry,
        request_ref.items_buffer_len,
        request_ref.payload_len,
        status,
    )
}

#[unsafe(no_mangle)]
/// # Safety
/// Request pointer/length pairs must be readable and `response` must be writable for its complete
/// ABI type.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_by_index_size_v1(
    request: *const ObjectReadBatchByIndexRequest,
    response: *mut ObjectReadBatchSizeResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_size_by_index(request, response) })
}

unsafe fn read_size_by_index(
    request: *const ObjectReadBatchByIndexRequest,
    response: *mut ObjectReadBatchSizeResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_size(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_size(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectReadBatchByIndexRequest>()
        || request.flags != 0
        || request.reserved != 0
    {
        return fail_size(response, INVALID_REQUEST);
    }
    let Some(context) = get_context(request.context_id) else {
        return fail_size(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_size(response, status),
    };
    let into = ObjectReadBatchByIndexIntoRequest {
        struct_size: size_i32::<ObjectReadBatchByIndexIntoRequest>(),
        context_id: request.context_id,
        items: request.items,
        count: request.count,
        flags: request.flags,
        reserved: request.reserved,
        items_buffer: ptr::null_mut(),
        items_buffer_len: 0,
        payload: ptr::null_mut(),
        payload_len: 0,
    };
    let mut retry = ObjectReadBatchRetryResponse::default();
    let status = unsafe { read_by_index_retry(&raw const into, &raw mut retry) };
    finish_size_from_retry(response, &retry);
    if !release_retry_handle(&retry) {
        return fail_size(response, INTERNAL_ERROR);
    }
    status
}

#[unsafe(no_mangle)]
/// # Safety
/// Request arrays and pointer/length pairs must be readable, `response` writable, and caller
/// buffers valid for their declared lengths.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_by_index_into_v1(
    request: *const ObjectReadBatchByIndexIntoRequest,
    response: *mut ObjectReadBatchIntoResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_into_by_index(request, response) })
}

#[unsafe(no_mangle)]
/// # Safety
/// The same requirements as [`haruki_assetstudio_context_read_objects_by_index_into_v1`] apply.
pub unsafe extern "C" fn haruki_assetstudio_context_read_objects_by_index_direct_into_v1(
    request: *const ObjectReadBatchByIndexIntoRequest,
    response: *mut ObjectReadBatchIntoResponse,
) -> i32 {
    ffi_boundary(|| unsafe { read_into_by_index(request, response) })
}

unsafe fn read_into_by_index(
    request: *const ObjectReadBatchByIndexIntoRequest,
    response: *mut ObjectReadBatchIntoResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_into(response);
    let Some(request_ref) = (unsafe { request.as_ref() }) else {
        return fail_into(response, NULL_POINTER);
    };
    let Some(context) = get_context(request_ref.context_id) else {
        return fail_into(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_into(response, status),
    };
    let mut retry = ObjectReadBatchRetryResponse::default();
    let status = unsafe { read_by_index_retry(request, &raw mut retry) };
    finish_into_from_retry(
        response,
        &retry,
        request_ref.items_buffer_len,
        request_ref.payload_len,
        status,
    )
}

unsafe fn read_by_index_retry(
    request: *const ObjectReadBatchByIndexIntoRequest,
    response: *mut ObjectReadBatchRetryResponse,
) -> i32 {
    let Some(response) = (unsafe { response.as_mut() }) else {
        return NULL_POINTER;
    };
    initialize_retry(response);
    let Some(request) = (unsafe { request.as_ref() }) else {
        return fail_retry(response, NULL_POINTER);
    };
    if request.struct_size < size_i32::<ObjectReadBatchByIndexIntoRequest>()
        || request.flags != 0
        || request.reserved != 0
        || request.items_buffer_len < 0
        || request.payload_len < 0
    {
        return fail_retry(response, INVALID_REQUEST);
    }
    response.context_id = request.context_id;
    let Some(context) = get_context(request.context_id) else {
        return fail_retry(response, CONTEXT_NOT_FOUND);
    };
    let _operation = match context.try_acquire() {
        Ok(operation) => operation,
        Err(status) => return fail_retry(response, status),
    };
    let items = match unsafe { request_slice(request.items, request.count, MAX_BATCH_COUNT) } {
        Ok(value) => value,
        Err(status) => return fail_retry(response, status),
    };
    let mut reads = Vec::with_capacity(items.len());
    let mut payload_bytes = 0_usize;
    for (index, item) in items.iter().enumerate() {
        if item.object_index < 0 {
            reads.push(BuiltRead::failure(
                i32::try_from(index).unwrap_or(i32::MAX),
                INVALID_REQUEST,
                "object_index cannot be negative",
            ));
            continue;
        }
        let object_index = usize::try_from(item.object_index)
            .ok()
            .filter(|value| *value < context.objects.len());
        let kind = unsafe { read_kind(item.kind_utf8, item.kind_utf8_len) };
        let image_format =
            unsafe { read_image_format(item.image_format_utf8, item.image_format_utf8_len) };
        let read = build_read(
            &context,
            index,
            object_index,
            0,
            kind,
            image_format,
            MAX_BATCH_PAYLOAD_BYTES.saturating_sub(payload_bytes),
        );
        payload_bytes = payload_bytes.saturating_add(read.payload.len());
        reads.push(read);
    }
    unsafe {
        finish_retry(
            request.context_id,
            reads,
            request.items_buffer,
            request.items_buffer_len,
            request.payload,
            request.payload_len,
            response,
        )
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn haruki_assetstudio_result_free(result_handle: i64) -> i32 {
    ffi_boundary(|| {
        let arena = results()
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(&result_handle));
        let Some(arena) = arena else {
            return CONTEXT_NOT_FOUND;
        };
        unsafe {
            free_pointer(arena.items_buffer);
            free_pointer(arena.payload);
        }
        OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn haruki_assetstudio_free_buffer(value: *mut u8) {
    let owned = !value.is_null()
        && legacy_buffers()
            .lock()
            .is_ok_and(|mut guard| guard.remove(&(value as usize)));
    if owned {
        unsafe { free(value.cast::<c_void>()) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn haruki_assetstudio_free_string(value: *mut u8) {
    haruki_assetstudio_free_buffer(value);
}

struct BuiltRead {
    index: i32,
    status: i32,
    error_code: i32,
    path_id: i64,
    type_id: i32,
    size: i64,
    payload: Vec<u8>,
    payload_kind: String,
    suggested_extension: String,
    error_message: String,
}

struct BoundedPayload {
    bytes: Vec<u8>,
    maximum: usize,
}

impl BoundedPayload {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedPayload {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let new_length = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| io::Error::other("payload length overflowed"))?;
        if new_length > self.maximum {
            return Err(io::Error::other(format!(
                "payload exceeds {} byte batch budget",
                self.maximum
            )));
        }
        self.bytes
            .try_reserve(input.len())
            .map_err(|error| io::Error::other(format!("cannot allocate payload: {error}")))?;
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn build_context(
    collection: AssetCollection,
    requested_asset_types: Vec<String>,
    indexed_asset_types: &[String],
) -> ContextState {
    let mut objects = Vec::new();
    let mut path_id_index = HashMap::new();
    let mut source_ordinal = 0_usize;
    let mut next_synthetic_path_id = -1_i64;
    for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
        for (object_index, object) in loaded.file.objects.iter().enumerate() {
            if !managed_v1_object_is_visible(&loaded.file, object.class_id) {
                continue;
            }
            let object_ordinal = source_ordinal;
            source_ordinal = source_ordinal.saturating_add(1);
            let include_array_layers = object.class_id == TEXTURE_2D_ARRAY_CLASS_ID
                && type_matches_filters(TEXTURE_2D_ARRAY_IMAGE_CLASS_ID, indexed_asset_types);
            let include_parent = type_matches_filters(object.class_id, indexed_asset_types);
            if !include_parent && !include_array_layers {
                continue;
            }
            let metadata = collection.object_metadata(file_index, object.path_id);
            let name = metadata
                .and_then(|metadata| metadata.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| object_name(&loaded.file, object_index, object_ordinal));
            let container = metadata
                .and_then(|metadata| metadata.container.as_deref())
                .unwrap_or_default()
                .to_owned();
            if include_parent {
                let index = objects.len();
                let entry = ObjectRef {
                    file_index,
                    object_index,
                    texture_array_layer: None,
                    path_id: object.path_id,
                    class_id: object.class_id,
                    size: object.byte_size,
                    name: name.clone(),
                    container: container.clone(),
                    type_name: class_name(object.class_id),
                    unique_id: format!("_#{object_ordinal}"),
                    source_file: loaded.path.clone(),
                };
                path_id_index.entry(object.path_id).or_insert(index);
                objects.push(entry);
            }
            if include_array_layers
                && let Ok(texture) = read_texture2d_array(
                    &collection,
                    &loaded.file,
                    object_index,
                    TextureArrayReadLimits::default(),
                )
            {
                let layer_size = u64::from(texture.width)
                    .saturating_mul(u64::from(texture.height))
                    .saturating_mul(4);
                for layer in 0..texture.layer_count() {
                    let index = objects.len();
                    let path_id = next_synthetic_path_id;
                    next_synthetic_path_id = next_synthetic_path_id.saturating_sub(1);
                    let entry = ObjectRef {
                        file_index,
                        object_index,
                        texture_array_layer: Some(layer),
                        path_id,
                        class_id: TEXTURE_2D_ARRAY_IMAGE_CLASS_ID,
                        size: layer_size,
                        name: format!("{}_{}", texture.name, layer + 1),
                        container: container.clone(),
                        type_name: class_name(TEXTURE_2D_ARRAY_IMAGE_CLASS_ID),
                        unique_id: String::new(),
                        source_file: loaded.path.clone(),
                    };
                    path_id_index.entry(path_id).or_insert(index);
                    objects.push(entry);
                }
            }
        }
    }
    ContextState {
        collection,
        objects,
        path_id_index,
        requested_asset_types,
        lifetime: AtomicUsize::new(0),
    }
}

fn managed_v1_object_is_visible(
    file: &assetstudio_core::serialized::SerializedFile,
    class_id: i32,
) -> bool {
    // The managed v1 oracle deliberately does not construct Shader objects
    // for Unity 2021 or newer, so they never enter its parsed object table.
    class_id != SHADER_CLASS_ID || file.unity_version.major < 2021
}

fn object_name(
    file: &assetstudio_core::serialized::SerializedFile,
    index: usize,
    source_ordinal: usize,
) -> String {
    if let Ok(Some(metadata)) =
        read_object_name_metadata(file, index, ObjectNameReadLimits::default())
    {
        if let Some(name) = metadata.name.filter(|name| !name.is_empty()) {
            return name;
        }
    }
    format!(
        "{}_#{}",
        class_name(file.objects[index].class_id),
        source_ordinal
    )
}

fn get_context(context_id: i64) -> Option<Arc<ContextState>> {
    contexts().read().ok()?.get(&context_id).cloned()
}

fn filtered_indexes(context: &ContextState, filters: &[String]) -> Vec<usize> {
    let filters = if filters.is_empty() {
        &context.requested_asset_types
    } else {
        filters
    };
    context
        .objects
        .iter()
        .enumerate()
        .filter_map(|(index, object)| matches_filters(object, filters).then_some(index))
        .collect()
}

fn matches_filters(object: &ObjectRef, filters: &[String]) -> bool {
    type_matches_filters(object.class_id, filters)
}

fn type_matches_filters(class_id: i32, filters: &[String]) -> bool {
    filters.is_empty()
        || filters.iter().any(|filter| {
            matches!(filter.as_str(), "all" | "*") || filter_matches_class_id(filter, class_id)
        })
}

fn filter_matches_class_id(filter: &str, class_id: i32) -> bool {
    if filter.parse::<i32>() == Ok(class_id) {
        return true;
    }
    if filter
        .strip_prefix("classid")
        .and_then(|value| value.parse::<i32>().ok())
        == Some(class_id)
    {
        return true;
    }
    if filter == "navmeshprojectsettings" {
        return class_id == 126;
    }
    if filter == "texture2darray" {
        return class_id == TEXTURE_2D_ARRAY_IMAGE_CLASS_ID;
    }
    known_class_name(class_id).is_some_and(|name| name.eq_ignore_ascii_case(filter))
}

fn valid_open_filter(filter: &str) -> bool {
    if matches!(filter, "all" | "*") || filter.parse::<i32>().is_ok() {
        return true;
    }
    if let Some(class_id) = filter
        .strip_prefix("classid")
        .and_then(|value| value.parse::<i32>().ok())
    {
        return known_class_name(class_id).is_some();
    }
    matches!(
        filter,
        "tex2d" | "image" | "tex2darray" | "audio" | "video" | "monobehavior" | "monobehaviour"
    ) || CLASS_ID_NAMES
        .iter()
        .any(|(_, name)| name.eq_ignore_ascii_case(filter))
}

fn normalize_type(value: &str) -> String {
    match value.trim().replace('_', "").to_ascii_lowercase().as_str() {
        "tex2d" | "image" => "texture2d".to_owned(),
        "tex2darray" => "texture2darray".to_owned(),
        "monobehavior" | "monobehaviour" => "monobehaviour".to_owned(),
        "audio" => "audioclip".to_owned(),
        "video" => "videoclip".to_owned(),
        normalized => normalized.to_owned(),
    }
}

fn default_asset_filters() -> Vec<String> {
    [
        "texture2d",
        "texture2darray",
        "sprite",
        "textasset",
        "monobehaviour",
        "font",
        "shader",
        "audioclip",
        "videoclip",
        "movietexture",
        "mesh",
        "animator",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn paginate(
    selected: &[usize],
    requested_offset: i32,
    requested_limit: i32,
) -> Result<(usize, usize, Vec<usize>), ()> {
    let offset = usize::try_from(requested_offset.max(0)).map_err(|_| ())?;
    let limit = if requested_limit <= 0 {
        selected.len().min(MAX_PAGE_LIMIT)
    } else {
        usize::try_from(requested_limit).map_err(|_| ())?
    };
    if limit > MAX_PAGE_LIMIT {
        return Err(());
    }
    let start = offset.min(selected.len());
    let end = start.saturating_add(limit).min(selected.len());
    Ok((offset, limit, selected[start..end].to_vec()))
}

fn normalize_lookup_limit(requested_limit: i32) -> Option<usize> {
    if requested_limit > i32::try_from(MAX_PAGE_LIMIT).ok()? {
        return None;
    }
    if requested_limit <= 0 {
        Some(MAX_PAGE_LIMIT)
    } else {
        usize::try_from(requested_limit).ok()
    }
}

fn populate_table_metadata(
    response: &mut ObjectTable,
    context_id: i64,
    offset: usize,
    limit: usize,
    total: usize,
    page: &[usize],
    context: &ContextState,
) -> Result<(), i32> {
    let next = offset.saturating_add(page.len());
    response.context_id = context_id;
    response.offset = i32::try_from(offset).unwrap_or(i32::MAX);
    response.limit = i32::try_from(limit).unwrap_or(i32::MAX);
    response.next_offset = if next < total {
        i32::try_from(next).unwrap_or(i32::MAX)
    } else {
        -1
    };
    response.has_more = i32::from(next < total);
    response.total_count = i32::try_from(total).unwrap_or(i32::MAX);
    response.returned_count = i32::try_from(page.len()).unwrap_or(i32::MAX);
    let mut string_bytes = 0_usize;
    for index in page {
        string_bytes = string_bytes
            .checked_add(object_string_len(&context.objects[*index], *index)?)
            .ok_or(INTERNAL_ERROR)?;
    }
    let strings_offset = table_string_offset(page.len()).ok_or(INTERNAL_ERROR)?;
    let buffer_len = strings_offset
        .checked_add(string_bytes)
        .filter(|length| *length <= MAX_OBJECT_TABLE_BUFFER_BYTES)
        .ok_or(INTERNAL_ERROR)?;
    response.string_data_len = i32::try_from(string_bytes).map_err(|_| INTERNAL_ERROR)?;
    response.buffer_len = i64::try_from(buffer_len).map_err(|_| INTERNAL_ERROR)?;
    Ok(())
}

#[allow(clippy::cast_ptr_alignment)]
unsafe fn write_object_table(
    response: &mut ObjectTable,
    page: &[usize],
    context: &ContextState,
    buffer: *mut u8,
    buffer_len: i64,
) -> i32 {
    let required = usize::try_from(response.buffer_len).unwrap_or(usize::MAX);
    let provided = usize::try_from(buffer_len).unwrap_or(0);
    if required != 0 && (buffer.is_null() || provided < required) {
        return fail_table(response, 8);
    }
    if required != 0 && !(buffer as usize).is_multiple_of(align_of::<AssetObject>()) {
        return fail_table(response, INVALID_REQUEST);
    }
    if required == 0 {
        return OK;
    }
    unsafe { ptr::write_bytes(buffer, 0, required) };
    let objects = buffer.cast::<AssetObject>();
    let strings_offset = table_string_offset(page.len()).ok_or(INTERNAL_ERROR);
    let Ok(strings_offset) = strings_offset else {
        return fail_table(response, INTERNAL_ERROR);
    };
    let strings = unsafe { buffer.add(strings_offset) };
    let mut cursor = 0_usize;
    for (slot, object_index) in page.iter().enumerate() {
        let object = &context.objects[*object_index];
        let mut native = AssetObject {
            index: i32::try_from(*object_index).unwrap_or(i32::MAX),
            type_id: object.class_id,
            path_id: object.path_id,
            size: i64::try_from(object.size).unwrap_or(i64::MAX),
            estimated_payload_capacity: i64::try_from(object.size).unwrap_or(i64::MAX),
            raw_payload_capacity: i64::try_from(object.size).unwrap_or(i64::MAX),
            payload_capacity_flags: 1 | 4,
            ..AssetObject::default()
        };
        unsafe {
            write_pool_string(
                strings,
                &mut cursor,
                &object.name,
                &mut native.name_offset,
                &mut native.name_len,
            );
        };
        unsafe {
            write_pool_string(
                strings,
                &mut cursor,
                &object.container,
                &mut native.container_offset,
                &mut native.container_len,
            );
        };
        unsafe {
            write_pool_string(
                strings,
                &mut cursor,
                &object.type_name,
                &mut native.type_offset,
                &mut native.type_len,
            );
        };
        unsafe {
            write_pool_string(
                strings,
                &mut cursor,
                &object.unique_id,
                &mut native.unique_id_offset,
                &mut native.unique_id_len,
            );
        };
        unsafe {
            write_pool_string(
                strings,
                &mut cursor,
                &object.source_file,
                &mut native.source_file_offset,
                &mut native.source_file_len,
            );
        };
        unsafe { objects.add(slot).write(native) };
    }
    response.objects = objects;
    response.string_data = strings;
    response.buffer = buffer;
    OK
}

fn build_read(
    context: &ContextState,
    request_index: usize,
    object_index: Option<usize>,
    requested_path_id: i64,
    kind: Result<String, ()>,
    image_format: Result<String, ()>,
    maximum_payload_bytes: usize,
) -> BuiltRead {
    let index = i32::try_from(request_index).unwrap_or(i32::MAX);
    let Ok(kind) = kind else {
        return BuiltRead::failure_with_path(
            index,
            requested_path_id,
            INVALID_REQUEST,
            "invalid UTF-8 request kind",
        );
    };
    let Ok(image_format) = image_format else {
        return BuiltRead::failure_with_path(
            index,
            requested_path_id,
            INVALID_REQUEST,
            "invalid UTF-8 image format",
        );
    };
    let Some(object_index) = object_index else {
        return BuiltRead::failure_with_path(
            index,
            requested_path_id,
            ASSET_NOT_FOUND,
            "asset was not found",
        );
    };
    let object = &context.objects[object_index];
    match read_payload(context, object, &kind, &image_format, maximum_payload_bytes) {
        Ok((payload, payload_kind, extension)) => BuiltRead {
            index,
            status: OK,
            error_code: OK,
            path_id: object.path_id,
            type_id: object.class_id,
            size: i64::try_from(object.size).unwrap_or(i64::MAX),
            payload,
            payload_kind: payload_kind.to_owned(),
            suggested_extension: extension,
            error_message: String::new(),
        },
        Err((status, message)) => BuiltRead::failure_for(index, object, status, &message),
    }
}

impl BuiltRead {
    fn failure(index: i32, status: i32, message: &str) -> Self {
        Self {
            index,
            status,
            error_code: status,
            path_id: 0,
            type_id: 0,
            size: 0,
            payload: Vec::new(),
            payload_kind: String::new(),
            suggested_extension: String::new(),
            error_message: message.to_owned(),
        }
    }

    fn failure_for(index: i32, object: &ObjectRef, status: i32, message: &str) -> Self {
        Self {
            path_id: object.path_id,
            type_id: object.class_id,
            size: i64::try_from(object.size).unwrap_or(i64::MAX),
            ..Self::failure(index, status, message)
        }
    }

    fn failure_with_path(index: i32, path_id: i64, status: i32, message: &str) -> Self {
        Self {
            path_id,
            ..Self::failure(index, status, message)
        }
    }
}

#[derive(Clone, Copy)]
enum LegacyOwnership {
    Buffers,
    Handle,
}

struct LegacyPacked {
    status: i32,
    error_code: i32,
    requested_count: i32,
    returned_count: i32,
    failed_count: i32,
    items: *mut LegacyObjectReadItemResponse,
    string_data: *mut u8,
    string_data_len: i32,
    items_buffer: *mut u8,
    items_buffer_len: i64,
    payload: *mut u8,
    payload_len: i64,
    result_handle: i64,
}

unsafe fn execute_legacy_batch(
    request: &LegacyObjectReadBatchRequest,
    ownership: LegacyOwnership,
) -> Result<LegacyPacked, i32> {
    if request.flags != 0 {
        return Err(INVALID_REQUEST);
    }
    let items = unsafe { request_slice(request.items, request.count, MAX_BATCH_COUNT) }?;
    let context = get_context(request.context_id).ok_or(CONTEXT_NOT_FOUND)?;
    let _operation = context.try_acquire()?;
    let reads = unsafe { build_reads_by_path(&context, items) };
    unsafe { pack_legacy_reads(request.context_id, &reads, ownership) }
}

unsafe fn build_reads_by_path(
    context: &ContextState,
    items: &[ObjectReadItemRequest],
) -> Vec<BuiltRead> {
    let mut reads = Vec::with_capacity(items.len());
    let mut payload_bytes = 0_usize;
    for (index, item) in items.iter().enumerate() {
        let object_index = context.path_id_index.get(&item.path_id).copied();
        let kind = unsafe { read_kind(item.kind_utf8, item.kind_utf8_len) };
        let image_format =
            unsafe { read_image_format(item.image_format_utf8, item.image_format_utf8_len) };
        let read = build_read(
            context,
            index,
            object_index,
            item.path_id,
            kind,
            image_format,
            MAX_BATCH_PAYLOAD_BYTES.saturating_sub(payload_bytes),
        );
        payload_bytes = payload_bytes.saturating_add(read.payload.len());
        reads.push(read);
    }
    reads
}

#[allow(clippy::cast_ptr_alignment)]
unsafe fn pack_legacy_reads(
    context_id: i64,
    reads: &[BuiltRead],
    ownership: LegacyOwnership,
) -> Result<LegacyPacked, i32> {
    let string_len = reads.iter().try_fold(0_usize, |total, read| {
        total
            .checked_add(read.payload_kind.len())?
            .checked_add(read.suggested_extension.len())
    });
    let string_len = string_len.ok_or(INTERNAL_ERROR)?;
    let items_bytes = reads
        .len()
        .checked_mul(size_of::<LegacyObjectReadItemResponse>())
        .ok_or(INTERNAL_ERROR)?;
    let items_offset = align_up(items_bytes, align_of::<u64>()).ok_or(INTERNAL_ERROR)?;
    let items_len = items_offset.checked_add(string_len).ok_or(INTERNAL_ERROR)?;
    if items_len > MAX_OBJECT_TABLE_BUFFER_BYTES {
        return Err(INTERNAL_ERROR);
    }
    let payload_len = reads
        .iter()
        .try_fold(0_usize, |total, read| total.checked_add(read.payload.len()))
        .ok_or(INTERNAL_ERROR)?;
    if payload_len > MAX_BATCH_PAYLOAD_BYTES {
        return Err(INTERNAL_ERROR);
    }

    let (items_buffer, payload) = match ownership {
        LegacyOwnership::Buffers => {
            let items_buffer = unsafe { allocate_registered_buffer(items_len) }?;
            match unsafe { allocate_registered_buffer(payload_len) } {
                Ok(payload) => (items_buffer, payload),
                Err(status) => {
                    haruki_assetstudio_free_buffer(items_buffer);
                    return Err(status);
                }
            }
        }
        LegacyOwnership::Handle => {
            let items_buffer = unsafe { allocate(items_len) };
            let payload = unsafe { allocate(payload_len) };
            if (items_len != 0 && items_buffer.is_null()) || (payload_len != 0 && payload.is_null())
            {
                unsafe {
                    free_pointer(items_buffer as usize);
                    free_pointer(payload as usize);
                }
                return Err(INTERNAL_ERROR);
            }
            (items_buffer, payload)
        }
    };

    if items_len != 0 {
        unsafe { ptr::write_bytes(items_buffer, 0, items_len) };
    }
    let strings = if items_len == 0 {
        ptr::null_mut()
    } else {
        unsafe { items_buffer.add(items_offset) }
    };
    let mut string_cursor = 0_usize;
    let mut payload_cursor = 0_usize;
    for (slot, read) in reads.iter().enumerate() {
        let mut native = LegacyObjectReadItemResponse {
            index: read.index,
            status: read.status,
            error_code: read.error_code,
            path_id: read.path_id,
            type_id: read.type_id,
            size: read.size,
            payload_offset: i64::try_from(payload_cursor).unwrap_or(i64::MAX),
            payload_len: i64::try_from(read.payload.len()).unwrap_or(i64::MAX),
            ..LegacyObjectReadItemResponse::default()
        };
        unsafe {
            write_pool_string(
                strings,
                &mut string_cursor,
                &read.payload_kind,
                &mut native.payload_kind_offset,
                &mut native.payload_kind_len,
            );
            write_pool_string(
                strings,
                &mut string_cursor,
                &read.suggested_extension,
                &mut native.suggested_extension_offset,
                &mut native.suggested_extension_len,
            );
        }
        if !read.payload.is_empty() {
            unsafe {
                ptr::copy_nonoverlapping(
                    read.payload.as_ptr(),
                    payload.add(payload_cursor),
                    read.payload.len(),
                );
            };
            payload_cursor += read.payload.len();
        }
        unsafe {
            items_buffer
                .cast::<LegacyObjectReadItemResponse>()
                .add(slot)
                .write(native);
        };
    }

    let failed = reads.iter().filter(|read| read.status != OK).count();
    let status = batch_status(reads, failed);
    let error_code = batch_error_code(reads, failed);
    let result_handle = if matches!(ownership, LegacyOwnership::Handle)
        && (!items_buffer.is_null() || !payload.is_null())
    {
        let handle = NEXT_RESULT_ID.fetch_add(1, Ordering::Relaxed);
        let arena = ResultArena {
            context_id,
            items_buffer: items_buffer as usize,
            payload: payload as usize,
        };
        let Ok(mut guard) = results().lock() else {
            unsafe {
                free_pointer(items_buffer as usize);
                free_pointer(payload as usize);
            }
            return Err(INTERNAL_ERROR);
        };
        guard.insert(handle, arena);
        handle
    } else {
        0
    };

    Ok(LegacyPacked {
        status,
        error_code,
        requested_count: i32::try_from(reads.len()).unwrap_or(i32::MAX),
        returned_count: i32::try_from(reads.len()).unwrap_or(i32::MAX),
        failed_count: i32::try_from(failed).unwrap_or(i32::MAX),
        items: items_buffer.cast::<LegacyObjectReadItemResponse>(),
        string_data: strings,
        string_data_len: i32::try_from(string_len).unwrap_or(i32::MAX),
        items_buffer,
        items_buffer_len: i64::try_from(items_len).unwrap_or(i64::MAX),
        payload,
        payload_len: i64::try_from(payload_len).unwrap_or(i64::MAX),
        result_handle,
    })
}

fn populate_legacy_batch(response: &mut LegacyObjectReadBatchResponse, packed: &LegacyPacked) {
    response.status = packed.status;
    response.error_code = packed.error_code;
    response.requested_count = packed.requested_count;
    response.returned_count = packed.returned_count;
    response.failed_count = packed.failed_count;
    response.items = packed.items;
    response.string_data = packed.string_data;
    response.string_data_len = packed.string_data_len;
    response.items_buffer = packed.items_buffer;
    response.items_buffer_len = packed.items_buffer_len;
    response.payload = packed.payload;
    response.payload_len = packed.payload_len;
}

fn populate_legacy_batch_handle(
    response: &mut LegacyObjectReadBatchHandleResponse,
    packed: &LegacyPacked,
) {
    response.status = packed.status;
    response.error_code = packed.error_code;
    response.requested_count = packed.requested_count;
    response.returned_count = packed.returned_count;
    response.failed_count = packed.failed_count;
    response.items = packed.items;
    response.string_data = packed.string_data;
    response.string_data_len = packed.string_data_len;
    response.items_buffer = packed.items_buffer;
    response.items_buffer_len = packed.items_buffer_len;
    response.payload = packed.payload;
    response.payload_len = packed.payload_len;
    response.result_handle = packed.result_handle;
}

fn read_payload(
    context: &ContextState,
    object: &ObjectRef,
    kind: &str,
    image_format: &str,
    maximum_payload_bytes: usize,
) -> Result<(Vec<u8>, &'static str, String), (i32, String)> {
    let loaded = &context.collection.serialized_files[object.file_index];
    let file = &loaded.file;
    let normalized = kind.trim().to_ascii_lowercase();
    let normalized = if normalized.is_empty() {
        "auto"
    } else {
        normalized.as_str()
    };
    if let Some(layer) = object.texture_array_layer {
        if !matches!(normalized, "auto" | "image") {
            return Err((
                UNSUPPORTED_KIND,
                "requested kind is unsupported for Texture2DArrayImage".to_owned(),
            ));
        }
        if image_format != "raw_rgba" {
            return Err((
                INVALID_REQUEST,
                "Texture2DArrayImage reads only support raw_rgba".to_owned(),
            ));
        }
        let maximum = u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX);
        let limits = TextureArrayReadLimits {
            maximum_payload_bytes: maximum,
            maximum_output_bytes: maximum.saturating_sub(36),
            maximum_bundle_bytes: maximum,
            ..TextureArrayReadLimits::default()
        };
        let texture = read_texture2d_array(&context.collection, file, object.object_index, limits)
            .map_err(internal_read_error)?;
        let image = texture
            .decode_layer_mip0_rgba8(layer, limits)
            .map_err(internal_read_error)?;
        let mut payload = BoundedPayload::new(maximum_payload_bytes);
        write_rgba_ir(&image, &mut payload).map_err(internal_read_error)?;
        return Ok((payload.into_inner(), "image_raw_rgba", ".rgba".to_owned()));
    }
    let simple_raw = matches!(
        object.class_id,
        AUDIO_CLIP_CLASS_ID | VIDEO_CLIP_CLASS_ID | MOVIE_TEXTURE_CLASS_ID | FONT_CLASS_ID
    );
    if normalized == "raw" && !simple_raw {
        return file
            .read_object_bytes(
                object.object_index,
                u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
            )
            .map(|bytes| (bytes, "raw", ".dat".to_owned()))
            .map_err(internal_read_error);
    }
    if normalized != "auto"
        && normalized != "text_bytes"
        && normalized != "audio"
        && normalized != "video"
        && normalized != "font"
        && normalized != "typetree_json"
        && normalized != "image"
        && normalized != "image_archive"
        && normalized != "shader"
        && normalized != "text"
        && normalized != "mesh"
        && normalized != "obj"
    {
        return Err((UNSUPPORTED_KIND, "unsupported object read kind".to_owned()));
    }
    if object.class_id == 49 && matches!(normalized, "auto" | "text_bytes") {
        return file
            .read_text_asset(object.object_index, maximum_payload_bytes)
            .map(|asset| (asset.script, "text_bytes", ".bytes".to_owned()))
            .map_err(internal_read_error);
    }
    if object.class_id == TEXTURE_2D_CLASS_ID && matches!(normalized, "auto" | "image") {
        if image_format != "raw_rgba" {
            return Err((
                INVALID_REQUEST,
                "Texture2D image reads only support raw_rgba".to_owned(),
            ));
        }
        let limits = TextureReadLimits {
            maximum_payload_bytes: u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
            maximum_output_bytes: u64::try_from(maximum_payload_bytes.saturating_sub(36))
                .unwrap_or(u64::MAX),
            ..TextureReadLimits::default()
        };
        let texture = read_texture2d(&context.collection, file, object.object_index, limits)
            .map_err(internal_read_error)?;
        let image = texture
            .decode_mip_rgba8(0, limits)
            .map_err(internal_read_error)?;
        let mut payload = BoundedPayload::new(maximum_payload_bytes);
        write_rgba_ir(&image, &mut payload).map_err(internal_read_error)?;
        return Ok((payload.into_inner(), "image_raw_rgba", ".rgba".to_owned()));
    }
    if object.class_id == TEXTURE_2D_ARRAY_CLASS_ID
        && matches!(normalized, "auto" | "image" | "image_archive")
    {
        if image_format != "raw_rgba" {
            return Err((
                INVALID_REQUEST,
                "Texture2DArray image reads only support raw_rgba".to_owned(),
            ));
        }
        let maximum = u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX);
        let limits = TextureArrayReadLimits {
            maximum_payload_bytes: maximum,
            maximum_output_bytes: maximum,
            maximum_bundle_bytes: maximum,
            ..TextureArrayReadLimits::default()
        };
        let texture = read_texture2d_array(&context.collection, file, object.object_index, limits)
            .map_err(internal_read_error)?;
        let mut payload = BoundedPayload::new(maximum_payload_bytes);
        write_texture2d_array_rgba_bundle(&texture, limits, &mut payload)
            .map_err(internal_read_error)?;
        return Ok((
            payload.into_inner(),
            "image_array_bundle_raw_rgba",
            String::new(),
        ));
    }
    if object.class_id == SPRITE_CLASS_ID && matches!(normalized, "auto" | "image") {
        if image_format != "raw_rgba" {
            return Err((
                INVALID_REQUEST,
                "Sprite image reads only support raw_rgba".to_owned(),
            ));
        }
        let maximum_image_bytes = maximum_payload_bytes.saturating_sub(36);
        let sprite_limits = SpriteReadLimits {
            maximum_output_pixels: u64::try_from(maximum_image_bytes / 4).unwrap_or(u64::MAX),
            maximum_output_bytes: u64::try_from(maximum_image_bytes).unwrap_or(u64::MAX),
            ..SpriteReadLimits::default()
        };
        let texture_limits = TextureReadLimits {
            maximum_payload_bytes: u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
            maximum_output_bytes: u64::try_from(maximum_image_bytes).unwrap_or(u64::MAX),
            ..TextureReadLimits::default()
        };
        let sprite =
            read_sprite(file, object.object_index, sprite_limits).map_err(internal_read_error)?;
        let image = decode_sprite_rgba8(
            &context.collection,
            file,
            &sprite,
            sprite_limits,
            texture_limits,
        )
        .map_err(internal_read_error)?;
        let mut payload = BoundedPayload::new(maximum_payload_bytes);
        write_rgba_ir_display_order(&image, &mut payload).map_err(internal_read_error)?;
        return Ok((payload.into_inner(), "image_raw_rgba", ".rgba".to_owned()));
    }
    if object.class_id == SHADER_CLASS_ID && matches!(normalized, "auto" | "shader" | "text") {
        let limits = ShaderReadLimits {
            maximum_script_bytes: u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
            maximum_output_bytes: u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
            ..ShaderReadLimits::default()
        };
        let shader = read_shader(file, object.object_index, limits).map_err(internal_read_error)?;
        let payload = shader
            .read_to_vec(u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX))
            .map_err(internal_read_error)?;
        return Ok((
            payload,
            shader.payload_kind(),
            shader.suggested_extension().to_owned(),
        ));
    }
    if object.class_id == MESH_CLASS_ID && matches!(normalized, "auto" | "mesh" | "obj") {
        let maximum_output_bytes = u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX);
        let limits = MeshReadLimits {
            maximum_output_bytes,
            ..MeshReadLimits::default()
        };
        let mesh = read_mesh(file, object.object_index, limits).map_err(internal_read_error)?;
        let mut payload = BoundedPayload::new(maximum_payload_bytes);
        write_mesh_obj(&mesh, &mut payload, maximum_output_bytes).map_err(internal_read_error)?;
        return Ok((payload.into_inner(), "mesh_obj", ".obj".to_owned()));
    }
    if object.class_id == MONO_BEHAVIOUR_CLASS_ID && matches!(normalized, "auto" | "typetree_json")
    {
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: maximum_payload_bytes,
            ..MonoBehaviourReadLimits::default()
        };
        return match read_mono_behaviour_json(file, object.object_index, false, limits) {
            Ok(payload) => Ok((payload.into_bytes(), "typetree_json", ".json".to_owned())),
            Err(assetstudio_core::Error::Unsupported(_)) => file
                .read_object_bytes(
                    object.object_index,
                    u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
                )
                .map(|bytes| (bytes, "raw", ".dat".to_owned()))
                .map_err(internal_read_error),
            Err(error) => Err(internal_read_error(error)),
        };
    }
    let limits = SimpleAssetReadLimits {
        maximum_payload_bytes: u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
        ..SimpleAssetReadLimits::default()
    };
    let simple = match object.class_id {
        AUDIO_CLIP_CLASS_ID if matches!(normalized, "auto" | "audio" | "raw") => {
            read_audio_clip(&context.collection, file, object.object_index, limits)
        }
        VIDEO_CLIP_CLASS_ID if matches!(normalized, "auto" | "video" | "raw") => {
            read_video_clip(&context.collection, file, object.object_index, limits)
        }
        MOVIE_TEXTURE_CLASS_ID if matches!(normalized, "auto" | "video" | "raw") => {
            read_movie_texture(file, object.object_index, limits)
        }
        FONT_CLASS_ID if matches!(normalized, "auto" | "font" | "raw") => {
            read_font(file, object.object_index, limits)
        }
        _ => {
            if matches!(normalized, "auto" | "typetree_json") {
                return read_json_payload(file, object.object_index, maximum_payload_bytes);
            }
            return Err((
                UNSUPPORTED_KIND,
                format!("requested kind is unsupported for {}", object.type_name),
            ));
        }
    }
    .map_err(internal_read_error)?;
    let bytes = simple
        .payload
        .read_to_vec(u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX))
        .map_err(internal_read_error)?;
    Ok((bytes, simple.payload_kind, simple.suggested_extension))
}

fn read_json_payload(
    file: &assetstudio_core::serialized::SerializedFile,
    object_index: usize,
    maximum_payload_bytes: usize,
) -> Result<(Vec<u8>, &'static str, String), (i32, String)> {
    match file.read_type_tree_value(object_index) {
        Ok(value) => {
            let mut bytes = BoundedPayload::new(maximum_payload_bytes);
            assetstudio_core::json::write_type_value_json(&value, &mut bytes, false)
                .map_err(internal_read_error)?;
            Ok((bytes.into_inner(), "typetree_json", ".json".to_owned()))
        }
        Err(_) => file
            .read_object_bytes(
                object_index,
                u64::try_from(maximum_payload_bytes).unwrap_or(u64::MAX),
            )
            .map(|bytes| (bytes, "raw", ".dat".to_owned()))
            .map_err(internal_read_error),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn internal_read_error(error: assetstudio_core::Error) -> (i32, String) {
    let status = if matches!(&error, assetstudio_core::Error::Unsupported(_)) {
        UNSUPPORTED_KIND
    } else {
        INTERNAL_ERROR
    };
    (status, error.to_string())
}

#[allow(clippy::cast_ptr_alignment)]
unsafe fn finish_retry(
    context_id: i64,
    mut reads: Vec<BuiltRead>,
    caller_items: *mut u8,
    caller_items_len: i64,
    caller_payload: *mut u8,
    caller_payload_len: i64,
    response: &mut ObjectReadBatchRetryResponse,
) -> i32 {
    let Some(string_len) = reads.iter().try_fold(0_usize, |total, read| {
        total.checked_add(read_string_len(read))
    }) else {
        return fail_retry(response, INTERNAL_ERROR);
    };
    let Some(items_bytes) = reads.len().checked_mul(size_of::<ObjectReadItemResponse>()) else {
        return fail_retry(response, INTERNAL_ERROR);
    };
    let Some(items_offset) = align_up(items_bytes, 8) else {
        return fail_retry(response, INTERNAL_ERROR);
    };
    let Some(items_len) = items_offset.checked_add(string_len) else {
        return fail_retry(response, INTERNAL_ERROR);
    };
    let Some(payload_len) = reads
        .iter()
        .try_fold(0_usize, |total, read| total.checked_add(read.payload.len()))
    else {
        return fail_retry(response, INTERNAL_ERROR);
    };
    if payload_len > MAX_BATCH_PAYLOAD_BYTES {
        return fail_retry(response, INTERNAL_ERROR);
    }
    let use_native_items = items_len != 0
        && (caller_items.is_null()
            || usize::try_from(caller_items_len).unwrap_or(0) < items_len
            || !(caller_items as usize).is_multiple_of(align_of::<ObjectReadItemResponse>()));
    let use_native_payload = payload_len != 0
        && (caller_payload.is_null()
            || usize::try_from(caller_payload_len).unwrap_or(0) < payload_len);
    let items_buffer = if use_native_items {
        unsafe { allocate(items_len) }
    } else {
        caller_items
    };
    let payload_buffer = if use_native_payload {
        unsafe { allocate(payload_len) }
    } else {
        caller_payload
    };
    if (items_len != 0 && items_buffer.is_null()) || (payload_len != 0 && payload_buffer.is_null())
    {
        unsafe {
            if use_native_items {
                free(items_buffer.cast::<c_void>());
            }
            if use_native_payload {
                free(payload_buffer.cast::<c_void>());
            }
        }
        return fail_retry(response, INTERNAL_ERROR);
    }
    if items_len != 0 {
        unsafe { ptr::write_bytes(items_buffer, 0, items_len) };
    }
    let strings = if items_len == 0 {
        ptr::null_mut()
    } else {
        unsafe { items_buffer.add(items_offset) }
    };
    let mut string_cursor = 0_usize;
    let mut payload_cursor = 0_usize;
    for (slot, read) in reads.iter_mut().enumerate() {
        let mut native = ObjectReadItemResponse {
            index: read.index,
            status: read.status,
            error_code: read.error_code,
            path_id: read.path_id,
            type_id: read.type_id,
            size: read.size,
            payload_offset: i64::try_from(payload_cursor).unwrap_or(i64::MAX),
            payload_len: i64::try_from(read.payload.len()).unwrap_or(i64::MAX),
            ..ObjectReadItemResponse::default()
        };
        unsafe {
            write_pool_string(
                strings,
                &mut string_cursor,
                &read.payload_kind,
                &mut native.payload_kind_offset,
                &mut native.payload_kind_len,
            );
        };
        unsafe {
            write_pool_string(
                strings,
                &mut string_cursor,
                &read.suggested_extension,
                &mut native.suggested_extension_offset,
                &mut native.suggested_extension_len,
            );
        };
        unsafe {
            write_pool_string(
                strings,
                &mut string_cursor,
                &read.error_message,
                &mut native.error_message_offset,
                &mut native.error_message_len,
            );
        };
        if !read.payload.is_empty() {
            unsafe {
                ptr::copy_nonoverlapping(
                    read.payload.as_ptr(),
                    payload_buffer.add(payload_cursor),
                    read.payload.len(),
                );
            };
            payload_cursor += read.payload.len();
        }
        unsafe {
            items_buffer
                .cast::<ObjectReadItemResponse>()
                .add(slot)
                .write(native);
        };
    }
    let failed = reads.iter().filter(|read| read.status != OK).count();
    let status = batch_status(&reads, failed);
    let error_code = batch_error_code(&reads, failed);
    response.status = status;
    response.error_code = error_code;
    response.context_id = context_id;
    response.requested_count = i32::try_from(reads.len()).unwrap_or(i32::MAX);
    response.returned_count = response.requested_count;
    response.failed_count = i32::try_from(failed).unwrap_or(i32::MAX);
    response.items = items_buffer.cast::<ObjectReadItemResponse>();
    response.string_data = strings;
    response.string_data_len = i32::try_from(string_len).unwrap_or(i32::MAX);
    response.items_buffer = items_buffer;
    response.items_buffer_len = i64::try_from(items_len).unwrap_or(i64::MAX);
    response.payload = payload_buffer;
    response.payload_len = i64::try_from(payload_len).unwrap_or(i64::MAX);
    response.required_items_buffer_len = response.items_buffer_len;
    response.required_string_data_len = response.string_data_len;
    response.required_payload_len = response.payload_len;
    response.ownership_flags = i32::from(use_native_items) | (i32::from(use_native_payload) << 1);
    if response.ownership_flags != 0 {
        let handle = NEXT_RESULT_ID.fetch_add(1, Ordering::Relaxed);
        let arena = ResultArena {
            context_id,
            items_buffer: if use_native_items {
                items_buffer as usize
            } else {
                0
            },
            payload: if use_native_payload {
                payload_buffer as usize
            } else {
                0
            },
        };
        if let Ok(mut guard) = results().lock() {
            guard.insert(handle, arena);
            response.result_handle = handle;
        } else {
            unsafe {
                if use_native_items {
                    free(items_buffer.cast::<c_void>());
                }
                if use_native_payload {
                    free(payload_buffer.cast::<c_void>());
                }
            }
            return fail_retry(response, INTERNAL_ERROR);
        }
    }
    status
}

fn batch_status(reads: &[BuiltRead], failed: usize) -> i32 {
    if failed == 0 || failed < reads.len() {
        OK
    } else {
        batch_error_code(reads, failed)
    }
}

fn batch_error_code(reads: &[BuiltRead], failed: usize) -> i32 {
    if failed == 0 {
        OK
    } else if failed < reads.len() {
        PARTIAL_FAILURE
    } else {
        let first = reads.first().map_or(INTERNAL_ERROR, |read| read.error_code);
        if reads.iter().all(|read| read.error_code == first) {
            first
        } else {
            INTERNAL_ERROR
        }
    }
}

fn read_string_len(read: &BuiltRead) -> usize {
    read.payload_kind.len() + read.suggested_extension.len() + read.error_message.len()
}

fn object_string_len(object: &ObjectRef, object_index: usize) -> Result<usize, i32> {
    object
        .name
        .len()
        .checked_add(object.container.len())
        .and_then(|value| value.checked_add(object.type_name.len()))
        .and_then(|value| value.checked_add(format!("_#{object_index}").len()))
        .and_then(|value| value.checked_add(object.source_file.len()))
        .ok_or(INTERNAL_ERROR)
}

fn table_string_offset(count: usize) -> Option<usize> {
    align_up(
        count.checked_mul(size_of::<AssetObject>())?,
        align_of::<u64>(),
    )
}

const fn align_up(value: usize, alignment: usize) -> Option<usize> {
    match value.checked_add(alignment - 1) {
        Some(value) => Some(value & !(alignment - 1)),
        None => None,
    }
}

unsafe fn write_pool_string(
    pool: *mut u8,
    cursor: &mut usize,
    value: &str,
    offset: &mut i32,
    length: &mut i32,
) {
    if value.is_empty() {
        return;
    }
    *offset = i32::try_from(*cursor).unwrap_or(i32::MAX);
    *length = i32::try_from(value.len()).unwrap_or(i32::MAX);
    unsafe { ptr::copy_nonoverlapping(value.as_ptr(), pool.add(*cursor), value.len()) };
    *cursor += value.len();
}

unsafe fn allocate(length: usize) -> *mut u8 {
    if length == 0 {
        ptr::null_mut()
    } else {
        unsafe { malloc(length).cast::<u8>() }
    }
}

unsafe fn allocate_legacy_buffer(bytes: &[u8]) -> Result<*mut u8, i32> {
    let buffer = unsafe { allocate_registered_buffer(bytes.len()) }?;
    if !bytes.is_empty() {
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
    }
    Ok(buffer)
}

unsafe fn allocate_registered_buffer(length: usize) -> Result<*mut u8, i32> {
    let buffer = unsafe { allocate(length) };
    if length != 0 && buffer.is_null() {
        return Err(INTERNAL_ERROR);
    }
    if length != 0 {
        let Ok(mut guard) = legacy_buffers().lock() else {
            unsafe { free(buffer.cast::<c_void>()) };
            return Err(INTERNAL_ERROR);
        };
        guard.insert(buffer as usize);
    }
    Ok(buffer)
}

unsafe fn free_pointer(address: usize) {
    if address != 0 {
        unsafe { free((address as *mut u8).cast::<c_void>()) };
    }
}

fn release_context_results(context_id: i64) {
    let arenas = if let Ok(mut guard) = results().lock() {
        let handles = guard
            .iter()
            .filter_map(|(handle, arena)| (arena.context_id == context_id).then_some(*handle))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| guard.remove(&handle))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    for arena in arenas {
        unsafe {
            free_pointer(arena.items_buffer);
            free_pointer(arena.payload);
        }
    }
}

unsafe fn request_slice<'a, T>(
    pointer: *const T,
    count: i32,
    maximum: usize,
) -> Result<&'a [T], i32> {
    let count = usize::try_from(count).map_err(|_| INVALID_REQUEST)?;
    if count > maximum {
        return Err(INVALID_REQUEST);
    }
    if count == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(NULL_POINTER);
    }
    if !(pointer as usize).is_multiple_of(align_of::<T>()) {
        return Err(INVALID_REQUEST);
    }
    let byte_length = count.checked_mul(size_of::<T>()).ok_or(INVALID_REQUEST)?;
    if byte_length > isize::MAX as usize {
        return Err(INVALID_REQUEST);
    }
    Ok(unsafe { slice::from_raw_parts(pointer, count) })
}

unsafe fn read_kind(pointer: *const u8, length: i32) -> Result<String, ()> {
    let value = unsafe { validate_optional_utf8(pointer, length) }?;
    Ok(if value.is_empty() {
        "auto".to_owned()
    } else {
        value.to_ascii_lowercase()
    })
}

unsafe fn read_image_format(pointer: *const u8, length: i32) -> Result<String, ()> {
    let value = unsafe { validate_optional_utf8(pointer, length) }?;
    Ok(if value.is_empty() {
        "raw_rgba".to_owned()
    } else {
        value.to_ascii_lowercase()
    })
}

unsafe fn parse_filters(pointer: *const u8, length: i32) -> Result<Vec<String>, ()> {
    let value = unsafe { validate_optional_utf8(pointer, length) }?;
    Ok(value
        .split([',', ';'])
        .map(normalize_type)
        .filter(|item| !item.is_empty())
        .collect())
}

unsafe fn parse_lookup_filters(pointer: *const u8, length: i32) -> Result<Vec<String>, ()> {
    let value = unsafe { parse_native_utf8_lossy(pointer, length) }?;
    Ok(value
        .split([',', ';'])
        .map(normalize_type)
        .filter(|item| !item.is_empty())
        .collect())
}

unsafe fn parse_native_utf8_lossy(pointer: *const u8, length: i32) -> Result<String, ()> {
    let length = usize::try_from(length).map_err(|_| ())?;
    if length == 0 {
        return Ok(String::new());
    }
    if length > MAX_UTF8_BYTES || pointer.is_null() {
        return Err(());
    }
    let bytes = unsafe { slice::from_raw_parts(pointer, length) };
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

unsafe fn validate_optional_utf8(pointer: *const u8, length: i32) -> Result<String, ()> {
    if length == 0 {
        return Ok(String::new());
    }
    unsafe { read_utf8(pointer, length) }
}

unsafe fn read_utf8(pointer: *const u8, length: i32) -> Result<String, ()> {
    let length = usize::try_from(length).map_err(|_| ())?;
    if length == 0 {
        return Ok(String::new());
    }
    if length > MAX_UTF8_BYTES || pointer.is_null() {
        return Err(());
    }
    let bytes = unsafe { slice::from_raw_parts(pointer, length) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ())
}

fn string_matches(value: &str, query: &str, contains: bool) -> bool {
    if query.is_empty() {
        return false;
    }
    let value = value.to_lowercase();
    let query = query.to_lowercase();
    if contains {
        value.contains(&query)
    } else {
        value == query
    }
}

// Canonical names mirror AssetStudio/ClassIDType.cs. Keep this sorted by numeric class ID so
// object indexing does not pay a linear scan for every serialized object. Enum value 126 has two
// C# names; NavMeshAreas is the stable display name and the other spelling remains a filter alias.
const CLASS_ID_NAMES: &[(i32, &str)] = &[
    (-187, "Texture2DArrayImage"),
    (-1, "UnknownType"),
    (0, "Object"),
    (1, "GameObject"),
    (2, "Component"),
    (3, "LevelGameManager"),
    (4, "Transform"),
    (5, "TimeManager"),
    (6, "GlobalGameManager"),
    (8, "Behaviour"),
    (9, "GameManager"),
    (11, "AudioManager"),
    (12, "ParticleAnimator"),
    (13, "InputManager"),
    (15, "EllipsoidParticleEmitter"),
    (17, "Pipeline"),
    (18, "EditorExtension"),
    (19, "Physics2DSettings"),
    (20, "Camera"),
    (21, "Material"),
    (23, "MeshRenderer"),
    (25, "Renderer"),
    (26, "ParticleRenderer"),
    (27, "Texture"),
    (28, "Texture2D"),
    (29, "OcclusionCullingSettings"),
    (30, "GraphicsSettings"),
    (33, "MeshFilter"),
    (41, "OcclusionPortal"),
    (43, "Mesh"),
    (45, "Skybox"),
    (47, "QualitySettings"),
    (48, "Shader"),
    (49, "TextAsset"),
    (50, "Rigidbody2D"),
    (51, "Physics2DManager"),
    (53, "Collider2D"),
    (54, "Rigidbody"),
    (55, "PhysicsManager"),
    (56, "Collider"),
    (57, "Joint"),
    (58, "CircleCollider2D"),
    (59, "HingeJoint"),
    (60, "PolygonCollider2D"),
    (61, "BoxCollider2D"),
    (62, "PhysicsMaterial2D"),
    (64, "MeshCollider"),
    (65, "BoxCollider"),
    (66, "CompositeCollider2D"),
    (68, "EdgeCollider2D"),
    (70, "CapsuleCollider2D"),
    (72, "ComputeShader"),
    (74, "AnimationClip"),
    (75, "ConstantForce"),
    (76, "WorldParticleCollider"),
    (78, "TagManager"),
    (81, "AudioListener"),
    (82, "AudioSource"),
    (83, "AudioClip"),
    (84, "RenderTexture"),
    (86, "CustomRenderTexture"),
    (87, "MeshParticleEmitter"),
    (88, "ParticleEmitter"),
    (89, "Cubemap"),
    (90, "Avatar"),
    (91, "AnimatorController"),
    (92, "GUILayer"),
    (93, "RuntimeAnimatorController"),
    (94, "ScriptMapper"),
    (95, "Animator"),
    (96, "TrailRenderer"),
    (98, "DelayedCallManager"),
    (102, "TextMesh"),
    (104, "RenderSettings"),
    (108, "Light"),
    (109, "CGProgram"),
    (110, "BaseAnimationTrack"),
    (111, "Animation"),
    (114, "MonoBehaviour"),
    (115, "MonoScript"),
    (116, "MonoManager"),
    (117, "Texture3D"),
    (118, "NewAnimationTrack"),
    (119, "Projector"),
    (120, "LineRenderer"),
    (121, "Flare"),
    (122, "Halo"),
    (123, "LensFlare"),
    (124, "FlareLayer"),
    (125, "HaloLayer"),
    (126, "NavMeshAreas"),
    (127, "HaloManager"),
    (128, "Font"),
    (129, "PlayerSettings"),
    (130, "NamedObject"),
    (131, "GUITexture"),
    (132, "GUIText"),
    (133, "GUIElement"),
    (134, "PhysicMaterial"),
    (135, "SphereCollider"),
    (136, "CapsuleCollider"),
    (137, "SkinnedMeshRenderer"),
    (138, "FixedJoint"),
    (140, "RaycastCollider"),
    (141, "BuildSettings"),
    (142, "AssetBundle"),
    (143, "CharacterController"),
    (144, "CharacterJoint"),
    (145, "SpringJoint"),
    (146, "WheelCollider"),
    (147, "ResourceManager"),
    (148, "NetworkView"),
    (149, "NetworkManager"),
    (150, "PreloadData"),
    (152, "MovieTexture"),
    (153, "ConfigurableJoint"),
    (154, "TerrainCollider"),
    (155, "MasterServerInterface"),
    (156, "TerrainData"),
    (157, "LightmapSettings"),
    (158, "WebCamTexture"),
    (159, "EditorSettings"),
    (160, "InteractiveCloth"),
    (161, "ClothRenderer"),
    (162, "EditorUserSettings"),
    (163, "SkinnedCloth"),
    (164, "AudioReverbFilter"),
    (165, "AudioHighPassFilter"),
    (166, "AudioChorusFilter"),
    (167, "AudioReverbZone"),
    (168, "AudioEchoFilter"),
    (169, "AudioLowPassFilter"),
    (170, "AudioDistortionFilter"),
    (171, "SparseTexture"),
    (180, "AudioBehaviour"),
    (181, "AudioFilter"),
    (182, "WindZone"),
    (183, "Cloth"),
    (184, "SubstanceArchive"),
    (185, "ProceduralMaterial"),
    (186, "ProceduralTexture"),
    (187, "Texture2DArray"),
    (188, "CubemapArray"),
    (191, "OffMeshLink"),
    (192, "OcclusionArea"),
    (193, "Tree"),
    (194, "NavMeshObsolete"),
    (195, "NavMeshAgent"),
    (196, "NavMeshSettings"),
    (197, "LightProbesLegacy"),
    (198, "ParticleSystem"),
    (199, "ParticleSystemRenderer"),
    (200, "ShaderVariantCollection"),
    (205, "LODGroup"),
    (206, "BlendTree"),
    (207, "Motion"),
    (208, "NavMeshObstacle"),
    (210, "SortingGroup"),
    (212, "SpriteRenderer"),
    (213, "Sprite"),
    (214, "CachedSpriteAtlas"),
    (215, "ReflectionProbe"),
    (216, "ReflectionProbes"),
    (218, "Terrain"),
    (220, "LightProbeGroup"),
    (221, "AnimatorOverrideController"),
    (222, "CanvasRenderer"),
    (223, "Canvas"),
    (224, "RectTransform"),
    (225, "CanvasGroup"),
    (226, "BillboardAsset"),
    (227, "BillboardRenderer"),
    (228, "SpeedTreeWindAsset"),
    (229, "AnchoredJoint2D"),
    (230, "Joint2D"),
    (231, "SpringJoint2D"),
    (232, "DistanceJoint2D"),
    (233, "HingeJoint2D"),
    (234, "SliderJoint2D"),
    (235, "WheelJoint2D"),
    (236, "ClusterInputManager"),
    (237, "BaseVideoTexture"),
    (238, "NavMeshData"),
    (240, "AudioMixer"),
    (241, "AudioMixerController"),
    (243, "AudioMixerGroupController"),
    (244, "AudioMixerEffectController"),
    (245, "AudioMixerSnapshotController"),
    (246, "PhysicsUpdateBehaviour2D"),
    (247, "ConstantForce2D"),
    (248, "Effector2D"),
    (249, "AreaEffector2D"),
    (250, "PointEffector2D"),
    (251, "PlatformEffector2D"),
    (252, "SurfaceEffector2D"),
    (253, "BuoyancyEffector2D"),
    (254, "RelativeJoint2D"),
    (255, "FixedJoint2D"),
    (256, "FrictionJoint2D"),
    (257, "TargetJoint2D"),
    (258, "LightProbes"),
    (259, "LightProbeProxyVolume"),
    (271, "SampleClip"),
    (272, "AudioMixerSnapshot"),
    (273, "AudioMixerGroup"),
    (280, "NScreenBridge"),
    (290, "AssetBundleManifest"),
    (292, "UnityAdsManager"),
    (300, "RuntimeInitializeOnLoadManager"),
    (301, "CloudWebServicesManager"),
    (303, "UnityAnalyticsManager"),
    (304, "CrashReportManager"),
    (305, "PerformanceReportingManager"),
    (310, "UnityConnectSettings"),
    (319, "AvatarMask"),
    (320, "PlayableDirector"),
    (328, "VideoPlayer"),
    (329, "VideoClip"),
    (330, "ParticleSystemForceField"),
    (331, "SpriteMask"),
    (362, "WorldAnchor"),
    (363, "OcclusionCullingData"),
    (1000, "SmallestEditorClassID"),
    (1001, "PrefabInstance"),
    (1002, "EditorExtensionImpl"),
    (1003, "AssetImporter"),
    (1004, "AssetDatabaseV1"),
    (1005, "Mesh3DSImporter"),
    (1006, "TextureImporter"),
    (1007, "ShaderImporter"),
    (1008, "ComputeShaderImporter"),
    (1020, "AudioImporter"),
    (1026, "HierarchyState"),
    (1027, "GUIDSerializer"),
    (1028, "AssetMetaData"),
    (1029, "DefaultAsset"),
    (1030, "DefaultImporter"),
    (1031, "TextScriptImporter"),
    (1032, "SceneAsset"),
    (1034, "NativeFormatImporter"),
    (1035, "MonoImporter"),
    (1037, "AssetServerCache"),
    (1038, "LibraryAssetImporter"),
    (1040, "ModelImporter"),
    (1041, "FBXImporter"),
    (1042, "TrueTypeFontImporter"),
    (1044, "MovieImporter"),
    (1045, "EditorBuildSettings"),
    (1046, "DDSImporter"),
    (1048, "InspectorExpandedState"),
    (1049, "AnnotationManager"),
    (1050, "PluginImporter"),
    (1051, "EditorUserBuildSettings"),
    (1052, "PVRImporter"),
    (1053, "ASTCImporter"),
    (1054, "KTXImporter"),
    (1055, "IHVImageFormatImporter"),
    (1101, "AnimatorStateTransition"),
    (1102, "AnimatorState"),
    (1105, "HumanTemplate"),
    (1107, "AnimatorStateMachine"),
    (1108, "PreviewAnimationClip"),
    (1109, "AnimatorTransition"),
    (1110, "SpeedTreeImporter"),
    (1111, "AnimatorTransitionBase"),
    (1112, "SubstanceImporter"),
    (1113, "LightmapParameters"),
    (1120, "LightingDataAsset"),
    (1121, "GISRaster"),
    (1122, "GISRasterImporter"),
    (1123, "CadImporter"),
    (1124, "SketchUpImporter"),
    (1125, "BuildReport"),
    (1126, "PackedAssets"),
    (1127, "VideoClipImporter"),
    (2000, "ActivationLogComponent"),
    (100_003, "MonoObject"),
    (100_004, "Collision"),
    (100_005, "Vector3f"),
    (100_006, "RootMotionData"),
    (100_007, "Collision2D"),
    (100_008, "AudioMixerLiveUpdateFloat"),
    (100_009, "AudioMixerLiveUpdateBool"),
    (100_010, "Polygon2D"),
    (19_719_996, "TilemapCollider2D"),
    (41_386_430, "AssetImporterLog"),
    (55_640_938, "GraphicsStateCollection"),
    (73_398_921, "VFXRenderer"),
    (76_251_197, "SerializableManagedRefTestClass"),
    (156_049_354, "Grid"),
    (156_483_287, "ScenesUsingAssets"),
    (171_741_748, "ArticulationBody"),
    (181_963_792, "Preset"),
    (277_625_683, "EmptyObject"),
    (285_090_594, "IConstraint"),
    (293_259_124, "TestObjectWithSpecialLayoutOne"),
    (294_290_339, "AssemblyDefinitionReferenceImporter"),
    (334_799_969, "SiblingDerived"),
    (
        342_846_651,
        "TestObjectWithSerializedMapStringNonAlignedStruct",
    ),
    (355_983_997, "AudioResource"),
    (367_388_927, "SubDerived"),
    (369_655_926, "AssetImportInProgressProxy"),
    (382_020_655, "PluginBuildInfo"),
    (387_306_366, "MemorySettings"),
    (403_037_116, "BuildMetaDataImporter"),
    (403_037_117, "BuildInstructionImporter"),
    (426_301_858, "EditorProjectAccess"),
    (468_431_735, "PrefabImporter"),
    (478_637_458, "TestObjectWithSerializedArray"),
    (478_637_459, "TestObjectWithSerializedAnimationCurve"),
    (483_693_784, "TilemapRenderer"),
    (488_575_907, "ScriptableCamera"),
    (612_988_286, "SpriteAtlasAsset"),
    (638_013_454, "SpriteAtlasDatabase"),
    (641_289_076, "AudioBuildInfo"),
    (644_342_135, "CachedSpriteAtlasRuntimeData"),
    (646_504_946, "RendererFake"),
    (655_991_488, "MultiplayerManager"),
    (662_584_278, "AssemblyDefinitionReferenceAsset"),
    (668_709_126, "BuiltAssetBundleInfoSet"),
    (687_078_895, "SpriteAtlas"),
    (747_330_370, "RayTracingShaderImporter"),
    (780_535_461, "BuildArchiveImporter"),
    (815_301_076, "PreviewImporter"),
    (825_902_497, "RayTracingShader"),
    (850_595_691, "LightingSettings"),
    (877_146_078, "PlatformModuleSetup"),
    (890_905_787, "VersionControlSettings"),
    (893_571_522, "CustomCollider2D"),
    (895_512_359, "AimConstraint"),
    (937_362_698, "VFXManager"),
    (947_337_230, "RoslynAnalyzerConfigAsset"),
    (954_905_827, "RuleSetFileAsset"),
    (994_735_392, "VisualEffectSubgraph"),
    (994_735_403, "VisualEffectSubgraphOperator"),
    (994_735_404, "VisualEffectSubgraphBlock"),
    (1_027_052_791, "LocalizationImporter"),
    (1_091_556_383, "Derived"),
    (1_111_377_672, "PropertyModificationsTargetTestObject"),
    (1_114_811_875, "ReferencesArtifactGenerator"),
    (1_152_215_463, "AssemblyDefinitionAsset"),
    (1_154_873_562, "SceneVisibilityState"),
    (1_183_024_399, "LookAtConstraint"),
    (1_210_832_254, "SpriteAtlasImporter"),
    (1_223_240_404, "MultiArtifactTestImporter"),
    (1_233_149_941, "AudioContainerElement"),
    (1_268_269_756, "GameObjectRecorder"),
    (1_307_931_743, "AudioRandomContainer"),
    (1_325_145_578, "LightingDataAssetParent"),
    (1_386_491_679, "PresetManager"),
    (1_392_443_030, "TestObjectWithSpecialLayoutTwo"),
    (1_403_656_975, "StreamingManager"),
    (1_480_428_607, "LowerResBlitTexture"),
    (1_521_398_425, "VideoBuildInfo"),
    (1_541_671_625, "C4DImporter"),
    (1_542_919_678, "StreamingController"),
    (1_557_264_870, "ShaderContainer"),
    (1_571_458_007, "RenderPassAttachment"),
    (1_597_193_336, "RoslynAdditionalFileAsset"),
    (1_628_831_178, "TestObjectVectorPairStringBool"),
    (1_642_787_288, "RoslynAdditionalFileImporter"),
    (1_652_712_579, "MultiplayerRolesData"),
    (1_660_057_539, "SceneRoots"),
    (1_731_078_267, "BrokenPrefabAsset"),
    (1_736_697_216, "AndroidAssetPackImporter"),
    (1_740_304_944, "VulkanDeviceFilterLists"),
    (1_742_807_556, "GridLayout"),
    (1_766_753_193, "AssemblyDefinitionImporter"),
    (1_773_428_102, "ParentConstraint"),
    (1_777_034_230, "RuleSetFileImporter"),
    (1_803_986_026, "FakeComponent"),
    (1_818_360_608, "PositionConstraint"),
    (1_818_360_609, "RotationConstraint"),
    (1_818_360_610, "ScaleConstraint"),
    (1_839_735_485, "Tilemap"),
    (1_896_753_125, "PackageManifest"),
    (1_896_753_126, "PackageManifestImporter"),
    (1_903_396_204, "RoslynAnalyzerConfigImporter"),
    (1_931_382_933, "UIRenderer"),
    (1_953_259_897, "TerrainLayer"),
    (1_971_053_207, "SpriteShapeRenderer"),
    (1_977_754_360, "NativeObjectType"),
    (1_981_279_845, "TestObjectWithSerializedMapStringBool"),
    (1_995_898_324, "SerializableManagedHost"),
    (2_058_629_509, "VisualEffectAsset"),
    (2_058_629_510, "VisualEffectImporter"),
    (2_058_629_511, "VisualEffectResource"),
    (2_059_678_085, "VisualEffectObject"),
    (2_083_052_967, "VisualEffect"),
    (2_083_778_819, "LocalizationAsset"),
    (2_089_858_483, "ScriptedImporter"),
    (2_103_361_453, "ShaderIncludeImporter"),
];

fn known_class_name(class_id: i32) -> Option<&'static str> {
    CLASS_ID_NAMES
        .binary_search_by_key(&class_id, |&(id, _)| id)
        .ok()
        .map(|index| CLASS_ID_NAMES[index].1)
}

fn class_name(class_id: i32) -> String {
    known_class_name(class_id).map_or_else(|| format!("ClassID_{class_id}"), str::to_owned)
}

fn initialize_open(response: &mut ContextOpenResponse) {
    *response = ContextOpenResponse {
        struct_size: size_i32::<ContextOpenResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        context_abi_version: CONTEXT_ABI_VERSION,
        ..ContextOpenResponse::default()
    };
}

fn initialize_table(response: &mut ObjectTable) {
    *response = ObjectTable {
        struct_size: size_i32::<ObjectTable>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_table_abi_version: OBJECT_TABLE_ABI_VERSION,
        ..ObjectTable::default()
    };
}

fn initialize_retry(response: &mut ObjectReadBatchRetryResponse) {
    *response = ObjectReadBatchRetryResponse {
        struct_size: size_i32::<ObjectReadBatchRetryResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_read_batch_abi_version: OBJECT_READ_BATCH_ABI_VERSION,
        object_read_batch_into_abi_version: OBJECT_READ_BATCH_INTO_ABI_VERSION,
        object_read_batch_direct_retry_abi_version: DIRECT_RETRY_ABI_VERSION,
        ..ObjectReadBatchRetryResponse::default()
    };
}

fn initialize_legacy_read(response: &mut ObjectReadResponse) {
    *response = ObjectReadResponse {
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_read_abi_version: OBJECT_READ_ABI_VERSION,
        ..ObjectReadResponse::default()
    };
}

fn initialize_legacy_batch(response: &mut LegacyObjectReadBatchResponse) {
    *response = LegacyObjectReadBatchResponse {
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_read_batch_abi_version: OBJECT_READ_BATCH_ABI_VERSION,
        ..LegacyObjectReadBatchResponse::default()
    };
}

fn initialize_legacy_batch_handle(response: &mut LegacyObjectReadBatchHandleResponse) {
    *response = LegacyObjectReadBatchHandleResponse {
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_read_batch_abi_version: OBJECT_READ_BATCH_ABI_VERSION,
        object_read_batch_handle_abi_version: OBJECT_READ_BATCH_HANDLE_ABI_VERSION,
        ..LegacyObjectReadBatchHandleResponse::default()
    };
}

fn initialize_size(response: &mut ObjectReadBatchSizeResponse) {
    *response = ObjectReadBatchSizeResponse {
        struct_size: size_i32::<ObjectReadBatchSizeResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_read_batch_abi_version: OBJECT_READ_BATCH_ABI_VERSION,
        object_read_batch_into_abi_version: OBJECT_READ_BATCH_INTO_ABI_VERSION,
        ..ObjectReadBatchSizeResponse::default()
    };
}

fn initialize_into(response: &mut ObjectReadBatchIntoResponse) {
    *response = ObjectReadBatchIntoResponse {
        struct_size: size_i32::<ObjectReadBatchIntoResponse>(),
        abi_version: ABI_VERSION,
        schema_version: SCHEMA_VERSION,
        object_read_batch_abi_version: OBJECT_READ_BATCH_ABI_VERSION,
        object_read_batch_into_abi_version: OBJECT_READ_BATCH_INTO_ABI_VERSION,
        ..ObjectReadBatchIntoResponse::default()
    };
}

fn finish_size_from_retry(
    response: &mut ObjectReadBatchSizeResponse,
    retry: &ObjectReadBatchRetryResponse,
) {
    response.status = retry.status;
    response.error_code = retry.error_code;
    response.context_id = retry.context_id;
    response.requested_count = retry.requested_count;
    response.returned_count = retry.returned_count;
    response.failed_count = retry.failed_count;
    response.required_items_buffer_len = retry.required_items_buffer_len;
    response.required_string_data_len = retry.required_string_data_len;
    response.required_payload_len = retry.required_payload_len;
    response.items_buffer_len = retry.required_items_buffer_len;
    response.string_data_len = retry.required_string_data_len;
    response.payload_len = retry.required_payload_len;
    response.duration_ms = retry.duration_ms;
}

fn finish_into_from_retry(
    response: &mut ObjectReadBatchIntoResponse,
    retry: &ObjectReadBatchRetryResponse,
    provided_items_len: i64,
    provided_payload_len: i64,
    status: i32,
) -> i32 {
    response.context_id = retry.context_id;
    response.requested_count = retry.requested_count;
    response.returned_count = retry.returned_count;
    response.failed_count = retry.failed_count;
    response.required_items_buffer_len = retry.required_items_buffer_len;
    response.required_string_data_len = retry.required_string_data_len;
    response.required_payload_len = retry.required_payload_len;
    response.duration_ms = retry.duration_ms;

    if retry.ownership_flags != 0 {
        if !release_retry_handle(retry) {
            return fail_into(response, INTERNAL_ERROR);
        }
        response.items_buffer_len = provided_items_len;
        response.payload_len = provided_payload_len;
        return fail_into(response, 8);
    }

    response.status = retry.status;
    response.error_code = retry.error_code;
    response.items = retry.items;
    response.string_data = retry.string_data;
    response.string_data_len = retry.string_data_len;
    response.items_buffer = retry.items_buffer;
    response.items_buffer_len = retry.items_buffer_len;
    response.payload = retry.payload;
    response.payload_len = retry.payload_len;
    status
}

fn release_retry_handle(retry: &ObjectReadBatchRetryResponse) -> bool {
    retry.result_handle == 0 || haruki_assetstudio_result_free(retry.result_handle) == OK
}

fn fail_open(response: &mut ContextOpenResponse, status: i32, started: Instant) -> i32 {
    response.status = status;
    response.error_code = status;
    response.duration_ms = elapsed_ms(started);
    status
}
fn fail_close(response: &mut ContextCloseResponse, status: i32, started: Instant) -> i32 {
    response.status = status;
    response.error_code = status;
    response.duration_ms = elapsed_ms(started);
    status
}
fn fail_table(response: &mut ObjectTable, status: i32) -> i32 {
    response.status = status;
    response.error_code = status;
    status
}
fn fail_retry(response: &mut ObjectReadBatchRetryResponse, status: i32) -> i32 {
    response.status = status;
    response.error_code = status;
    status
}
fn fail_legacy_read(response: &mut ObjectReadResponse, status: i32, started: Instant) -> i32 {
    response.status = status;
    response.error_code = status;
    response.duration_ms = elapsed_ms(started);
    status
}
fn fail_legacy_batch(
    response: &mut LegacyObjectReadBatchResponse,
    status: i32,
    started: Instant,
) -> i32 {
    response.status = status;
    response.error_code = status;
    response.duration_ms = elapsed_ms(started);
    status
}
fn fail_legacy_batch_handle(
    response: &mut LegacyObjectReadBatchHandleResponse,
    status: i32,
    started: Instant,
) -> i32 {
    response.status = status;
    response.error_code = status;
    response.duration_ms = elapsed_ms(started);
    status
}
fn fail_size(response: &mut ObjectReadBatchSizeResponse, status: i32) -> i32 {
    response.status = status;
    response.error_code = status;
    status
}
fn fail_into(response: &mut ObjectReadBatchIntoResponse, status: i32) -> i32 {
    response.status = status;
    response.error_code = status;
    status
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}
fn size_i32<T>() -> i32 {
    i32::try_from(size_of::<T>()).expect("ABI structure size fits in i32")
}

fn ffi_boundary(call: impl FnOnce() -> i32) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).unwrap_or(INTERNAL_ERROR)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::mem::offset_of;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn exported_layouts_match_expected_64_bit_sizes() {
        assert_eq!(size_of::<ContextOpenRequest>(), 80);
        assert_eq!(size_of::<ContextOpenResponse>(), 96);
        assert_eq!(size_of::<ContextCloseRequest>(), 24);
        assert_eq!(size_of::<ContextCloseResponse>(), 48);
        assert_eq!(size_of::<LimitsResponse>(), 88);
        assert_eq!(size_of::<CapabilitiesResponse>(), 144);
        assert_eq!(size_of::<AbiLayoutResponse>(), 88);
        assert_eq!(size_of::<ObjectListRequest>(), 48);
        assert_eq!(size_of::<ObjectListIntoRequest>(), 64);
        assert_eq!(size_of::<ObjectLookupRequest>(), 80);
        assert_eq!(size_of::<ObjectLookupIntoRequest>(), 96);
        assert_eq!(size_of::<ObjectTable>(), 112);
        assert_eq!(size_of::<AssetObject>(), 104);
        assert_eq!(size_of::<ObjectReadRequest>(), 48);
        assert_eq!(size_of::<ObjectReadResponse>(), 128);
        assert_eq!(size_of::<LegacyObjectReadItemResponse>(), 72);
        assert_eq!(size_of::<LegacyObjectReadBatchRequest>(), 24);
        assert_eq!(size_of::<LegacyObjectReadBatchResponse>(), 112);
        assert_eq!(size_of::<LegacyObjectReadBatchHandleResponse>(), 128);
        assert_eq!(
            offset_of!(
                LegacyObjectReadBatchHandleResponse,
                object_read_batch_handle_abi_version
            ),
            112
        );
        assert_eq!(
            offset_of!(LegacyObjectReadBatchHandleResponse, result_handle),
            120
        );
        assert_eq!(size_of::<ObjectReadBatchIntoRequest>(), 72);
        assert_eq!(size_of::<ObjectReadBatchRequest>(), 40);
        assert_eq!(size_of::<ObjectReadBatchByIndexIntoRequest>(), 72);
        assert_eq!(size_of::<ObjectReadBatchByIndexRequest>(), 40);
        assert_eq!(size_of::<ObjectReadItemResponse>(), 80);
        assert_eq!(size_of::<ObjectReadBatchSizeResponse>(), 120);
        assert_eq!(size_of::<ObjectReadBatchIntoResponse>(), 152);
        assert_eq!(size_of::<ObjectReadBatchRetryResponse>(), 168);
        assert_eq!(align_of::<ObjectReadBatchRetryResponse>(), 8);
        assert_eq!(offset_of!(ObjectReadBatchRetryResponse, result_handle), 144);
        assert_eq!(
            offset_of!(ObjectReadBatchRetryResponse, ownership_flags),
            152
        );
        assert_eq!(offset_of!(ObjectReadBatchRetryResponse, reserved), 160);
    }

    #[test]
    fn zero_length_utf8_allows_a_null_pointer_without_forming_a_slice() {
        assert_eq!(unsafe { read_utf8(ptr::null(), 0) }, Ok(String::new()));
    }

    #[test]
    fn capability_and_layout_responses_are_self_consistent() {
        let mut capabilities = CapabilitiesResponse::default();
        assert_eq!(
            unsafe { haruki_assetstudio_capabilities_v1(&raw mut capabilities) },
            OK
        );
        assert_eq!(
            usize::try_from(capabilities.struct_size).unwrap(),
            size_of::<CapabilitiesResponse>()
        );
        assert_eq!(
            capabilities.object_read_abi_version,
            OBJECT_READ_ABI_VERSION
        );
        assert_eq!(
            capabilities.object_read_batch_handle_abi_version,
            OBJECT_READ_BATCH_HANDLE_ABI_VERSION
        );
        assert_eq!(capabilities.supports_typed_object_read, 1);
        assert_eq!(capabilities.supports_typed_object_lookup, 0);
        assert_eq!(
            capabilities.supports_caller_provided_object_lookup_buffers,
            0
        );
        let mut layout = AbiLayoutResponse::default();
        assert_eq!(
            unsafe { haruki_assetstudio_abi_layout_v1(&raw mut layout) },
            OK
        );
        assert_eq!(
            usize::try_from(layout.asset_object).unwrap(),
            size_of::<AssetObject>()
        );
        assert_eq!(
            usize::try_from(layout.object_read_batch_retry_response_v1).unwrap(),
            size_of::<ObjectReadBatchRetryResponse>()
        );
    }

    #[test]
    fn result_handles_release_exact_owned_buffers_once() {
        let handle = NEXT_RESULT_ID.fetch_add(1, Ordering::Relaxed);
        let items = unsafe { allocate(8) };
        let payload = unsafe { allocate(8) };
        results().lock().unwrap().insert(
            handle,
            ResultArena {
                context_id: 99,
                items_buffer: items as usize,
                payload: payload as usize,
            },
        );
        assert_eq!(haruki_assetstudio_result_free(handle), OK);
        assert_eq!(haruki_assetstudio_result_free(handle), CONTEXT_NOT_FOUND);
    }

    #[test]
    fn mixed_all_failure_batches_report_internal_status_consistently() {
        let reads = vec![
            BuiltRead::failure(0, ASSET_NOT_FOUND, "missing"),
            BuiltRead::failure(1, UNSUPPORTED_KIND, "unsupported"),
        ];
        assert_eq!(batch_status(&reads, 2), INTERNAL_ERROR);
        assert_eq!(batch_error_code(&reads, 2), INTERNAL_ERROR);

        let reads = vec![
            BuiltRead {
                index: 0,
                status: OK,
                error_code: OK,
                path_id: 7,
                type_id: 49,
                size: 11,
                payload: b"payload".to_vec(),
                payload_kind: "text_bytes".to_owned(),
                suggested_extension: ".bytes".to_owned(),
                error_message: String::new(),
            },
            BuiltRead::failure(1, ASSET_NOT_FOUND, "missing"),
        ];
        assert_eq!(batch_status(&reads, 1), OK);
        assert_eq!(batch_error_code(&reads, 1), PARTIAL_FAILURE);
    }

    #[test]
    fn unsupported_core_features_keep_the_public_ffi_error_family() {
        let (status, message) =
            internal_read_error(assetstudio_core::Error::unsupported("packed Sprite layout"));
        assert_eq!(status, UNSUPPORTED_KIND);
        assert!(message.contains("packed Sprite layout"));

        let (status, _) = internal_read_error(assetstudio_core::Error::invalid_data("truncated"));
        assert_eq!(status, INTERNAL_ERROR);
    }

    #[test]
    fn class_id_names_cover_the_csharp_enum_and_keep_unknown_ids_distinct() {
        assert_eq!(CLASS_ID_NAMES.len(), 392);
        assert!(CLASS_ID_NAMES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(
            CLASS_ID_NAMES
                .iter()
                .filter(|&&(class_id, _)| (0..=363).contains(&class_id))
                .count(),
            220
        );

        for (class_id, expected) in [
            (1, "GameObject"),
            (4, "Transform"),
            (21, "Material"),
            (74, "AnimationClip"),
            (115, "MonoScript"),
            (224, "RectTransform"),
            (329, "VideoClip"),
            (687_078_895, "SpriteAtlas"),
        ] {
            assert_eq!(class_name(class_id), expected);
        }

        assert_eq!(class_name(364), "ClassID_364");
        assert_eq!(class_name(-42), "ClassID_-42");
        assert_ne!(class_name(364), class_name(365));
    }

    #[test]
    fn class_id_filters_accept_csharp_names_aliases_and_diagnostic_ids() {
        for (filter, class_id) in [
            ("Material", 21),
            ("game_object", 1),
            ("AnimationClip", 74),
            ("MonoScript", 115),
            ("tex2d", 28),
            ("image", 28),
            ("mono_behavior", 114),
            ("audio", 83),
            ("video", 329),
            ("NavMeshProjectSettings", 126),
            ("21", 21),
            ("ClassID_364", 364),
        ] {
            assert!(type_matches_filters(class_id, &[normalize_type(filter)]));
        }
        assert!(!type_matches_filters(4, &[normalize_type("Material")]));
        assert!(type_matches_filters(4, &["all".to_owned()]));
        assert!(type_matches_filters(4, &["*".to_owned()]));
    }

    #[test]
    fn opens_lists_reads_and_closes_a_text_asset_through_the_c_abi() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let retained_path = std::env::var_os("ASSETSTUDIO_FFI_FIXTURE_PATH").map(PathBuf::from);
        let path = retained_path.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("assetstudio-ffi-{unique}.assets"))
        });
        fs::write(&path, synthetic_text_asset()).unwrap();
        let path_bytes = path.to_string_lossy().into_owned().into_bytes();

        let open_request = ContextOpenRequest {
            struct_size: size_i32::<ContextOpenRequest>(),
            input_path_utf8: path_bytes.as_ptr(),
            input_path_utf8_len: i32::try_from(path_bytes.len()).unwrap(),
            unity_version_utf8: ptr::null(),
            unity_version_utf8_len: 0,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            output_dir_utf8: ptr::null(),
            output_dir_utf8_len: 0,
            load_all_assets: 0,
            flags: 0,
            reserved: 0,
        };
        let mut open_response = ContextOpenResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_open_v1(&raw const open_request, &raw mut open_response)
            },
            OK
        );
        assert_eq!(open_response.exportable_asset_count, 1);
        assert_eq!(open_response.unity_version_utf8_len, 11);
        assert_eq!(
            unsafe {
                slice::from_raw_parts(
                    open_response.unity_version_utf8,
                    usize::try_from(open_response.unity_version_utf8_len).unwrap(),
                )
            },
            b"2022.3.62f1"
        );
        haruki_assetstudio_free_buffer(open_response.buffer);

        let list_request = ObjectListRequest {
            struct_size: size_i32::<ObjectListRequest>(),
            context_id: open_response.context_id,
            offset: 0,
            limit: 16,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            flags: 0,
            reserved: 0,
        };
        let mut table_size = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_size_v1(
                    &raw const list_request,
                    &raw mut table_size,
                )
            },
            OK
        );
        let table_words = usize::try_from(table_size.buffer_len)
            .unwrap()
            .div_ceil(size_of::<u64>());
        let mut table_buffer = vec![0_u64; table_words];
        let list_into = ObjectListIntoRequest {
            struct_size: size_i32::<ObjectListIntoRequest>(),
            context_id: open_response.context_id,
            offset: 0,
            limit: 16,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            flags: 0,
            reserved: 0,
            buffer: table_buffer.as_mut_ptr().cast::<u8>(),
            buffer_len: i64::try_from(table_buffer.len() * size_of::<u64>()).unwrap(),
        };
        let mut table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_into_v1(
                    &raw const list_into,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table.returned_count, 1);
        let listed = unsafe { *table.objects };
        assert_eq!(listed.type_id, 49);
        assert_eq!(listed.path_id, 7);

        let mut owned_table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_v1(
                    &raw const list_request,
                    &raw mut owned_table,
                )
            },
            OK
        );
        assert_eq!(owned_table.returned_count, 1);
        assert!(!owned_table.buffer.is_null());
        haruki_assetstudio_free_buffer(owned_table.buffer);

        let lookup_request = ObjectLookupRequest {
            struct_size: size_i32::<ObjectLookupRequest>(),
            context_id: open_response.context_id,
            lookup_kind: 1,
            path_id: 7,
            query_utf8: ptr::null(),
            query_utf8_len: 0,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            offset: 0,
            limit: 1,
            flags: 0,
            reserved: 0,
        };
        let mut owned_lookup = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const lookup_request,
                    &raw mut owned_lookup,
                )
            },
            OK
        );
        assert_eq!(owned_lookup.returned_count, 1);
        assert_eq!(unsafe { (*owned_lookup.objects).path_id }, 7);
        haruki_assetstudio_free_buffer(owned_lookup.buffer);

        let kind = b"auto";
        let image_format = b"raw_rgba";
        let read_item = ObjectReadItemByIndexRequest {
            object_index: 0,
            kind_utf8: kind.as_ptr(),
            kind_utf8_len: i32::try_from(kind.len()).unwrap(),
            image_format_utf8: image_format.as_ptr(),
            image_format_utf8_len: i32::try_from(image_format.len()).unwrap(),
        };
        let size_request = ObjectReadBatchByIndexRequest {
            struct_size: size_i32::<ObjectReadBatchByIndexRequest>(),
            context_id: open_response.context_id,
            items: &raw const read_item,
            count: 1,
            flags: 0,
            reserved: 0,
        };
        let mut size_response = ObjectReadBatchSizeResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_by_index_size_v1(
                    &raw const size_request,
                    &raw mut size_response,
                )
            },
            OK
        );
        assert_eq!(size_response.required_payload_len, 11);
        let item_words = usize::try_from(size_response.required_items_buffer_len)
            .unwrap()
            .div_ceil(size_of::<u64>());
        let mut read_items_buffer = vec![0_u64; item_words];
        let mut read_payload_buffer = vec![0_u8; 11];
        let read_into_request = ObjectReadBatchByIndexIntoRequest {
            struct_size: size_i32::<ObjectReadBatchByIndexIntoRequest>(),
            context_id: open_response.context_id,
            items: &raw const read_item,
            count: 1,
            flags: 0,
            reserved: 0,
            items_buffer: read_items_buffer.as_mut_ptr().cast::<u8>(),
            items_buffer_len: i64::try_from(read_items_buffer.len() * size_of::<u64>()).unwrap(),
            payload: read_payload_buffer.as_mut_ptr(),
            payload_len: i64::try_from(read_payload_buffer.len()).unwrap(),
        };
        let mut into_response = ObjectReadBatchIntoResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_by_index_into_v1(
                    &raw const read_into_request,
                    &raw mut into_response,
                )
            },
            OK
        );
        assert_eq!(read_payload_buffer, b"ffi payload");

        let undersized_request = ObjectReadBatchByIndexIntoRequest {
            items_buffer: ptr::null_mut(),
            items_buffer_len: 0,
            payload: ptr::null_mut(),
            payload_len: 0,
            ..read_into_request
        };
        let mut undersized_response = ObjectReadBatchIntoResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_by_index_direct_into_v1(
                    &raw const undersized_request,
                    &raw mut undersized_response,
                )
            },
            8
        );
        assert_eq!(undersized_response.required_payload_len, 11);

        let path_read_item = ObjectReadItemRequest {
            path_id: 7,
            kind_utf8: kind.as_ptr(),
            kind_utf8_len: i32::try_from(kind.len()).unwrap(),
            image_format_utf8: image_format.as_ptr(),
            image_format_utf8_len: i32::try_from(image_format.len()).unwrap(),
        };
        let path_read_request = ObjectReadBatchIntoRequest {
            struct_size: size_i32::<ObjectReadBatchIntoRequest>(),
            context_id: open_response.context_id,
            items: &raw const path_read_item,
            count: 1,
            flags: 0,
            items_buffer: read_items_buffer.as_mut_ptr().cast::<u8>(),
            items_buffer_len: i64::try_from(read_items_buffer.len() * size_of::<u64>()).unwrap(),
            payload: read_payload_buffer.as_mut_ptr(),
            payload_len: i64::try_from(read_payload_buffer.len()).unwrap(),
            reserved: 0,
        };
        let mut path_into_response = ObjectReadBatchIntoResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_direct_into_v1(
                    &raw const path_read_request,
                    &raw mut path_into_response,
                )
            },
            OK
        );
        assert_eq!(read_payload_buffer, b"ffi payload");

        let legacy_single_request = ObjectReadRequest {
            context_id: open_response.context_id,
            path_id: 7,
            kind_utf8: kind.as_ptr(),
            kind_utf8_len: i32::try_from(kind.len()).unwrap(),
            image_format_utf8: image_format.as_ptr(),
            image_format_utf8_len: i32::try_from(image_format.len()).unwrap(),
        };
        let mut legacy_single = ObjectReadResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_object_v1(
                    &raw const legacy_single_request,
                    &raw mut legacy_single,
                )
            },
            OK
        );
        assert_eq!(legacy_single.type_id, 49);
        assert_eq!(legacy_single.payload_len, 11);
        assert_eq!(
            unsafe { slice::from_raw_parts(legacy_single.payload, 11) },
            b"ffi payload"
        );
        assert_eq!(
            unsafe {
                slice::from_raw_parts(
                    legacy_single.payload_kind,
                    usize::try_from(legacy_single.payload_kind_len).unwrap(),
                )
            },
            b"text_bytes"
        );
        assert_eq!(
            unsafe {
                slice::from_raw_parts(
                    legacy_single.suggested_extension,
                    usize::try_from(legacy_single.suggested_extension_len).unwrap(),
                )
            },
            b".bytes"
        );
        assert!(
            legacy_buffers()
                .lock()
                .unwrap()
                .contains(&(legacy_single.payload as usize))
        );
        assert!(
            legacy_buffers()
                .lock()
                .unwrap()
                .contains(&(legacy_single.buffer as usize))
        );
        haruki_assetstudio_free_buffer(legacy_single.payload);
        haruki_assetstudio_free_buffer(legacy_single.buffer);
        assert!(
            !legacy_buffers()
                .lock()
                .unwrap()
                .contains(&(legacy_single.payload as usize))
        );
        assert!(
            !legacy_buffers()
                .lock()
                .unwrap()
                .contains(&(legacy_single.buffer as usize))
        );

        let legacy_batch_request = LegacyObjectReadBatchRequest {
            context_id: open_response.context_id,
            items: &raw const path_read_item,
            count: 1,
            flags: 0,
        };
        let invalid_utf8 = [0xff_u8];
        let invalid_missing_item = ObjectReadItemRequest {
            path_id: 999,
            kind_utf8: invalid_utf8.as_ptr(),
            kind_utf8_len: 1,
            image_format_utf8: ptr::null(),
            image_format_utf8_len: 0,
        };
        let invalid_missing_request = ObjectReadBatchIntoRequest {
            struct_size: size_i32::<ObjectReadBatchIntoRequest>(),
            context_id: open_response.context_id,
            items: &raw const invalid_missing_item,
            count: 1,
            flags: 0,
            items_buffer: ptr::null_mut(),
            items_buffer_len: 0,
            payload: ptr::null_mut(),
            payload_len: 0,
            reserved: 0,
        };
        let mut invalid_missing_response = ObjectReadBatchRetryResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_direct_retry_v1(
                    &raw const invalid_missing_request,
                    &raw mut invalid_missing_response,
                )
            },
            INVALID_REQUEST
        );
        assert_eq!(invalid_missing_response.error_code, INVALID_REQUEST);
        assert_eq!(unsafe { (*invalid_missing_response.items).path_id }, 999);
        assert_eq!(
            unsafe { (*invalid_missing_response.items).status },
            INVALID_REQUEST
        );
        assert_eq!(
            haruki_assetstudio_result_free(invalid_missing_response.result_handle),
            OK
        );

        let missing_item = ObjectReadItemRequest {
            kind_utf8: ptr::null(),
            kind_utf8_len: 0,
            ..invalid_missing_item
        };
        let missing_legacy_request = LegacyObjectReadBatchRequest {
            context_id: open_response.context_id,
            items: &raw const missing_item,
            count: 1,
            flags: 0,
        };
        let mut missing_legacy_response = LegacyObjectReadBatchResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_v1(
                    &raw const missing_legacy_request,
                    &raw mut missing_legacy_response,
                )
            },
            ASSET_NOT_FOUND
        );
        assert_eq!(unsafe { (*missing_legacy_response.items).path_id }, 999);
        assert_eq!(
            unsafe { (*missing_legacy_response.items).status },
            ASSET_NOT_FOUND
        );
        haruki_assetstudio_free_buffer(missing_legacy_response.items_buffer);

        let negative_index_item = ObjectReadItemByIndexRequest {
            object_index: -1,
            kind_utf8: invalid_utf8.as_ptr(),
            kind_utf8_len: 1,
            image_format_utf8: ptr::null(),
            image_format_utf8_len: 0,
        };
        let negative_index_request = ObjectReadBatchByIndexIntoRequest {
            struct_size: size_i32::<ObjectReadBatchByIndexIntoRequest>(),
            context_id: open_response.context_id,
            items: &raw const negative_index_item,
            count: 1,
            flags: 0,
            reserved: 0,
            items_buffer: ptr::null_mut(),
            items_buffer_len: 0,
            payload: ptr::null_mut(),
            payload_len: 0,
        };
        let mut negative_index_response = ObjectReadBatchRetryResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_by_index_direct_retry_v1(
                    &raw const negative_index_request,
                    &raw mut negative_index_response,
                )
            },
            INVALID_REQUEST
        );
        assert_eq!(unsafe { (*negative_index_response.items).path_id }, 0);
        assert_eq!(
            haruki_assetstudio_result_free(negative_index_response.result_handle),
            OK
        );

        let invalid_legacy_batch_request = LegacyObjectReadBatchRequest {
            flags: 1,
            ..legacy_batch_request
        };
        let mut invalid_legacy_batch = LegacyObjectReadBatchResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_v1(
                    &raw const invalid_legacy_batch_request,
                    &raw mut invalid_legacy_batch,
                )
            },
            INVALID_REQUEST
        );
        assert!(invalid_legacy_batch.items_buffer.is_null());
        assert!(invalid_legacy_batch.payload.is_null());
        let mut legacy_batch = LegacyObjectReadBatchResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_v1(
                    &raw const legacy_batch_request,
                    &raw mut legacy_batch,
                )
            },
            OK
        );
        assert_eq!(legacy_batch.returned_count, 1);
        assert_eq!(legacy_batch.items_buffer_len, 88);
        let legacy_item = unsafe { *legacy_batch.items };
        assert_eq!(legacy_item.index, 0);
        assert_eq!(legacy_item.path_id, 7);
        assert_eq!(legacy_item.payload_offset, 0);
        assert_eq!(legacy_item.payload_len, 11);
        assert_eq!(
            unsafe {
                slice::from_raw_parts(
                    legacy_batch
                        .string_data
                        .add(usize::try_from(legacy_item.payload_kind_offset).unwrap()),
                    usize::try_from(legacy_item.payload_kind_len).unwrap(),
                )
            },
            b"text_bytes"
        );
        assert_eq!(
            unsafe {
                slice::from_raw_parts(
                    legacy_batch
                        .string_data
                        .add(usize::try_from(legacy_item.suggested_extension_offset).unwrap()),
                    usize::try_from(legacy_item.suggested_extension_len).unwrap(),
                )
            },
            b".bytes"
        );
        assert_eq!(
            unsafe { slice::from_raw_parts(legacy_batch.payload, 11) },
            b"ffi payload"
        );
        haruki_assetstudio_free_buffer(legacy_batch.items_buffer);
        haruki_assetstudio_free_buffer(legacy_batch.payload);

        let mut legacy_handle = LegacyObjectReadBatchHandleResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_handle_v1(
                    &raw const legacy_batch_request,
                    &raw mut legacy_handle,
                )
            },
            OK
        );
        assert_ne!(legacy_handle.result_handle, 0);
        assert_eq!(legacy_handle.object_read_batch_handle_abi_version, 1);
        assert_eq!(unsafe { (*legacy_handle.items).path_id }, 7);
        assert_eq!(
            unsafe { slice::from_raw_parts(legacy_handle.payload, 11) },
            b"ffi payload"
        );
        assert!(
            !legacy_buffers()
                .lock()
                .unwrap()
                .contains(&(legacy_handle.items_buffer as usize))
        );
        assert!(
            !legacy_buffers()
                .lock()
                .unwrap()
                .contains(&(legacy_handle.payload as usize))
        );
        let legacy_result_handle = legacy_handle.result_handle;

        let read_request = ObjectReadBatchByIndexIntoRequest {
            struct_size: size_i32::<ObjectReadBatchByIndexIntoRequest>(),
            context_id: open_response.context_id,
            items: &raw const read_item,
            count: 1,
            flags: 0,
            reserved: 0,
            items_buffer: ptr::null_mut(),
            items_buffer_len: 0,
            payload: ptr::null_mut(),
            payload_len: 0,
        };
        let mut read_response = ObjectReadBatchRetryResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_by_index_direct_retry_v1(
                    &raw const read_request,
                    &raw mut read_response,
                )
            },
            OK
        );
        assert_eq!(read_response.ownership_flags, 3);
        assert_ne!(read_response.result_handle, 0);
        let payload = unsafe {
            slice::from_raw_parts(
                read_response.payload,
                usize::try_from(read_response.payload_len).unwrap(),
            )
        };
        assert_eq!(payload, b"ffi payload");
        assert_eq!(
            haruki_assetstudio_result_free(read_response.result_handle),
            OK
        );

        let close_request = ContextCloseRequest {
            struct_size: size_i32::<ContextCloseRequest>(),
            context_id: open_response.context_id,
            flags: 0,
            reserved: 0,
        };
        let context = get_context(open_response.context_id).unwrap();
        let active_operation = context.try_acquire().unwrap();
        let mut busy_close_response = ContextCloseResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_close_v1(
                    &raw const close_request,
                    &raw mut busy_close_response,
                )
            },
            CONTEXT_BUSY
        );
        assert_eq!(busy_close_response.error_code, CONTEXT_BUSY);
        drop(active_operation);

        let mut close_response = ContextCloseResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_close_v1(
                    &raw const close_request,
                    &raw mut close_response,
                )
            },
            OK
        );
        assert!(matches!(context.try_acquire(), Err(OPERATION_BUSY)));
        assert_eq!(
            haruki_assetstudio_result_free(legacy_result_handle),
            CONTEXT_NOT_FOUND
        );

        let override_version = b"2019.4.0f1";
        let filter = b"audio";
        let filtered_open_request = ContextOpenRequest {
            unity_version_utf8: override_version.as_ptr(),
            unity_version_utf8_len: i32::try_from(override_version.len()).unwrap(),
            asset_types_csv_utf8: filter.as_ptr(),
            asset_types_csv_utf8_len: i32::try_from(filter.len()).unwrap(),
            ..open_request
        };
        let mut filtered_open_response = ContextOpenResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_open_v1(
                    &raw const filtered_open_request,
                    &raw mut filtered_open_response,
                )
            },
            OK
        );
        assert_eq!(filtered_open_response.exportable_asset_count, 0);
        assert_eq!(filtered_open_response.object_index_count, 0);
        assert_eq!(
            unsafe {
                slice::from_raw_parts(
                    filtered_open_response.unity_version_utf8,
                    usize::try_from(filtered_open_response.unity_version_utf8_len).unwrap(),
                )
            },
            override_version
        );
        haruki_assetstudio_free_buffer(filtered_open_response.buffer);
        let filtered_close_request = ContextCloseRequest {
            context_id: filtered_open_response.context_id,
            ..close_request
        };
        let mut filtered_close_response = ContextCloseResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_close_v1(
                    &raw const filtered_close_request,
                    &raw mut filtered_close_response,
                )
            },
            OK
        );
        if retained_path.is_none() {
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn load_all_indexes_non_exportable_objects_without_changing_default_mode() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("assetstudio-ffi-load-all-{unique}.assets"));
        let mut material = Vec::new();
        push_aligned_string(&mut material, "material");
        fs::write(&path, synthetic_raw_asset(21, 41, &material)).unwrap();
        let path_bytes = path.to_string_lossy().into_owned().into_bytes();
        let mut request = ContextOpenRequest {
            struct_size: size_i32::<ContextOpenRequest>(),
            input_path_utf8: path_bytes.as_ptr(),
            input_path_utf8_len: i32::try_from(path_bytes.len()).unwrap(),
            unity_version_utf8: ptr::null(),
            unity_version_utf8_len: 0,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            output_dir_utf8: ptr::null(),
            output_dir_utf8_len: 0,
            load_all_assets: 0,
            flags: 0,
            reserved: 0,
        };

        let mut default_response = ContextOpenResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_open_v1(&raw const request, &raw mut default_response)
            },
            OK
        );
        assert_eq!(default_response.exportable_asset_count, 0);
        assert_eq!(default_response.object_index_count, 0);
        haruki_assetstudio_free_buffer(default_response.buffer);
        close_test_context(default_response.context_id);

        request.load_all_assets = 1;
        let mut all_response = ContextOpenResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_open_v1(&raw const request, &raw mut all_response)
            },
            OK
        );
        assert_eq!(all_response.exportable_asset_count, 1);
        assert_eq!(all_response.object_index_count, 1);
        let list_request = ObjectListRequest {
            struct_size: size_i32::<ObjectListRequest>(),
            context_id: all_response.context_id,
            offset: 0,
            limit: 1,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            flags: 0,
            reserved: 0,
        };
        let mut table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_v1(&raw const list_request, &raw mut table)
            },
            OK
        );
        assert_eq!(table.returned_count, 1);
        assert_eq!(unsafe { (*table.objects).type_id }, 21);
        assert_eq!(unsafe { (*table.objects).path_id }, 41);
        haruki_assetstudio_free_buffer(table.buffer);
        haruki_assetstudio_free_buffer(all_response.buffer);
        close_test_context(all_response.context_id);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn managed_v1_projection_skips_unconstructed_modern_shaders_and_keeps_ordinals() {
        let mut text = Vec::new();
        push_aligned_string(&mut text, "kept");
        push_i32(&mut text, 4);
        text.extend_from_slice(b"text");
        let mut shader = Vec::new();
        push_aligned_string(&mut shader, "modern shader");
        let bytes = synthetic_multi_raw_asset(&[(48, 9, shader), (49, 10, text)]);
        let collection = AssetCollection::load(
            "projection.assets",
            assetstudio_core::source::Region::from_bytes(bytes),
        )
        .unwrap();

        let context = build_context(collection, Vec::new(), &[]);

        assert_eq!(context.objects.len(), 1);
        assert_eq!(context.objects[0].class_id, 49);
        assert_eq!(context.objects[0].path_id, 10);
        assert_eq!(context.objects[0].unique_id, "_#0");
        assert_eq!(context.path_id_index.get(&9), None);
        assert_eq!(context.path_id_index.get(&10), Some(&0));
    }

    #[test]
    fn context_open_rejects_unknown_asset_type_names_before_loading() {
        let path = b"does-not-need-to-exist.assets";
        let unknown = b"DefinitelyNotAClass";
        let request = ContextOpenRequest {
            struct_size: size_i32::<ContextOpenRequest>(),
            input_path_utf8: path.as_ptr(),
            input_path_utf8_len: i32::try_from(path.len()).unwrap(),
            unity_version_utf8: ptr::null(),
            unity_version_utf8_len: 0,
            asset_types_csv_utf8: unknown.as_ptr(),
            asset_types_csv_utf8_len: i32::try_from(unknown.len()).unwrap(),
            output_dir_utf8: ptr::null(),
            output_dir_utf8_len: 0,
            load_all_assets: 0,
            flags: 0,
            reserved: 0,
        };
        let mut response = ContextOpenResponse::default();

        assert_eq!(
            unsafe { haruki_assetstudio_context_open_v1(&raw const request, &raw mut response) },
            INVALID_REQUEST
        );
        assert_eq!(response.error_code, INVALID_REQUEST);
    }

    #[test]
    fn direct_read_exports_a_synthetic_mesh_as_obj() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("assetstudio-ffi-mesh-{unique}.assets"));
        fs::write(&path, synthetic_raw_asset(43, 7, &resident_mesh_object())).unwrap();
        let path_bytes = path.to_string_lossy().into_owned().into_bytes();
        let open_request = ContextOpenRequest {
            struct_size: size_i32::<ContextOpenRequest>(),
            input_path_utf8: path_bytes.as_ptr(),
            input_path_utf8_len: i32::try_from(path_bytes.len()).unwrap(),
            unity_version_utf8: ptr::null(),
            unity_version_utf8_len: 0,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            output_dir_utf8: ptr::null(),
            output_dir_utf8_len: 0,
            load_all_assets: 0,
            flags: 0,
            reserved: 0,
        };
        let mut open = ContextOpenResponse::default();
        assert_eq!(
            unsafe { haruki_assetstudio_context_open_v1(&raw const open_request, &raw mut open) },
            OK
        );
        assert_eq!(open.object_index_count, 1);

        let kind = b"mesh";
        let item = ObjectReadItemRequest {
            path_id: 7,
            kind_utf8: kind.as_ptr(),
            kind_utf8_len: i32::try_from(kind.len()).unwrap(),
            image_format_utf8: ptr::null(),
            image_format_utf8_len: 0,
        };
        let request = ObjectReadBatchIntoRequest {
            struct_size: size_i32::<ObjectReadBatchIntoRequest>(),
            context_id: open.context_id,
            items: &raw const item,
            count: 1,
            flags: 0,
            items_buffer: ptr::null_mut(),
            items_buffer_len: 0,
            payload: ptr::null_mut(),
            payload_len: 0,
            reserved: 0,
        };
        let mut response = ObjectReadBatchRetryResponse::default();
        let status = unsafe {
            haruki_assetstudio_context_read_objects_direct_retry_v1(
                &raw const request,
                &raw mut response,
            )
        };
        assert_eq!(status, OK);
        assert_eq!(response.failed_count, 0);
        assert_eq!(response.returned_count, 1);
        assert_ne!(response.result_handle, 0);
        let read = unsafe { *response.items };
        assert_eq!(read.status, OK);
        assert_eq!(read.type_id, 43);
        assert_eq!(read.path_id, 7);
        let string_data = unsafe {
            slice::from_raw_parts(
                response.string_data,
                usize::try_from(response.string_data_len).unwrap(),
            )
        };
        let payload_kind_start = usize::try_from(read.payload_kind_offset).unwrap();
        let payload_kind_end = payload_kind_start
            .checked_add(usize::try_from(read.payload_kind_len).unwrap())
            .unwrap();
        assert_eq!(
            &string_data[payload_kind_start..payload_kind_end],
            b"mesh_obj"
        );
        let extension_start = usize::try_from(read.suggested_extension_offset).unwrap();
        let extension_end = extension_start
            .checked_add(usize::try_from(read.suggested_extension_len).unwrap())
            .unwrap();
        assert_eq!(&string_data[extension_start..extension_end], b".obj");
        let payload = unsafe {
            slice::from_raw_parts(
                response.payload,
                usize::try_from(response.payload_len).unwrap(),
            )
        };
        assert_eq!(
            payload,
            concat!(
                "g tri\r\n",
                "v -1 0 0\r\n",
                "v -0 1 0\r\n",
                "v -0 0 1\r\n",
                "g tri_0\r\n",
                "f 3/3/3 2/2/2 1/1/1\r\n",
            )
            .as_bytes()
        );
        assert_eq!(haruki_assetstudio_result_free(response.result_handle), OK);
        haruki_assetstudio_free_buffer(open.buffer);
        close_test_context(open.context_id);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn texture_array_table_projects_synthetic_layers_and_reads_them_individually() {
        let source = [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 16, 17, 18];
        let bytes = synthetic_raw_asset(187, 7, &resident_texture_array_object(&source));
        let collection = AssetCollection::load(
            "array.assets",
            assetstudio_core::source::Region::from_bytes(bytes.clone()),
        )
        .unwrap();
        let filters = default_asset_filters();
        let context = Arc::new(build_context(collection, filters.clone(), &filters));
        assert_eq!(context.objects.len(), 2);
        assert_eq!(context.objects[0].class_id, TEXTURE_2D_ARRAY_IMAGE_CLASS_ID);
        assert_eq!(context.objects[0].path_id, -1);
        assert_eq!(context.objects[0].name, "array_1");
        assert_eq!(context.objects[0].texture_array_layer, Some(0));
        assert_eq!(context.objects[1].class_id, TEXTURE_2D_ARRAY_IMAGE_CLASS_ID);
        assert_eq!(context.objects[1].path_id, -2);
        assert_eq!(context.objects[1].name, "array_2");
        assert_eq!(context.objects[1].texture_array_layer, Some(1));
        let context_id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
        contexts().write().unwrap().insert(context_id, context);

        let list_request = ObjectListRequest {
            struct_size: size_i32::<ObjectListRequest>(),
            context_id,
            offset: 0,
            limit: 10,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            flags: 0,
            reserved: 0,
        };
        let mut table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_v1(&raw const list_request, &raw mut table)
            },
            OK
        );
        assert_eq!(table.returned_count, 2);
        let listed = unsafe { slice::from_raw_parts(table.objects, 2) };
        assert_eq!(listed[0].type_id, TEXTURE_2D_ARRAY_IMAGE_CLASS_ID);
        assert_eq!(listed[0].path_id, -1);
        assert_eq!(listed[1].type_id, TEXTURE_2D_ARRAY_IMAGE_CLASS_ID);
        assert_eq!(listed[1].path_id, -2);
        haruki_assetstudio_free_buffer(table.buffer);

        let image_kind = b"image";
        let image_format = b"raw_rgba";
        let item = ObjectReadItemRequest {
            path_id: -1,
            kind_utf8: image_kind.as_ptr(),
            kind_utf8_len: i32::try_from(image_kind.len()).unwrap(),
            image_format_utf8: image_format.as_ptr(),
            image_format_utf8_len: i32::try_from(image_format.len()).unwrap(),
        };
        let request = ObjectReadBatchIntoRequest {
            struct_size: size_i32::<ObjectReadBatchIntoRequest>(),
            context_id,
            items: &raw const item,
            count: 1,
            flags: 0,
            items_buffer: ptr::null_mut(),
            items_buffer_len: 0,
            payload: ptr::null_mut(),
            payload_len: 0,
            reserved: 0,
        };
        let mut direct = ObjectReadBatchRetryResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_objects_direct_retry_v1(
                    &raw const request,
                    &raw mut direct,
                )
            },
            OK
        );
        assert_eq!(direct.failed_count, 0);
        assert_eq!(direct.returned_count, 1);
        assert_ne!(direct.result_handle, 0);
        let direct_item = unsafe { *direct.items };
        assert_eq!(direct_item.type_id, TEXTURE_2D_ARRAY_IMAGE_CLASS_ID);
        assert_eq!(direct_item.path_id, -1);
        let strings = unsafe {
            slice::from_raw_parts(
                direct.string_data,
                usize::try_from(direct.string_data_len).unwrap(),
            )
        };
        let kind_start = usize::try_from(direct_item.payload_kind_offset).unwrap();
        let kind_end = kind_start + usize::try_from(direct_item.payload_kind_len).unwrap();
        assert_eq!(&strings[kind_start..kind_end], b"image_raw_rgba");
        let extension_start = usize::try_from(direct_item.suggested_extension_offset).unwrap();
        let extension_end =
            extension_start + usize::try_from(direct_item.suggested_extension_len).unwrap();
        assert_eq!(&strings[extension_start..extension_end], b".rgba");
        let direct_payload = unsafe {
            slice::from_raw_parts(direct.payload, usize::try_from(direct.payload_len).unwrap())
        };
        assert_texture_array_layer(direct_payload, &[5, 6, 7, 8, 1, 2, 3, 4]);
        assert_eq!(haruki_assetstudio_result_free(direct.result_handle), OK);

        let auto_kind = b"auto";
        let legacy_request = ObjectReadRequest {
            context_id,
            path_id: -2,
            kind_utf8: auto_kind.as_ptr(),
            kind_utf8_len: i32::try_from(auto_kind.len()).unwrap(),
            image_format_utf8: image_format.as_ptr(),
            image_format_utf8_len: i32::try_from(image_format.len()).unwrap(),
        };
        let mut legacy = ObjectReadResponse::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_read_object_v1(
                    &raw const legacy_request,
                    &raw mut legacy,
                )
            },
            OK
        );
        assert_eq!(legacy.type_id, TEXTURE_2D_ARRAY_IMAGE_CLASS_ID);
        assert_eq!(legacy.path_id, -2);
        let legacy_kind = unsafe {
            slice::from_raw_parts(
                legacy.payload_kind,
                usize::try_from(legacy.payload_kind_len).unwrap(),
            )
        };
        assert_eq!(legacy_kind, b"image_raw_rgba");
        let legacy_extension = unsafe {
            slice::from_raw_parts(
                legacy.suggested_extension,
                usize::try_from(legacy.suggested_extension_len).unwrap(),
            )
        };
        assert_eq!(legacy_extension, b".rgba");
        let legacy_payload = unsafe {
            slice::from_raw_parts(legacy.payload, usize::try_from(legacy.payload_len).unwrap())
        };
        assert_texture_array_layer(legacy_payload, &[15, 16, 17, 18, 11, 12, 13, 14]);
        haruki_assetstudio_free_buffer(legacy.payload);
        haruki_assetstudio_free_buffer(legacy.buffer);
        close_test_context(context_id);

        let all_collection = AssetCollection::load(
            "array.assets",
            assetstudio_core::source::Region::from_bytes(bytes),
        )
        .unwrap();
        let all_context = build_context(all_collection, filters, &[]);
        assert_eq!(all_context.objects.len(), 3);
        assert_eq!(all_context.objects[0].class_id, TEXTURE_2D_ARRAY_CLASS_ID);
        assert_eq!(all_context.objects[0].path_id, 7);
        assert_eq!(
            all_context.objects[1].class_id,
            TEXTURE_2D_ARRAY_IMAGE_CLASS_ID
        );
        assert_eq!(all_context.objects[1].path_id, -1);
        assert_eq!(all_context.objects[2].path_id, -2);
        let (bundle, payload_kind, extension) = read_payload(
            &all_context,
            &all_context.objects[0],
            "image",
            "raw_rgba",
            4096,
        )
        .unwrap();
        assert_eq!(payload_kind, "image_array_bundle_raw_rgba");
        assert!(extension.is_empty());
        assert_texture_array_bundle(&bundle);
    }

    #[test]
    fn load_all_keeps_explicit_filters_but_indexes_other_types() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("assetstudio-ffi-load-all-filter-{unique}"));
        fs::create_dir(&directory).unwrap();
        let material_path = directory.join("material.assets");
        let game_object_path = directory.join("game-object.assets");
        let mut material = Vec::new();
        push_aligned_string(&mut material, "material");
        let mut game_object = Vec::new();
        push_i32(&mut game_object, 0);
        push_i32(&mut game_object, 0);
        push_aligned_string(&mut game_object, "game object");
        fs::write(&material_path, synthetic_raw_asset(21, 41, &material)).unwrap();
        fs::write(&game_object_path, synthetic_raw_asset(1, 42, &game_object)).unwrap();
        let path_bytes = directory.to_string_lossy().into_owned().into_bytes();
        let filter = b"Material";
        let request = ContextOpenRequest {
            struct_size: size_i32::<ContextOpenRequest>(),
            input_path_utf8: path_bytes.as_ptr(),
            input_path_utf8_len: i32::try_from(path_bytes.len()).unwrap(),
            unity_version_utf8: ptr::null(),
            unity_version_utf8_len: 0,
            asset_types_csv_utf8: filter.as_ptr(),
            asset_types_csv_utf8_len: i32::try_from(filter.len()).unwrap(),
            output_dir_utf8: ptr::null(),
            output_dir_utf8_len: 0,
            load_all_assets: 1,
            flags: 0,
            reserved: 0,
        };
        let mut open = ContextOpenResponse::default();
        assert_eq!(
            unsafe { haruki_assetstudio_context_open_v1(&raw const request, &raw mut open) },
            OK
        );
        assert_eq!(open.object_index_count, 2);
        assert_eq!(open.exportable_asset_count, 1);

        let mut list_request = ObjectListRequest {
            struct_size: size_i32::<ObjectListRequest>(),
            context_id: open.context_id,
            offset: 0,
            limit: 10,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            flags: 0,
            reserved: 0,
        };
        let mut table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_v1(&raw const list_request, &raw mut table)
            },
            OK
        );
        assert_eq!(table.returned_count, 1);
        assert_eq!(unsafe { (*table.objects).type_id }, 21);
        haruki_assetstudio_free_buffer(table.buffer);

        let game_object_filter = b"GameObject";
        list_request.asset_types_csv_utf8 = game_object_filter.as_ptr();
        list_request.asset_types_csv_utf8_len = i32::try_from(game_object_filter.len()).unwrap();
        let mut game_objects = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_list_objects_v1(
                    &raw const list_request,
                    &raw mut game_objects,
                )
            },
            OK
        );
        assert_eq!(game_objects.returned_count, 1);
        assert_eq!(unsafe { (*game_objects.objects).type_id }, 1);
        haruki_assetstudio_free_buffer(game_objects.buffer);
        haruki_assetstudio_free_buffer(open.buffer);
        close_test_context(open.context_id);

        fs::remove_file(material_path).unwrap();
        fs::remove_file(game_object_path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn typed_lookup_matches_path_name_container_type_and_filter_semantics() {
        let context_id = install_synthetic_lookup_context();

        let mut path_request = lookup_test_request(context_id, 1, 10, &[]);
        let mut table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const path_request,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table.limit, 65_536);
        assert_eq!(table.total_count, 1);
        assert_eq!(table_paths(&table), [10]);
        assert_eq!(
            table_string(&table, unsafe { (*table.objects).name_offset }, unsafe {
                (*table.objects).name_len
            }),
            b"Alpha"
        );
        assert_eq!(
            table_string(
                &table,
                unsafe { (*table.objects).container_offset },
                unsafe { (*table.objects).container_len }
            ),
            b"assets/hero"
        );
        haruki_assetstudio_free_buffer(table.buffer);

        // Managed FindObject selects the first duplicate PathId before applying a type filter.
        // It must not fall through to the later TextAsset with the same PathId.
        let text_filter = b"TextAsset";
        path_request.asset_types_csv_utf8 = text_filter.as_ptr();
        path_request.asset_types_csv_utf8_len = i32::try_from(text_filter.len()).unwrap();
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const path_request,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table.total_count, 0);
        assert!(table.buffer.is_null());

        let query = b"aLpHa";
        let filters = b"GameObject;Material";
        let mut name_request = lookup_test_request(context_id, 2, 0, query);
        name_request.asset_types_csv_utf8 = filters.as_ptr();
        name_request.asset_types_csv_utf8_len = i32::try_from(filters.len()).unwrap();
        name_request.flags = 1;
        name_request.limit = 16;
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const name_request,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table.total_count, 3);
        assert_eq!(table_paths(&table), [10, 20, 30]);
        haruki_assetstudio_free_buffer(table.buffer);

        let container_query = b"HERO";
        let mut container_request = lookup_test_request(context_id, 3, 0, container_query);
        container_request.flags = 1;
        container_request.limit = 16;
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const container_request,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table_paths(&table), [10, 20]);
        haruki_assetstudio_free_buffer(table.buffer);

        let type_query = b"material";
        let type_filter = b"Material";
        let mut type_request = lookup_test_request(context_id, 4, 0, type_query);
        type_request.asset_types_csv_utf8 = type_filter.as_ptr();
        type_request.asset_types_csv_utf8_len = i32::try_from(type_filter.len()).unwrap();
        type_request.limit = 16;
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const type_request,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table_paths(&table), [10, 30]);
        haruki_assetstudio_free_buffer(table.buffer);

        close_test_context(context_id);
    }

    #[test]
    fn typed_lookup_size_into_preserves_pagination_and_required_size() {
        let context_id = install_synthetic_lookup_context();
        let query = b"alpha";
        let mut request = lookup_test_request(context_id, 2, 0, query);
        request.limit = 1;

        let mut size = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(&raw const request, &raw mut size)
            },
            OK
        );
        assert_eq!(size.offset, 0);
        assert_eq!(size.limit, 1);
        assert_eq!(size.total_count, 2);
        assert_eq!(size.returned_count, 1);
        assert_eq!(size.next_offset, 1);
        assert_eq!(size.has_more, 1);
        assert!(size.objects.is_null());
        assert!(size.string_data.is_null());
        assert!(size.buffer.is_null());
        assert!(size.buffer_len > i64::try_from(size_of::<AssetObject>()).unwrap());

        let required = usize::try_from(size.buffer_len).unwrap();
        let mut storage = vec![0_u64; required.div_ceil(size_of::<u64>())];
        let mut into_request = ObjectLookupIntoRequest {
            struct_size: size_i32::<ObjectLookupIntoRequest>(),
            context_id,
            lookup_kind: request.lookup_kind,
            path_id: request.path_id,
            query_utf8: request.query_utf8,
            query_utf8_len: request.query_utf8_len,
            asset_types_csv_utf8: request.asset_types_csv_utf8,
            asset_types_csv_utf8_len: request.asset_types_csv_utf8_len,
            offset: request.offset,
            limit: request.limit,
            flags: request.flags,
            reserved: request.reserved,
            buffer: storage.as_mut_ptr().cast::<u8>(),
            buffer_len: size.buffer_len - 1,
        };
        let mut too_small = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_into_v1(
                    &raw const into_request,
                    &raw mut too_small,
                )
            },
            8
        );
        assert_eq!(too_small.status, 8);
        assert_eq!(too_small.error_code, 8);
        assert_eq!(too_small.buffer_len, size.buffer_len);
        assert_eq!(too_small.total_count, 2);
        assert!(too_small.objects.is_null());
        assert!(too_small.buffer.is_null());

        into_request.buffer_len = size.buffer_len;
        let mut first_page = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_into_v1(
                    &raw const into_request,
                    &raw mut first_page,
                )
            },
            OK
        );
        assert_eq!(first_page.buffer, into_request.buffer);
        assert_eq!(table_paths(&first_page), [10]);

        request.offset = 1;
        let mut second_page = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const request,
                    &raw mut second_page,
                )
            },
            OK
        );
        assert_eq!(second_page.total_count, 2);
        assert_eq!(second_page.offset, 1);
        assert_eq!(second_page.next_offset, -1);
        assert_eq!(second_page.has_more, 0);
        assert_eq!(table_paths(&second_page), [30]);
        haruki_assetstudio_free_buffer(second_page.buffer);

        close_test_context(context_id);
    }

    #[test]
    fn typed_lookup_validates_request_order_and_matches_native_utf8_fallback() {
        let context_id = install_synthetic_lookup_context();

        let empty_name = lookup_test_request(context_id, 2, 0, &[]);
        let mut table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const empty_name,
                    &raw mut table,
                )
            },
            INVALID_REQUEST
        );

        let invalid_utf8 = [0xff_u8];
        let invalid_utf8_query = lookup_test_request(context_id, 2, 0, &invalid_utf8);
        table = ObjectTable::default();
        // Encoding.UTF8.GetString in NativeExports replaces malformed sequences rather than
        // throwing, so this is a valid, non-matching query rather than an invalid request.
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const invalid_utf8_query,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table.total_count, 0);

        let mut null_query = lookup_test_request(context_id, 2, 0, &[]);
        null_query.query_utf8_len = 1;
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const null_query,
                    &raw mut table,
                )
            },
            INVALID_REQUEST
        );

        let query = b"Alpha";
        let invalid_kind = lookup_test_request(context_id, 99, 0, query);
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const invalid_kind,
                    &raw mut table,
                )
            },
            INVALID_REQUEST
        );

        let mut ignored_fields = lookup_test_request(context_id, 2, 0, query);
        ignored_fields.flags = 0x4000;
        ignored_fields.reserved = 73;
        ignored_fields.offset = -99;
        ignored_fields.limit = 1;
        table = ObjectTable::default();
        // NativeExports only consumes flag bit zero and does not reject unknown flags/reserved.
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const ignored_fields,
                    &raw mut table,
                )
            },
            OK
        );
        assert_eq!(table.total_count, 2);
        assert_eq!(table.offset, 0);
        haruki_assetstudio_free_buffer(table.buffer);

        let mut oversized = lookup_test_request(context_id, 2, 0, query);
        oversized.limit = 65_537;
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const oversized,
                    &raw mut table,
                )
            },
            INVALID_REQUEST
        );

        let mut malformed = lookup_test_request(i64::MAX, 99, 0, &[]);
        malformed.struct_size = 0;
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const malformed,
                    &raw mut table,
                )
            },
            INVALID_REQUEST
        );
        malformed.struct_size = size_i32::<ObjectLookupRequest>();
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(
                    &raw const malformed,
                    &raw mut table,
                )
            },
            CONTEXT_NOT_FOUND
        );

        let mut malformed_into = ObjectLookupIntoRequest {
            struct_size: 0,
            context_id: i64::MAX,
            lookup_kind: 99,
            path_id: 0,
            query_utf8: ptr::null(),
            query_utf8_len: 0,
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            offset: 0,
            limit: 0,
            flags: 0,
            reserved: 0,
            buffer: ptr::null_mut(),
            buffer_len: 0,
        };
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_into_v1(
                    &raw const malformed_into,
                    &raw mut table,
                )
            },
            INVALID_REQUEST
        );
        malformed_into.struct_size = size_i32::<ObjectLookupIntoRequest>();
        table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_into_v1(
                    &raw const malformed_into,
                    &raw mut table,
                )
            },
            CONTEXT_NOT_FOUND
        );

        close_test_context(context_id);
    }

    #[test]
    fn production_loader_metadata_drives_container_lookup_through_all_buffer_paths() {
        let bytes = synthetic_container_asset();
        let collection = AssetCollection::load(
            "container.assets",
            assetstudio_core::source::Region::from_bytes(bytes),
        )
        .unwrap();
        let context = Arc::new(build_context(collection, Vec::new(), &[]));
        let context_id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
        contexts().write().unwrap().insert(context_id, context);

        let query = b"Assets/Main";
        let mut request = lookup_test_request(context_id, 3, 0, query);
        request.limit = 16;
        let mut size = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(&raw const request, &raw mut size)
            },
            OK
        );
        assert_eq!(size.total_count, 1);
        assert_eq!(size.returned_count, 1);

        let required = usize::try_from(size.buffer_len).unwrap();
        let mut storage = vec![0_u64; required.div_ceil(size_of::<u64>())];
        let into = ObjectLookupIntoRequest {
            struct_size: size_i32::<ObjectLookupIntoRequest>(),
            context_id,
            lookup_kind: request.lookup_kind,
            path_id: request.path_id,
            query_utf8: request.query_utf8,
            query_utf8_len: request.query_utf8_len,
            asset_types_csv_utf8: request.asset_types_csv_utf8,
            asset_types_csv_utf8_len: request.asset_types_csv_utf8_len,
            offset: request.offset,
            limit: request.limit,
            flags: request.flags,
            reserved: request.reserved,
            buffer: storage.as_mut_ptr().cast(),
            buffer_len: size.buffer_len,
        };
        let mut caller_owned = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_into_v1(
                    &raw const into,
                    &raw mut caller_owned,
                )
            },
            OK
        );
        assert_eq!(table_paths(&caller_owned), [7]);
        let listed = unsafe { *caller_owned.objects };
        assert_eq!(
            table_string(&caller_owned, listed.container_offset, listed.container_len),
            b"Assets/Main"
        );

        let mut owned = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(&raw const request, &raw mut owned)
            },
            OK
        );
        assert_eq!(table_paths(&owned), [7]);
        haruki_assetstudio_free_buffer(owned.buffer);
        close_test_context(context_id);
    }

    #[test]
    fn production_context_open_drives_real_and_empty_names_through_all_lookup_buffers() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("assetstudio-ffi-production-name-lookup-{unique}"));
        fs::create_dir(&directory).unwrap();
        let mut named = Vec::new();
        push_aligned_string(&mut named, "Production Material");
        let mut empty = Vec::new();
        push_aligned_string(&mut empty, "");
        fs::write(
            directory.join("a.assets"),
            synthetic_raw_asset(21, 41, &named),
        )
        .unwrap();
        fs::write(
            directory.join("b.assets"),
            synthetic_raw_asset(21, 42, &empty),
        )
        .unwrap();

        let path = directory.to_string_lossy().into_owned().into_bytes();
        let asset_types = b"Material";
        let open_request = ContextOpenRequest {
            struct_size: size_i32::<ContextOpenRequest>(),
            input_path_utf8: path.as_ptr(),
            input_path_utf8_len: i32::try_from(path.len()).unwrap(),
            unity_version_utf8: ptr::null(),
            unity_version_utf8_len: 0,
            asset_types_csv_utf8: asset_types.as_ptr(),
            asset_types_csv_utf8_len: i32::try_from(asset_types.len()).unwrap(),
            output_dir_utf8: ptr::null(),
            output_dir_utf8_len: 0,
            load_all_assets: 0,
            flags: 0,
            reserved: 0,
        };
        let mut open = ContextOpenResponse::default();
        assert_eq!(
            unsafe { haruki_assetstudio_context_open_v1(&raw const open_request, &raw mut open) },
            OK
        );
        assert_eq!(open.exportable_asset_count, 2);

        let query = b"production material";
        let mut request = lookup_test_request(open.context_id, 2, 0, query);
        request.limit = 1;
        let mut size = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_size_v1(&raw const request, &raw mut size)
            },
            OK
        );
        assert_eq!(size.total_count, 1);
        assert_eq!(size.returned_count, 1);

        let required = usize::try_from(size.buffer_len).unwrap();
        let mut storage = vec![0_u64; required.div_ceil(size_of::<u64>())];
        let into_request = ObjectLookupIntoRequest {
            struct_size: size_i32::<ObjectLookupIntoRequest>(),
            context_id: open.context_id,
            lookup_kind: request.lookup_kind,
            path_id: request.path_id,
            query_utf8: request.query_utf8,
            query_utf8_len: request.query_utf8_len,
            asset_types_csv_utf8: request.asset_types_csv_utf8,
            asset_types_csv_utf8_len: request.asset_types_csv_utf8_len,
            offset: request.offset,
            limit: request.limit,
            flags: request.flags,
            reserved: request.reserved,
            buffer: storage.as_mut_ptr().cast(),
            buffer_len: size.buffer_len,
        };
        let mut caller_owned = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_into_v1(
                    &raw const into_request,
                    &raw mut caller_owned,
                )
            },
            OK
        );
        assert_eq!(table_paths(&caller_owned), [41]);
        let caller_item = unsafe { *caller_owned.objects };
        assert_eq!(
            table_string(&caller_owned, caller_item.name_offset, caller_item.name_len),
            b"Production Material"
        );

        let mut owned = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(&raw const request, &raw mut owned)
            },
            OK
        );
        assert_eq!(table_paths(&owned), [41]);
        haruki_assetstudio_free_buffer(owned.buffer);

        // The C# Core engine uses TypeString + "_#" + its deterministic object ordinal when
        // a supported object has an empty display name.
        let fallback = b"material_#1";
        let fallback_request = lookup_test_request(open.context_id, 2, 0, fallback);
        let mut fallback_table = ObjectTable::default();
        assert_eq!(
            unsafe {
                haruki_assetstudio_context_lookup_objects_v1(
                    &raw const fallback_request,
                    &raw mut fallback_table,
                )
            },
            OK
        );
        assert_eq!(table_paths(&fallback_table), [42]);
        haruki_assetstudio_free_buffer(fallback_table.buffer);

        haruki_assetstudio_free_buffer(open.buffer);
        close_test_context(open.context_id);
        fs::remove_dir_all(directory).unwrap();
    }

    fn install_synthetic_lookup_context() -> i64 {
        let objects = vec![
            lookup_object(0, 10, 21, "Alpha", "assets/hero"),
            lookup_object(1, 10, 49, "Duplicate", "shadow"),
            lookup_object(2, 20, 1, "alpha beta", "Assets/Hero/Sub"),
            lookup_object(3, 30, 21, "ALPHA", "other"),
        ];
        let mut path_id_index = HashMap::new();
        for (index, object) in objects.iter().enumerate() {
            path_id_index.entry(object.path_id).or_insert(index);
        }
        let context_id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
        contexts().write().unwrap().insert(
            context_id,
            Arc::new(ContextState {
                collection: AssetCollection::default(),
                objects,
                path_id_index,
                requested_asset_types: Vec::new(),
                lifetime: AtomicUsize::new(0),
            }),
        );
        context_id
    }

    fn lookup_object(
        index: usize,
        path_id: i64,
        class_id: i32,
        name: &str,
        container: &str,
    ) -> ObjectRef {
        ObjectRef {
            file_index: 0,
            object_index: index,
            texture_array_layer: None,
            path_id,
            class_id,
            size: 16,
            name: name.to_owned(),
            container: container.to_owned(),
            type_name: class_name(class_id),
            unique_id: format!("_#{index}"),
            source_file: "synthetic.assets".to_owned(),
        }
    }

    fn lookup_test_request(
        context_id: i64,
        lookup_kind: i32,
        path_id: i64,
        query: &[u8],
    ) -> ObjectLookupRequest {
        ObjectLookupRequest {
            struct_size: size_i32::<ObjectLookupRequest>(),
            context_id,
            lookup_kind,
            path_id,
            query_utf8: if query.is_empty() {
                ptr::null()
            } else {
                query.as_ptr()
            },
            query_utf8_len: i32::try_from(query.len()).unwrap(),
            asset_types_csv_utf8: ptr::null(),
            asset_types_csv_utf8_len: 0,
            offset: 0,
            limit: 0,
            flags: 0,
            reserved: 0,
        }
    }

    fn table_paths(table: &ObjectTable) -> Vec<i64> {
        if table.returned_count == 0 {
            return Vec::new();
        }
        let objects = unsafe {
            slice::from_raw_parts(
                table.objects,
                usize::try_from(table.returned_count).unwrap(),
            )
        };
        objects.iter().map(|object| object.path_id).collect()
    }

    fn table_string(table: &ObjectTable, offset: i32, length: i32) -> Vec<u8> {
        if length == 0 {
            return Vec::new();
        }
        let offset = usize::try_from(offset).unwrap();
        let length = usize::try_from(length).unwrap();
        let pool = unsafe {
            slice::from_raw_parts(
                table.string_data,
                usize::try_from(table.string_data_len).unwrap(),
            )
        };
        pool[offset..offset + length].to_vec()
    }

    fn close_test_context(context_id: i64) {
        let request = ContextCloseRequest {
            struct_size: size_i32::<ContextCloseRequest>(),
            context_id,
            flags: 0,
            reserved: 0,
        };
        let mut response = ContextCloseResponse::default();
        assert_eq!(
            unsafe { haruki_assetstudio_context_close_v1(&raw const request, &raw mut response) },
            OK
        );
    }

    fn synthetic_text_asset() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "ffi text");
        push_i32(&mut object, 11);
        object.extend_from_slice(b"ffi payload");

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, 49);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 1);
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, 0);
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(&object);
        bytes
    }

    fn synthetic_raw_asset(class_id: i32, path_id: i64, object: &[u8]) -> Vec<u8> {
        synthetic_raw_asset_for_version("2022.3.62f1", class_id, path_id, object)
    }

    fn synthetic_raw_asset_for_version(
        unity_version: &str,
        class_id: i32,
        path_id: i64,
        object: &[u8],
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, class_id);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 1);
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&path_id.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, 0);
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn synthetic_container_asset() -> Vec<u8> {
        let mut bundle = Vec::new();
        push_aligned_string(&mut bundle, "bundle");
        push_i32(&mut bundle, 1);
        push_i32(&mut bundle, 0);
        bundle.extend_from_slice(&7_i64.to_le_bytes());
        push_i32(&mut bundle, 1);
        push_aligned_string(&mut bundle, "Assets/Main");
        push_i32(&mut bundle, 0);
        push_i32(&mut bundle, 1);
        push_i32(&mut bundle, 0);
        bundle.extend_from_slice(&7_i64.to_le_bytes());
        push_i32(&mut bundle, 0);
        push_i32(&mut bundle, 0);
        push_i32(&mut bundle, 0);
        bundle.extend_from_slice(&0_i64.to_le_bytes());
        bundle.extend_from_slice(&0_u32.to_le_bytes());
        push_aligned_string(&mut bundle, "bundle-name");
        push_i32(&mut bundle, 0);
        bundle.push(0);

        let mut material = Vec::new();
        push_aligned_string(&mut material, "container material");
        synthetic_multi_raw_asset(&[(142, 100, bundle), (21, 7, material)])
    }

    fn synthetic_multi_raw_asset(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
        let mut class_ids = objects
            .iter()
            .map(|(class_id, _, _)| *class_id)
            .collect::<Vec<_>>();
        class_ids.sort_unstable();
        class_ids.dedup();
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, i32::try_from(class_ids.len()).unwrap());
        for class_id in &class_ids {
            push_i32(&mut metadata, *class_id);
            metadata.push(0);
            metadata.extend_from_slice(&(-1_i16).to_le_bytes());
            metadata.extend_from_slice(&[0_u8; 16]);
        }
        push_i32(&mut metadata, i32::try_from(objects.len()).unwrap());
        let mut offset = 0_u64;
        for (class_id, path_id, object) in objects {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&i64::try_from(offset).unwrap().to_le_bytes());
            metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
            let type_index = class_ids
                .iter()
                .position(|candidate| candidate == class_id)
                .unwrap();
            push_i32(&mut metadata, i32::try_from(type_index).unwrap());
            offset += u64::try_from(object.len()).unwrap();
        }
        for _ in 0..4 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + offset;
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        for (_, _, object) in objects {
            bytes.extend_from_slice(object);
        }
        bytes
    }

    fn resident_mesh_object() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "tri");
        push_i32(&mut object, 1); // submeshes
        push_i32(&mut object, 0); // first byte
        push_i32(&mut object, 3); // index count
        push_i32(&mut object, 0); // triangle-list topology
        push_i32(&mut object, 0); // base vertex
        push_i32(&mut object, 0); // first vertex
        push_i32(&mut object, 3); // vertex count
        object.extend_from_slice(&[0_u8; 24]); // local AABB

        for _ in 0..3 {
            push_i32(&mut object, 0);
        }
        push_i32(&mut object, 0); // blend-shape full weights
        push_i32(&mut object, 0); // bind poses
        push_i32(&mut object, 0); // bone name hashes
        push_i32(&mut object, 0); // root bone hash
        push_i32(&mut object, 0); // bone AABBs
        push_i32(&mut object, 0); // variable bone weights
        push_i32(&mut object, 0); // mesh compression + flags
        push_i32(&mut object, 0); // 16-bit indices
        push_i32(&mut object, 6); // index buffer bytes
        object.extend_from_slice(&0_u16.to_le_bytes());
        object.extend_from_slice(&1_u16.to_le_bytes());
        object.extend_from_slice(&2_u16.to_le_bytes());
        align_with_base(&mut object, 0, 4);

        push_i32(&mut object, 3); // VertexData vertex count
        push_i32(&mut object, 1); // one position channel
        object.extend_from_slice(&[0, 0, 0, 3]);
        let positions = [[1.0_f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        push_i32(&mut object, 36);
        for position in positions {
            for component in position {
                object.extend_from_slice(&component.to_le_bytes());
            }
        }
        align_with_base(&mut object, 0, 4);

        for _ in 0..4 {
            push_empty_packed_float(&mut object);
        }
        for _ in 0..3 {
            push_empty_packed_int(&mut object);
        }
        push_empty_packed_float(&mut object);
        for _ in 0..2 {
            push_empty_packed_int(&mut object);
        }
        push_i32(&mut object, 0); // compressed UV info
        object.extend_from_slice(&[0_u8; 24]); // local AABB
        push_i32(&mut object, 0); // usage flags
        push_i32(&mut object, 0); // 2022 cooking options
        push_i32(&mut object, 0); // convex collision data
        push_i32(&mut object, 0); // triangle collision data
        object.extend_from_slice(&[0_u8; 8]); // mesh metrics
        object.extend_from_slice(&0_i64.to_le_bytes()); // stream offset
        push_i32(&mut object, 0); // stream size
        push_aligned_string(&mut object, "");
        object
    }

    fn resident_texture_array_object(source: &[u8]) -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "array");
        push_i32(&mut object, 0); // forced fallback format
        object.extend_from_slice(&[0, 0]); // downscale fallback and alpha optional
        align_with_base(&mut object, 0, 4);
        push_i32(&mut object, 0); // color space
        push_i32(&mut object, 4); // R8G8B8A8_UNorm GraphicsFormat
        push_i32(&mut object, 1); // width
        push_i32(&mut object, 2); // height
        push_i32(&mut object, 2); // depth
        push_i32(&mut object, 1); // mip count
        object.extend_from_slice(&u32::try_from(source.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&[0_u8; 24]); // GL texture settings
        push_i32(&mut object, 7); // usage mode
        object.push(1); // readable
        align_with_base(&mut object, 0, 4);
        push_i32(&mut object, i32::try_from(source.len()).unwrap());
        object.extend_from_slice(source);
        object
    }

    fn assert_texture_array_layer(payload: &[u8], expected_pixels: &[u8]) {
        assert_eq!(&payload[..16], b"HARUKI_RGBAIR_V1");
        let mut header = 16;
        assert_eq!(take_i32(payload, &mut header), 1);
        assert_eq!(take_i32(payload, &mut header), 2);
        assert_eq!(take_i32(payload, &mut header), 4);
        assert_eq!(take_i32(payload, &mut header), 1);
        assert_eq!(take_i32(payload, &mut header), 0);
        assert_eq!(&payload[header..], expected_pixels);
    }

    fn assert_texture_array_bundle(payload: &[u8]) {
        const BUNDLE_MAGIC: &[u8; 30] = b"HARUKI_ASSET_PAYLOAD_BUNDLE_V1";
        assert_eq!(&payload[..BUNDLE_MAGIC.len()], BUNDLE_MAGIC);
        let mut cursor = BUNDLE_MAGIC.len();
        assert_eq!(take_i32(payload, &mut cursor), 2);
        let mut entries = Vec::new();
        for expected_name in ["layer_0000.rgba", "layer_0001.rgba"] {
            let name_len = usize::try_from(take_i32(payload, &mut cursor)).unwrap();
            let payload_len = usize::try_from(take_i64(payload, &mut cursor)).unwrap();
            let name_end = cursor.checked_add(name_len).unwrap();
            assert_eq!(&payload[cursor..name_end], expected_name.as_bytes());
            cursor = name_end;
            entries.push(payload_len);
        }
        assert_eq!(entries, [44, 44]);

        for (length, expected_pixels) in entries.into_iter().zip([
            &[5, 6, 7, 8, 1, 2, 3, 4][..],
            &[15, 16, 17, 18, 11, 12, 13, 14][..],
        ]) {
            let end = cursor.checked_add(length).unwrap();
            let entry = &payload[cursor..end];
            assert_eq!(&entry[..16], b"HARUKI_RGBAIR_V1");
            let mut header = 16;
            assert_eq!(take_i32(entry, &mut header), 1);
            assert_eq!(take_i32(entry, &mut header), 2);
            assert_eq!(take_i32(entry, &mut header), 4);
            assert_eq!(take_i32(entry, &mut header), 1);
            assert_eq!(take_i32(entry, &mut header), 0);
            assert_eq!(&entry[header..], expected_pixels);
            cursor = end;
        }
        assert_eq!(cursor, payload.len());
    }

    fn take_i32(input: &[u8], cursor: &mut usize) -> i32 {
        let end = cursor.checked_add(4).unwrap();
        let value = i32::from_le_bytes(input[*cursor..end].try_into().unwrap());
        *cursor = end;
        value
    }

    fn take_i64(input: &[u8], cursor: &mut usize) -> i64 {
        let end = cursor.checked_add(8).unwrap();
        let value = i64::from_le_bytes(input[*cursor..end].try_into().unwrap());
        *cursor = end;
        value
    }

    fn push_empty_packed_float(output: &mut Vec<u8>) {
        push_i32(output, 0); // item count
        output.extend_from_slice(&0_f32.to_le_bytes());
        output.extend_from_slice(&0_f32.to_le_bytes());
        push_i32(output, 0); // data bytes
        align_with_base(output, 0, 4);
        output.push(0); // bit size
        align_with_base(output, 0, 4);
    }

    fn push_empty_packed_int(output: &mut Vec<u8>) {
        push_i32(output, 0); // item count
        push_i32(output, 0); // data bytes
        align_with_base(output, 0, 4);
        output.push(0); // bit size
        align_with_base(output, 0, 4);
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        while !output.len().is_multiple_of(4) {
            output.push(0);
        }
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
