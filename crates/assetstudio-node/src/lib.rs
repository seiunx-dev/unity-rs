//! Thin Node-API bindings over the safe high-level Rust API.

use std::sync::Arc;

use assetstudio_core::mesh::MeshReadLimits;
use assetstudio_core::source::Region;
use assetstudio_core::sprite::SpriteReadLimits;
use assetstudio_core::studio::{Studio, StudioObject};
use assetstudio_core::texture::TextureReadLimits;
use assetstudio_core::texture_array::TextureArrayReadLimits;
use napi::bindgen_prelude::{AsyncTask, BigInt, Buffer};
use napi::{Env, Error, Result, Status, Task};
use napi_derive::napi;

const DEFAULT_PAYLOAD_LIMIT: u64 = 512 * 1024 * 1024;
const DEFAULT_PAGE_LIMIT: u32 = 4096;
const MAXIMUM_PAGE_LIMIT: u32 = 1_000_000;

#[napi(object)]
pub struct FileInfo {
    pub index: u32,
    pub path: String,
    pub unity_version: String,
    pub object_count: u32,
}

#[napi(object)]
pub struct ObjectInfo {
    pub file_index: u32,
    pub object_index: u32,
    pub source_path: String,
    pub path_id: BigInt,
    pub class_id: i32,
    pub byte_size: BigInt,
    pub name: Option<String>,
    pub container: Option<String>,
}

#[napi(object)]
pub struct ResourceInfo {
    pub index: u32,
    pub path: String,
    pub byte_size: BigInt,
}

#[napi(object)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Buffer,
}

/// One opened collection. All format work is delegated to `assetstudio-core`.
#[napi]
pub struct AssetStudio {
    studio: Arc<Studio>,
}

