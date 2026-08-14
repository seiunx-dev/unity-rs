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

托管 C# 实现位于独立仓库 [`Team-Haruki/AssetStudio`](https://github.com/Team-Haruki/AssetStudio)，仅作差分测试 oracle，不是 Rust、Python、Node 或 CLI 的运行时依赖；差分门通过 `ASSETSTUDIO_REPO` 或同级目录定位它。旧 `assetstudio-ffi` 源码已排除在 Cargo workspace 之外。

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
- Core 426 项普通测试通过，8 项依赖可选 vgmstream oracle 的测试在本机额外执行并全部通过；
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
   - **已补齐**：serialized format v13-v22 全部版本门；UnityFS v6 内联 blocks-info、UnityFS v6 尾部 blocks-info、UnityFS v7 强制 16 字节对齐、LZ4/LZ4HC/Zstd 压缩块与压缩 blocks-info（含同时压缩 + 尾部布局）、legacy UnityRaw v6、gzip 流。容器差分首轮即发现两处命名分歧（bundle 条目标签、gzip/brotli 把可移植名变成字面量 `"gzip"`，后者会让压缩序列化文件永远无法被外部引用按名匹配），均已修复。
   - **v5-v12 已补齐（2026-08-14）**：这些格式必然带 TypeTree，13 之后那个可以关掉树的 flag 还不存在，所以 tree-less fixture 做不到；自己编一棵树等于让两个 reader 去比对 Unity 从没写过的形状。改用 `tools/generate_typetree_fixtures.py` 从 UnityPy 自带的 `lzma.tpk` 里取真实的 TextAsset 树，产出 JSON 入库（TPK 本身不 vendor，脚本也不进 CI，只在开发时跑一次；派生链在脚本头部写明）。差分矩阵现在覆盖 5-21，首轮全对：9 以下头部在文件尾、7 起才有 Unity 版本串、8 起才有 target platform、11 起 destroyed 字段换成 script type index、树在 10 和 12 从递归编码换成 blob——这些门此前全靠 Rust writer 自己的假设。
   - **仍缺**：UnityWebData（Unity/Tuanjie 两种签名）、ZIP、split 组和 LZMA 块已于 2026-08-14 补齐差分（LZMA 那条此前记为`lzma-rust2` 没有 encoder，实际上它有，只是藏在默认关闭的 `encoder` feature 后面；开发依赖打开它即可，发布出去的 crate 仍然只解压）；Cubism 模型；Shader 5.3-5.4 的 subprogram blob 已补上差分（2026-08-14），5.5+ 序列化程序仍未对照。补 5.3 时发现托管仓库的 `ShaderConverter` 有真实缺陷：`HeaderBytes`（第 15 行）用第 893 行才声明的 `header` 初始化，C# 按声明顺序跑静态初始化器，所以 `header` 还是 null，整个类型的静态构造函数必抛 `ArgumentNullException`——也就是说托管侧的 shader 导出（Convert/WriteTo）在 pin 的这个 revision 上是全挂的。oracle 因此绕开 `ShaderConverter`，直接用同文件里另一个类型 `ShaderProgram` 的 Read/Export，header 字节在 oracle 里写死；等上游修好静态初始化顺序，就能直接调 `Convert` 并把 5.5+ 一并接上。
   - oracle harness 接受任意输入路径，上述补强全部不需要专有样本。
   - **第二 oracle 已就位**：`crates/assetstudio-python/tests/unitypy_oracle.py` 用 UnityPy（独立实现，不需要 .NET）对照对象顺序、PathID、classID、字节大小、名称和原始载荷哈希，14 个 fixture 首轮全对。刻意不比较解码后的像素与网格——UnityPy 走的是本项目已链接的同一个 `texture2ddecoder`，网格/shader 又是 AssetStudio 的转写，比了不构成独立证据。UnityPy 解析不出名字时（它的名称查找依赖自带的 TypeTree 数据库，不覆盖所有 class/版本）记为跳过并报数，而不是当成"双方都认为是空串"。

2. **真实游戏语料覆盖不足**
   - 当前合成 fixture 和差分 oracle 已覆盖大量版本门与格式分支，但不能替代跨游戏、跨平台、跨 Unity 版本的真实 corpus。
   - 需要持续扩充旧 Unity、Unity 5.x、2019/2020/2021/2022/2023、Unity 6、Tuanjie，以及大小端和平台资源样本。
   - 对象顺序、名称、container、PathID、原始 payload hash、像素/PCM/模型语义和错误分类都需要进入版本化快照。

