# AssetStudio Rust 重写进度与缺口

最后更新：2026-08-14

本文记录 Rust 重写的交付范围、当前能力、验证证据和剩余缺口。更细的逐格式兼容矩阵见 [`README.md`](README.md)，私有真实游戏语料的运行方式见 [`corpus/README.md`](corpus/README.md)。

## 目标范围

重写的正式交付面为：

- `assetstudio-core`：不依赖 .NET 的安全 Rust 解析和导出内核；
- `assetstudio-rs`：通过 PyO3 直接调用 Rust Core 的 Python 3.9+ abi3 包；
- `assetstudio`：用于批处理、调试和回归验证的原生 CLI；
- `assetstudio-rs-node`：通过 napi-rs 直接调用 Rust Core 的可选 Node-API 包。

以下内容明确不属于重写目标，也不计入完成条件：

- WinForms 或其他 GUI；
- 旧版自定义 C ABI 的兼容、发布或语义复刻。

现有 C# 源码暂时保留为差分测试 oracle，不是 Rust、Python、Node 或 CLI 的运行时依赖。旧 `assetstudio-ffi` 源码已排除在公开 Cargo workspace 之外。

## 当前阶段

项目当前处于 **Rust/Python Beta** 阶段：主流程已经可用，但还不能宣称覆盖全部 Unity/Tuanjie 版本长尾，也尚未达到可以删除托管 oracle 的证据强度。

| 交付面 | 当前状态 | 说明 |
| --- | --- | --- |
| Rust Core | Beta，主流程可用 | 主要容器、SerializedFile、常见资产、场景、动画和导出链路已实现；长尾格式继续补齐 |
| Python | Beta，主要目标接口 | 直接绑定 Core，覆盖加载、枚举、资源读取、主要资产读取、FBX、Live2D、导出和解包 |
| CLI | Beta | inspect/info/list/scene、export/extract、FBX、Animator/SplitObjects、Live2D 已接入 |
| Node.js | 可选 Preview/Beta 子集 | 同步与 Promise worker API 已覆盖加载、枚举及主要读取路径；专用资产接口仍少于 Python |
| .NET 运行时退役 | 尚未完成 | 公开 Rust/Python/Node 不依赖 .NET；C# 仍用于差分 oracle 和格式核验 |

这些状态是风险分级，不是按代码行数计算的完成百分比。

## 已完成能力

### 输入、容器和资源

- SerializedFile v5-v22，支持大小端、32/64 位 PathID、TypeTree、外部引用和有界对象区间；
- UnityFS v6/v7，支持 None、LZ4/LZ4HC、LZMA、Zstd；
- UnityWeb/UnityRaw v1-v6、UnityWebData/TuanjieWebData；
- gzip、Brotli、ZIP Stored/Deflate；
- `.split0`...`.splitN` 惰性拼接、递归容器发现、外部资源和跨文件 PPtr；
- UnityCN 加密包检测，以及 UnityArchive 签名识别后的明确拒绝；
- Oodle 通过调用方注入的精确长度安全 decoder 接口接入，Core 不链接或分发专有库；
- Unity 版本解析对齐托管优先级：调用方显式版本 > 仅在 format < 7 时生效的 bundle revision > 文件自身声明；文件版本被 strip 且无覆盖时回退 bundle revision（相对 C# 硬抛的有意偏离，已记录）；
- 可选的逐输入容错策略：`LoadFailurePolicy::SkipInput` 保留能解析的输入并把跳过项记入 `AssetCollection::diagnostics`，Python 为 `skip_unreadable_inputs`，CLI 默认启用并以部分失败退出码报告。

### 资产解析和导出

