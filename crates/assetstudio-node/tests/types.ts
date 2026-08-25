import { AssetStudio } from "../index.js";
import type {
  AclDecodeRequest,
  AclDecodedClip,
  AnimationClipInfo,
  AnimatorOverrideControllerInfo,
  AssetBundleInfo,
  AudioClip,
  Avatar,
  AvatarPathEntry,
  ExportConfiguration,
  LegacyAnimationInfo,
  Live2DPackageSet,
  ModelTextureLimits,
  MonoBehaviourJson,
  MonoBehaviourSchema,
  OpenOptions,
  PreloadDataInfo,
  ResourceManagerInfo,
  SceneLimits,
  SceneNode,
  SpriteAtlasInfo,
  SpriteAtlasRenderData,
  SpriteAtlasRenderDataKey,
  SpriteAtlasSecondaryTexture,
  SpriteMetadata,
  SpriteMetadataLimits,
  SpriteRenderData,
  SpriteSecondaryTexture,
  SpriteSettings,
  SpriteTriangle,
} from "../index.js";

declare const studio: AssetStudio;

const openOptions: OpenOptions = {
  maximumPathBytes: 1024 * 1024,
  maximumTotalPathBytes: 64 * 1024 * 1024,
};
const openedWithPathLimits: AssetStudio = AssetStudio.openWith(
  "fixture.assets",
  openOptions,
);

function decodeAcl(request: AclDecodeRequest): AclDecodedClip {
  void request.compressedTracks;
  void request.decoderMap;
  return {
    times: [],
    bindingIndices: [],
    values: [],
    followingCurveOffset: 0,
  };
}

const fbx: Promise<Buffer> = studio.readFbxWithAclDecoder(decodeAcl, 1024);
const binaryFbx: Promise<Buffer> = studio.readFbxBinaryWithAclDecoder(
  decodeAcl,
  1024,
);
const selectedFbx: Promise<Buffer> = studio.readGameObjectFbxWithAclDecoder(
  0,
  1n,
  decodeAcl,
  true,
  1024,
);
const motion = studio.readCubismClipMotionWithAclDecoder(
  0,
  1n,
  decodeAcl,
  { parameters: ["ParamAngleX"], parts: ["PartArmL"] },
  false,
  1024,
);
const opened: Promise<AssetStudio> = AssetStudio.openWithOodle(
  "fixture.bundle",
  (input, expectedLength) => {
    void input;
    return Buffer.alloc(expectedLength);
  },
);
const sceneLimits: SceneLimits = {
  maximumGameObjects: 100_000,
  maximumTotalComponents: 1_000_000,
  maximumIndexBytes: 64 * 1024 * 1024,
};
const scene: SceneNode[] = studio.sceneWithLimits(sceneLimits);
const textureLimits: ModelTextureLimits = {
  maximumTextureReferences: 1_024,
  maximumTextures: 128,
  maximumNameIndexBytes: 8 * 1024 * 1024,
  maximumMetadataBytes: 32 * 1024 * 1024,
  maximumTotalEncodedBytes: 256 * 1024 * 1024,
  maximumSingleTextureBytes: 64 * 1024 * 1024,
};
const modelObj = studio.readModelObj(
  "model.mtl",
  1024,
  "raw-rgba",
  textureLimits,
);
const texturedFbx = studio.readFbxWithTextures(1024, "tga", textureLimits);
const legacyAnimation: LegacyAnimationInfo = studio.readLegacyAnimation(0, 1n);
const overrideController: AnimatorOverrideControllerInfo =
  studio.readAnimatorOverrideController(0, 1n);
const assetBundle: AssetBundleInfo = studio.readAssetBundle(0, 1n);
const resourceManager: ResourceManagerInfo = studio.readResourceManager(0, 1n);
const preloadData: PreloadDataInfo = studio.readPreloadData(0, 1n);
const spriteAtlas: SpriteAtlasInfo = studio.readSpriteAtlas(0, 1n);
const atlasEntry: SpriteAtlasRenderData = spriteAtlas.renderDataEntries[0];
const atlasKey: SpriteAtlasRenderDataKey = atlasEntry.key;
const atlasSecondary: SpriteAtlasSecondaryTexture[] | undefined =
  atlasEntry.secondaryTextures;