3. **平台和版本长尾尚未闭合**
   - Unity 6000.2 `MeshLodInfo` 和虚拟几何布局缺少可验证公开样本；
   - Tuanjie 虚拟几何 cluster 尚未解码；
   - UnityArchive 没有样本验证的公开格式，当前仅识别并明确拒绝；
   - **UnityCN 加密已实现解密（2026-08-14）**：需调用方通过 `BundleOpenOptions`/`AssetLoadOptions` 或 Python `unity_cn_key=` 提供 16 字节密钥，仓库不内置任何密钥；无密钥时仍明确拒绝。检测改为 flag 驱动（不再用推测式解析探测），因此 blocks-info 本身被加密的常见情况会直接指出是 UnityCN，而不是报"LZ4 数据无效"。密钥校验与表派生所需的 AES-128 在本 crate 内实现并用 FIPS-197 向量验证；解密走 LZ4 token 流，literal 段不动，两条 0xFF 扩展链和每次偏移推进都做了边界检查。算法理解来自 UnityPy 与其致谢的 PGRStudio，代码为按行为重写。

4. **Tuanjie ACL 尚无内置纯 Rust 解码器**
   - ACL 容器、边界、hash、decoder map 和输出形状已验证；
   - Rust/Python 可注入安全 decoder；
   - 若希望完全开箱即用，仍需一个许可清晰、样本差分通过的纯 Rust ACL 2.x 解码实现。

### P1：主要功能长尾

1. **模型/FBX**
   - 当前输出为确定性的 ASCII FBX 7.4；尚无 binary FBX；
   - **新增 `obj` 命令（2026-08-14）**：整模型导出为 Wavefront OBJ + 同名 `.mtl` + 同级贴图。OBJ 没有层级，因此节点变换烘进世界空间、顶点索引跨 group 累加；面引用只写网格真正有的通道，与 `export` 写单个 Mesh 的 `.obj`（照抄托管 writer 无条件 `v/vt/vn`）刻意不同；
   - **贴图已写出（2026-08-14）**：`scene_textures.rs` 解析材质的贴图 PPtr、按对象去重解码一次、分配稳定文件名，writer 发射连线到 `DiffuseColor`/`NormalMap`/`SpecularColor`/`Bump` 的 `Texture`/`Video` 对，UV offset/scale 取自材质自己的 `TexEnv`，属性名映射沿用托管 reader 的 `_MainTex`/`_BumpMap`/`Specular`/`Normal` 规则。这确实改动了"单文件原子发布"契约：图片写在 FBX 同级目录，`--no-textures` 可退回纯几何，`--texture-format` 可换 PNG 以外的格式。贴图名来自资产因而不可信，一律削成单个路径分量，已存在的文件不覆盖；批量导出共用一个名字分配器，避免两张同名贴图互相顶掉。解析不到 `Texture2D` 或解码失败的引用记为 skip 并报数，不拖垮整个模型；
   - **`CompressedMesh` 已纳入托管差分（2026-08-14）**：加了两个 fixture——一个同时带顶点流和打包向量（验证叠加规则），一个是 Unity 实际写出的形态（顶点流为空）。首轮就查出实现是二选一分支：有打包数据就完全忽略顶点流，而托管是把打包结果按字段叠加到顶点流之上，每个块各自按 item count 判断。已改为叠加。另外空通道的表示也统一了：托管会分配零长数组，这边是 None，两者含义相同，manifest 两侧都归一为“没有这个通道”。
   - **`CompressedMesh` 打包几何已解码（2026-08-14）**：`packed_bits.rs` 提供共享的 `PackedFloatVector`/`PackedIntVector` 位流读取，顶点、八面体法线/切线加符号位、UV（读 packed channel descriptor）、31 量化蒙皮权重和索引缓冲全部还原；浮点刻意保持 f32，加宽会让 OBJ 文本与 oracle 分叉。Unity 6000.2 MeshLOD 和虚拟几何仍会明确报 Unsupported。