- TextAsset、Font、MovieTexture、AudioClip、VideoClip；
- Texture2D、Texture2DArray、Sprite、SpriteAtlas，以及 JPEG/PNG/BMP/TGA/lossless WebP/raw RGBA 输出；
- 常见整数、浮点、BC、ETC/EAC、ASTC、PVRTC、ATC、Crunch、Xbox 360 和 Switch mip0 纹理路径；
- Material、Shader、Mesh、BuildSettings、PlayerSettings；Mesh 的 position/normal/UV0 通道接受托管 reader 支持的全部浮点顶点格式（Float32、Float16 及归一化 8/16 位），覆盖开启 Vertex Compression 的构建；
- TypeTree dump 文本复刻 .NET 默认浮点渲染（含科学计数法切换阈值与两位指数），并按 InvariantCulture 固定；`TypeValue` 保留 float/double 源宽度，TypeTree JSON 不再输出加宽后的双精度展开；
- MonoScript、内嵌 TypeTree MonoBehaviour，以及可信外部完整 schema；
- GameObject/Transform/Renderer/Animator 场景层级、跨文件引用和稳定对象元数据；
- AnimationClip、AnimatorController、Avatar、动画绑定图；
- 通用层级 ASCII FBX 7.4，覆盖普通/蒙皮网格、材质槽、骨骼、静态 blend shape 和已验证动画采样；
- Live2D MOC、model3、纹理 PNG、expression、motion、physics、pose、display-info 和参数组；
- 有界递归解包，以及拒绝符号链接、同目录临时文件和原子发布的安全导出。

### 公开 API 和发布

- Rust 高层 `Studio` API 可直接从路径、单个内存区域或多文件内存集合加载；
- Python 提供惰性/分页枚举、资源读取、主要专用 reader、schema/ACL/Oodle 适配器、导出和解包；
- Python wheel 使用 `cp39-abi3`，CI 构建 Linux、Windows、macOS 的 x86-64/ARM64 组合，并在构建解释器和 Python 3.14 上安装测试；
- Python sdist 会被重新构建成 wheel 并执行完整 API 测试；
- Node 提供本地模块、TypeScript 声明、同步 API 和 libuv worker Promise API；
- Rust Core crate、Python wheel/sdist、Node 包和原生 CLI 都有独立发布校验。

## 当前验证证据

截至本文更新时间，当前工作树已通过：

- `cargo fmt --all -- --check`；
- `cargo clippy --workspace --all-targets --locked -- -D warnings`；
- `cargo test --workspace --all-targets --locked --no-fail-fast`；
- Core 424 项普通测试通过，8 项依赖可选 vgmstream oracle 的测试在本机额外执行并全部通过；
- C#→Rust 托管差分 oracle 通过；
- TypeTree dump 浮点文本对照 .NET 10 实测生成的 849 个取值（边界值 + 位模式扫描）逐字节一致，期望值以 fixture 形式入库；
- `cargo doc --workspace --no-deps` 在 `-D warnings` 下通过；
- `cargo package -p assetstudio-core` 的包内容和独立重建通过；
- Python 锁定 wheel、sdist 及从 sdist 重建的 wheel 均可安装并通过 API/类型桩测试；
- Node 原生 addon 测试和 `npm pack --dry-run` 通过；
- `git diff --check` 通过。

CI 在 Linux、Windows、macOS 上运行 Rust 测试，并分别验证 Python、Node、CLI 和托管差分任务。真实游戏文件仍通过私有 corpus manifest 接入，仓库不提交专有输入数据。

**证据强度提示**：上述大部分测试是 Rust 内部的合成往返，只能证明读写自洽。真正的跨实现证据只有托管差分 oracle 和 vgmstream 音频差分两处，覆盖面见下方 P0 第 1 项。

## 明确缺口

### P0：完成声明前必须补强

