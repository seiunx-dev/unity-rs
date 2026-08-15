import { AssetStudio } from "../index.js";
import type {
  AclDecodeRequest,
  AclDecodedClip,
  ModelTextureLimits,
  SceneLimits,
  SceneNode,
} from "../index.js";

declare const studio: AssetStudio;

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
};
const scene: SceneNode[] = studio.sceneWithLimits(sceneLimits);
const textureLimits: ModelTextureLimits = {
  maximumTextures: 128,
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

void fbx;
void motion;
void opened;
void scene;
void modelObj;
void texturedFbx;