2. **纹理和音频**
   - Switch 更低 mip、stripped mip 和未进入受验证 GOB 表的格式仍缺；
   - `Texture2D` 的 `m_ImageCount != 1` / `m_TextureDimension != 2` 会在格式分发前拒绝，而托管 converter 直接解首张图；PVRTC 还要求 2 的幂尺寸与 16x8/8x8 下限，因此只能取 mip0。这些拒绝条件此前未见于文档；
   - **AnimationClip 关键帧值已纳入托管差分（2026-08-14）**：此前只比曲线条数、sample rate、wrap mode、ACL 头和 streaming 信息，关键帧本身（时间、值、两侧切线）只有本项目自己的期望背书。现在 rotation/euler/position/scale/float 五类曲线的路径与每个关键帧都按浮点位模式（不是十进制文本，避免舍入差异被掩盖）哈希对照。新增一个真的带关键帧的 fixture，并加了断言确保这五行都非空——否则曲线块解析失败时两边会一致地得到空哈希，看起来通过实则什么都没验证。列表缺失与列表为空统一按空处理，两者含义相同。
   - **tight-mesh sprite 已纳入托管差分（2026-08-14），并因此修掉一个真实缺陷**：oracle 之前的 sprite 载荷是在 C# 里另写了一遍矩形裁剪，等于拿本项目跟自己的假设比，而且根本到不了 tight 路径；现在直接调 AssetStudio 的 `SpriteHelper.GetImage`，图集 render-data、tight 裁剪、alpha mask、downscale 全走托管实现。首轮就查出 `sprite.rs` 读 submesh 的 localAABB 跳了 32 字节，实际是 6 个 float 24 字节（`mesh.rs` 三处都是对的）。凡是带 submesh 的 sprite——也就是所有 tight 打包的 sprite，而 tight 正是 Unity 的默认 mesh type——submesh 之后的字段（index buffer、vertex data、texture rect、packing settings）全部错位。单元测试没发现是因为 fixture 是照着同样错的布局手写的。修好之后 8x8 tight fixture 的 64 个像素与托管逐字节一致，说明 mask 光栅化本身也对得上。
   - **Switch GOB 反交织已纳入托管差分（2026-08-14）**：3 个 fixture（RGBA32 两种 block height，加一个 BC7）端到端对照，全部一致。GOB 布局只取决于 texel 大小和 platform blob 里的 block height 指数，这三个就把两者都覆盖了。DXT5 和 ASTC 不放进来，理由和块格式矩阵一样：前者是已记录的 s3tc 偏离，后者随机字节会命中保留编码，都与交织无关。顺带确认了一件事：`texture2ddecoder` 的 ASTC 解码器在畸形输入上会 panic（减法下溢），但 Core 早已用 catch_unwind 包住外部解码器，所以对外仍然是报错而不是崩溃——不可信输入不崩溃这条不变量成立。
   - **Crunch 已纳入托管差分（2026-08-14）**：6 个真实 CRN 载荷（classic DXT1/DXT5 走 2017.2，UnityCrunch DXT1/DXT5/ETC1/ETC2A 走 2022.3）端到端对照，全部一致。单元测试此前只比解码器本身对着 C++ oracle 的哈希；这一条走的是调用方真正的路径：Texture2D 解析、头部嗅探、转码、mip0 解码，连选哪个 Crunch 方言的版本门也一并比了。fixture builder 现在按 revision 生成 Texture2D 布局（2017.3 的 fallback 块、2018.2 的 streaming 对、2019.3 的 mip limit、2020 的 stripped mip 与 64 位流偏移、2022.2 的 mip-limit group），因此老版本纹理布局本身也进了差分。
   - **块压缩纹理解码差分已建立（2026-08-14），并因此修掉一个真实缺陷**：oracle 之前只比原始 payload 字节（那是直接从盘上读的），所有块解码器都只有本项目自己的往返测试背书。现在比解码后的像素，覆盖 BC4/BC5/BC7、ETC_RGB4、ETC2_RGB/RGBA1/RGBA8、EAC_R/RG 及其 signed 变体共 11 种格式。首轮就查出 `texture2ddecoder` 0.1.2 的 EAC 入口把 48 位索引流按小端整数读，而格式是最高位在前——同一个 crate 自己的 ETC2 alpha 解码器读的是大端并且与托管解码器一致，等于 crate 跟自己不一致。受影响的是 EAC_R/EAC_R_SIGNED/EAC_RG/EAC_RG_SIGNED，即移动端常见的法线图和 mask 图，像素会整块错位。已在 `texture.rs` 内实现 EAC 解码取代 crate 的入口：只改字节序，算术仍按格式的 11 位空间做（multiplier 为 0 时代入的 1 是 11 位步长，不是 8 位；照 8 位算会让那一块的调制范围放大 8 倍——这一点也是差分查出来的）。
   - **BC6H 与 ASTC 暂未纳入差分**：随机字节会命中编码器永远不会产出的保留编码，两边对这类输入的处理本就不同（ASTC 托管侧给 error color，本项目直接报错）。要判定需要真实编码器产出的 fixture。
   - **DXT5 与 DXT1 同属已记录的 s3tc 偏离**：托管的颜色调色板复刻 NV4x 硬件，本项目跟规范；DXT5 的 alpha 半边（BC4）能对上，颜色半边对不上，正好印证根因。
   - **DXT1 punch-through alpha 已裁决（2026-08-14）：跟 s3tc 规范**。`q0 <= q1` 模式下 index 3 解为透明黑 `(0,0,0,0)`，与独立解码器（Pillow）一致；AssetStudio 原生 `bcn.cpp` 给不透明黑 `(0,0,0,255)`，复刻的是 NV4x 时代硬件行为。这是对 oracle 的有意偏离——镂空贴图的遮罩区应当透明而非黑块——已在 `texture.rs` 注释、测试和兼容矩阵中记录。UnityPy 无法作为第三方仲裁：它与本项目共用同一个 `texture2ddecoder` 上游。（同批复核确认 DXT3/DXT5 调色板不是缺陷：Rust 符合 s3tc 规范，原生解码器复刻的是 NV4x 时代硬件行为，且 C# 侧根本没有 DXT3 解码器。）
   - multistream MPEG/Opus 和少数平台音频 codec 仍保留原始数据；
   - Opus/MPEG 的 vgmstream 差分目前使用全零 fixture，验证的是分帧而非采样内容；且 8 个音频差分全部 `#[ignore]`，CI 未执行；
   - 新增 codec 必须先有真实样本和独立 oracle，不能只凭推测实现。