#[napi]
impl AssetStudio {
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        Studio::open(path)
            .map(|studio| Self {
                studio: Arc::new(studio),
            })
            .map_err(core_error)
    }

    /// Opens a path on a libuv worker so container discovery does not block
    /// the JavaScript event loop.
    #[must_use]
    #[napi(ts_return_type = "Promise<AssetStudio>")]
    pub fn open_async(path: String) -> AsyncTask<OpenPathTask> {
        AsyncTask::new(OpenPathTask { path })
    }

    /// Opens one in-memory asset, bundle, or resource after copying the Node
    /// buffer into Rust-owned immutable storage.
    #[napi(factory)]
    pub fn from_buffer(
        data: &[u8],
        name: Option<String>,
        maximum_bytes: Option<i64>,
    ) -> Result<Self> {
        let maximum = byte_limit(maximum_bytes)?;
        let actual =
            u64::try_from(data.len()).map_err(|_| invalid_arg("buffer length does not fit u64"))?;
        if actual > maximum {
            return Err(invalid_arg(format!(
                "input buffer is {actual} bytes, exceeding limit {maximum}"
            )));
        }
        Studio::open_region(
            name.unwrap_or_else(|| "memory.assets".to_owned()),
            Region::from_bytes(data.to_vec()),
        )
        .map(|studio| Self {
            studio: Arc::new(studio),
        })
        .map_err(core_error)
    }

    /// Copies a Node buffer once, then parses it on a libuv worker.
    #[napi(ts_return_type = "Promise<AssetStudio>")]
    pub fn from_buffer_async(
        data: &[u8],
        name: Option<String>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<OpenBufferTask>> {
        let maximum = byte_limit(maximum_bytes)?;
        let actual =
            u64::try_from(data.len()).map_err(|_| invalid_arg("buffer length does not fit u64"))?;
        if actual > maximum {
            return Err(invalid_arg(format!(
                "input buffer is {actual} bytes, exceeding limit {maximum}"
            )));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(data.len()).map_err(|error| {
            Error::from_reason(format!("cannot allocate input buffer: {error}"))
        })?;
        bytes.extend_from_slice(data);
        Ok(AsyncTask::new(OpenBufferTask {
            bytes,
            name: name.unwrap_or_else(|| "memory.assets".to_owned()),
        }))
    }

    #[napi(getter)]
    pub fn file_count(&self) -> Result<u32> {
        count_u32(self.studio.file_count(), "file count")
    }

    #[napi(getter)]
    pub fn object_count(&self) -> Result<u32> {
        count_u32(self.studio.object_count(), "object count")
    }

    #[napi(getter)]
    pub fn resource_count(&self) -> Result<u32> {
        count_u32(self.studio.resource_count(), "resource count")
    }

    #[napi]
    pub fn file_page(&self, offset: Option<u32>, limit: Option<u32>) -> Result<Vec<FileInfo>> {
        let (offset, limit) = page(offset, limit)?;
        let available = self.studio.file_count().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "file metadata page")?;
        for file in self.studio.files().skip(offset).take(count) {
            output.push(FileInfo {
                index: count_u32(file.index(), "file index")?,
                path: copy_string(file.path(), "file path")?,
                unity_version: copy_string(file.unity_version(), "Unity version")?,
                object_count: count_u32(file.object_count(), "file object count")?,
            });
        }
        Ok(output)
    }

    #[napi]
    pub fn object_page(
        &self,
        file_index: u32,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<ObjectInfo>> {
        let file_index = usize::try_from(file_index).expect("u32 fits usize");
        let file = self
            .studio
            .file(file_index)
            .ok_or_else(|| invalid_arg(format!("file index {file_index} was not found")))?;
        let (offset, limit) = page(offset, limit)?;
        let available = file.object_count().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "object metadata page")?;
        for object_index in offset..offset + count {
            let object = self
                .studio
                .object_by_index(file_index, object_index)
                .ok_or_else(|| Error::from_reason("validated object index vanished"))?;
            output.push(object_info(object)?);
        }
        Ok(output)
    }

    #[napi]
    pub fn resource_page(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<ResourceInfo>> {
        let (offset, limit) = page(offset, limit)?;
        let available = self.studio.resource_count().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "resource metadata page")?;
        for resource in self.studio.resources().skip(offset).take(count) {
            output.push(ResourceInfo {
                index: count_u32(resource.index(), "resource index")?,
                path: copy_string(resource.path(), "resource path")?,
                byte_size: resource.byte_size().into(),
            });
        }
        Ok(output)
    }

    #[napi]
    pub fn read_resource(&self, resource_index: u32, maximum_bytes: Option<i64>) -> Result<Buffer> {
        let index = usize::try_from(resource_index).expect("u32 fits usize");
        self.studio
            .resource(index)
            .ok_or_else(|| invalid_arg(format!("resource index {index} was not found")))?
            .read(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi]
    pub fn read_raw(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_raw(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_raw_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::Raw)
    }

    #[napi]
    pub fn read_text(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        let maximum = usize_limit(maximum_bytes)?;
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_text_bytes(maximum)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_text_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::Text)
    }

    #[napi]
    pub fn read_type_tree_json(
        &self,
        file_index: u32,
        path_id: BigInt,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_type_tree_json(pretty.unwrap_or(false), usize_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_type_tree_json_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(
            file_index,
            path_id,
            maximum_bytes,
            ByteReadKind::TypeTreeJson {
                pretty: pretty.unwrap_or(false),
            },
        )
    }

    #[napi]
    pub fn read_type_tree_dump(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_type_tree_dump(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_type_tree_dump_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(
            file_index,
            path_id,
            maximum_bytes,
            ByteReadKind::TypeTreeDump,
        )
    }

    #[napi]
    pub fn read_shader(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_shader_text(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_shader_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::Shader)
    }

    #[napi]
    pub fn read_mesh_obj(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        let maximum = byte_limit(maximum_bytes)?;
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_mesh_obj(MeshReadLimits {
                maximum_object_bytes: maximum,
                maximum_output_bytes: maximum,
                ..MeshReadLimits::default()
            })
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_mesh_obj_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::MeshObj)
    }

    #[napi]
    pub fn read_texture(
        &self,
        file_index: u32,
        path_id: BigInt,
        mip_level: Option<u32>,
        maximum_bytes: Option<i64>,
    ) -> Result<RgbaImage> {
        let maximum = byte_limit(maximum_bytes)?;
        let image = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .decode_texture_mip(
                mip_level.unwrap_or(0),
                TextureReadLimits {
                    maximum_payload_bytes: maximum,
                    maximum_output_bytes: maximum,
                    maximum_decoder_working_bytes: maximum,
                    ..TextureReadLimits::default()
                },
            )
            .map_err(core_error)?;
        Ok(RgbaImage {
            width: image.width,
            height: image.height,
            pixels: image.pixels.into(),
        })
    }

    #[napi(ts_return_type = "Promise<RgbaImage>")]
    pub fn read_texture_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        mip_level: Option<u32>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadTextureTask>> {
        Ok(AsyncTask::new(ReadTextureTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            mip_level: mip_level.unwrap_or(0),
            maximum: byte_limit(maximum_bytes)?,
        }))
    }

    #[napi]
    pub fn read_texture_array(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Vec<RgbaImage>> {
        let maximum = byte_limit(maximum_bytes)?;
        let images = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .decode_texture_array_mip0(texture_array_limits(maximum))
            .map_err(core_error)?;
        convert_images(images)
    }

    #[napi(ts_return_type = "Promise<Array<RgbaImage>>")]
    pub fn read_texture_array_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadTextureArrayTask>> {
        Ok(AsyncTask::new(ReadTextureArrayTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            maximum: byte_limit(maximum_bytes)?,
        }))
    }

    #[napi]
    pub fn read_sprite(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<RgbaImage> {
        let maximum = byte_limit(maximum_bytes)?;
        let image = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .decode_sprite(sprite_limits(maximum), texture_limits(maximum))
            .map_err(core_error)?;
        Ok(convert_image(image))
    }

    #[napi(ts_return_type = "Promise<RgbaImage>")]
    pub fn read_sprite_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadSpriteTask>> {
        Ok(AsyncTask::new(ReadSpriteTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            maximum: byte_limit(maximum_bytes)?,
        }))
    }
}