const spriteLimits: SpriteMetadataLimits = {
  maximumEntries: 1_000,
  maximumStringBytes: 1_024 * 1_024,
  maximumTotalStringBytes: 2 * 1_024 * 1_024,
  maximumMeshBytes: 16 * 1_024 * 1_024,
};
const spriteMetadata: SpriteMetadata = studio.readSpriteMetadata(
  0,
  1n,
  spriteLimits,
);
const spriteRenderData: SpriteRenderData = spriteMetadata.renderData;
const spriteSettings: SpriteSettings = spriteRenderData.settings;
const spriteSecondary: SpriteSecondaryTexture[] =
  spriteRenderData.secondaryTextures;
const spriteTriangles: SpriteTriangle[] = spriteRenderData.meshTriangles;
const animationClip: AnimationClipInfo = studio.readAnimationClipInfo(0, 1n);
const animationClipPathId: bigint = animationClip.pathId;
const animationClipAclBytes: bigint | undefined =
  animationClip.aclTrackByteCount;
const animationClipStreamingOffset: bigint | undefined =
  animationClip.streamingOffset;
const avatar: Avatar = studio.readAvatar(0, 1n);
const avatarPaths: AvatarPathEntry[] = avatar.paths;
const avatarPathId: bigint = avatar.pathId;
const autoAudio: AudioClip = studio.readAudioClip(0, 1n);
const rawAudio: AudioClip = studio.readAudioClip(0, 1n, "raw", 1024);
const compatibilityAudio: AudioClip = studio.readAudio(0, 1n, 1024);
const monoBehaviourJson: MonoBehaviourJson = studio.readMonoBehaviourJson(
  0,
  1n,
  true,
  1024,
);
const monoBehaviourJsonAsync: Promise<MonoBehaviourJson> =
  studio.readMonoBehaviourJsonAsync(0, 1n, false, 1024);
const audioPayloadKind: string = autoAudio.payloadKind;
const exportOptions: ExportConfiguration = {
  mode: "typetree-json",
  filenameFormat: "asset-name-path-id",
  imageFormat: "webp",
  jpegQuality: 90,
  audioFormat: "wav",
  overwriteExisting: false,
  restoreTextAssetExtension: true,
  prettyJson: false,
  maximumObjects: 100,
  maximumTotalOutputBytes: 1024,
  maximumMetadataBytes: 1024,
  maximumRawObjectBytes: 1024,
  maximumTypeTreeJsonBytes: 1024,
  maximumTypeTreeDumpBytes: 1024,
  maximumTextAssetBytes: 1024,
  maximumSimpleAssetBytes: 1024,
  maximumAudioOutputBytes: 1024,
  maximumTextureOutputBytes: 1024,
  maximumTextureArrayOutputBytes: 1024,
  maximumTextureArrayBundleBytes: 1024,
  maximumSpriteOutputBytes: 1024,
  maximumShaderOutputBytes: 1024,
  maximumMonobehaviourJsonBytes: 1024,
  maximumMeshObjectBytes: 1024,
  maximumMeshOutputBytes: 1024,
};
const exportReport = studio.exportWithOptions("output", exportOptions);
const live2dSchemas: MonoBehaviourSchema[] = [
  {
    assemblyName: "Live2D.Cubism.dll",
    namespace: "Live2D.Cubism.Core",
    className: "CubismModel",
    unityVersion: "2022.3.55t4",
    nodes: [
      {
        typeName: "MonoBehaviour",
        fieldName: "Base",
        level: 0,
        align: false,
      },
    ],
  },
];
const live2dWithSchemas: Live2DPackageSet =
  studio.readLive2DPackagesWithSchemas(
    live2dSchemas,
    512 * 1024 * 1024,
    4 * 1024 * 1024 * 1024,
  );