1. **托管差分 oracle 覆盖面仍不足（系统性根因）**
   - 这不是理论风险：2026-08-14 修掉的 FBX blend shape 增量、FBX 矩阵约定、Node 纹理行序、bundle 版本覆盖四个缺陷，全部被手写的测试期望值锁死，正是因为它们从未与 C# 对照过。
   - **已补齐**：serialized format v13-v22 全部版本门；UnityFS v6 内联 blocks-info、UnityFS v6 尾部 blocks-info、UnityFS v7 强制 16 字节对齐、legacy UnityRaw v6、gzip 流。容器差分首轮即发现两处命名分歧（bundle 条目标签、gzip/brotli 把可移植名变成字面量 `"gzip"`，后者会让压缩序列化文件永远无法被外部引用按名匹配），均已修复。
   - **仍缺**：v5-v12（需要真实 TypeTree，tree-less fixture 做不到）；bundle 内块压缩（`lz4_flex` 当前只开解码 feature）；LZMA/Zstd 块；UnityWebData、ZIP、split 组；压缩纹理、Crunch、Switch、tight-mesh sprite；Cubism 模型；AnimationClip 关键帧值；Shader 只覆盖了 5.2 直连脚本，5.3-5.4 与 5.5+ 序列化程序未对照（`oracle/Program.cs` 目前遇到 subprogram blob 会直接抛错，要先解除这个 guard）。
   - oracle harness 接受任意输入路径，上述补强全部不需要专有样本。

2. **真实游戏语料覆盖不足**
   - 当前合成 fixture 和差分 oracle 已覆盖大量版本门与格式分支，但不能替代跨游戏、跨平台、跨 Unity 版本的真实 corpus。
   - 需要持续扩充旧 Unity、Unity 5.x、2019/2020/2021/2022/2023、Unity 6、Tuanjie，以及大小端和平台资源样本。
   - 对象顺序、名称、container、PathID、原始 payload hash、像素/PCM/模型语义和错误分类都需要进入版本化快照。

3. **平台和版本长尾尚未闭合**
   - Unity 6000.2 `MeshLodInfo` 和虚拟几何布局缺少可验证公开样本；
   - Tuanjie 虚拟几何 cluster 尚未解码；
   - UnityArchive 没有样本验证的公开格式，当前仅识别并明确拒绝；
   - UnityCN 加密 payload 仅检测，不包含解密器。

4. **Tuanjie ACL 尚无内置纯 Rust 解码器**
   - ACL 容器、边界、hash、decoder map 和输出形状已验证；
   - Rust/Python 可注入安全 decoder；
   - 若希望完全开箱即用，仍需一个许可清晰、样本差分通过的纯 Rust ACL 2.x 解码实现。

### P1：主要功能长尾

1. **模型/FBX**
   - 当前输出为确定性的 ASCII FBX 7.4；尚无 binary FBX；
   - **贴图未写出**：材质的贴图 PPtr 已进入 model IR，但 writer 不发射 `Texture`/`Video` 节点，带贴图模型导出为无贴图。`fbx` 命令会报告丢弃的绑定数量，不再静默。要真正写出还需决定图片文件落盘位置，这会改动当前"单文件原子发布"的输出契约；
   - `CompressedMesh` 打包几何、Unity 6000.2 MeshLOD 和虚拟几何仍会明确报 Unsupported。

2. **纹理和音频**
   - Switch 更低 mip、stripped mip 和未进入受验证 GOB 表的格式仍缺；
   - `Texture2D` 的 `m_ImageCount != 1` / `m_TextureDimension != 2` 会在格式分发前拒绝，而托管 converter 直接解首张图；PVRTC 还要求 2 的幂尺寸与 16x8/8x8 下限，因此只能取 mip0。这些拒绝条件此前未见于文档；
   - **DXT1 punch-through alpha 待裁决**：`q0 <= q1` 模式下 index 3 的 alpha，Rust 与独立解码器（Pillow）给 `(0,0,0,0)`，AssetStudio 原生 `bcn.cpp` 给 `(0,0,0,255)`。该模式在真实 1-bit-alpha 内容中会命中，属于"跟 spec 还是跟参考实现"的取舍，需要显式决定并记录。（同批复核确认 DXT3/DXT5 调色板不是缺陷：Rust 符合 s3tc 规范，原生解码器复刻的是 NV4x 时代硬件行为，且 C# 侧根本没有 DXT3 解码器。）
   - multistream MPEG/Opus 和少数平台音频 codec 仍保留原始数据；
   - Opus/MPEG 的 vgmstream 差分目前使用全零 fixture，验证的是分帧而非采样内容；且 8 个音频差分全部 `#[ignore]`，CI 未执行；
   - 新增 codec 必须先有真实样本和独立 oracle，不能只凭推测实现。