impl AssetStudio {
    fn object(&self, file_index: u32, path_id: i64) -> Result<StudioObject<'_>> {
        let file_index = usize::try_from(file_index).expect("u32 fits usize");
        studio_object(&self.studio, file_index, path_id)
    }

    fn read_bytes_task(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
        kind: ByteReadKind,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        Ok(AsyncTask::new(ReadBytesTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            maximum: byte_limit(maximum_bytes)?,
            kind,
        }))
    }
}

pub struct OpenPathTask {
    path: String,
}

impl Task for OpenPathTask {
    type Output = Studio;
    type JsValue = AssetStudio;

    fn compute(&mut self) -> Result<Self::Output> {
        Studio::open(&self.path).map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(AssetStudio {
            studio: Arc::new(studio),
        })
    }
}

pub struct OpenBufferTask {
    bytes: Vec<u8>,
    name: String,
}

impl Task for OpenBufferTask {
    type Output = Studio;
    type JsValue = AssetStudio;

    fn compute(&mut self) -> Result<Self::Output> {
        let bytes = std::mem::take(&mut self.bytes);
        Studio::open_region(self.name.clone(), Region::from_bytes(bytes)).map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(AssetStudio {
            studio: Arc::new(studio),
        })
    }
}

#[derive(Clone, Copy)]
enum ByteReadKind {
    Raw,
    Text,
    TypeTreeJson { pretty: bool },
    TypeTreeDump,
    Shader,
    MeshObj,
}

pub struct ReadBytesTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
    kind: ByteReadKind,
}

impl Task for ReadBytesTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        let object = studio_object(&self.studio, self.file_index, self.path_id)?;
        match self.kind {
            ByteReadKind::Raw => object.read_raw(self.maximum),
            ByteReadKind::Text => object.read_text_bytes(
                usize::try_from(self.maximum)
                    .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?,
            ),
            ByteReadKind::TypeTreeJson { pretty } => object.read_type_tree_json(
                pretty,
                usize::try_from(self.maximum)
                    .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?,
            ),
            ByteReadKind::TypeTreeDump => object.read_type_tree_dump(self.maximum),
            ByteReadKind::Shader => object.read_shader_text(self.maximum),
            ByteReadKind::MeshObj => object.read_mesh_obj(MeshReadLimits {
                maximum_object_bytes: self.maximum,
                maximum_output_bytes: self.maximum,
                ..MeshReadLimits::default()
            }),
        }
        .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

pub struct ReadTextureTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    mip_level: u32,
    maximum: u64,
}

impl Task for ReadTextureTask {
    type Output = assetstudio_core::texture::RgbaImage;
    type JsValue = RgbaImage;

    fn compute(&mut self) -> Result<Self::Output> {
        studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_texture_mip(
                self.mip_level,
                TextureReadLimits {
                    maximum_payload_bytes: self.maximum,
                    maximum_output_bytes: self.maximum,
                    maximum_decoder_working_bytes: self.maximum,
                    ..TextureReadLimits::default()
                },
            )
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, image: Self::Output) -> Result<Self::JsValue> {
        Ok(convert_image(image))
    }
}

pub struct ReadTextureArrayTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
}

impl Task for ReadTextureArrayTask {
    type Output = Vec<assetstudio_core::texture::RgbaImage>;
    type JsValue = Vec<RgbaImage>;

    fn compute(&mut self) -> Result<Self::Output> {
        studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_texture_array_mip0(texture_array_limits(self.maximum))
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, images: Self::Output) -> Result<Self::JsValue> {
        convert_images(images)
    }
}

pub struct ReadSpriteTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
}

impl Task for ReadSpriteTask {
    type Output = assetstudio_core::texture::RgbaImage;
    type JsValue = RgbaImage;

    fn compute(&mut self) -> Result<Self::Output> {
        studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_sprite(sprite_limits(self.maximum), texture_limits(self.maximum))
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, image: Self::Output) -> Result<Self::JsValue> {
        Ok(convert_image(image))
    }
}