const monoBehaviourJsonWithSchemasAsync: Promise<MonoBehaviourJson> =
  studio.readMonoBehaviourJsonWithSchemasAsync(
    0,
    1n,
    live2dSchemas,
    true,
    1024,
  );
const live2dWithAcl: Promise<Live2DPackageSet> =
  studio.readLive2DPackagesWithAclDecoder(
    decodeAcl,
    live2dSchemas,
    512 * 1024 * 1024,
    4 * 1024 * 1024 * 1024,
  );

void fbx;
void binaryFbx;
void selectedFbx;
void motion;
void opened;
void openOptions;
void openedWithPathLimits;
void scene;
void modelObj;
void texturedFbx;
void legacyAnimation;
void overrideController;
void assetBundle;
void resourceManager;
void preloadData;
void spriteAtlas;
void atlasEntry;
void atlasKey;
void atlasSecondary;
void spriteMetadata;
void spriteRenderData;
void spriteSettings;
void spriteSecondary;
void spriteTriangles;
void animationClip;
void animationClipPathId;
void animationClipAclBytes;
void animationClipStreamingOffset;
void avatar;
void avatarPaths;
void avatarPathId;
void autoAudio;
void rawAudio;
void compatibilityAudio;
void monoBehaviourJson;
void monoBehaviourJsonAsync;
void audioPayloadKind;
void exportOptions;
void exportReport;
void live2dSchemas;
void live2dWithSchemas;
void monoBehaviourJsonWithSchemasAsync;
void live2dWithAcl;