3. **MonoBehaviour schema 来源**
   - 内嵌 TypeTree 和调用方提供的可信完整 schema 已支持；
   - 自动从 managed assembly/dummy DLL 生成 schema 仍是独立的离线可信工具工作，不会在解析进程中加载或执行 DLL。

4. **Node 专用 reader 完整度**
   - Node 公开面为 15 个同步方法加 9 个 Promise 方法；Live2D 与 FBX 是完全缺失而非不完整；
   - 除专用 reader 外还缺：export、extract、场景层级、MonoBehaviour/MonoScript、BuildSettings/PlayerSettings、AnimationClip/AnimatorController/Avatar、ACL 检视与注入、Oodle decoder 注入、Unity 版本覆盖、多文件内存加载、`read_resource_range`/按路径读资源；
   - Node 是可选交付面，因此优先级低于 Core 和 Python 的真实语料兼容。

5. **Live2D 参数组来源与散件发现**
   - `CubismMoc` 的 parameter/part 标识表已解析，但包管线只用它做内存预算；托管 `Live2DExtractor` 用这两张表跑 EyeBlink/LipSync 启发式，因此仅有 MOC、缺少活动组件的包在 Rust 侧会得到空参数组；
   - 没有针对独立 `CubismExpressionData`/`CubismFadeMotionData`/`CubismPhysicsController` 的容器级发现回退。

### 设计上保留的外部适配器

以下能力不应通过在 Core 中静态链接不明来源或专有二进制来“补齐”：

- Oodle：由用户提供有授权的 decoder，Core 只接受安全的精确输入/输出接口；
- 外部 MonoBehaviour schema：由可信离线工具生成，运行时只消费数据结构；
- 在内置 ACL 解码器完成前，Tuanjie ACL 可由调用方提供 decoder。

这些是明确的安全和授权边界，不等同于旧 C ABI。

## 后续优先顺序

1. 把托管差分 oracle 从裸 `.assets` 扩到容器、版本门和已实现的资产解码路径（P0 第 1 项，全部不需要专有样本，且能防止同类缺陷再生）；
2. 扩充真实 corpus 和 C#→Rust 差分快照，按实际命中率排序缺口；
3. 裁决 DXT1 punch-through alpha，并让 8 个 vgmstream 音频差分进入 CI、把全零 fixture 换成有内容的样本；
4. 扩展 FBX 贴图输出（含图片落盘契约）与可选 binary 输出；
5. 把 MOC3 标识表接入 Live2D 参数组，并补散件发现回退；
6. 获取样本并实现 Unity 6000.2 MeshLOD/虚拟几何，而不是猜测布局；
7. 完成许可清晰的纯 Rust Tuanjie ACL 解码；
8. 补齐高命中率的平台纹理/音频长尾；
9. 在 Core/Python 稳定后继续提升 Node 专用 API 覆盖。

## 完成判定

只有同时满足以下条件，才可把 Rust 重写标记为完成：

- Rust Core 和 Python 主流程不依赖 .NET、GUI 或旧 C ABI；
- 支持矩阵中的“Implemented”项均有相应单测、边界测试或差分证据；
- 代表性真实 corpus 在支持的 Unity/Tuanjie/平台矩阵上稳定通过；
- 未实现格式均有明确、稳定的 Unsupported 行为，不产生静默错误输出；
- Rust crate、Python wheel/sdist、可选 Node 包和 CLI 的跨平台发布任务通过；
- 导出/解包保持有界、拒绝路径穿越和符号链接目标，并采用安全原子发布；
- C# 只需作为历史参考或可选 oracle，不再承担用户运行时功能。

在达到这些条件前，项目可以作为 Beta 使用，但不应把“测试通过”误写成“所有 Unity 游戏都已兼容”。

## 维护规则

每次更新本文件时：

- 更新顶部日期；
- 只把有代码和验证证据的能力移入“已完成”；
- 发现新格式时先记录样本来源、版本门和预期行为；
- 不用缩小目标或删除失败 fixture 的方式提高完成度；
- GUI 和旧 C ABI 始终列为非目标，而不是待办缺口。