fn studio_object(studio: &Studio, file_index: usize, path_id: i64) -> Result<StudioObject<'_>> {
    studio.object(file_index, path_id).ok_or_else(|| {
        invalid_arg(format!(
            "object path ID {path_id} was not found in file index {file_index}"
        ))
    })
}

fn texture_limits(maximum: u64) -> TextureReadLimits {
    TextureReadLimits {
        maximum_payload_bytes: maximum,
        maximum_output_bytes: maximum,
        maximum_decoder_working_bytes: maximum,
        ..TextureReadLimits::default()
    }
}

fn texture_array_limits(maximum: u64) -> TextureArrayReadLimits {
    TextureArrayReadLimits {
        maximum_payload_bytes: maximum,
        maximum_output_bytes: maximum,
        maximum_decoder_working_bytes: maximum,
        maximum_bundle_bytes: maximum,
        ..TextureArrayReadLimits::default()
    }
}

fn sprite_limits(maximum: u64) -> SpriteReadLimits {
    SpriteReadLimits {
        maximum_mesh_bytes: maximum,
        maximum_output_pixels: maximum / 4,
        maximum_output_bytes: maximum,
        maximum_working_bytes: maximum,
        maximum_raster_operations: maximum,
        ..SpriteReadLimits::default()
    }
}

fn convert_image(image: assetstudio_core::texture::RgbaImage) -> RgbaImage {
    RgbaImage {
        width: image.width,
        height: image.height,
        pixels: image.pixels.into(),
    }
}

fn convert_images(images: Vec<assetstudio_core::texture::RgbaImage>) -> Result<Vec<RgbaImage>> {
    let mut output = reserve(images.len(), "Texture2DArray images")?;
    output.extend(images.into_iter().map(convert_image));
    Ok(output)
}

fn object_info(object: StudioObject<'_>) -> Result<ObjectInfo> {
    Ok(ObjectInfo {
        file_index: count_u32(object.file_index(), "object file index")?,
        object_index: count_u32(object.object_index(), "object index")?,
        source_path: copy_string(object.source_path(), "object source path")?,
        path_id: object.path_id().into(),
        class_id: object.class_id(),
        byte_size: object.byte_size().into(),
        name: object
            .name()
            .map(|value| copy_string(value, "object name"))
            .transpose()?,
        container: object
            .container()
            .map(|value| copy_string(value, "object container"))
            .transpose()?,
    })
}

fn page(offset: Option<u32>, limit: Option<u32>) -> Result<(usize, usize)> {
    let offset = usize::try_from(offset.unwrap_or(0)).expect("u32 fits usize");
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if limit > MAXIMUM_PAGE_LIMIT {
        return Err(invalid_arg(format!(
            "page limit {limit} exceeds {MAXIMUM_PAGE_LIMIT}"
        )));
    }
    Ok((offset, usize::try_from(limit).expect("u32 fits usize")))
}

fn byte_limit(value: Option<i64>) -> Result<u64> {
    match value {
        None => Ok(DEFAULT_PAYLOAD_LIMIT),
        Some(value) if value >= 0 => u64::try_from(value)
            .map_err(|_| invalid_arg("maximumBytes does not fit an unsigned 64-bit integer")),
        Some(value) => Err(invalid_arg(format!(
            "maximumBytes must be non-negative, received {value}"
        ))),
    }
}

fn usize_limit(value: Option<i64>) -> Result<usize> {
    usize::try_from(byte_limit(value)?)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))
}

fn count_u32(value: usize, field: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::from_reason(format!("{field} does not fit u32")))
}

fn bigint_i64(value: BigInt, field: &str) -> Result<i64> {
    let BigInt { sign_bit, words } = value;
    if words.len() != 1 {
        return Err(invalid_arg(format!("{field} does not fit signed 64 bits")));
    }
    let magnitude = words[0];
    if sign_bit {
        if magnitude == i64::MIN.unsigned_abs() {
            Ok(i64::MIN)
        } else {
            i64::try_from(magnitude)
                .map(|number| -number)
                .map_err(|_| invalid_arg(format!("{field} does not fit signed 64 bits")))
        }
    } else {
        i64::try_from(magnitude)
            .map_err(|_| invalid_arg(format!("{field} does not fit signed 64 bits")))
    }
}

fn reserve<T>(count: usize, field: &str) -> Result<Vec<T>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|error| Error::from_reason(format!("cannot allocate {field}: {error}")))?;
    Ok(output)
}

fn copy_string(value: &str, field: &str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|error| Error::from_reason(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
    Ok(output)
}

fn invalid_arg(message: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, message.into())
}

fn core_error(error: assetstudio_core::Error) -> Error {
    match error {
        assetstudio_core::Error::Unsupported(message) => {
            Error::new(Status::GenericFailure, message)
        }
        other => Error::new(Status::InvalidArg, other.to_string()),
    }
}