3. **MonoBehaviour schema 来源**
   - 内嵌 TypeTree 和调用方提供的可信完整 schema 已支持；
   - 自动从 managed assembly/dummy DLL 生成 schema 仍是独立的离线可信工具工作，不会在解析进程中加载或执行 DLL。

4. **Node 专用 reader 完整度**
   - Node 公开面从 15 个同步方法加 9 个 Promise 方法扩到 35 + 9：读取面新增 `readAudio`、`readMonoScript`、`readMaterial`、`readBuildSettings`、`readPlayerSettings`、`readAvatar`、`readAnimationClipInfo`、`readAnimatorController`、`readAclTracks`（只读 ACL 头，够调用方判断自己的 decoder 能不能处理）、`readMonoBehaviourJsonWithSchemas`（用调用方提供的可信 schema 还原被剥掉的托管字段；schema 是纯数据，查找过程不执行任何资产控制的代码）、`readResourceRange`、`resourceIndexByPath`、`scene`；输出面新增 `readStaticFbx`、`readFbx`（含动画）、`readFbxWithTextures`（贴图随 FBX 一起返回，由调用方决定写哪）、`export`、静态 `extract`、`live2DPackages`、`readLive2DPackages`；加载面新增工厂方法 `openWithVersion`、`fromBuffers` 与 `openWithOodle`。Material 属性值刻意只给名字不给值：它们按表分类型，硬摊平到 JS 只会丢信息。
   - Core 侧同时补上 `Studio::write_fbx_with_textures`：此前贴图输出只有 CLI 走得到，库调用方拿不到。它返回贴图集合而不是自己写盘——这个方法只持有一个输出流，没有目录可以写同级文件，由调用方决定落在哪里。
   - Live2D 包发现与落盘、FBX 静态几何/动画/贴图均已接；
   - Oodle decoder 注入已接（`openWithOodle`，只提供异步形式：解码回调要在事件循环上跑而 worker 在等它，同步调用会把该跑回调的那条线程堵死）；剩 ACL decoder 注入未接；
   - Node 是可选交付面，因此优先级低于 Core 和 Python 的真实语料兼容。

5. **Live2D 散件发现**
   - MOC3 标识表已接入参数组（与托管一致：MOC 的表覆盖组件推导出的名字），仅有 MOC、缺少活动组件的包不再得到空参数组。与托管的一处有意偏离：托管是无条件覆盖，因此 MOC 版本不带标识表时连组件名也会被清空；这里只在 MOC 确实带表时覆盖；
   - **散件发现回退已补（2026-08-14）**：模型组件图走不到时，回落到同一个序列化文件里的独立 `CubismExpressionData`/`CubismFadeMotionData`/`CubismPhysicsController`。语义跟托管一致——只在图路线什么都没拿到时才回落，因为表达式顺序由 `CubismExpressionList` 定义，扫文件复现不出来。作用域取序列化文件（托管取 container group），这是本 reader 最接近的等价物，也能防止一个 bundle 里的散件挂到另一个 bundle 的模型上。动作的回落顺序是：fade controller 的列表 → 散件 fade motion → AnimationClip。

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
4. 扩展可选 binary FBX 输出（贴图输出已完成）；
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