// Keep one ordinary TypeScript consumer for every public AssetStudio member.
// The source-level API audit reads this file and fails when a generated
// declaration is not exercised here; tsc then verifies the actual arguments
// and return types rather than merely checking that the name exists.
function consumeEveryAssetStudioMember(studio: AssetStudio): void {
  void new AssetStudio("fixture.assets");
  void AssetStudio.openWithVersion("fixture.assets", "2022.3.62f1");
  void AssetStudio.openWith("fixture.assets", openOptions);
  void AssetStudio.openWithOodle(
    "fixture.bundle",
    (_input, expectedLength) => Buffer.alloc(expectedLength),
    openOptions,
  );
  void AssetStudio.openAsync("fixture.assets", openOptions);
  void AssetStudio.fromBuffer(
    new Uint8Array(),
    "memory.assets",
    1024,
    openOptions,
  );
  void AssetStudio.fromBufferAsync(
    new Uint8Array(),
    "memory.assets",
    1024,
    openOptions,
  );
  void AssetStudio.fromBuffers([], 1024, openOptions);
  void AssetStudio.extract("fixture.bundle", "output", false);

  void studio.fileCount;
  void studio.objectCount;
  void studio.resourceCount;
  void studio.filePage(0, 1);
  void studio.objectPage(0, 0, 1);
  void studio.resourcePage(0, 1);
  void studio.readResource(0, 1024);
  void studio.readResourceRange(0, 0n, 1n, 1024);
  void studio.resourceIndexByPath("archive:/resource.resS");
  void studio.readRaw(0, 1n, 1024);
  void studio.readRawAsync(0, 1n, 1024);
  void studio.readText(0, 1n, 1024);
  void studio.readTextAsync(0, 1n, 1024);
  void studio.readTypeTreeJson(0, 1n, true, 1024);
  void studio.readTypeTreeJsonAsync(0, 1n, true, 1024);
  void studio.readTypeTreeDump(0, 1n, 1024);
  void studio.readTypeTreeDumpAsync(0, 1n, 1024);
  void studio.readShader(0, 1n, 1024);
  void studio.readShaderAsync(0, 1n, 1024);
  void studio.readMeshObj(0, 1n, 1024);
  void studio.readMeshObjAsync(0, 1n, 1024);
  void studio.readTexture(0, 1n, 0, 1024);
  void studio.readTextureAsync(0, 1n, 0, 1024);
  void studio.readTextureArray(0, 1n, 1024);
  void studio.readTextureArrayAsync(0, 1n, 1024);
  void studio.readSpriteAtlas(0, 1n, 10, 1024, 4096);
  void studio.readSpriteMetadata(0, 1n, spriteLimits);
  void studio.readSprite(0, 1n, 1024);
  void studio.readSpriteAsync(0, 1n, 1024);
  void studio.readAudio(0, 1n, 1024);
  void studio.readAudioClip(0, 1n, "auto", 1024);
  void studio.readMonoScript(0, 1n, 1024);
  void studio.readMaterial(0, 1n, 1024);
  void studio.readBuildSettings(0, 1n, 1024);
  void studio.readPlayerSettings(0, 1n, 1024);
  void studio.readAvatar(0, 1n, 1024);
  void studio.scene(100);
  void studio.sceneWithLimits(sceneLimits);
  void studio.readStaticFbx(1024);
  void studio.readStaticFbxBinary(1024);
  void studio.readFbx(1024);
  void studio.readFbxWithAclDecoder(decodeAcl, 1024);
  void studio.readFbxBinary(1024);
  void studio.readFbxBinaryWithAclDecoder(decodeAcl, 1024);
  void studio.splitObjectFbxCandidates();
  void studio.animatorFbxCandidates();
  void studio.readGameObjectFbx(0, 1n, true, 1024);
  void studio.readGameObjectFbxWithAclDecoder(0, 1n, decodeAcl, true, 1024);
  void studio.readFont(0, 1n, 1024);
  void studio.readMovieTexture(0, 1n, 1024);
  void studio.readVideoClip(0, 1n, 1024);
  void studio.export("output", false);
  void studio.exportWithOptions("output", exportOptions);
  void studio.readAnimationClipInfo(0, 1n, 1024);
  void studio.readLegacyAnimation(0, 1n, 1024);
  void studio.readAnimatorOverrideController(0, 1n, 1024);
  void studio.readAssetBundle(0, 1n, 10, 1024, 4096);
  void studio.readResourceManager(0, 1n, 10, 1024, 4096);
  void studio.readPreloadData(0, 1n, 10, 1024, 4096);
  void studio.readAnimatorController(0, 1n, 1024);
  void studio.readCubismPhysics(0, 1n, 60, 1024);
  void studio.readCubismExpression(0, 1n, 1024);
  void studio.readCubismFadeMotion(0, 1n, 1024);
  void studio.readCubismPosePart(0, 1n, 1024);
  void studio.readCubismDisplayInfo(0, 1n, 1024);
  void studio.readCubismClipMotion(0, 1n, undefined, false, 1024);
  void studio.readCubismClipMotionWithAclDecoder(
    0,
    1n,
    decodeAcl,
    undefined,
    false,
    1024,
  );
  void studio.live2DPackages();
  void studio.readLive2DPackages(1024);
  void studio.readLive2DPackagesWithSchemas(live2dSchemas, 1024, 4096);
  void studio.readLive2DPackagesWithAclDecoder(
    decodeAcl,
    live2dSchemas,
    1024,
    4096,
  );
  void studio.readModelObj("model.mtl", 1024, "png", textureLimits);
  void studio.readFbxWithTextures(1024, "png", textureLimits);
  void studio.readAclTracks(0, 1n, 1024);
  void studio.readMonoBehaviourJson(0, 1n, true, 1024);
  void studio.readMonoBehaviourJsonAsync(0, 1n, true, 1024);
  void studio.readMonoBehaviourJsonWithSchemas(
    0,
    1n,
    live2dSchemas,
    true,
    1024,
  );
  void studio.readMonoBehaviourJsonWithSchemasAsync(
    0,
    1n,
    live2dSchemas,
    true,
    1024,
  );
}

void consumeEveryAssetStudioMember;
