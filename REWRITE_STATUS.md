# unity-rs 重写进度与缺口

最后更新：2026-08-27（Asia/Shanghai）

本文记录 Rust 重写的交付范围、当前能力、验证证据和剩余缺口。更细的逐格式兼容矩阵见 [`README.md`](README.md)，私有真实游戏语料的运行方式见 [`corpus/README.md`](corpus/README.md)。

## 目标范围

重写的正式交付面为：

- `unity-rs-core`：不依赖 .NET 的安全 Rust 解析和导出内核；
- `unity-rs`：通过 PyO3 直接调用 Rust Core 的 Python 3.9+ abi3 包；
- `unity-rs`：用于批处理、调试和回归验证的原生 CLI；
- `unity-rs-node`：通过 napi-rs 直接调用 Rust Core 的可选 Node-API 包。

以下内容明确不属于重写目标，也不计入完成条件：

- WinForms 或其他 GUI；
- 旧版自定义 C ABI 的兼容、发布或语义复刻。

暂时拿不到真实样本或独立 oracle 的 Unity/Tuanjie 版本与平台格式保留在
`corpus/` 验收工程，并在兼容矩阵中标记为 **Not tested**。版本处理采用两层策略
（2026-08-26 起）：容器格式门、各资产类的版本下限、Tuanjie 构建和 stripped 版本
命中时继续稳定返回 `Unsupported`，不猜布局；而**高于已验证上限的标准 Unity 版本**
默认按最新已知布局尝试解析（UnityPy 风格），解析不匹配时仍以 `Unsupported` 报错并
注明该次尝试，`strict_unity_versions` 选项可恢复直接拒绝。它们仍是未来兼容性 1.0
的证据缺口，但按本次迁移约定不阻塞无头 Rust/Python 运行时重写的完成。

托管 C# 实现位于独立仓库 [`Team-Haruki/AssetStudio`](https://github.com/Team-Haruki/AssetStudio)，仅作差分测试 oracle，不是 Rust、Python、Node 或 CLI 的运行时依赖；差分门通过 `UNITY_RS_ORACLE_REPO` 或同级目录定位它。旧 `unity-rs-ffi`/context handle 源码已从仓库删除，不再只是排除在 Cargo workspace 之外。

## 当前阶段

无头 Rust/Python 运行时重写已经完成；项目仍处于 **Rust/Python Beta** 阶段，因为还不能
宣称覆盖全部 Unity/Tuanjie 版本长尾，也尚未达到可以删除托管 oracle 的证据强度。

| 交付面 | 当前状态 | 说明 |
| --- | --- | --- |
| Rust Core | Beta，主流程可用 | 主要容器、SerializedFile、常见资产、场景、动画和导出链路已实现；长尾格式继续补齐 |
| Python | Beta，主要目标接口 | 直接绑定 Core，覆盖加载、枚举、资源读取、主要资产读取、FBX、Live2D、导出和解包 |
| CLI | Beta | inspect/info/list/scene、export/extract、FBX、Animator/SplitObjects、Live2D 已接入 |
| Node.js | 可选 Beta | 同步与 Promise worker API 已覆盖加载、枚举及主要读取路径；保留 `scene(maximumGameObjects?)`，并通过 `sceneWithLimits` 暴露与 Core/Python 相同的六类场景预算；路径与单/多内存输入都可组合传入加载选项（Unity 版本、UnityCN 密钥、失败容忍策略、输入上限），Live2D 包连同 diagnostics 一起返回，外部 schema 可恢复 stripped 包，Promise worker 可在同一次调用中再注入 Tuanjie ACL decoder；模型导出（场景 OBJ 与带贴图的 FBX）与 Python 等价；Core→Node 映射、Rust addon、生成声明和严格 TypeScript 消费均有机器审计 |
| .NET 运行时退役 | 已完成 | 默认构建、安装和用户工作流完全不需要 .NET；C# 只用于显式差分 oracle 和格式核验 |

这些状态是风险分级，不是按代码行数计算的完成百分比。
Python 的 wheel/sdist 发布元数据与本表统一使用 PyPI 的 Beta classifier；安装后门禁同时
拒绝旧 Alpha classifier，避免代码、文档与包索引显示不同成熟度。Python 本地开发与发布
命令也已改用独立仓库根下的 `crates/unity-rs-python`，不再引用迁仓前的 `rust/` 前缀。

### 距离 1.0 还差什么

无 GUI 的 Rust/Python 主流程、CLI 和可选 Node 绑定已经闭合；当前离 1.0 的主要差距是
**证据收口**，而不是恢复旧 C ABI 或继续扩张公开接口：

1. **可自主完成的发布收口已完成（2026-08-25）**：已声明支持路径的 hostile input、
   累计内存/输出和 worker/GIL 边界完成第四十九项治理；零跳过本地门禁与最终 HEAD 的
   公开多平台矩阵再次全绿。项目迁至 `seiunx-dev/unity-rs` 与首次正式发布现已完成；
   后续版本号、registry 发布和 GitHub Release 仍由维护者决定，不属于解析或绑定实现缺口。
2. **需要真实样本的验收**：补 Tuanjie 2022.3.x、Nintendo Switch、Unity 4/5/2017 和
   带完整托管快照的代表性 corpus。只对样本实际命中的长尾补实现；UnityArchive、虚拟几何
   cluster、Switch stripped/低 mip 及平台 codec 在没有可验证布局或独立 oracle 时在兼容
   矩阵中标记为 **Not tested**；这些路径命中时继续稳定返回 `Unsupported`，不猜字段，也不
   作为已经支持的能力宣传。高于已验证上限的标准 Unity 版本按默认宽松策略尝试最新已知
   布局，但同样保持 **Not tested** 状态——宽松解析不是验证，提升文档化上限仍需带 fixture。
3. **退役证据**：用上述 corpus 生成版本化 manifest，覆盖对象顺序、PathID/class、名称和
   container、原始载荷 hash、主要解码结果或稳定错误族。C# 只保留为可选历史 oracle；默认
   构建、安装和用户工作流必须继续完全不需要 .NET。

额外平台 codec、内置纯 Rust Tuanjie ACL decoder，以及向上游提交 `ruopus`/纹理解码器修复
属于 1.0 后可继续推进的增强；它们不应通过引入未授权专有二进制或猜测格式来换取“完成”。

## 已完成能力

### 输入、容器和资源

- SerializedFile v5-v22，支持大小端、32/64 位 PathID、TypeTree、外部引用和有界对象区间；
- UnityFS v6-v8，支持 None、LZ4/LZ4HC、LZMA、Zstd；
- UnityWeb/UnityRaw v1-v6、UnityWebData/TuanjieWebData；
- gzip、Brotli、ZIP Stored/Deflate；
- `.split0`...`.splitN` 惰性拼接、递归容器发现、外部资源和跨文件 PPtr；
- 根输入标签和递归发现路径现分别受单路径与累计 UTF-8 字节预算约束，默认与既有
  Python/Node 内存输入契约一致为 1 MiB/64 MiB；Bundle/WebData/ZIP 会继承调用方更低的
  路径上限，gzip/Brotli 复用原路径不重复计费。嵌套路径按精确长度先检查并
  `try_reserve_exact`，反斜杠在写入最终缓冲时直接规范化，不再先 `replace` 出一份中间
  String；文件系统根路径中的无效 Unicode 也先流式计算 replacement 后的精确 UTF-8
  长度、通过单项/累计预算后才分配 `LoadPath`。Python 三个加载入口和 Node
  `OpenOptions` 均可收紧这两项；
- UnityCN 加密包检测与调用方密钥解密（无密钥时明确拒绝），以及 UnityArchive 签名识别后的明确拒绝；
- Oodle 通过调用方注入的精确长度安全 decoder 接口接入，Core 不链接或分发专有库；
- Unity 版本解析对齐托管优先级：调用方显式版本 > 仅在 format < 7 时生效的 bundle revision > 文件自身声明；文件版本被 strip 且无覆盖时回退 bundle revision（相对 C# 硬抛的有意偏离，已记录）；
- 可选的逐输入容错策略：`LoadFailurePolicy::SkipInput` 保留能解析的输入并把跳过项记入
  `AssetCollection::diagnostics`；默认 256 MiB 的独立累计预算精确限制保留的路径与错误消息，
  每条消息在格式化时直接截为最多 4096 字节的合法 UTF-8 前缀。Python/Node 均可配置预算并
  通过 count/page API 有界读取，CLI 默认启用并以部分失败退出码报告。
- 递归 Loader 会在 UnityFS/legacy bundle/WebData/ZIP 的整批 child 入队之前把条目数计入共享
  `maximum_discovered_files`，随后对 `VecDeque` 执行可失败预留；超限批次不会先把百万条
  `PendingInput` 写进队列再在出队时失败。gzip/Brotli 只在成功解压出 child 后收费；最终
  SerializedFile 与 Resource 表也在每次增长前 `try_reserve(1)`，因此计数合法但内存不足时返回
  有类型的错误而不是 allocator abort；

### 资产解析和导出

- TextAsset、Font、MovieTexture、AudioClip、VideoClip；
- Texture2D、Texture2DArray、Sprite、SpriteAtlas，以及 JPEG/PNG/BMP/TGA/lossless WebP/raw RGBA 输出；
- 常见整数、浮点、BC、ETC/EAC、ASTC、PVRTC、ATC、Crunch、Xbox 360 和 Switch mip0 纹理路径；
- Material、Shader、Mesh、BuildSettings、PlayerSettings；Mesh 的 position/normal/UV0 通道接受托管 reader 支持的全部浮点顶点格式（Float32、Float16 及归一化 8/16 位），覆盖开启 Vertex Compression 的构建；
- TypeTree dump 文本复刻 .NET 默认浮点渲染（含科学计数法切换阈值与两位指数），并按 InvariantCulture 固定；`TypeValue` 保留 float/double 源宽度，TypeTree JSON 不再输出加宽后的双精度展开；
- `TypeTree` 内部根字段投影会完整遍历并验证对象、数组、map、对齐、尾部字节和
  `SerializeReference` registry，但只保留调用方指定的直接字段；Live2D catalog 读取
  `CubismModel._moc` 已切到这条路径，不再为了一个 PPtr 物化模型中与它无关的百万级数组。
  合成回归在 1 KiB 物化预算下证明完整动态树会稳定拒绝、`_moc` 投影仍能解析，并锁定
  目标字段之后的截断尾部与 reference-type 数据仍必须失败或完整消费，不能用提前返回绕过校验；
- `SerializeReference` registry 的类型身份解析会为每个对象惰性建立借用字符串的排序索引，
  随后的 registry entry 通过二分查找；reference-type 树形校验结果使用按类型编号的布尔缓存。
  两个辅助表都在分配前纳入 `maximum_materialized_bytes`，重复声明仍保持源文件首项优先，
  不再让大量 entry × 大量近似 reference type 或大量不同 type 形成二次方 CPU 放大；
- MonoScript、内嵌 TypeTree MonoBehaviour，以及可信外部完整 schema；
- GameObject/Transform/Renderer/Animator 场景层级、跨文件引用和稳定对象元数据；
- AnimationClip、AnimatorController、Avatar、动画绑定图；
- 模型动画的 GameObject→path 查询和托管兼容的 `FindFrameByPath` 任意后缀匹配均有
  预建索引：精确 key 通过排序表二分，后缀按末级名称精确分组、对完整路径反向 radix 排序，
  二分范围后以 range-min 恢复原 DFS 首项；所选 Avatar 的 hash→path fallback 也按
  `(hash, source_index)` 排序后二分并保留重复 hash 的首声明，因此大量 track 不再逐条线扫
  整棵模型或 Avatar path 表；
- 通用层级 ASCII/Binary FBX 7.4，覆盖普通/蒙皮网格、材质与贴图、骨骼、静态 blend shape 和已验证动画采样；
- ASCII FBX 的材质/贴图规划会按实际可识别绑定数精确执行可失败预留；唯一贴图索引、贴图计划和
  每材质绑定表都不再依赖不可失败增长。重复绑定仍只生成一个 Texture/Video 对象，并保持源遍历中
  首个绑定的 UV offset/scale 获胜；
- Live2D MOC、model3、纹理 PNG、expression、motion、physics、pose、display-info 和参数组；
  同文件散件 expression/motion/physics 回退通过 `(file, role, object_index)` 排序索引二分，
  不再为每个模型扫描集合级角色表；身份索引和散件索引共享显式字节预算；
- 动画绑定图在构建时生成有预算的 `GameObject→Animator` 排序索引；Live2D 的 clip fallback
  通过二分保留源对象首项语义，不再为每个模型线扫全部 Animator binding；
- Cubism clip motion 的 Parameter/Part 目标表先以源顺序、首项优先的有界哈希集合去重，再精确
  统计完整路径和每级后缀所需的 CRC 查询数与输入工作量；哈希表一次可失败预留，字符串、目标数、
  哈希数、累计哈希输入和逻辑索引字节分别有预算，避免逆序唯一目标的二次方去重、逐级 `drain`
  搬移和预留不足后的不可恢复增长；
- Cubism FadeMotion 的 Parameter/Part 目标表在每个模型或包内只构建一次 borrowed 排序索引，
  对目标数、单条/累计字符串和逻辑索引字节分别计费；每条曲线通过二分分类并保持 Parameter
  优先于同名 PartOpacity 的既有语义，规划校验和最终物化共用该索引，不再形成
  `curves × targets` 的重复线扫；
- 动画图的待解析 controller 集合、controller/clip 构建索引均在写入前执行显式逻辑字节预算和
  `try_reserve`；构建期哈希表在最终索引分配前释放，公开 controller/clip 查询使用一次精确预留的
  排序 Vec 二分，不再依赖百万级逐节点 `BTreeMap`/`BTreeSet` 分配；
- 静态 `ModelIr` 的 GameObject、Mesh、Material、Avatar 构建索引使用统一逻辑字节预算和
  `HashMap::try_reserve`，构建表释放后改建精确预留的排序 Vec；四类公开 lookup 保留首次引用的
  源数组顺序并通过二分完成，不再保留八张逐节点分配的树索引；解析 Mesh、Material、Avatar
  源对象时也复用 Loader 的 collection-wide pathID 排序索引，不再为每个资产重新线扫整个对象表；
- Loader 的 collection-wide 对象名称/container 元数据在构建时使用显式计数与逻辑字节预算，
  新 identity 先检查限制并对 owning Vec 和临时 HashMap 执行 `try_reserve(1)`；MonoScript class
  临时表共享同一预算。构建完成后释放临时表并把唯一元数据排序为 Vec，公开精确 lookup 通过
  二分完成；重复名称、container 和 MonoScript identity 保持源遍历中的后写覆盖语义，且不重复计费；
- 场景层级的 Transform-owner cache 与最终 `GameObject` lookup 共用显式索引字节预算；
  cache 采用预留后写入的可失败哈希表，最终索引采用确定排序 Vec 二分，不再让百万级合法层级
  通过不可失败的逐节点树分配把内存不足升级为进程 abort；
- 有界递归解包，以及拒绝符号链接、同目录临时文件和原子发布的安全导出；文件系统
  名称中的非 UTF-8 数据按 Unix 字节或 Windows UTF-16 的现有 replacement 语义流式清洗，
  不再先物化完整 `to_string_lossy` 临时字符串。replacement 后的诊断标签长度先计入单项和
  累计路径预算再精确分配，输出组件则在逐字符清洗时直接执行 240 字节便携上限。

### 公开 API 和发布

- Rust 高层 `Studio` API 可直接从路径、单个内存区域或多文件内存集合加载；
- Rust 高层 `Studio` 的整场景 OBJ/MTL 现与 FBX、单 Mesh OBJ 和 TypeTree 文本一样，通过有界 writer 与 `try_reserve` 物化；材质库名的拥有型副本也使用可失败分配。Python 与 Node 直接复用这条 Core 路径，不再先写入不可失败增长的普通 `Vec`；
- binary FBX 的高层场景投影会在构造几何、蒙皮、形变、动画和 Connections 数组前检查长度并执行可失败分配，输出预算随后在编码器每个内部缓冲增长前生效，不再先完整物化后比较长度；数值数组直接流入 zlib，不再额外复制一份完整未压缩字节。低层 `FbxBinaryWriteLimits` 另在递归编码前限制最终输出、节点、属性、非空深度和单数组元素；旧 `write_fbx_binary`/`read_fbx_binary` 保持兼容并包装默认预算，显式 limits 失败不会向调用方 writer 留半个文件。公开属性层覆盖 FBX 7.4 的全部标准 scalar/array code，含 raw 与 deflated `b` 布尔数组以及非零字节为 `true` 的读取语义。独立 verifier 现在真正执行输入、节点、属性、非空深度、单数组元素和累计展开工作预算；文件读取本身封顶为 `maximum_input_bytes + 1`，不会先由 `Path.read_bytes()` 无界物化后才检查。压缩数组以 64 KiB 有界块实际解压，只接受声明长度精确相等的单个完整 zlib member，并拒绝截断、额外输出和压缩流尾随字节；所有 offset 使用 checked arithmetic，并完整验证 footer ID、1–16 字节 padding、重复版本、保留区、结尾 magic 与文件精确结束位置；恶意超大 count、解压炸弹、过深树、提前 null terminator、损坏/truncated/trailing footer 和精确输出上限均有回归；
- 原生 CLI 的 Live2D/FBX 候选、输出名称、便携大小写键和只读统计表同样使用可失败分配；`info`/`list`/`inspect` 先在有界增长的哈希表中计数，再按键排序后输出，既不依赖哈希随机顺序，也不为每段转义文本额外物化一个 `String`；
- CLI `inspect` 的独立目录扫描按 replacement 后的 UTF-8 长度执行与 Loader 相同的单路径和
  累计路径预算；失败文件名直接逐字符 replacement 并 `escape_default` 写入输出流，不再先
  完整构造 lossy 路径再转义；成功根路径和递归 gzip/ZIP 标签也通过组合式 `Display` 直接
  写入，不再为每层 `display().to_string()` 或 `format!` 复制完整前缀；
- Python 提供惰性/分页枚举、资源读取、主要专用 reader、schema/ACL/Oodle 适配器、导出和解包；`SceneLimits` 可独立收紧 GameObject、组件、Transform 子项、材质、骨骼和层级边预算；Core 的 I/O、无效数据和未支持功能分别保留为标准 `OSError` 子类、`ValueError` 和 `NotImplementedError`，所有 Rust→Python 字节复制以及可能很大的场景、候选、图片层和报告转换都使用可失败分配，并以 `MemoryError` 报告内存不足；
- Python wheel 使用 `cp39-abi3`，CI 构建 Linux、Windows、macOS 的 x86-64/ARM64 组合，并在构建解释器和 Python 3.14 上安装测试；
- Python sdist 会被重新构建成 wheel 并执行完整 API 测试；
- `AssetCollection` 的 SerializedFile/资源表通过只读 slice 公开；低层调用方以
  `into_parts` 无克隆地取回无索引的拥有型文件/资源/诊断表，修改后用 `from_parts` 重建，
  再显式建立引用/资源索引或解析对象元数据。外部安全 Rust 代码不能在索引建立后直接改
  PathID、换序或插入同名资源，因此不会让首匹配语义与缓存表静默分叉；编译失败 rustdoc
  回归锁定这条边界，parts 往返回归则锁定诊断保留、派生 metadata/index 丢弃和重建；
- 旧 `unity-rs-ffi` 已于 2026-08-23 从仓库删除，而不是继续作为 excluded crate 保留；
  root workspace 不再需要 `exclude` 例外，本地遗留的独立 target/Cargo.lock/dylib 缓存也已清理。
  Rust 公开 `Studio`、Python stub 与 Node 声明本来已经直接持有高层对象、没有 context handle；
  交付范围门禁现在同时拒绝旧 FFI 源文件和 `StudioContext`/`context_id`/`contextId` 回流，
  反向自测逐个恢复两份旧源码路径并污染三类公开面，均必须失败。内部 parser/动画/FBX 的
  私有 `*Context` 仍保留，因为它们是有生命周期的借用状态，不是用户 API 或数字 handle。
  此外，原先为了让绑定共享文件系统路径转换而从 Core 根部隐藏导出的两个
  `#[doc(hidden)] pub` helper 也已移除；doc-hidden 符号仍属于 Rust semver 公共面，不能作为
  绑定内部接口。Core walker 现为 `pub(crate)`，Python 和 Node 各自保留私有两趟实现，并
  分别维持 `MemoryError` 与 napi 错误分类；交付范围门禁会阻止这两个 helper 名称重新从
  Core 根部导出。
  整个 `crates/unity-rs-ffi` 路径（包括仅含 ignored cache 的空壳）、root `Cargo.toml` 与
  `.gitignore` 都不得保留旧 crate；全部 Core、
  Python 与 Node Rust 源文件还会扫描公开声明，避免非根模块重新引入 `pub *Context`。
  每个 workspace package 的生产 target 还必须精确等于 Core lib、CLI bin、Python cdylib 或
  Node cdylib；在现有 package 里偷加第二个 GUI/bin target 不再能绕过包名检查。全部第一方
  Rust 源同时拒绝 `no_mangle`/`export_name` 和公开 C/system ABI 函数；内部受控 codec
  adapter 仍可声明调用方函数指针，但不能重新发布一套自定义 C ABI。交付范围反向测试
  8/8、CI 结构反向测试 13/13 通过；删除及 helper 私有化后重新执行完整
  `outputs quality rust python node typing security cross` 与 Linux amd64/arm64 原生矩阵，
  Core/CLI、release CLI、Python wheel 和 Node addon/npm 全部通过且零组跳过。Python
  安装后公开面本就双向核对 `__all__` 与 stub；Node addon 另新增精确顶层导出断言，当前
  `Object.keys` 必须恰为 `UnityRs`，不能静默多出未声明的 `Context` runtime class；
  Python/Node 的 `cdylib` 仍会由 PyO3/napi 生成各自宿主规定的模块入口，这是两种语言加载
  native extension 所必需的 ABI，不是本项目另行设计、发布或承诺兼容的 C API/context；
- Node 提供本地模块、TypeScript 声明、同步 API 和 libuv worker Promise API；`readAudioClip` 与 Core/Python 共用 `auto`/`raw`/`wav` 策略和输出预算，旧的 raw-only `readAudio` 继续兼容；`exportWithOptions` 完整暴露 Core 的模式、命名、图片/音频、JSON、覆盖和全部单项/累计预算，旧的紧凑 export 调用继续兼容；Tuanjie ACL decoder 可通过 worker 注入整场景 ASCII/binary FBX、选定 GameObject FBX、Cubism motion 和完整 Live2D 包，完整包调用还能同时消费可信外部 schema；旧的 `scene(maximumGameObjects?)` 调用保持兼容，`sceneWithLimits` 可分别限制 GameObject、组件、Transform 子项、材质、骨骼和层级边；
- Node tarball 门禁不再只数 `.node` 文件个数：它按当前 CI 平台精确要求
  `darwin-{x64,arm64}`、`linux-{x64,arm64}-gnu` 或 `win32-{x64,arm64}-msvc` 文件名，随后
  真正 `npm pack`、在临时消费者中离线安装该 `.tgz`，再从安装目录 `require()` 并断言运行时
  顶层导出恰为 `UnityRs`。安装后的 `index.d.ts` 还会被重新解析，并与安装后的 native
  class 双向逐项核对 static method、instance method 和 getter；当前精确锁定 85 个方法与 4 个
  属性，另以保持数量不变的重命名反向测试证明不是只数成员。源码树能加载但发布包漏文件、
  声明/运行时漂移或带错架构时都会直接失败。macOS
  arm64 debug/release 与 Linux amd64/arm64 release 容器已实际通过；2026-08-24 的正式
  GitHub runner 又完成 Windows x64/arm64 以及其余五个平台的发布包验证；
- 原生 CLI 发布任务会先把二进制、`LICENSE`、第三方 notices 与完整许可证集合 stage 到最终
  上传目录，再执行该目录中的 `unity-rs[.exe] --help`；不再只运行构建目录原件后假定复制品
  等价。矩阵审计同时锁定六个平台的 staged 路径和 stage-before-smoke 顺序，本地临时 artifact
  与 Linux amd64/arm64 容器也执行同一份复制后的二进制；2026-08-24 的手工发布矩阵已让六个
  CLI artifact 和六个 Node artifact 全部在对应 GitHub runner 上构建、smoke 并上传；
- Rust Core crate、Python wheel/sdist、Node 包和原生 CLI 都有独立发布校验。
- 四类发布产物都携带项目 `LICENSE`、第三方归属摘要和由锁定依赖图生成的完整许可证文本；生成器当前覆盖 97 个非开发依赖，遇到缺少许可证文件、依赖更新或分发副本漂移会直接失败。
- 交付范围由 `tools/check_delivery_scope.py` 与各产物内容检查共同锁定：workspace 只能包含 Core、CLI、Python 和可选 Node，三个前端必须直接依赖 Core；仓库不得重新出现旧 `unity-rs-ffi` 源文件，Rust `Studio`、Python stub 与 Node 声明不得公开 `Context`/`context_id` handle；发布 crate、wheel/sdist 与 npm 包拒绝 GUI、旧 FFI 和 C# 工程文件。托管 oracle 仍可作为仓库测试输入存在，但不会进入运行时依赖或二进制交付面。

## 当前验证证据

截至本文更新时间，当前工作树已通过：

- `cargo fmt --all -- --check`；
- `cargo clippy --workspace --all-targets --locked -- -D warnings`；
- `cargo test --workspace --locked --no-fail-fast`；Python crate 是由
  Python loader 加载的 PyO3 `cdylib`，其 Cargo target 明确设置
  `test = false`，不要用 `--all-targets` 强迫 Cargo 在 macOS 上把它当成
  独立测试可执行文件运行；那条路径绕过 Python loader，因而没有
  `Python3.framework` 的运行时 rpath。wheel 安装后的公开面和完整 API
  测试由下方独立 Python 门禁负责；
- **当前工作树的 14 个本地验收分组已于 2026-08-23 全部实跑通过、零跳过**：先执行
  `outputs quality rust python node typing security cross` 与 `linux`，覆盖格式/声明/API/
  六平台矩阵/交付范围审计、Clippy、rustdoc、workspace、RustSec、输出格式、Windows/Linux
  交叉编译、CLI/Python/Node 发布包，以及 Linux amd64/arm64 原生运行；再执行
  `cli-package oracle audio python314 unitypy`，覆盖独立 release CLI staging、.NET 托管差分、
  MonoBehaviour schema 生成器、vgmstream 音频差分、Python 3.14 abi3 和 UnityPy 第三实现差分。
  UnityPy 组使用本机已有 UnityPy 的 Homebrew Python 重新构建并安装同一 wheel，不依赖
  Xcode Python 的空 system-site；没有联网补依赖或把缺失前置条件记成成功；
- **加载/解包文件系统非 UTF-8 路径的分配边界已于 2026-08-22 收口**：此前 Loader 的
  `LoadPath::from_path`/`from_precharged_path` 与 extractor 的单文件名、目录相对组件和
  诊断标签都会先完整调用 `to_string_lossy()`，Unix 恶意无效字节可在单路径或累计路径预算
  拒绝它之前膨胀成 replacement 字符串。现抽出一个 Core 内部公共流式遍历：Unix 按
  UTF-8 错误边界发出 replacement character，Windows 按 `decode_utf16` 把未配对 surrogate
  替换，保持现有 lossy 命名语义；逐案例回归还把结果和标准 `to_string_lossy` 对照。
  Loader 与 extractor 都先扫描 replacement 后的精确 UTF-8 长度、提交单项/额外累计预算，
  再只分配一次；输出组件则在逐字符清洗控制字符和 Windows 保留字符时同步执行 240 字节
  便携上限。Unix 回归锁定 80 个无效字节恰好得到 240 字节、81 个在 243 字节处拒绝、
  混合无效字节/保留字符，以及 Loader/extractor 的单项和累计预算失败都不提交预算。
  本轮 Core lib 547 项通过、10 项可选 vgmstream oracle 忽略，畸形输入 6/6 与严格
  Clippy 通过；完整
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`
  也通过且零组跳过，包含 Windows x86-64 Core/CLI/Python 交叉编译。另执行
  `tools/local_ci.py --fail-on-skip linux`，在 Linux amd64/arm64 两个原生容器中实际创建并
  加载无效字节文件名，Core/CLI 全测、release CLI、Python wheel 和可选 Node addon 的
  安装后测试与打包全部通过，零组跳过；
- **Python/Node 报告路径与 CLI 诊断的最后几处整串 lossy 分配已于 2026-08-22 收口**：
  Core 的 Loader/extractor 已经能按平台 replacement 语义流式遍历 `OsStr`，但 Python 的
  export/extraction 报告仍先 `path.to_string_lossy()`，再把这份可能已经分配好的 Cow 复制到
  可失败 String；Node export report 同样如此。现两个绑定各自使用私有两趟 walker：第一趟
  精确计算 replacement 后的 UTF-8 长度，Python 用自己的 `try_reserve_exact` 保持
  `MemoryError` 分类，Node 用 napi 错误映射，第二趟只复制一次并复核长度；Core 内部使用
  同一语义，但不再向公开 crate 根泄漏绑定 helper。CLI 的参数诊断和
  `--mono-schema` 错误标签也改为把 replacement/escape 直接写入 formatter，不再先
  `to_string_lossy` 或 `display().to_string()`。Core 逐案例仍与标准库 lossy 结果比对，Node
  另用真实 `0xff` Unix 路径锁定绑定接线，Linux wheel API 还会把带 `0xff` 的输出根传入
  Python、实际导出并断言报告路径包含 replacement character；Core 定向 1/1、Node 4/4、CLI 33/33、workspace
  严格 Clippy 和重新构建 wheel/sdist 后的完整 Python API 门禁均通过。随后完整
  `outputs quality rust python node typing security cross` 与 Linux amd64/arm64 原生矩阵
  再次通过且零组跳过；首次 arm64 容器启动只遇到一次 Docker Hub 鉴权 EOF，严格重试后
  两架构 Core/CLI、release CLI、Python wheel 和 Node addon/npm 全部实际运行通过；
- **CLI inspect 非 UTF-8 路径与递归标签已于 2026-08-22 收口**：其目录队列此前只按平台编码原始
  字节计费，打开失败后又对完整路径执行 `to_string_lossy()`，无效字节既能绕过名义上的
  UTF-8 路径预算，也会在转义输出前产生 replacement-expanded 临时 String。现根目录和每个
  child 都先流式计算 replacement 后的精确长度、事务性提交单项/累计预算，再按较小的原始
  编码长度复制 `PathBuf`；错误行通过 `EscapedOsStr` 把 replacement character 的
  `escape_default` 结果直接写入 formatter；成功根路径使用 `LossyOsStr` 流式 replacement，
  gzip/Brotli 与 ZIP entry 通过借用父标签的 `NestedInspectLabel` 逐层组合，递归深度增加时不再
  重复分配整条标签。单测覆盖原始 2 字节→UTF-8 4 字节的单项拒绝、
  精确边界、child 全路径累计拒绝与预算不提交；Linux 进程测试用实际 `bad-0xff.gz` 截断文件
  锁定部分失败退出码、ASCII `\\u{fffd}` 诊断和稳定 summary；
- **CLI 路径参数的二次复制已于 2026-08-22 收口**：进程入口虽已把参数限制为 65,536 项、
  单项 1 MiB 和累计 64 MiB，并以 `try_reserve` 保存第一份 `OsString`，后续 read-only、裸/legacy、
  FBX/OBJ、批量 FBX、Live2D、export 与 extract 解析器仍用 `PathBuf::from(&OsString)` 再复制
  input/output；allocator 失败因此绕过 `Result`，五个现代写命令的位置 `Vec` 也会在首个路径
  push 时不可失败地增长。现七类入口统一按平台原始编码计算精确字节数，先
  `PathBuf::try_reserve_exact` 再复制，不经过 UTF-8；每个双路径命令先可失败地预留完整两项，
  第三项在复制前立即拒绝。解析阶段其余集合逐一核对后只剩可重复 `export --class` 的过滤表仍
  直接 `push`；现同样在每项前 `try_reserve`，保留调用顺序、重复 ID 和负 synthetic class ID，
  无效值不会改变已有表。回归锁定 Unicode 路径逐字保真、两项表不因第三项增长，以及
  `[28, 114, -187, 28]` 的精确过滤顺序；CLI 单元 33 项与全部进程测试、严格 CLI Clippy 均通过；
- **Core binary FBX 分配边界已于 2026-08-22 收口**：Rust 公开 parser 原先允许
  资产声明控制递归深度，并把 zlib 数组直接无界 `read_to_end`，property offset
  也存在未 checked 的加法；writer 则在完整文件和数组 scratch 分配完后才比较
  `maximum_output_bytes`。现 verifier 有显式输入/节点/属性/深度/数组元素/累计拥有型
  分配预算，压缩数组按 `count × element_width` 精确解压，只接受一个完整 zlib member，
  并拒绝截断、多余输出和压缩流尾随字节；所有拥有型
  容器在增长前 `try_reserve`；writer 的文件、record、property、children、zlib 和
  footer 缓冲都在增长前执行输出预算，数值数组直接流入 zlib，不再复制一份完整 raw
  bytes。场景投影的顶点/索引、cluster、blend shape、关键帧和 Connections 也在 checked
  cardinality 后通过 `try_reserve` 增长；同轮修正 binary 动画把 Y/Z 曲线也连接为 `d|X`
  的错误，并锁定三个 component connection。公开 parser 的深度预算现在只计非空 record，
  所以零预算可验证空 root list，位于深度边界的 list terminator 也不会被误算成一层。
  parser 读完根终止符后继续验证完整 footer，截断、尾随字节以及六个 footer 区段的损坏
  writer 现在也有对称的 output/node/property/depth/array limits，默认 256 层会在递归前
  拒绝过深的调用方节点树；零深度仍允许空 root list，任一预算失败都发生在调用方输出流
  写入之前。body 加 footer ID 正好对齐时仍要求完整 16 字节 padding。24 项定向测试覆盖
  精确预算、全部标准属性、raw/deflated boolean array、压缩/未压缩 round-trip、超大
  count、解压炸弹、深度/节点/分配上限、提前 terminator 和场景投影
  分配溢出。独立 Python verifier 另以不调用 Rust writer 的 fixture 锁定输入、节点、
  属性、非空深度、单数组元素和累计展开字节六类预算；默认 256 层上限也避免恶意树
  落到 Python `RecursionError`。修复后的完整工作树执行
  `tools/local_ci.py --fail-on-skip quality rust python node typing security` 以及独立
  `outputs` 格式校验组全部通过，零组跳过；
- **主导出文件名分配与可移植边界已于 2026-08-22 收口**：此前资产名和源文件名
  先通过 `String::with_capacity`、`clone` 或 `to_owned` 完整复制，最终输出组件又没有
  把扩展名及碰撞时追加的 ` @PathID` 纳入统一上限。现源文件分组前缀、清洗后的资产
  名、文件名格式后缀、扩展名和最坏碰撞后缀共同服从 240 字节预算；所有输入派生的
  String 副本均先 `try_reserve_exact`，超限对象作为稳定导出失败报告而不是交给不同
  文件系统产生不一致结果。只含任意长首尾空格/点号的名称仍保持旧语义成为
  `unnamed`，但通过借用切片完成，不物化整串。定向测试覆盖 ASCII/三字节 UTF-8、
  240/241 字节边界、扩展名、碰撞后缀及原有清洗/PathID 格式。修复后的完整工作树
  执行 `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security`，
  Rust/Python/Node 构建、测试、发布包、类型、安全与六类输出校验全部通过，零组跳过；
- **主导出报告、碰撞索引和落盘路径已于 2026-08-22 收口**：此前文件成功原子发布后才
  复制 `ExportRecord.source` 并为 `report.exported` 扩容；allocator 失败会让 API 返回错误，
  但磁盘上已经留下调用方无法从报告识别的成功文件。碰撞 `HashSet` 又为每个对象保存
  `absolute_output_path.to_string_lossy().to_lowercase()`，百万对象会重复保留同一条长 output
  root，Unicode lowercase 和临时错误文本也走不可失败分配。现成功 report slot 与完整
  record 在创建临时文件之前准备，发布后只执行已有容量的 `Vec::push`；失败文本通过
  fallible `fmt::Write` 保持原错误族。每个源文件分组使用独立碰撞表，只保存最多 240 字节
  组件的精确 fallible Unicode lowercase key，命名顺序和跨组同名语义不变。output root 的
  lexical normalization、分组/文件 join、逐组件安全目录检查、临时文件名及 Windows
  replacement backup 全部先 checked 计算并 `try_reserve_exact`。回归覆盖 Unicode
  `İ -> i + combining dot`、两组同名均保持 `demo.lua`、failure family、空 child、路径
  normalization、深目录创建和 replacement backup 名；34 项 export/image/model 相关测试、
  Core 全目标编译和严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，
  Rust/Python/Node 构建、测试、发布包、类型、安全与六类输出校验全部通过，零组跳过；
- **三条原子发布路径的提交点语义已于 2026-08-22 统一**：主导出、递归解包和模型同级
  贴图都使用同目录临时文件 + hard-link no-clobber；主导出/解包覆盖时另有 replacement
  backup。旧实现会在目标 hard-link 或新文件 rename 已经成功后继续以 `?` 删除临时 link
  或旧 backup，清理失败便把已提交的目标重新报告成失败，形成磁盘状态与 report 不一致。
  现 hard-link/rename 成功就是提交点：临时 link 删除失败时保持 `persisted=false`，由 Drop
  再试；replacement 成功后把清理目标切换为旧 backup，同样先尝试并由 Drop 重试。清理
  错误不再改变已发布结果，写入/同步/链接/rename 本身的失败仍照常传播。解包 backup 名
  同轮去掉不可失败 `with_extension`，改为 checked/fallible 组件与路径构造。19 项
  extraction、34 项 export/image/model 和 11 项 scene-texture 定向测试、Core 全目标编译及
  严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，
  Rust/Python/Node 构建、测试、发布包、类型、安全与六类输出校验全部通过，零组跳过；
- **模型同级贴图的名称、诊断和发布报告已于 2026-08-22 收口**：自动路径此前用
  `BTreeMap`、普通 `Vec::push`、`String::clone`/`to_owned` 和 `format!` 保留资产派生的
  名称、材质 property、skip reason 与 written path；公共 `push_texture` 又允许调用方把
  `../outside.png` 送入 `directory.join`，从而越过目标目录。现对象/名称索引改为只用于
  查找、不参与输出顺序的 `HashMap` 并在插入前 `try_reserve`；名称清洗、碰撞后缀、保留
  名前缀、property、diagnostic 和路径副本均先 checked 计算并可失败分配，对象解析复用
  collection 的 PathID 索引而不再线扫。写盘边界会在创建临时文件前重新验证空名、绝对
  或多组件路径、控制/保留字符、Windows device name 和 240 字节上限；成功 written report
  的全部 slot 也在第一张图片发布前预留。回归覆盖 `../`/绝对/嵌套/`CON.png`/241 字节
  手工名称、资产名清洗、三路稳定碰撞、跨模型同对象复用、count/累计字节预算、失败诊断、
  no-clobber 与放弃临时文件清理；11 项 scene-texture 定向测试、Core 全目标编译及严格
  workspace Clippy 已通过；完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security`，Rust/Python/
  Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、无头交付范围
  与六类输出格式校验全部通过，零组跳过。同一快照继续执行严格
  `cli-package oracle cross python314` 与 `linux`：release CLI 的构建/运行/法律文件 staging、
  托管差分和 MonoBehaviour schema 生成器、Linux x86-64 workspace 与 Windows x86-64
  Core/CLI/Python 交叉编译、Python 3.14 abi3 wheel
  的安装后公开面/完整 API，以及 Linux amd64/arm64 容器中的 Core+CLI 全测试、release CLI、
  Python wheel 和 Node 24 release addon/JS/TypeScript/npm tarball 均通过，仍为零组跳过；
- **模型贴图共享名称索引的累计预算与大小写碰撞已于 2026-08-25 收口**：
  `SceneTextureLimits.maximum_textures` 只限制单个模型解析出的唯一贴图，而 CLI 批量 FBX 会在
  全部候选之间复用一张 `SceneTextureNames`；旧表为每个对象永久保留实际文件名和 collision key
  两份 `String`，却没有累计字节预算，最多百万候选可以在每个模型都满足 4,096 张上限时继续放大
  隐藏驻留。旧 key 还是大小写敏感的，`Body.png` 与 `body.png` 在 Linux 上被视为不同，到了常见
  Windows/macOS 文件系统却指向同一个文件。现新增 64 MiB 默认
  `maximum_name_index_bytes`，由 Rust `SceneTextureLimits`、Python/Node `ModelTextureLimits`
  共同暴露；实际名称副本与 Unicode lowercase identity 按精确 UTF-8 长度累计，跨模型复用同一
  allocator 时预算继续生效。预算失败发生在表插入前且不改变名称、碰撞游标或计数；所有名称先按
  lowercase identity 认领，大小写不同的第二个对象稳定得到 ` (1)` 后缀。Core 回归锁定
  `Body.rgba` 的 17/18 字节公开读取边界、`Body.png`/`body.png` 的 39/40 字节事务边界，以及
  `İ.png` 的 6 字节实际名加 7 字节 lowercase key；安装后的 Python 与 Node 测试按各自真实 fixture
  文件名计算少一字节预算并验证同一错误，TypeScript 与 Python 3.9 类型消费也显式传入新字段。
  提交 `31d1288` 的 scene-texture 定向测试 16/16、严格 workspace Clippy，以及
  完整 `quality rust python node typing oracle` 零跳过本地门禁通过；公开常规矩阵
  [32785686979](https://github.com/seiunx-dev/unity-rs/actions/runs/32785686979) 为 16 个实际 job
  全绿、2 个手工发布条件 job 正常跳过、0 失败；
- **FBX/OBJ/贴图的多文件发布已于 2026-08-22 改为整批事务**：旧 CLI 的单文件 FBX、
  `split-objects`/`animator` 候选和 OBJ 都会先发布模型，再写同级贴图或 MTL；例如目标 OBJ
  尚不存在、同名 MTL 已存在时，命令会失败却留下
  一个新 OBJ，且该文件引用的是调用前已有的 MTL。贴图集合本身也会逐张立即提交，后面的
  名称校验、写入或发布失败会留下前面已经写成的子集。现一组贴图在任一后续失败时逆序删除
  本次调用新发布的文件，已有而被 no-clobber 跳过的文件从不进入回滚集合；单 FBX 与每个
  batch candidate 都在自己的贴图整批成功后最后发布，OBJ 则按贴图、MTL、OBJ 的顺序提交，
  MTL/OBJ 的晚失败会回滚同一命令此前新建的文件。所有载荷仍先在同目录临时文件中完整写入、
  flush、sync 和关闭，最终 hard-link no-clobber 的原子边界不变。同轮还修正 CLI 自己的
  FBX 与单 MOC 临时文件：hard-link 成功即为提交点，删除临时名字失败只让 `Drop` 重试清理，
  不再把已经可见的目标重新报告成失败。完整 Live2D 包也在临时目录 rename 并同步后完成提交；
  之后的发布锁删除及其目录 sync 只属于 cleanup，删除失败由 `Drop` 重试，不再反转包的成功状态。
  同轮复核发现完整包虽会逐组件拒绝输出根里的符号链接，单 MOC 仍直接 `create_dir_all`，会沿调用方
  选中的 symlink 把文件写到目录外；两条命令现共用同一安全目录创建器，在创建任何输出前拒绝
  symlink 和非目录组件。Unix 进程回归用真实 symlink 指向空目录，锁定单 MOC 返回运行错误且真实
  目标保持为空。继续向下审计又发现 FBX 与 Live2D 的目录创建器会向上遍历缺失路径，并为每一级
  保存一份完整 `PathBuf`；深层 CLI 参数因此产生 O(深度×路径长度) 的复制，且 FBX 的相对深层
  新目录没有绝对锚点时会直接失败。两套实现现进一步收敛为公共线性 helper：相对路径只规范化
  一次，随后用一个预留到最终长度的缓冲逐组件检查/创建；所有中间组件都检查 symlink/非目录，
  所以即使 symlink 后面已经存在真实子目录也无法绕过。macOS 仅为系统固定 `/var`→`/private/var`
  和 `/tmp`→`/private/tmp` 保留精确 canonical 例外。回归覆盖 symlink 后已有目录及 64 层相对 FBX
  输出；FBX/OBJ 15 项、单 MOC 5 项与完整包 7 项端到端测试均通过。
  回归锁定贴图第三项非法时首项消失而既有文件保持、
  FBX 在完整贴图批次前不可见、既有 FBX 冲突时新贴图回滚、模拟临时名 cleanup 失败仍返回
  已提交，以及既有 MTL 冲突时 OBJ 不存在且旧 MTL 字节不变。批处理原有 16 GiB 累计上限也
  从只统计 FBX 本体改为统计每个 FBX 加上该候选实际新发布的贴图文件大小；共享/既有而跳过的
  文件不重复计费，超限在模型提交前回滚贴图。单次 `fbx`/`obj` 的
  `--maximum-output-bytes` 也从只限制主模型文件（OBJ 甚至分别给 OBJ 和 MTL 各一份完整额度）
  改为限制本次新发布的模型文档、MTL 和贴图累计字节；预算在 MTL/OBJ/FBX 提交前核对，超限
  回滚本次新贴图，已有或共享而跳过的贴图不重复计费。新增端到端回归先实测同一 fixture 的
  OBJ/MTL 大小，再用“每个文件都能单独通过、两者合计必超”的额度验证两份最终文件都不存在；
  另以失败注入锁定 Live2D 锁 cleanup 不反转提交。
  单 MOC 的 4 GiB 累计预算此前还在发布前永久增加 `scheduled_bytes`，所以一个因目标已存在、
  写入或 hard-link 失败的模型仍会占住额度，后面的模型可能在磁盘实际为零时被误拒；summary
  却只报告成功的 `exported_bytes`。现预检只根据已成功字节计算候选总量并返回它，hard-link
  提交成功后才把该值写入状态；失败不占额度，成功路径也不再在提交后执行第二次可能失败的
  checked add。低上限状态回归锁定两次未提交的满额度预检都成功且计数保持零，模拟第一次提交后
  再加一字节才拒绝；单 MOC 5 项端到端与严格 CLI Clippy 通过。
  scene-texture 12 项、CLI 单元 33 项和 FBX/OBJ
  CLI 15 项定向测试全部通过。完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，格式、
  结构审计、Clippy、rustdoc、workspace 测试、RustSec、输出格式、Windows 交叉编译、Node
  debug/release addon 与 npm 包、Python wheel/sdist/安装后 API 和严格 Python 3.9 类型消费
  全部通过，零组跳过；`tools/local_ci.py --fail-on-skip linux` 又在 Linux amd64/arm64 原生
  容器中通过 Core/CLI、release CLI、Python wheel 与 Node addon/npm 包全部运行门禁，仍为
  零组跳过；
- **WebData/UnityFS 目录的派生文件名已于 2026-08-22 改为可失败分配**：两条 reader
  原本已经在读取完整 path 时执行 UTF-8/终止符/长度上限和 `try_reserve`，随后却对
  `/` 或 `\\` 后的便携叶子再次直接 `to_owned`；这份为公开 entry table 保留的第二个
  String 因而仍可在 allocator 失败时越过 Core 的 `Result` 边界。现先借用定位叶子，按
  精确 UTF-8 字节数 `try_reserve_exact` 后复制；完整 path、派生 file name、entry Vec 和
  payload Region 均保持各自既有合同。回归真实解析 `folder\\目录\\表.bin`，逐字段验证
  Windows separator 与 Unicode，并锁定 UnityFS 的低 path limit 在复制前拒绝；7 项 WebData、
  31 项 bundle/legacy-bundle 测试及严格 workspace Clippy 已通过；完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **Font/MovieTexture/AudioClip/VideoClip 的派生字符串已于 2026-08-22 收口**：四条
  reader 原先分别用 `to_owned` 保存静态扩展名和从 VideoClip 原始路径截出的扩展名；旧
  AudioClip 的外部资源名还通过 `format!("{}.resS", loaded.path)` 构造，不仅不可失败，
  也可能在原文件名刚好位于 `maximum_string_bytes` 边界时通过追加后缀绕过调用方预算。
  现所有拥有型扩展名均按精确长度 `try_reserve_exact` 后复制；VideoClip 只借用最后一个
  portable component 的 extension，无后缀时仍稳定返回 `.video`；legacy `.resS` 名先 checked
  计算完整长度、比较字符串预算，再一次预留并写入。回归覆盖 Windows separator + Unicode
  VideoClip path、无后缀 fallback，以及 `legacy-stream` 名本身恰好允许但集合 path 追加
  `.resS` 后必须拒绝；20 项 simple-asset 测试及严格 workspace Clippy 已通过；完整工作树
  随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **外部 MonoBehaviour schema 文档已于 2026-08-22 进入统一预算**：CLI 原先对每个
  `--mono-schema` 直接调用 `fs::read`，会在 Core 看见文档前按整个文件大小分配；Core 的
  `from_json` 随后把 serde JSON 已拥有的 assembly/class/namespace/version 和每个节点的
  type/name 再用 `to_owned` 复制，既没有条目/节点/解码后字符串总预算，重复文档也能逐份
  绕过任何单文件限制。现新增 `MonoBehaviourSchemaDocumentLimits`：默认限制 256 MiB 文档、
  100,000 个条目、单条 100,000/全文件 1,000,000 个节点、单字符串 16 MiB 和解码后字符串
  总计 256 MiB；所有保留字符串先计费并精确可失败预留，JSON 中非字符串 namespace 不再
  静默变为空串。CLI 用 16 KiB 固定缓冲逐块读取，最多接受 1,024 份文档，并把文档字节、
  entries、nodes 与 decoded strings 的剩余额度贯穿后续每一份文件；因此大文件和重复参数
  都会在扩容前稳定返回 usage/invalid-data 错误。回归覆盖字节边界、entry/per-entry/total
  node、Unicode escape 解码后的 UTF-8 长度、单/总字符串、非字符串 namespace、流式读取
  和重复 flag 总预算；8 项 Core 与 3 项 CLI schema 定向测试、严格 workspace Clippy 已通过；
  完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **Live2D 散件回退的内存与 CPU 放大已于 2026-08-24 收口**：散落在同一 SerializedFile 中的
  `CubismFadeMotionData`/`CubismExpressionData` 原先先从集合级 role table 收集第二份
  identity `Vec`，之后才在逐项投影时触发 motion/expression 数量限制；角色上限达到百万级时，
  即使调用方给了更低的输出数量预算，也可能先产生一次与整表同量级的重复分配。第一轮先改成
  惰性稳定遍历，消除了 identity 副本，却仍会让每个模型分别为 expression、motion 的计数与
  投影以及 physics 首项重新扫描全表，最坏形成 `models × roles` 的 CPU 放大。现保留原有按
  `(file_index, object_index)` 二分的身份表，并额外一次建立按
  `(file_index, CubismRole, object_index)` 排序的散件索引；两次 `partition_point` 即可得到同文件、
  同角色的连续 slice，slice 内仍按对象发现顺序排列，因此 expression/motion 的稳定顺序和
  physics 首项语义不变。两张表在任何预留前按实际元素大小统一计入新增的
  `maximum_role_index_bytes`，默认 256 MiB，不为加速获得无界第二份内存。
  Texture 源名称、MOC identifier 名称和空名称 fallback 的拥有型副本也改为精确可失败复制。
  回归以 16,384 个逆序、交错角色条目验证输出仍为发现顺序、physics 仍取第一项，并重复
  16,384 次查询锁定每次少于 32 次索引比较；另一项以精确容量低一字节证明在分配前稳定拒绝。
  Core 常规测试 567 项通过、12 项有独立可选 oracle 的音频测试忽略，6 项畸形输入扫描及严格
  Clippy 通过。完整工作树随后执行
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，Rust/Python/Node 构建、
  workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过；提交 `ee6f668`
  的公开常规矩阵
  [32700977598](https://github.com/seiunx-dev/unity-rs/actions/runs/32700977598) 为 16/16 验证 job 全绿；
- **Live2D clip fallback 的 Animator 扫描已于 2026-08-24 收口**：`PackageState::model_clip_keys`
  原先虽然只构建一次 `AnimationGraph`，却会为每个需要动画回退的模型再对全部 Animator binding
  执行一次 `.iter().find`；默认十万模型和百万 binding 足以把已验证的普通对象布局放大为
  `models × animators`。现 `AnimationGraph` 在构建时一次生成按
  `(SceneObjectKey, source_index)` 排序的私有索引，公开 `animator(game_object)` 通过二分查找；
  `source_index` 作为次键保持重复 GameObject 时源文件首项优先。新增
  `maximum_animator_index_bytes`（默认 64 MiB）在任何预留前按精确元素大小检查，容量恰好时允许，
  少一字节则稳定拒绝。回归使用 16,384 个逆序 key、重复目标和 16,384 次重复查询，锁定每次
  少于 20 次比较、重复首项、跨文件缺失以及 public graph build 的预算路径。Core 常规测试
  569 项通过、12 项独立可选音频 oracle 测试忽略，6 项畸形输入扫描及严格 Clippy 通过；完整
  工作树随后执行 `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过。
  提交 `b8d0766` 的公开常规矩阵
  [32704054064](https://github.com/seiunx-dev/unity-rs/actions/runs/32704054064) 为 16/16 验证 job 全绿；
- **Live2D 输出名称的重复后缀扫描已于 2026-08-25 收口**：包目录、纹理、expression、
  fade/clip/loose motion 原先各自用 `HashSet<String>` 记住规范化名称，但每次重名都从无后缀、
  ` @f{file}_p{path}`、`_2` 重新扫描；合法的重复 PathID 或显式占用后续候选可以把一组
  名称放大为二次工作。现统一使用一张“规范化名称 → 稳定数字 ID”表，并只为发生碰撞的
  `(base ID, SceneObjectKey)` 保存下一个未检查 ordinal；游标不复制潜在的长 base，所有表增长
  都用 `try_reserve`，且游标命中后仍重新检查全局 case-insensitive claim，交叉占位不会被跳过。
  回归锁定 `Face`/`face` 原格式、显式占用 `_2` 后继续 `_3`、不可能容量请求失败后已有状态不变，
  以及 16,384 次相同 base/身份恰为 `2 × N - 1` 次候选探测、末项为 `_16383`。提交 `f88cb53`
  的 Live2D 定向测试 22/22、严格 Core Clippy、Rust/Python/Node/typing/oracle 零跳过本地门禁通过；
  公开常规矩阵
  [32768660736](https://github.com/seiunx-dev/unity-rs/actions/runs/32768660736) 为 16/16 验证 job 全绿，
  另两项仅 tag/release 条件 artifact 任务按设计跳过；
- **Live2D 隐藏名称索引的未计费驻留已于 2026-08-25 收口**：`maximum_total_name_bytes`
  原先计入资产来源名称和最终输出名称，却没有计入包目录、纹理、expression、motion claim 表中
  保留的 Unicode lowercase key，也没有计入 display-info 去重索引；合法名称的大小写折叠可能扩张
  UTF-8 字节数，从而在已经满足公开预算后继续保留大量隐藏字符串。现所有新保留的 lowercase key
  与来源/输出名称共用同一累计预算，单个 key 在分配前按折叠后的精确长度检查；预算拒绝不会改变
  claim、suffix cursor 或计数器。回归锁定已保留 4 字节后再申请 5 字节、总上限 8 字节时稳定拒绝
  且状态不变，并用 `İ` 证明 2 字节来源折叠为 3 字节 key 时会在分配前拒绝。提交 `f8f630e`
  的 Live2D 定向测试 23/23、严格 Core Clippy，以及完整 Rust/Python/Node/typing/oracle 零跳过
  本地门禁通过；公开常规矩阵
  [32781978836](https://github.com/seiunx-dev/unity-rs/actions/runs/32781978836) 为 16 个实际 job
  全绿、2 个手工发布条件 job 正常跳过、0 失败；
- **FBX 批量输出名称的错误上限与重复扫描已于 2026-08-25 收口**：SplitObjects/Animator
  规划允许最多 1,000,000 个候选，但 CLI 原先以 `HashSet<String>` 保存名称，并让每个同名
  候选从无后缀、`~1` 重新扫描；循环还误用“创建临时文件最多尝试 1,024 次”的常量，导致
  第 1,026 个同名 GameObject 在模型候选、名称字节和总输出预算都未耗尽时仍必然失败。现
  `FbxBatchNames` 用一张 case-folded `HashMap<String, u64>` 同时保存 claim 和每个 base 的下一
  未检查后缀；碰撞名后来成为 base 时也从自身 `~1` 开始，显式占用未来候选仍会被重新检查。
  表在批次开始时按候选数可失败预留，后续增长也用 `try_reserve`，容量失败前不更新 claim/cursor；
  1,024 常量只继续约束真正的临时文件名尝试。回归锁定 `Face`/`face~1`、显式 `FACE~2` 后
  继续 `face~3`，不可能容量请求后状态不变，以及 16,384 个同名候选成功得到
  `Shared~16383` 且恰为 `2 × N - 1` 次探测。提交 `a9b0665` 的 CLI 定向测试、全目标严格
  Clippy、Rust/Python/Node/typing/oracle 零跳过本地门禁通过；公开常规矩阵
  [32772105204](https://github.com/seiunx-dev/unity-rs/actions/runs/32772105204) 为 16/16 验证 job 全绿，
  另两项仅 tag/release 条件 artifact 任务按设计跳过；
- **SceneHierarchy 的不可失败索引分配已于 2026-08-24 收口**：场景读取本来已经对
  GameObject、组件、Transform 子项、材质、骨骼和层级边执行集合级计数，并对输出 Vec 使用
  `try_reserve`，但 Transform→GameObject owner cache 与最终 GameObject→node lookup 仍是逐项
  分配的 `BTreeMap`；默认百万 GameObject/千万组件下，格式有效的输入可以绕过其他可失败分配，
  在树节点分配失败时直接终止进程。现 owner cache 改为每个新 key 先计费、再
  `HashMap::try_reserve(1)`、最后写入，重复 Transform key 只更新值而不重复收费；最终 lookup
  在临时 cache 释放后一次精确预留 `(SceneObjectKey, node_index)` Vec，按 key 排序、拒绝重复身份，
  查询通过二分完成。两者共同受 `SceneHierarchyLimits.maximum_index_bytes` 约束，默认 256 MiB；
  Python `SceneLimits.maximum_index_bytes` 与 Node `maximumIndexBytes` 已同步公开并有运行时/类型声明
  消费测试。规模回归用 16,384 个逆序、跨三个 file index 的 GameObject 做 16,384 次查询，锁定
  每次少于 20 次比较；另覆盖重复身份、最终索引精确容量/低一字节拒绝，以及 owner cache 精确
  单项容量、重复更新和第二项预算前拒绝。Core 常规测试 571 项通过、12 项独立可选音频 oracle
  测试忽略，6 项畸形输入扫描及严格 workspace Clippy 通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，Rust/Python/Node 构建、
  workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过。提交 `b77192a`
  的公开常规矩阵
  [32706872352](https://github.com/seiunx-dev/unity-rs/actions/runs/32706872352) 为 16/16 验证 job 全绿；
- **AnimationGraph 的 controller/clip 不可失败树索引已于 2026-08-24 收口**：旧构建状态以
  `BTreeSet` 保存最多 1,000,000 个待解析 controller，并以两个 `BTreeMap` 保存最多 1,000,000 个
  controller 和 2,000,000 个 clip；每项都进行不可失败的树节点分配，而且待解析集合是在数量
  上限检查前插入。现待解析集合和构建期 lookup 改为 `HashSet`/`HashMap`，先检查 count 与新增的
  `maximum_lookup_index_bytes`，再分别 `try_reserve(1)` 后写入；默认 128 MiB 的同一逻辑预算覆盖
  同时存活的 queued/controller/clip 三张构建表。完成解析后先释放全部临时哈希表，再按 controller
  与 clip 的合计精确容量建立排序 Vec，重复身份明确拒绝，公开 `controller()`/`clip()` 以二分保持
  原源数组位置。规模回归使用各 16,384 个逆序 controller/clip，逐项查询并锁定总比较次数为
  对数级；另覆盖最终合计容量恰好通过、少一字节在分配前拒绝、两类重复键拒绝和正式 graph build
  的零预算路径。Core 573 项常规测试通过、12 项独立可选音频 oracle 测试忽略，6 项畸形输入扫描
  及严格 workspace Clippy 通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，Rust/Python/Node 构建、
  workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过。提交 `21ede89`
  的公开常规矩阵
  [32709579106](https://github.com/seiunx-dev/unity-rs/actions/runs/32709579106) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **ModelIr 的四类构建/最终树索引已于 2026-08-24 收口**：旧实现同时以四张 `BTreeMap`
  构建 GameObject、Mesh、Material、Avatar 的 key→源数组位置，并把相同四张树直接保留在最终
  `ModelIr`；默认上限分别达到 1,000,000 个节点和三类各 1,000,000 个资产，格式有效输入可在
  八组逐节点分配中把内存不足升级为进程 abort。现四张构建表只在新 identity 出现时统一计费，
  先检查 `maximum_index_bytes`（默认 256 MiB），再 `HashMap::try_reserve(1)`；Mesh、Material、
  Avatar 输出 Vec 也在写入前可失败预留，重复引用不重复计费。完成构建后先释放全部临时哈希表，
  再按四类总条目精确预检并分别建立按 `(SceneObjectKey, source_index)` 排序的 Vec，重复身份明确
  拒绝，`node[_index]`、`mesh[_index]`、`material[_index]` 与 `avatar` 均以二分恢复源数组位置。
  规模回归用 16,384 个逆序节点做 16,384 次查询并锁定每次少于 20 次比较；另覆盖最终容量恰好
  通过/少一字节在分配前拒绝、重复节点拒绝，以及真实 collection 中两节点共享同一 Mesh/Material
  时只按 2+1+1+1 个唯一节点/资产收费。Core 576 项常规测试通过、12 项独立可选音频 oracle
  测试忽略，6 项畸形输入扫描及严格 workspace Clippy 通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，Rust/Python/Node 构建、
  workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过。提交 `01594a2`
  的公开常规矩阵
  [32712675787](https://github.com/seiunx-dev/unity-rs/actions/runs/32712675787) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **Loader 的 collection-wide 对象元数据树索引已于 2026-08-24 收口**：旧实现以一张长期保留的
  `BTreeMap` 同时承载对象名称和 container，并在重建时再以 `BTreeMap` 构建；MonoScript class
  解析还保留第三张临时 `BTreeMap`。默认允许一千万 container assignment 和一百万名称 assignment，
  合法但巨大的输入可让逐节点不可失败分配把 allocator failure 升级为进程终止。现构建状态以 owning
  Vec 保存唯一元数据，并用只参与精确查找的临时 HashMap 指向 Vec 位置；每个新 identity 在增长前
  检查 `maximum_object_metadata_entries`（默认一千万）和共享的
  `maximum_object_metadata_index_bytes`（默认 1 GiB），随后分别 `try_reserve(1)`。MonoScript class
  HashMap 纳入同一逻辑字节预算；重复 key 直接覆盖旧值，不新增 entry 或重复收费。解析结束后先释放
  所有临时表，再把 owning Vec 按 `(file_index, path_id)` 排序；`object_metadata()` 通过二分保持精确
  查找，名称和 container 的后写覆盖语义不变。规模回归使用 16,384 个逆序 PathID 做 16,384 次查询，
  锁定每次少于 20 次比较；另覆盖 entry count/字节预算恰好与低一字节拒绝、拒绝前不写入、重复
  metadata 与 MonoScript class last-wins 且不重复收费，以及公开 `resolve_object_metadata` 的真实
  Material 命名路径。Loader 28/28、Core 严格 Clippy 和完整
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过；提交
  `e53210a` 的公开常规矩阵
  [32716234330](https://github.com/seiunx-dev/unity-rs/actions/runs/32716234330) 为 16/16 验证 job 全绿；
- **Loader 递归待处理队列与最终 collection 表的不可失败增长已于 2026-08-24 收口**：旧实现只在
  `PendingInput` 出队时递增 `maximum_discovered_files` 计数；一个已经解析出百万目录项的 bundle、
  WebData 或 ZIP 会先用普通 `VecDeque::push_back` 把整批路径、Region 和版本 hint 留在内存中，
  直到逐项出队才越界。AssetsFile/ResourceFile 被接受后又分别直接 `push` 到最终 Vec。现根输入和
  每个容器批次在读取/物化各 child 前先 checked 计算共享 discovered count：超限会保留已发生的
  work charge、但不调用 reserve；允许的批次一次 `VecDeque::try_reserve(entry_count)` 后才逐项写入，
  gzip/Brotli 则在成功解压出唯一 child 后执行同一路径。最终 serialized/resource 表也在解析或提交
  下一项前 `try_reserve(1)`。内部回归锁定允许三项时 root+2 child 精确预留、第四项超限后队列长度和
  capacity 均不变且共享预算保持越界状态；公开回归用 16,384-entry WebData，证明 root+16,384 的
  精确上限完整、稳定地产生 16,384 个资源，而低一项在入队前拒绝。Loader 30/30、Core 严格 Clippy
  和完整 `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过；
  提交 `e5ee1a4` 的公开常规矩阵
  [32719238507](https://github.com/seiunx-dev/unity-rs/actions/runs/32719238507) 为 16/16 验证 job 全绿；
- **ASCII FBX 的材质/贴图二次规划分配已于 2026-08-24 收口**：场景贴图集合本身已有输入数量与
  可失败分配边界，但 ASCII FBX 写入前会把已识别的 Material texture environment 再复制成每材质
  binding plan，并另外建立唯一贴图索引与 texture plan；旧实现使用逐节点分配的 `BTreeMap`，两个
  输出 Vec 又直接 `push`。单个 Material 合法地最多可含 1,000,000 个 texture environment，因此
  第二轮规划仍可能在 allocator failure 时终止进程。现唯一贴图索引改为只做精确查询、不参与输出
  排序的 `HashMap`，每个新 key 先 `try_reserve(1)`；texture plan 同样在写入前可失败预留，每材质
  binding 表则先精确统计 `slot.is_some()` 的实际数量并一次预留。输出顺序继续来自源 binding 遍历，
  所以哈希随机状态不会泄漏到 FBX 字节，重复贴图仍采用第一次绑定的 UV transform。规模回归用
  16,384 个指向同一贴图的 Diffuse binding，锁定一个 Texture/Video plan、16,384 条材质连线和首项
  offset/scale；13/13 ASCII FBX 定向测试、严格 Core Clippy 以及完整
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过。提交
  `122a946` 的公开常规矩阵
  [32722019278](https://github.com/seiunx-dev/unity-rs/actions/runs/32722019278) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **Cubism clip motion 目标索引的 CPU/分配放大已于 2026-08-24 收口**：旧实现对每个
  Parameter/Part 用 `named.iter().any` 去重，逆序唯一目标会形成二次方比较；哈希表只按
  `named.len() * 2` 预留，但带 `/` 的 ID 会额外生成每级后缀哈希，随后 `entry` 增长不可失败；
  每级后缀还通过 `String::drain` 搬移剩余内容。现用预留后的 borrowed `HashSet` 在源遍历中完成
  首项去重，保留 Parameter 与 PartOpacity 同名但不同身份的语义；临时集合释放后，先线性精确计算
  完整路径及全部后缀的哈希条目和 CRC 输入字节，再一次 `try_reserve` 最终索引，后缀改用 borrowed
  slice 前进。Core 入口同时补上单字符串、累计字符串、目标数、哈希数、哈希工作量和逻辑索引字节
  六类预算，默认分别允许 16 MiB、256 MiB、1,000,000、2,000,000、512 MiB 和 256 MiB；Python
  与直接 Rust 调用因此不再依赖 Node 的预校验。规模回归用 16,384 个逆序唯一 Parameter、一个重复
  Parameter 和一个同名 PartOpacity 锁定源顺序、首项语义与类型区分；嵌套 `Group/ParamAngleX`
  同时锁定完整路径、中间后缀和末级后缀，另有每类预算低一项/精确相等回归。7/7 定向测试、严格
  workspace Clippy 以及完整 `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`
  均通过，后者零组跳过。提交 `ca3171d` 的公开常规矩阵
  [32725964995](https://github.com/seiunx-dev/unity-rs/actions/runs/32725964995) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **Cubism FadeMotion 曲线目标分类的 CPU 放大已于 2026-08-24 收口**：旧实现对每条曲线先线扫
  全部 parameter，再线扫全部 part；默认最多允许 1,000,000 条曲线，而 MOC 目标表可合计达到
  2,000,000 项，包规划和最终物化还会对同一模型重复执行分类。现新增
  `CubismMotionTargetIndexLimits`，默认分别限制 2,000,000 个名称、16 MiB 单字符串、128 MiB
  累计字符串和 64 MiB 逻辑索引；索引以 `try_reserve_exact` 预留 borrowed `&str` Vec，排序去重后
  二分查询，不复制目标字符串。`Opacity`/`EyeBlink`/`LipSync` 的 Model 分类、Parameter 优先于
  同名 PartOpacity、显式 part 表以及大小写不敏感的 `part` 回退语义均保持不变；包规划与物化各自
  为每个模型/包构建一次并传给所有 FadeMotion 复用。规模回归使用 16,384 个逆序唯一 parameter、
  重复项及同名 part，执行 16,384 次索引查询，并让公开 writer 写出 4,096 条指向旧最坏位置的曲线，
  整组约 0.04 秒完成；另有四类预算的精确边界/低一项拒绝，以及 package planning/materialize
  预算传播回归。FadeMotion 5/5、package 19/19、严格 workspace Clippy 和完整
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过。
  提交 `42fe0b2` 的公开常规矩阵
  [32730428675](https://github.com/seiunx-dev/unity-rs/actions/runs/32730428675) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **ModelIr 目标资产解析的 `assets × object table` 放大已于 2026-08-24 收口**：场景 PPtr
  已经通过 Loader 的 `(file, pathID) → object_index` 排序索引解析并验证类型，但 Mesh、Material、
  Avatar 被登记进 ModelIr 时，旧 `require_key_target` 又对目标 SerializedFile 的全部 objects 执行
  `iter().position`。默认最多允许各 1,000,000 个目标资产，SerializedFile 又允许 10,000,000 个对象，
  因此正常加载明明已经付过一次索引成本，模型构建仍可被放大为 `目标资产数 × 对象数`。现该路径
  直接复用 collection-wide pathID 二分查询；公开可变对象表导致缓存过期时仍沿用既有验证和线性
  fallback，不会因性能修复改变首个重复 PathID 或 stale-index 的安全语义。内部 probe 回归构造
  16,384 个逆序唯一 Mesh 对象并逐一解析全部 PathID，比较次数严格低于 `16,384 × 20`，整组约
  0.02 秒；完整 Core 单测、严格 workspace Clippy、fmt/diff-check 以及
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过。
  提交 `a646d15` 的公开常规矩阵
  [32733901273](https://github.com/seiunx-dev/unity-rs/actions/runs/32733901273) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **TypeTree 子树边界的 `schema nodes × runtime values` 放大已于 2026-08-24 收口**：reader、
  root-field projection 和 legacy dump 原先每次需要跳到下一兄弟时都从当前节点向后线扫。普通类的
  单次遍历尚可由 128 层深度限制约束，但数组/map 会为每个运行时元素复用同一份 data/pair schema；
  在 2,000,000 个 schema 节点、32,000,000 个运行时值的公开上限内，两个单项上限会被乘起来，
  dump 又会按物化数组重复同样的扫描。现每棵实际使用的主树先以一趟 preorder 栈构建 exclusive
  subtree-end 表，所有边界查询为 O(1)；`SerializeReference` 的 reference type 只在首次出现时
  校验并建表，随后按 type index 复用。索引、按需 reference-type cache 和构建期最坏 stack 都使用
  fallible reserve，保留索引字节计入既有 `maximum_materialized_bytes`，低预算在分配前拒绝；空树、
  level jump、重复 root、首声明 reference type 和错误路径恢复主树的语义不变。规模回归分别构造
  32,768 节点深链与 32,768 节点宽树，逐一查询全部边界，并以 probe 断言构建工作不超过 `2 × N`；
  TypeTree 定向 25/25、Core library 590 项常规测试通过（12 项可选外部 oracle 在总门禁另行实跑）、
  malformed-input 6/6、严格 workspace Clippy/fmt/diff-check，以及
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过。
  提交 `d9cff60` 的公开常规矩阵
  [32738542372](https://github.com/seiunx-dev/unity-rs/actions/runs/32738542372) 为 16/16 验证 job 全绿，
  另两项仅手工 release 条件任务按设计跳过；
- **SpriteAtlas 回填的 `sprites × atlases × packedSprites` 放大已于 2026-08-24 收口**：旧的
  `effective_sprite_atlas` 为每个待解码 Sprite 重新遍历 collection 的所有对象，完整解析每个
  SpriteAtlas，再逐条解析 `packedSprites` PPtr 并比较对象 Region。批量 export 以及高层
  `StudioObject::decode_sprite`（Python/Node 共用）因此会把同一批 Atlas 元数据重复解析到 Sprite
  数量次。现 normal loader 在 collection-wide PPtr/resource index 之后只解析一次有效 Atlas，按
  `(sprite file, object index, atlas discovery order)` 保留 assignment，排序后二分查询；显式 Atlas、
  unresolved/wrong-class TryGet、首个 master 不再被 variant 替换、无 master 时后出现 variant 覆盖，
  以及 managed construction 失败 Atlas 不参与回填的语义保持不变。保留 Atlas 与 assignment 共用
  `maximum_sprite_atlas_index_entries`，容量增长全部 fallible，低一项预算在保留前失败并事务性清空
  派生索引；`from_loaded_parts` 等低层未建索引 collection 继续走原兼容扫描。Rust 高层、Python、
  Node 与批量 export 均通过带 file index 的共用路径使用该索引，resident/atlas 纹理 PPtr 也复用
  collection pathID/portable-name 索引。规模回归构造 16,384 个 assignment 并逆序查询全部 Sprite，
  边界 probe 严格低于 `N × 40`；真实 variant+master fixture 证明 indexed 结果仍选 master，4-entry
  精确预算成功而 3-entry 在分配前拒绝。Core 604 项中 592 项常规测试通过、12 项可选外部 oracle
  在总门禁实际执行，malformed-input 6/6、严格 workspace Clippy/fmt/diff-check，以及
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过。
  提交 `2c9fb42` 的公开常规矩阵
  [32743479062](https://github.com/seiunx-dev/unity-rs/actions/runs/32743479062) 为 16/16 验证 job 全绿，
  覆盖 Rust、Python、Node、UnityPy、managed 与 vgmstream，另两项仅手工 release 条件任务按设计跳过；
- **Sprite 解码的逐 Sprite 图集页重解已于 2026-08-27 收口**：同一图集页上的每个 Sprite 都要解析
  并解码同一张 `Texture2D`（atlas 页或 resident color/alpha 纹理），批量解码 N 个共页 Sprite 会把
  该页的整页解码成本重复 N 次；生产实测一个 1 Texture2D + 10 Sprite 的 bundle 上，单个 Sprite 的
  解码耗时约为整页解码的 8 倍（UnityPy 在 `SpriteHelper.get_image` 按 `texture.path_id` 缓存，
  没有这项放大）。现 `AssetCollection` 持有一个内部有界 MRU 缓存（最多 4 页、累计像素字节
  256 MiB 封顶，每页本就先受调用方 `TextureReadLimits` 约束），Sprite 解码路径以解析后的
  `(collection file index, object index, 完整 TextureReadLimits)` 为键复用 mip-0 解码页：只有完全
  相同的 limits 才命中，解码失败从不缓存，克隆 collection 从空缓存开始，命中与重解的像素逐字节
  相同，因此不构成任何静默回退或行为变化。高层 `decode_sprite`（Rust/CLI/Python/Node 共用的
  `by_file_index` 路径）与批量 export 自动受益；仅凭 `&SerializedFile` 的低层本地引用继续按原样
  逐次解码。非 Sprite 的 `decode_texture_mip` 面不经过缓存。回归覆盖：缓存单测验证仅精确
  identity+limits 命中、MRU 逐出、字节预算逐出、超预算单页直接不缓存、重插替换；行为测试用两个
  共享一张纹理的 Sprite 断言第二次解码命中缓存（stats 1 hit/1 miss）、不同 limits 强制重解、
  且三种路径像素一致。`cargo fmt --check`、`cargo check --locked --all-targets`、严格 workspace
  Clippy `-D warnings` 与完整 `cargo test --locked` 均通过（Core 库 631 项常规测试全绿，12 项
  可选外部依赖测试按设计跳过；CLI/Python/Node/进程级集成套件同轮全部通过）；
- **逐对象图像编码已于 2026-08-27 暴露到绑定层**：Core 的 PNG/JPEG/BMP/TGA/lossless-WebP/
  raw-RGBA 编码器此前只能通过整集合 `export()` 的落盘布局使用，Python/Node 调用方拿到
  `RgbaImage` 后想存单张图只能自带编码器（Pillow/sharp），既慢又绕开 Core 已有的预算语义。
  现 Core 新增 `image_export::encode_rgba_image` 与 `write_rgba_image_with_options`，与流式
  writer 共用同一组编码器和 `BoundedWriter` 输出预算，缓冲区预留取原始像素估计与输出上限的
  较小值且全程 fallible；格式旋钮收敛在 `ImageEncodeOptions`，默认值逐字节复现历史输出。
  PNG 侧：`PngCompression`（`fast`/`default`/`best` 映射 flate2 level 1/6/9，另接受显式
  `Level(0..=9)`，超 9 拒绝不钳制）与 `PngFilter`（`none` 保持历史 filter-type-0 输出；
  `adaptive` 按 libpng/ImageSharp 的最小绝对差启发逐行在五种标准 filter 里选优，工作内存
  恒为单行 stride 且 fallible 预留）。动机是下游真实语料实测（member_cutout 全量，2,609 张）：
  固定 `Compression::default()` 加无 filter 在吞吐场景是负收益——同体积下比 Pillow level=3
  慢 1.9 倍、比同一生态 image crate 的 `CompressionType::Fast`+Adaptive 编码环节慢约 6.6 倍。
  JPEG 侧透出 jpeg-encoder 已有能力：`JpegSampling`（`auto` 保持质量驱动的历史默认，
  显式 4:4:4/4:2:2/4:2:0）、progressive、optimized Huffman，以及 `jpeg_background`（给定
  RGB 底色时按 `round(c*a/255+bg*(1-a/255))` 整数合成半透明像素，替代默认的丢 alpha 语义；
  合成缓冲计入同一工作预算）。所有 PNG 档位/filter 无损，Core 回归断言五种压缩组合解出的
  扫描线逐字节一致、adaptive 输出经独立 unfilter 复原后与原始像素逐字节相等且在渐变图上
  严格小于无 filter 输出、显式 4:2:0 与 auto 基线逐字节一致、透明像素合成白底后解码近白而
  丢 alpha 路径保持原 RGB。`export()` 行为不受任何旋钮影响。Python 在
  `RgbaImage.encode(image_format="png", *, jpeg_quality, jpeg_sampling, jpeg_progressive,
  jpeg_optimized_huffman, jpeg_background, compression（名字或 0–9 整数）, png_filter,
  maximum_bytes)` 返回 `bytes`（编码在 `py.detach` 内完成，GIL 边界已纳入 surface 审计门），
  Node 静态 `UnityRs.encodeImage`/`encodeImageAsync` 的 options 同形（`compression` 为
  `string | number`；napi worker `compute` 内编码；像素 Buffer 先按声明尺寸做长度校验、再
  fallible 拷贝，worker 不触碰 JS 内存）。两端格式串、质量/档位/采样校验与默认值完全一致；
  `RgbaImage` 三端仍是 display 行序，编码固定 `Display` 不再翻转。回归：Core 单测锁定 owned
  输出与流式 writer 逐字节一致、预算与质量拒绝；Python/Node 各自验证 PNG 签名+IHDR 尺寸、
  各档位/filter/数字级有效性、JPEG 各旋钮产出可解码且异于基线、raw-RGBA IR 魔数、非法格式/
  预算/质量/档位/采样/filter/底色形状的错误族（PNG 中途超预算保留 I/O 族），Node 另断言
  async 与 sync 字节一致；`.pyi`、严格 mypy 消费端、napi 重生成声明、严格 TS 消费端与安装包
  87 方法计数全部同步。
  `cargo fmt/check/clippy -D warnings/test`、两个 API surface 审计与
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 零跳过通过；
- **legacy streamed AudioClip 的 `clips × serialized files` 放大已于 2026-08-24 收口**：旧版
  AudioClip 的外部资源只存 offset/size，reader 必须从拥有它的 `.assets` 路径派生 `.resS` 名称；
  旧入口为每个 clip 用指针相等线扫 `AssetCollection` 的完整 SerializedFile 表，目标在表尾时批量
  读取或导出会形成 `AudioClip 数 × 文件数` 工作。现新增按稳定 collection `file_index` 读取
  `AudioClipAsset`/raw payload 的公开入口，直接从 owning slot 借用路径；高层 `StudioObject` 和
  batch export 统一改走该入口，Python 与 Node 因直接复用 Studio 同步受益，不引入 collection
  context、句柄或新缓存。只持有独立 `&SerializedFile` 的低层 Rust API 保留原指针扫描兼容路径，
  现代或 inline clip 不会额外查表。规模回归构造 16,384 个 SerializedFile，把有效 legacy streamed
  clip 放在最后一项：indexed 入口 probe 精确为 0，兼容入口精确为 16,384，二者都解析同一 payload；
  越界 file index 也在读取前稳定拒绝。Core 605 项中 593 项常规测试通过、12 项可选外部 oracle
  在总门禁实际执行，malformed-input 6/6、严格 workspace Clippy/fmt/diff-check，以及
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 均通过，后者零组跳过。
  提交 `9daca17` 的公开常规矩阵
  [32746907255](https://github.com/seiunx-dev/unity-rs/actions/runs/32746907255) 为 16/16 验证 job 全绿，
  覆盖 Rust、Python、Node、UnityPy、managed 与 vgmstream，另两项仅手工 release 条件任务按设计跳过；
- **FBX blend-shape 动画的 `tracks × morph channels` 放大已于 2026-08-25 收口**：ASCII 与
  binary FBX 共用的场景 planner 原先会为每条 `ModelBlendShapeTrack` 重新扫描全部 geometry 与
  morph channel；合法的大模型因此可把动画 track 数与 channel 数相乘。现仅在 clip 实际含有
  blend-shape track 时一次构造借用字符串的 `MorphChannelIndex`，按 model、channel name 和来源
  顺序排序，随后以 `partition_point` 二分；重复 `(model, name)` 继续选择最先发现的源 channel，
  不改变既有输出语义。索引先做 checked 计数和精确 fallible reserve，其逻辑字节在分配前受现有
  FBX 输出预算限制；没有 morph track 的写出不建立索引。ASCII 与 binary writer 都走这条共享
  planner，所以 Rust、Python 与 Node 的高层 FBX 路径同步受益。规模回归构造 16,384 个逆序唯一
  channel 和一条重复项，再逆序查询全部 channel：probe 严格低于 `N × 20`，重复项保持首项 ID，
  精确索引字节预算成功而少一字节在分配前拒绝；公共 ASCII writer 也验证同一预算确实贯通。
  Core 606 项中 594 项常规测试通过、12 项可选外部 oracle 在总门禁实际执行，malformed-input
  6/6；严格 workspace Clippy/fmt/diff-check 与
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 全部通过且零组跳过。
  提交 `90f572d` 的公开常规矩阵
  [32750879788](https://github.com/seiunx-dev/unity-rs/actions/runs/32750879788) 为 16/16 验证 job 全绿，
  覆盖 Rust、Python、Node、UnityPy、managed 与 vgmstream，另两项仅手工 release 条件任务按设计跳过；
- **AnimationGraph 的 `Animators × bound clips` 未计费复制已于 2026-08-25 收口**：图构建器会把
  direct `AnimatorController` 的完整 clip 列表复制到每个绑定 Animator（override controller 使用其
  base controller），供 FBX 与 Live2D 直接消费；此前这些派生 `bound_clips` 不计入
  `maximum_edges`，并由不可失败的 `Vec::clone_from` 增长。一个合法共享 controller 因而能在已经
  通过 controller/Animator 数量与原始 edge 上限后产生 `Animator 数 × clip 数` 的额外驻留内存。
  现构建器先以 checked arithmetic 预计算全部派生引用，把它们计入现有 graph edge 预算；只有整批
  预检通过后才逐 Animator `try_reserve_exact` 并按原 managed 顺序复制，分配失败也返回结构化错误。
  override→base 选择、null clip 槽位和公开 `bound_clips` 形状均保持不变。规模回归构造 16,384 个
  Animator 共用 16,384-entry controller，并用低于单份列表一项的预算证明在任何副本增长前拒绝且
  所有输出列表仍为空；独立精确预算回归验证 4×3 个槽位完整保序，真实公共 fixture 又锁定原始引用
  占 10 条、预算 10 因派生一条失败而 11 成功。Core 608 项中 596 项常规测试通过、12 项可选
  外部 oracle 在总门禁实际执行，malformed-input 6/6；严格 workspace Clippy/fmt/diff-check 与
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle` 全部通过且零组跳过。
  提交 `5482d07` 的公开常规矩阵
  [32755098612](https://github.com/seiunx-dev/unity-rs/actions/runs/32755098612) 为 16/16 验证 job 全绿，
  覆盖 Rust、Python、Node、UnityPy、managed 与 vgmstream，另两项仅手工 release 条件任务按设计跳过；
- **`SerializeReference` 身份字符串已于 2026-08-22 纳入现有物化预算**：TypeTree reader
  已经把 `ReferencedManagedType` 的 class/namespace/assembly 三条字符串保留在输出树中，
  但原先为匹配 SerializedFile reference types 又逐条 `clone` 到临时
  `ManagedTypeIdentity`；这三份副本不在 `maximum_materialized_bytes` 计数中，单条字符串
  默认又允许 16 MiB。现 reader 直接借用刚解析的三条字符串完成 reference type 查找，
  立即保存解析后的 type-tree index，随后仍把原始 identity value 按既有结构交给调用方，
  不再产生第二组拥有型字符串。未声明类型的错误也只报告 namespace/class/assembly 的 UTF-8
  字节数，不再为了诊断再次复制或回显资产控制的大字符串；null entry、三字段精确匹配和
  “数据出现在 identity 前”拒绝语义均不变。9 项 TypeTree 定向测试覆盖有效、null、未声明、
  嵌套 registry 与错误边界，严格 workspace Clippy 通过。完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **`SerializeReference` reference-type 查找与校验已于 2026-08-24 去除二次方放大**：
  旧 reader 对每个 registry entry 都线扫最多 1,000,000 个 reference type，之后又用
  `Vec::contains` 判断该 type tree 是否已经校验；大量近似身份会形成 `entry × type`，
  大量不同类型则形成累计 `O(type²)`。现每次对象读取只惰性建立一次借用
  class/namespace/assembly 的排序索引，键相同时再按原始序号排序，二分命中因此仍返回首个声明；
  树形校验缓存改为按 reference-type index 的布尔表，首次校验后为 O(1) 查询。两个表均在
  `try_reserve_exact` 前按精确容量计入 `maximum_materialized_bytes`，低预算失败不会留下半个缓存。
  16,384 个近似类型、16,384 次重复命中、16,384 个不同 type tree 的规模回归锁定索引只建一次、
  首声明语义和全部校验位；另有低一字节的 lookup/cache 预算测试证明分配前稳定拒绝。
  完整工作树执行 `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过；
- **模型动画路径绑定已于 2026-08-24 去除 `tracks × nodes` 扫描**：旧
  `ModelPathIndex::path` 按 GameObject key 线扫全部节点，legacy 显式曲线和 Avatar path fallback
  的 `resolve_suffix` 又为每条绑定执行同样的全表 `EndsWith` 搜索；默认节点/track 上限达到百万级，
  合成对象无需任何未验证 Unity 布局即可触发超线性 CPU 工作。现 key 使用按
  `SceneObjectKey` 排序的索引二分；后缀索引先按末级名称精确分组，再以不复制反向字符串的
  MSD radix 对完整 UTF-8 path 排序，查询用两次二分取得任意 byte-suffix 范围，并以迭代
  range-min tree 返回最小源遍历序号。这样仍精确保留托管 `FindChilds(name)` +
  `Path.EndsWith(path, Ordinal)` 的末级名称、从组件名中段开始的任意后缀及首项语义，
  没有缩成只认 `/` 边界。所有 key/radix scratch/name group/range tree `Vec` 在分配前由新增的
  `maximum_path_index_bytes` 累计预算约束，`maximum_path_hashes` 也会先于索引分配预检。
  UTF-8、空名、重复路径和每个字符边界的查询逐项与旧线性 oracle 比对；8,192 个同名叶子、
  16,385 个总节点的回归锁定广义和中段后缀均少于 128 次索引比较，并验证精确 key、首项及
  低一字节索引预算。继续审计发现模型 hash 未命中时，旧实现仍会为每条 bound sample 线扫
  当前 Avatar 的完整 path 表，形成独立的 `tracks × avatar_paths` 放大。现只为本次实际选中的
  Avatar 构建排序 hash 索引，重复选择先去重，缺失/未选 Avatar 不物化；hash 总数和构建期 key、
  selected-index、lookup `Vec` 字节与模型路径索引共用原有两项累计预算，不获得第二套额度。
  16,384 个近似 hash 的重复查询保持每次对数级比较，重复 hash 仍返回源文件第一项；另有
  未选 key 不访问、重复选择只建一份、count 低一项及 byte 低一字节的分配前拒绝回归。
  Core 565 项常规测试、6 项畸形输入扫描及严格 Clippy 通过；最终工作树执行
  `tools/local_ci.py --fail-on-skip quality rust python node typing oracle`，Rust/Python/Node 构建、
  workspace 测试、wheel/sdist/npm 包、严格类型和托管差分全部通过，零组跳过；提交 `dac020e`
  的公开常规矩阵 run 32698603856 为 16/16 验证 job 全绿；
- **整场景 OBJ/MTL 临时分组已于 2026-08-22 去除输入规模复制**：OBJ writer 原先为每个
  renderable Renderer 创建 `ObjGroup` 时完整 `clone` 其材质槽 `Vec`，随后 MTL writer
  又为每个 submesh 用 `MaterialName::to_string()` 物化名称，并把所有已写名称保存在第二个
  `Vec<String>`。这些数据在 `ModelIr` 中已经存在且受场景预算约束，重复持有不会增加输出
  信息，只会让大模型在写出前额外占用与材质引用数同量级的内存。现 `ObjGroup` 直接借用
  Renderer 的材质切片；分组数和全部 submesh material slot 数先 checked 累加，分别通过
  `try_reserve_exact`/`HashSet::try_reserve` 建立临时索引。MTL 去重以
  `Option<SceneObjectKey>` 为稳定身份，仍按首次遍历顺序写出，并直接格式化到 bounded writer，
  不再保留名称副本。新增回归以 slice pointer 同一性证明两个 group 均借用原 Renderer，
  并验证共享材质仍只写一个 `newmtl`；9 项 whole-model OBJ 定向测试和严格 workspace Clippy
  通过。完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **Binary FBX 场景字符串已于 2026-08-22 在节点树构造前进入输出预算**：场景投影原先
  会在调用 binary encoder 前，用 `format!`/`to_owned` 构造 model、geometry、material、
  texture/video、skin/cluster、blend shape/target shape 和 animation 的全部拥有型字符串属性；
  encoder 的输出与 node/property/array 预算虽然完整，但看不到这批已经发生的分配。现新增
  `SceneStringBudget`，以调用方同一个 `maximum_output_bytes` 为累计上限，所有资产派生名称先
  checked 计算 prefix/value/suffix 的精确 UTF-8 长度并计费，再通过 `try_reserve_exact` 一次
  构造；重复写入 FBX 的纹理文件名按每个实际 property 分别计费，因此预算对应真实拥有型
  node tree，而不是只检查最长单串。静态 FBX 7.4 关键字和最终字节布局保持不变。新增公开
  writer 回归用 20 字节上限证明 `Model::root` 与 `Geometry::quad` 累计到 25 字节时，在 binary
  encoder 和调用方输出写入前稳定拒绝；几何、纹理、skin、blend shape、动画共 9 项场景
  定向测试及严格 workspace Clippy 通过。完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **Python/Node 导出选项解析已于 2026-08-22 去除输入规模副本并收紧诊断**：Python 的
  export mode、image format、audio format 与 Node 的 export mode、filename format、image
  format、audio format 原先都会在 `trim` 后调用 `to_ascii_lowercase`，为任意调用方字符串
  分配一份等长小写副本；拒绝值时又把完整原串插进异常。Node 的缺省 PNG 分支还会创建一条
  临时拥有型 `String`。现所有匹配都直接借用 trim 后的 `&str` 并使用
  `eq_ignore_ascii_case`，保持已有大小写不敏感、下划线/连字符和别名语义；Node 图片路径移动
  napi-rs 已交付的拥有型参数而不复制，缺省值直接借用静态 `"png"`。无效值不超过 64 个
  UTF-8 字节时仍保留原有可读诊断，超过后只报告精确字节数，不把资产外部的 Python/JavaScript
  长字符串复制进第二条错误消息。Rust 单测锁定四类 Node alias、默认值与 4096 字节 Unicode
  拒绝；安装后的 Python API 和 Node debug/release addon 测试真实走混合大小写/首尾空白及
  三/四类长值，并断言错误不回显输入。绑定定向门禁与严格 Node Clippy 通过；完整工作树随后
  执行 `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **原生 CLI 的 `argv` 保留与错误诊断已于 2026-08-22 建立分配边界**：进程入口原先对
  `env::args_os().skip(1)` 直接 `collect::<Vec<OsString>>()`，在任何数量/字节限制生效前用
  infallible Vec 增长保留全部参数；全局 load-option 过滤又 clone 一份剩余参数，重复
  `--mono-schema` 路径表同样普通增长。legacy `-m` 还先 `to_string_lossy().into_owned()`，再
  `to_ascii_lowercase()` 建第二份模式字符串，未知 option/格式错误会把完整调用方参数写入异常。
  现入口最多保留 65,536 项、单项 1 MiB 编码字节和累计 64 MiB，checked 累加后每项
  `try_reserve`；过滤表、每条 `OsString` 副本、schema `PathBuf` 和 schema 路径表均在增长前
  可失败预留，文档数量上限也前移到路径复制之前。legacy mode 直接借用有效 UTF-8 并以
  `eq_ignore_ascii_case` 分派，所有 parser 诊断通过同一 Display wrapper：64 编码字节以内保留
  旧可读文本，更长或非文本参数只形成有界摘要，不再回显整串。单元测试用可注入低预算分别
  锁定 count/per-item/cumulative 三条边界、Unicode 编码长度和无回显；进程测试真实启动 CLI，
  断言 68 字节未知选项以 usage exit 2 返回且 stderr 不含输入。CLI 26 项单测与 52 项进程集成
  测试、严格 CLI Clippy 均通过；完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **Core 三条分类/规范化热路径已于 2026-08-22 去除整串输入副本**：单文件加载会为父目录
  每个同 stem 候选调用 `extension.to_ascii_lowercase()` 才判断 `.resS`/`.resource`；Cubism
  fade-motion 的 PartOpacity 回退会为每条 parameter ID 小写化完整 String 再找 `part`；解包
  每个 WebData/ZIP/bundle 条目则先 `path.replace('\\', "/")`，再做绝对路径、drive、`..` 和
  component 清洗。三者的输入本来都已有上限，但这些临时副本既不增加输出信息，也在目录、曲线
  或 entry 数量上重复发生。现 companion extension 原地 `eq_ignore_ascii_case` 静态两项；motion
  ID 通过字节窗口做与旧 `to_ascii_lowercase().contains("part")` 完全相同的 ASCII 匹配；archive
  path 直接以 `['/', '\\']` 双分隔符单趟扫描，首字节同时拒绝 Unix/Windows absolute，不再保留
  normalized String。fixture 新增混合大小写 `resources.ReSS`、4096 字节后缀 `PART` motion ID，
  以及安全 `safe\\nested\\data.bin`；原有 `../`、反斜杠父跳转、Windows drive、UNC 和混合
  traversal 仍逐类拒绝。Core 544 项单测全部通过（另 10 项既有 vgmstream 外部 oracle 测试按
  设计 ignored），严格 Core Clippy 通过；完整工作树随后执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security cross`，
  Rust/Python/Node 构建、workspace 测试、wheel/sdist/npm 包、严格类型、RustSec、许可证、
  无头交付范围、六类输出格式以及 Linux/Windows 交叉编译全部通过，零组跳过；
- **公共 EndianReader 字符串分配已于 2026-08-22 收口**：`read_c_string` 与
  `read_c_string_required` 原先只给前 256 字节预留空间，之后每个字节通过普通
  `Vec::push` 不可失败扩容；三条 UTF-8 路径又依赖
  `String::from_utf8_lossy(...).into_owned()`，全非法输入可在字节缓冲之外再产生约三倍
  的不可失败拥有型字符串。现 C 字符串在调用方上限和剩余输入的交集内按块
  `try_reserve_exact`，定长字符串复用已检查的 `read_bytes`；有效 UTF-8 直接接管原
  Vec，非法序列先按 `Utf8Error::valid_up_to/error_len` 精确计算 U+FFFD replacement
  后长度，再一次可失败预留并分段写入，保持 .NET replacement fallback 语义。回归
  覆盖跨越 256 字节增长、非法起始/过长/续字节、尾部截断序列、全非法三倍展开、
  必需终止符恰在限制外以及非必需字符串读满限制。修复后的完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security`，
  Rust/Python/Node 构建、测试、发布包、类型、安全与六类输出校验全部通过，零组跳过；
- **加载路径预算已于 2026-08-22 贯通全部正式 API**：此前 `AssetLoadLimits` 只限制
  输入个数、目录项和展开载荷，没有限制根标签或递归拼接后的拥有型路径；大量零/小载荷
  长名称可绕开字节预算，而 `nested_path` 还会先 `replace` 出子路径副本、再由 `format!`
  生成完整路径。现每个根路径和新发现的 Bundle/WebData/ZIP 路径只计一次，分别服从
  单项与累计 UTF-8 字节预算；gzip/Brotli 仅移动同一条路径，不重复计费。嵌套路径先用
  checked arithmetic 算出精确长度并检查预算，再 `try_reserve_exact` 一次写入，反斜杠
  同步规范化，无中间 String。默认 1 MiB/64 MiB 与既有 Python/Node 内存输入上限一致，
  Python 的路径、单 buffer、多文件入口及 Node `OpenOptions` 均可收紧。Core 回归真实走
  根标签、累计多根和含反斜杠 WebData 子项；安装后的 wheel/sdist 及 Node debug/release
  addon 也验证公开选项和错误。完整工作树执行 `tools/local_ci.py --fail-on-skip outputs
  quality rust python node typing security` 全部通过，零组跳过；
- **文件系统枚举预算已于 2026-08-22 前移到保留分配之前**：旧目录入口会先把整棵
  `read_dir` 结果收集成 `DirEntry`/`PathBuf`，最后才把文件标签送进加载路径预算；
  单文件 companion resource 与显式 split segment 的父目录扫描也没有消费目录数和目录项
  预算，超大空目录或超长名称因而能在拒绝之前放大队列与路径分配。现根目录、每个子项的
  文件系统编码路径、输入目录和目录项都在进入排序列表/遍历队列之前计费；`PathBuf` 先按
  checked 长度执行 `try_reserve_exact` 再拼接，最终 UTF-8 标签只补计 lossy 转换产生的额外
  字节，不把普通路径重复收费。companion 与 split 扫描复用同一组预算；split 分组改为已
  预留的排序 `Vec` 顺序归并，不再依赖不可失败增长的树集合，同时保留“真实 base 文件优先于
  `.splitN`”的旧语义。回归覆盖单文件 companion 的目录/目录项上限、目录根路径耗尽后在
  第一个子项立即拒绝、split 数字排序与 base 优先；22 项 loader 测试和严格 workspace
  Clippy 已通过；完整工作树执行 `tools/local_ci.py --fail-on-skip outputs quality rust
  python node typing security` 后，Rust/Python/Node 构建、测试、发布包、类型、安全与六类
  输出校验全部通过，零组跳过；
- **原生 CLI 的递归 `inspect` 已于 2026-08-22 使用相同的分配前路径预算**：此前只读
  inspector 虽有文件、目录和目录项计数上限，却仍把 root `to_owned` 并对每个项调用
  `DirEntry::path()` 后才保留，且这三组常量与 Core 默认值各自维护。现 inspector 直接取
  `AssetLoadLimits::default()`，根/子路径在加入目录队列或文件表前按文件系统编码字节检查
  1 MiB 单项和 64 MiB 累计预算、checked 计算分隔符与完整长度并
  `try_reserve_exact`；只对确认是目录/普通文件的项形成路径，计数上限也不再复制常量。
  单元回归锁定单路径失败不消费预算、累计失败保持事务性，全部 CLI 套件和严格 workspace
  Clippy 已通过；完整工作树执行 `tools/local_ci.py --fail-on-skip outputs quality rust python
  node typing security` 后，所有 Rust/Python/Node 构建、测试、发布包、类型、安全与输出校验
  通过，零组跳过；
- **外部资源路径解析已于 2026-08-22 从逐次分配线扫改为安全索引**：旧
  `AssetCollection::resource` 每查一次都先给请求分配规范化 String，再为资源表中的每
  一项重复 `replace`/`to_owned`；Texture2D/Texture2DArray/Mesh/Audio/Video 的 streamed
  payload 会反复走这条路径，大集合因此同时产生 O(对象×资源) 扫描与短命分配。现加载
  完成后建立“规范化完整路径”和“便携文件名”两张排序下标表，不复制任何路径键；查询
  通过 allocation-free 字节迭代器完成 ASCII-insensitive、反斜杠和 `archive:/` 语义，
  两类命中取最早发现下标，保持旧的 union/first-match 契约。两张表共用独立条目预算和
  可失败预留。后续复核发现“公开表可随时修改、命中后复验”仍不足以维护该契约：调用方
  可把更早下标改成相同 PathID/便携资源名，而缓存中的较晚命中仍然有效，单点复验会静默
  返回后项。现 `serialized_files`/`resources` 对 crate 外只提供只读 slice；调用方若需替换
  内容，以 `into_parts` 消费并移动出原表、修改后交给 `from_parts`，再显式 rebuild/resolve，
  不需要复制可能很大的 source-bound Region。这样正常查询保持索引复杂度，不必为
  防任意外部 mutation 在每次命中前重新线扫；rustdoc `compile_fail` 用例同时锁定两个字段
  不能由外部安全代码清空。
  `Studio::resource_by_path` 直接复用返回下标，也不再命中后为了找下标再扫第二遍。Core
  全量、畸形输入与严格 workspace Clippy 通过；完整工作树执行 `tools/local_ci.py
  --fail-on-skip outputs quality rust python node typing security` 全部通过，零组跳过；
- **解包路径组件的校验前分配已于 2026-08-22 收口**：归档条目虽然一直有
  240 字节的可移植组件上限，旧实现却先按完整不可信组件长度
  `String::with_capacity`，清洗完成后才拒绝超限；恶意超长文件名因此能在失败前
  迫使进程申请一块同样大的内存。现改为逐个 UTF-8 字符计算清洗后的下一长度，
  只有仍位于 240 字节预算内才执行 `try_reserve_exact` 和写入；Windows 保留名需要
  添加前缀时同样先检查剩余预算。ASCII、三字节 UTF-8、恰好 240 字节、241 字节
  拒绝及保留名边界均有回归；Core 532 项单测中的 522 项常规测试、6 项畸形输入
  扫描和严格 all-target Clippy 已通过，10 项外部 oracle 测试按声明保持 ignored。
  修复后的完整工作树再次执行 `tools/local_ci.py --fail-on-skip outputs quality rust
  python node typing security`，Rust/Python/Node 构建、测试、发布包、类型、安全与六类
  输出校验全部通过，零组跳过；
- **递归解包的文件系统输入路径已于 2026-08-22 纳入分配前预算**：此前归档内部输出名
  有 `maximum_path_bytes`，目录输入扫描却仍会先 `root.to_owned()`/`DirEntry::path()`，最多
  一百万个深路径可在解包器看见任何路径限制之前进入队列和文件列表。现
  `ExtractionLimits` 同时提供单路径上限与默认 64 MiB 的 `maximum_total_path_bytes`；根路径、
  子目录和普通文件都先按文件系统编码长度计费，再以 checked 长度
  `try_reserve_exact` 后拼接。目录项只在确认是目录/普通文件后形成路径，最终诊断标签的
  lossy UTF-8 增量也计入累计预算。目录输入 roots 的结果表改为显式可失败预留，relative
  path 不再先 `to_owned` 一份中间副本；Python `ExtractionLimits` 的运行时属性、类型桩和
  严格消费测试均暴露同一参数。Core 回归覆盖根路径刚好耗尽累计预算、单路径超限以及原有
  目录项上限，14 项 extraction 测试和严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，全部
  Rust/Python/Node 构建、测试、发布包、类型、安全与输出校验通过，零组跳过；
- **递归解包的调用方与嵌套诊断标签已于 2026-08-22 共用路径预算**：仅限制文件系统
  路径仍留下另一条放大路径——`extract_region` 的 root label 不受限，Bundle/Web/ZIP/wrapper
  每层又通过 `format!` 加一份 parent、通过 `replace` 加一份 child；大量长条目可在输出路径
  已受限时继续堆积拥有型诊断字符串。现 root label 在创建输出目录前计费，嵌套标签先以
  checked arithmetic 计算 `parent + "::" + child`，事务性消费单项/累计预算，再一次
  `try_reserve_exact` 并在写入时把反斜杠规范化；失败不消费预算、不留下输出目录。
  `ExtractionLimits` 的 Rust 文档明确该预算同时覆盖文件系统路径、调用方标签、归档路径和
  完整递归标签。回归锁定 root 的分配前拒绝、13 字节累计边界、规范化和失败后的预算不变；
  15 项 extraction 测试和严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，全部
  Rust/Python/Node 构建、测试、发布包、类型、安全与输出校验通过，零组跳过；
- **`ExtractionReport` 的增长与失败文本已于 2026-08-22 改为可失败分配**：此前
  `extracted`、`skipped_existing`、`failures` 三张表都直接 `Vec::push`，source 直接
  `to_owned`，错误又直接 `to_string`；百万级有界条目仍可能在 allocator 失败时越过 Core
  的 `Result` 契约。现三张表逐项 `try_reserve`，source 走精确 fallible copy，错误通过
  自定义 `fmt::Write` 在每个片段写入前预留，仍保持 I/O/InvalidData/Unsupported 的原展示。
  原子写入前同时预留成功记录与并发 no-clobber skip 记录并只复制一次 source，因此不会出现
  文件已经发布、随后报告扩容失败而调用方看不到成功的状态；早期 existing skip 同样在副作用
  前完成分配。所有 `record_failure` 调用现在传播 `Result`，报告自身分配失败不会被降成普通
  资产失败继续运行。16 项 extraction 测试和严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，全部
  Rust/Python/Node 构建、测试、发布包、类型、安全与输出校验通过，零组跳过；
- **解包输出的 portable claims 索引已于 2026-08-22 改为 fallible 且纳入路径预算**：
  旧实现为每次 `contains/get/insert` 都通过 `to_string_lossy().to_lowercase()` 和
  `collect::<PathBuf>()` 重建 key，最多百万项的 `BTreeMap::insert` 又没有 `try_reserve`；
  allocator 失败仍可越过 Core 错误边界。claims 从不参与命名迭代，只做精确 membership，
  因而现改为逐次 `try_reserve` 的 `HashMap` 不改变稳定文件树。portable key 先遍历组件计算
  Unicode lowercase 展开后的精确 UTF-8 长度，以当前 retained 路径字节检查单项/累计预算，
  再一次预留并直接写入；临时查询不提交字节，真正插入在 HashMap 预留成功后才事务性提交，
  同一候选不再为 contains 与 insert 分配两次。回归覆盖 `İ -> i + combining dot` 展开、临时
  lookup 不消费预算、差一字节拒绝、失败后预算不变、ASCII-insensitive `~1` 碰撞和 claims
  数量；17 项 extraction 测试及严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，全部
  Rust/Python/Node 构建、测试、发布包、类型、安全与输出校验通过，零组跳过；
- **解包同名叶子与冲突父目录的后缀扫描已于 2026-08-25 线性化**：portable claims 已经
  是 O(1) membership，但 `allocate_path` 对每个同名条目仍从无后缀重新试到 `~N`；大量
  被清洗成同一名称的归档因此会重复构造并检查 1+2+...+N 个候选。父目录若先被一串文件
  `tree`/`tree~1`...占用，每个子项也会重新扫描整串。现叶子按 portable desired path 与
  file/directory kind 保存“下一个尚未检查的后缀”，父目录保存上次已经验证可复用的后缀；
  每次仍重新核对 claim 和文件系统状态，后来出现的新冲突会继续推进。游标表逐项
  `try_reserve`，新 key 与 claim 一起在所有检查成功后提交，并作为另一份 retained path
  计入原有累计路径预算。16,384 个同名叶子精确只探测 `N+1` 次；4,096 个阻塞父目录后缀
  加 16,384 个子项只探测 `4,096+16,384` 次；5 字节恰好预算成功、4 字节拒绝且 claims/
  游标/预算均不部分提交。23 项 extraction、完整 Core 614 项、畸形输入 6/6、严格 Clippy、
  Rust/Python/Node/typing/oracle 零跳过本地门禁及公开矩阵
  [32764678899](https://github.com/seiunx-dev/unity-rs/actions/runs/32764678899) 全绿；
- **解包输出路径的构造已于 2026-08-22 改为 checked/fallible**：claims 安全化之后，
  Bundle/WebData/ZIP 条目、`_unpacked` 容器目录、父目录碰撞重建、wrapper 解码名、`~N`
  碰撞后缀、最终绝对候选和临时发布名仍分别通过 `PathBuf::join`、`collect::<PathBuf>`、
  `clone` 或 `format!` 构造；其中完整路径往往在比较 `maximum_path_bytes` 前已经分配，
  `_unpacked`/`.decoded`/碰撞后缀也可能把已清洗组件再次推过 240 字节边界。现统一先以
  checked arithmetic 计算组件、分隔符和完整路径的精确编码长度，先检查 portable/调用方
  上限，再 `try_reserve_exact` 一次写入；空 parent 保持原路径而不会因 `push("")` 多出
  尾部分隔符。父目录各组件只 fallible copy 一次，碰撞候选从原始组件重建；内部输出名
  必须保持 UTF-8，不再通过 `to_string_lossy` 额外分配。回归覆盖空 parent、完整路径恰差
  一字节、父级重建、240 字节容器名追加 `_unpacked`、大小写 wrapper 去后缀、`.decoded`
  扩张、两位数碰撞后缀、suffix 后完整路径上限以及临时文件名；18 项 extraction 测试、
  Core 全目标编译和严格 workspace Clippy 已通过；完整工作树执行
  `tools/local_ci.py --fail-on-skip outputs quality rust python node typing security` 后，
  Rust/Python/Node 构建、测试、发布包、类型、安全与六类输出校验全部通过，零组跳过；
- **调用方解包输出根的规范化与祖先扫描已于 2026-08-22 线性化**：资产控制的相对路径
  收口后，`lexical_absolute` 仍通过不可失败 `to_owned/join` 复制调用方路径，规范化缓冲也
  未预留；查找最近存在祖先时，每退一层又把已积累 suffix 完整复制到新 `PathBuf`，深层
  不存在目录因此是 O(depth²) 拷贝。现绝对/相对根都走 fallible 路径副本或 join，规范化与
  符号链接祖先扫描按输入编码长度一次预留；最近祖先只缩短一份 ancestor，命中后通过
  `strip_prefix` 一次 fallible copy 剩余 suffix，创建目录前再为最终追加长度预留。绝对路径
  中的 `.`/`..` 语义、越过文件系统 root 的拒绝、macOS 受信任系统 alias 和 no-follow
  规则不变。新增回归锁定 lexical normalization、四层不存在 suffix 的 ancestor/suffix
  拆分与安全创建；19 项 extraction 测试、Core 全目标编译和严格 workspace Clippy 通过；
  完整工作树执行 `tools/local_ci.py --fail-on-skip outputs quality rust python node typing
  security` 后，Rust/Python/Node 构建、测试、发布包、类型、安全与六类输出校验全部通过，
  零组跳过；
- **Node 完整 Live2D adapter 已于 2026-08-22 验证**：新增同步
  `readLive2DPackagesWithSchemas` 和 Promise
  `readLive2DPackagesWithAclDecoder`，后者可在一个 worker 调用中同时接收
  外部 schema 与 ACL decoder。JavaScript fixture 真实走 stripped
  `CubismModel`/`CubismRenderer`、跨文件 `Texture2D`、Tuanjie
  `AnimatorController` 和两帧非空 ACL 曲线；无 schema、无 decoder、错误
  decoder、单文件/总预算均有独立断言。`tools/local_ci.py --fail-on-skip
  node` 的 debug/release addon、JS/TS、包内容和 npm tarball 八步全绿，随后
  `quality rust security typing` 也全部通过，零组跳过；
- **Node 绑定边界的可失败分配已于 2026-08-22 收口**：同步/异步内存输入、
  多文件 Region、外部 schema、FBX 候选与动画 PathID、Live2D 包元数据、模型
  贴图/诊断列表，以及 ACL/Oodle callback 的输入、输出与 f64→f32 数组转换，
  均先 `try_reserve` 再复制或移动，不再使用不可失败的 `to_vec`、`collect`、
  `Vec::with_capacity` 或大对象 `clone`。直接 physics3/exp3/motion3 JSON 和带
  贴图 FBX 输出改用同时检查写入计数、输出上限和分配失败的 bounded writer。
  后续审计又确认 napi-rs 对回调返回的 `Vec<T>` 会先按 JavaScript 数组长度执行
  `Vec::with_capacity`，使 ACL 返回值能在 Core 校验前触发不可失败的大分配。现 TSFN
  先把结果保留为 opaque JavaScript value，在事件循环侧核对对象形状、times/bindings/
  values 三张表的声明长度、frame×curve 乘法和 `AclDecodeLimits`，再以 `try_reserve`
  逐项复制；values 直接由 JavaScript `number` 窄化为 `f32`，不再生成中间
  `Vec<f64>`。三项 Rust 单测锁定超限、writer 计数不符和 ACL 有序窄化；JavaScript
  新回归用会抛异常的数组 getter 证明长度不匹配时没有读取任何元素，原有 fixture
  继续覆盖 fromBuffer/fromBuffers、模型贴图、ACL、Oodle 与完整 Live2D callback。
  同类审计随后发现 `fromBuffers(Vec<MemoryInput>)`、四个外部 schema 入口以及
  `MonoBehaviourSchema.nodes: Vec<SchemaNode>` 仍会在绑定函数运行前走 napi-rs 的
  eager `Vec::with_capacity`。现公开 TypeScript 形状不变，Rust 参数改为 raw `Array`：
  文件/schema/node 数量先验拒绝，名称和 schema 字符串先读 UTF-8 长度并计入单项/
  累计预算，再通过唯一审计过的 N-API copy 写入 `try_reserve` 成功的缓冲区。稀疏
  JavaScript 数组在首元素安装会抛异常的 getter，分别证明超限 input/schema/nodes
  都在访问任何元素前失败；生成声明仍保留原参数和可选 `undefined | null` 兼容。
  公开输入面继续反向枚举后，唯一剩余的嵌套列表是
  `CubismMotionTargets.parameters/parts`；它们原先同样会先变成 `Vec<String>`。
  现两张表先合并检查 `maximum_curves`，每个名称和累计 UTF-8 字节分别复用
  `CubismClipMotionReadLimits`，再逐项可失败复制。第四个稀疏数组 getter 回归证明
  超限 target 表也不会读取首元素；同步/ACL worker 两个入口共用该转换，TypeScript
  参数继续是可选 `CubismMotionTargets | undefined | null`。至此生成声明中所有
  JavaScript→Rust 数组输入均已分类：其余 `Array` 字段是 Rust→JavaScript 输出；
  `tools/local_ci.py --fail-on-skip quality node` 的 quality、debug/release addon、JS/TS、
  包内容与 npm tarball 全部通过，零组跳过；
- **Core/Python 整场景 OBJ/MTL 物化已于 2026-08-22 收口**：`Studio` 审计确认
  其他直接字节结果均已通过 `Region::read_to_vec` 或 `LimitedBuffer` 返回，唯独
  `read_model_obj` 仍把 OBJ 与 MTL 写入不可失败增长的普通 `Vec`，并用
  `to_owned` 复制材质库名。现三者分别改为有界 writer 和 `try_reserve`；Core
  单测锁定精确上限与越界状态，Python wheel/sdist API 测试锁定精确字节预算
  成功、少一字节稳定失败。修复后 `tools/local_ci.py --fail-on-skip quality rust
  python` 与独立 `node` 门禁全部通过，零组跳过；Node 同样直接复用这条 Core
  路径，并已由 debug/release addon、JavaScript、TypeScript 和 npm tarball 验证；
- **Python 内存 writer 审计已于 2026-08-22 收口**：继续逐项扫描 binding 的
  `Vec`/`String` 写入后，确认带贴图 FBX 仍直接写普通 `Vec`，expression、physics、
  fade-motion 和 clip-motion JSON 也只预留 64 KiB，超过后会回到不可失败扩容。
  五条路径现统一使用 `BoundedPythonOutput`：每次写入先检查累计上限并
  `try_reserve`，分配失败为 `MemoryError`，预算耗尽为 `ValueError`，真实文件 I/O
  仍保留标准 `OSError` 子类。Core 侧的四个 Live2D sink-validation 入口同时把
  纯内存输出预算错误从 `Error::Io` 纠正为 `Error::InvalidData`。Python API 回归
  锁定 textured FBX、exp3、physics3 和 motion3 的精确上限成功、少一字节失败；
  29 项 Core Live2D 定向测试、严格 Core/Python Clippy，以及零跳过的 `python`、
  `quality`、`rust`、`node` 门禁全部通过；
- **CLI 分配边界审计已于 2026-08-22 收口**：Live2D 候选表、清洗后的模型名、
  Unicode 小写碰撞键、FBX 批量名称以及 `info`/`list`/`inspect` 的版本和 class
  汇总均在增长前 `try_reserve`，分配失败稳定转为数据错误；输出转义改为直接实现
  `Display` 的流式写入，不再先 `collect` 一份等长或更长的临时字符串。统计仍在输出前
  排序，回归同时锁定重复计数、class/版本顺序以及 `İ` 小写扩张。当前 CLI 的 73 项
  unit/integration 测试与 all-target 严格 Clippy 已通过；
- **Node 的直接/异步 MonoBehaviour JSON 入口已于 2026-08-22 补齐**：此前只有
  `readMonoBehaviourJsonWithSchemas`，即使文件自带 TypeTree，JavaScript 也必须传一个
  无意义的空 schema 数组。现在 `readMonoBehaviourJson` 直接走内嵌树，stripped 对象仍
  明确要求可信 schema；直接与 schema 两条路径都另有 worker-backed Promise 入口，
  共用同一个输出预算和 Core reader，并返回 `embedded`/`schema` 来源。JavaScript
  fixture 锁定同步/异步内嵌树成功、异步少一字节预算失败、同步/异步 stripped 拒绝、
  异步 schema 恢复和错误 class 拒绝，生成的 napi-rs 声明由严格 TypeScript 调用；
- **Python 的 `MonoBehaviourSchema` 构建已于 2026-08-22 释放 GIL**：逐项审计绑定后，
  加载、资源、场景、FBX、模型、动画、TypeTree、纹理、Sprite、音频、材质、设置、
  Cubism、导出、Live2D 和解包等高成本 Core 路径本来已经通过 `Python::detach` 执行，
  唯一确定遗漏是 schema 构造仍可在持有 GIL 时验证并转换至多一百万个节点、累计
  256 MiB 字符串并建立 registry。审计继续发现 PyO3 原先会在 Rust 检查一百万节点/
  256 MiB 字符串上限前，先把整个 Python list 自动转换成拥有型 `Vec<String>`；现改为
  先读取 Python list 长度、逐项计算 UTF-8 累计预算，再做可失败复制，超限不会先物化
  无界 Rust 输入。只有这段 Python 字符串/tuple 转换保留在 GIL 内，TypeTreeNode 转换、
  registry 构建及其可失败分配移入 detached 区域。安装后 API 回归先用 1,000,001 个
  无效元素证明节点数闸门发生在元素转换之前，再把 Python 线程切换间隔提高到 1,000 秒，
  证明辅助线程在进入 Rust 构造器前没有运行，并要求它在 100,000 节点 schema 构造期间
  取得 GIL；release wheel、sdist、由 sdist 重建的 wheel 以及完整 API 测试均已通过。
  macOS x64 / Python 3.14 的公开 runner 随后暴露了“只给辅助线程一次调度窗口”造成的偶发
  误报；`3ca4c20` 把探针改为最多八个有界的 detached 构造窗口，同时继续把 Python 线程
  切换间隔保持在 1,000 秒，因此真正持有 GIL 的实现仍无法通过，正确释放 GIL 的实现则不再
  依赖单个操作系统调度瞬间。公开矩阵
  [32760790292](https://github.com/seiunx-dev/unity-rs/actions/runs/32760790292) 已在六平台
  Rust/Python/Node 作业中验证该探针；
  当前工作树随后执行 `tools/local_ci.py --fail-on-skip quality rust python node typing`
  全绿，零组跳过；
- **Python 调用方列表与 ACL adapter 输出的前置预算已于 2026-08-22 收口**：继续扫
  `Vec` 自动提取后发现 `from_memory_files(maximum_files=...)` 原先会在检查文件数、文件名
  和累计字节前先把整张 Python 表复制成 `Vec<(String, bytes)>`；schema 集合和 Cubism
  target 名称也依赖 PyO3 的 eager `Vec`，而 ACL callback 的 times/bindings/values 更会在
  Core 检查 frame×curve 与 `maximum_values` 前完整物化。现这些入口都先读取 Python list
  长度，按现有预算验证，再逐项以 `try_reserve`/可失败字符串或字节复制构造拥有型值；
  ACL adapter 持有与 Core 相同的 `AclDecodeLimits`，三张返回列表的声明长度和乘法预算在
  任何元素转换前完成。`unity_cn_key` 同时收紧到 `.pyi` 已声明的 `bytes | str`，直接复制
  精确 16 字节，不再把任意整数序列先抽成 `Vec<u8>`。安装后回归分别用无效文件 tuple、
  超限 schema 无效节点和 ACL 无效元素证明闸门发生在元素转换之前，并覆盖有效 raw/string
  key。继续向 Core 追踪后又发现 `AclClip::decode_with` 原先先调用 decoder、再检查声明的
  frame/curve/value 是否越过 `AclDecodeLimits`；恶意资产仍可先诱使任意 Rust/Python/Node
  adapter 分配超限输出。现只依赖请求头的三项检查在 decoder 调用前完成，返回后的 shape、
  binding、时间和值验证仍完整保留；Core 用会 panic 的 decoder 锁定三种超限请求均不可达
  callback，Python 另以标志位和无效列表元素分别证明前置拒绝与返回列表长度闸门；
- **CI 的常规作业步骤已于 2026-08-15 在本机完整复跑一遍**（`cargo fmt --check`、`clippy -D warnings`、`cargo doc -D warnings`、`cargo package -p unity-rs-core`、workspace 测试、Node 的 build/test/pack、Python 的 release wheel 与 sdist 两条路各跑 `installed_wheel.py` 与 `python_api.py`、UnityPy 差分、托管差分、vgmstream 音频差分），全部通过；当前主机上的 release CLI 与 Node tarball 也按发布作业的命令构建、直接执行/测试并检查了包内容。工作流另经锁定的 `actionlint` 1.7.12 验证，六个平台使用的 runner label 也逐项对过 [GitHub 当前官方清单](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。六平台发布矩阵本身尚未在 GitHub runner 上跑过，无法在本机复现的仍是 Windows 运行及其他架构的 runner 环境。之所以要手工跑一遍：CI 自 LZMA 那次提交起就没跑过，而这一跑就发现 Python 侧其实已经坏了一段时间（见上面 sprite fixture 那条）。
- **Linux 双架构交付面已于 2026-08-15 实跑**：`python3 tools/local_ci.py linux` 在 `linux/amd64` 与 `linux/arm64` 容器中分别完成 Core+CLI 全量测试、release CLI 构建/执行/法律文件 staging、`cp39-abi3` Python wheel 的 release 构建/安装/公开 API 测试，以及 Node 24 release addon 的构建/加载、JavaScript 与严格 TypeScript 消费测试、包内容检查和真实 npm tarball 构建。Node 在只读源码的干净临时副本中打包，宿主机已有的其他平台 addon 不可能混入产物；六个当前步骤均已实跑通过。该结果证明当前工作树的 Linux x86-64/ARM64 运行与发布产物面，不替代仍待 GitHub runner 验证的 Windows/macOS 六路发布矩阵。
- **发布法律文件已按实际产物验证（2026-08-15）**：根目录补齐项目 MIT `LICENSE`，`tools/generate_dependency_licenses.py` 从 `Cargo.lock` 与 `cargo metadata` 的非开发依赖闭包生成 97 个依赖的许可证全文聚合，并同步 Core、Python、Node 的分发副本。Core `.crate`、实际 npm tarball、Python wheel、Python sdist 及由 sdist 重建的 wheel、release CLI staging 目录均检查三份法律文件存在且与根副本逐字节一致；门禁第一次运行就抓到了 Core 内陈旧的 NOTICE，而不是只确认“有同名文件”。同轮还发现并修复 Python 3.9 测试文件使用 `int | str` 却未延迟求值的支持下限回归；3.9 wheel/API 已重新通过。
- **输出格式的独立校验已建立（2026-08-15）**：本项目产出的几种文件此前只有自己校自己——binary FBX 的读写是一起写的（记录头里存的是绝对结束偏移，写入端事后回填，读取端若与写入端理解一致，错的偏移一样会被接受），ASCII FBX 根本没有 oracle（托管走 FBX SDK），WAV 头由本项目写、又由本项目的 helper 读回去比对采样；PNG 的 chunk 分帧、CRC、IHDR 与逐行 filter 全是本项目手写的，而它的单元测试用本项目自己的 `png_crc32` 去核对 CRC——CRC 表要是错了，测试照样过，每个看图软件都打不开。现在四者都有了照格式另写的第二实现：`tools/validate_fbx_binary.py`（有界输入、节点/属性/深度/数组预算，magic、版本、每条记录的结束偏移、属性数与属性区长度、raw/deflated 数组的实际长度与单一完整 zlib member、嵌套列表的 null 记录、footer 的 id/对齐/重复版本/结尾 magic）、`tools/validate_fbx_ascii.py`（括号配平、必备段、`Definitions` 声明的 Count 与实际对象数、`C:` 连线引用的 id 是否存在、`*N` 数组的值个数）、`tools/validate_wav_output.py`（直接交给 Python 标准库的 `wave` 模块读，它不是为本项目写的，字段自相矛盾就会拒绝）、`tools/validate_png_output.py`（用标准库 `zlib` 提供独立的 CRC-32 与 inflate：逐 chunk 核对 CRC、校验 IHDR 为 8 位色彩类型 6 且非隔行、解压 IDAT、还原 0–4 号 filter、要求 IHDR 在首 IEND 在尾且其后无残留，最后把像素与源数据逐字节比对，包含 Unity 自下而上到 PNG 自上而下的翻行）。另外 `json.rs`（TypeTree dump 的 JSON emitter）也是手写分帧，而它原有的测试同样是拿手写字符串对比——两边同一套理解，天然一致，且覆盖的形状都很浅。现补了随机树的往返测试：确定性 xorshift 生成任意 `TypeValue` 树，pretty 与 compact 两种形式都交给 serde_json 的 parser（与 writer 不共享任何代码）解析，再与本模块声明的映射逐值比对；对象键序不比（解析后的对象无序），那部分仍由原有手写测试覆盖。末尾的覆盖断言不是装饰：十一个 variant、空容器、容器套容器、非有限浮点、未配对代理项各至少出现一次，把生成器限制成只出叶子会让 variant 断言失败。五种人为破坏均被抓住（裸 `NaN`、对象字段间少逗号、typeless 元数据少逗号、map 的 key/value 互换、float 加宽成 f64），其中前三种只有真正的 parser 才看得见。BMP 与 TGA 同理：两者都是本项目手写的二进制布局，而且是那种「写错了也照样能看」的布局——32 位 BMP 的通道掩码写错、TGA 的 descriptor 把行序写反，任何按惯例假定 BGRA/top-down 的读取器都会照样显示正确的图，直到遇上一个真按文件头来读的读取器；file size、pixel offset、image size 这类冗余字段更是错了也不影响解码。`tools/validate_image_output.py` 按两个格式的规则解码：行序取自文件头而不是假定，BMP 的通道排列取自文件声明的掩码而不是假定 BGRA，再与源像素逐字节比对。六种人为破坏均被抓住（BMP 红蓝掩码互换、BMP 高度写成正数即声称 bottom-up、BMP file size 少算文件头、BMP image size 清零、TGA descriptor 声称 bottom-up、行数据按 RGBA 而非 BGRA 写）。五者都接进了 `tools/local_ci.py` 的 `outputs` 组，并且都反向验证过：人为破坏 zlib 数组的截断、尾随垃圾、声明元素数，或改坏任一条记录的结束偏移、footer 区段、PNG IDAT 的 CRC、IEND 截断，都会由对应第二实现准确拒绝。
  值得记一笔的是，写这些校验器的过程中有三次是**校验器自己错了**——binary FBX 的 footer 最初漏了版本前的 4 个零字节；随后又把 `body + footer ID` 已经 16 字节对齐时规范要求的完整 padding 块误算成 0，普通 CLI fixture 因未落在该边界而一直通过，现由不调用 Rust writer 的 11 字节根名 fixture 锁定；WAV 那边则把 legacy AudioClip 的第一个字段当成了声道数（实际是 `format`，声道数是 `format >> 1`）。这恰恰是第二实现的价值：分歧会逼人去对格式或对读取端的实现，而不是让代码自我确认。
- **畸形输入扫描已建立（2026-08-15）**：`crates/unity-rs-core/tests/malformed_input.rs`。"不可信输入不崩溃"此前只有各模块零散的拒绝用例背书——那些覆盖的是有人想到的情况。现在拿有效输入批量损坏（单比特翻转、截断、把长度字段改成 `0xFFFFFFFF`/`0x80000000` 一类的毒值），每个结果只允许是解析成功或 `Err`，panic 即失败并报出确切的偏移量以便复现。两处防自欺的断言：种子本身必须能解析（否则损坏的是本来就坏的东西，全是空洞的 Err），以及损坏后仍能解析的比例必须够高（否则全被文件头挡掉，根本进不到对象表）。另有三条把损坏的数据送到真正的解码器：一条把真实压缩载荷（ASTC LDR/HDR、BC6H、Crunch）包进 Texture2D 再损坏后解码——第一版直接喂原始载荷，而那些不是序列化文件，reader 在嗅探阶段就退掉了，解码器一次都没跑到，正是这份文件要防的那种测试。这条验证的是外部解码器的 catch_unwind 护栏确实生效（已反向验证：把护栏外面插一个 panic，扫描立刻失败）。另一条损坏真实 FSB5 音频后走 `detect_direct_wav`/`write_direct_wav`，覆盖 codec 分发与 Vorbis 解码——Vorbis 尤其值得测，它的 setup header 是从表里重建的而不是从流里读的，损坏的流可以配上一个解析得干干净净的 setup。第三条损坏 MOC3 头：那是本项目里唯一一处由载荷自己决定 reader 下一步去哪儿看的地方（四个表偏移在固定位置，然后按定宽切标识符记录），因此偏移或计数被改就是直接的越界邀请，翻转位置也刻意偏向头部。三条都要求"损坏后仍有成功解出的样本"，否则说明解码器压根没跑到。
- **第二份提取的差分从 400 个 bundle 跑到了全部 2,778 个（2026-08-16），三处发现全在"报告方式"上而不在解析上**：此前这条差分只跑过前 400 个 case，而 637 个 OBJ 里有 636 个、577 个 txt 里几乎全部都在 400 之后——也就是说网格与文本这两类其实基本没被这条差分覆盖过。跑完之后：**OBJ 637 个比对、598 个逐值相同、39 个"本项目没导出"**；**txt 577 个里 571 个"不一致"**；**PNG 2,609 个里 214 个"超出容差"**。后两条查下来都不是本项目的问题，但也都不该就这么放着：
  - **txt 那 571 个是那份提取把二进制 TextAsset 当文本解码后写回去了**。抓到的证据是逐字节的：本项目导出的 `0039_02.txt` 以 `1f 8b 08` 开头（gzip magic），交给 Python 标准库 `gzip` 能解出 17,635 字节的 JSON；那份提取的同名文件第二个字节是 `?`（0x3f），`gzip` 直接拒绝，而且它有 1,044 个 `?` 字节、本项目只有 8 个。机制也已精确复现：把本项目的字节按 UTF-8 解码、每个解不出来的字节替换成一个 `?` 再编码回去，得到的就是那份提取的文件，**逐字节相等**（长度都是 2,403，因为每个坏字节换一个 `?`）。所以这一行比的不是两个实现的分歧，是一个被破坏了的 oracle。工具现在按这个精确变换归因，记成 `.txt oracle re-encoded` 而不是不一致；**没有放宽任何东西**：只要差异不能被这个变换完全解释就照样失败。已知盲点写在代码注释里——差异如果只落在本来就解不出来的字节上，两边都会塌成 `?`，这条看不见。
  - **PNG 那 214 个是报告方式把边界情况说成了灾难**。工具只报最坏的那个像素，于是一张 4,171,800 像素里只有 **21 个**像素超容差的 sprite，报出来是"alpha differs by 255, drawn value by 109.00"——和一张整体解码错误的贴图长得一模一样。实测那张最吓人的：373,063 个像素与提取不同，其中 324,432 个 alpha 完全相同、48,603 个 alpha 差 1（都是解码器容差内的颜色差），真正超限的只有 21 个，且全是本项目透明、提取那边有内容的遮罩边缘像素——正是本文早已记录的紧密网格边规则分歧，而本项目的光栅器由托管差分逐字节钉住。工具现在同时报出"超限像素数 / 总像素数"，于是"21 / 4,171,800"和"两百万 / 四百万"再也不会长得一样。带计数重跑全量后可以给出一句以前给不出的话：**2,609 张图里 214 张有分歧，逐张最多 62 个像素（占那张的 0.0015%），被报出来的这批合计 578 个坏像素 / 113,262,530 个像素 = 0.0005%**，且无一例外落在遮罩边缘。这不是"容差内"，是"分歧的规模已经量化"。
  - **第三处是问题清单的封顶方式**：原先 40 条是全局共享的，于是按字母序最靠前又最吵的 PNG 把名额占满，那 39 个"本项目没导出"的 mesh 一条都没出现在清单里，只能从统计行里看出来——我也正是这么发现它们的。现已按类别各自封顶，一类吵不会把另一类压掉。这和上面两条是同一个毛病的三种形态：**报告方式会让真实信号消失，和检查本身没跑是一回事**。
  - **第四处是我自己犯的错，而且犯的正是本文一直在批评的那种**：那 39 个"没导出"的 mesh，我只抽查了一个、看到"Mesh has no vertices"就写进了文档和提交信息说它们"至少部分是已记录的空 mesh"。逐个查完之后**只有 5 个是**，另外 **34 个是这个工具自己的名字归一化不够**——Spine 网格在资产里叫 `Skeleton Prefab Mesh [Spine GameObject (x)]`，那份提取写成 `Skeleton_Prefab_Mesh__Spine_GameObject__x__`，而工具只把空格换成下划线，方括号和圆括号没管，于是 34 个**明明导出了而且正确**的文件被报成"本项目什么都没写"。归一化补齐后（任何非字母数字/`_.-` 的字符各换一个 `_`）：**632 个 OBJ 比对、632 个逐值相同、0 个不同**，剩下 5 个才是真正的空 mesh。也就是说这条差分此前有 34 个网格从来没被真正比较过，而它们全部正确。教训跟前三处一样，只是这次主语是我：**从 n=1 外推就是在编**，而"没导出"这种最该警觉的报告，恰恰最值得先怀疑报告本身。
  - **第五处又是我从 n=1 外推，而且这次外推盖住了两个真东西（2026-08-16）**：上面那句"214 张 PNG 的分歧无一例外落在遮罩边缘"，是我详查了一张之后写的。把全部 2,609 张图逐像素分类（超限像素分成"一侧全透明=遮罩分歧"和"两侧都可见=颜色分歧"）之后：**4,496 个是遮罩像素，12,189,000 个是颜色像素**，后者集中在 3 张图上：
    - **`NotoSansJP-Regular_SDF_Atlas.png` 一张就占 12,181,114 个**。这是一张 4096×4096 的 **Alpha8** 字体图集：本项目展开成 `(255,255,255,a)`，那份提取展开成 `(0,0,0,a)`，alpha 完全相同、RGB 完全相反。查托管源码 `Texture2DConverter.DecodeAlpha8` 是 `buff.Fill(0xFF)` 之后只写 alpha 字节，**即白色，与本项目一致**——分歧还是在提取那一侧。但这暴露了一个真的覆盖缺口：**托管差分的纹理格式表全是块压缩格式，Alpha8 这类"未用通道填什么"属于约定而非唯一解的格式一个都没有**。已补 `assert_channel_convention_textures`，覆盖 Alpha8/ARGB4444/RGB24/RGB565/R16/RGBA4444/BGRA32/RHalf/RGHalf/RFloat/RGFloat/RG16/R8 共 13 个，全部与托管一致；反向验证：把 Alpha8 改成展开黑色（正是那份提取的做法），差分立刻抓住。补的过程中还踩到并修掉一个 fixture 陷阱——half/float 四个格式最初用随机字节，而托管写的是 `(byte)MathF.Round(v*255f)`，这个 cast 在超出 0..1 时是 C# 未定义的转换行为，随机字节几乎全部超界（还有 NaN 和无穷），比的就成了两种语言的越界转换而不是两个解码器；已改成在 f16 与 f32 中都能精确表示且落在 0..1 内的取值，四个格式随即一致。这与 ASTC fixture 用真编码器而不用随机字节是同一个道理。
    - **另外两张是同名冲突**：那个 bundle 里 60 多个 `Texture2D` 都叫 `item_icon`，本项目用 path ID 区分，工具把 path ID 剥掉后全部塌成同一个键，于是拿**两张不同的图标**在比——报出来是每通道差 3 到 5，小到正好像解码器分歧，这是最难察觉的一种假信号。工具现在检测到一个名字被多个资产占用就记为 `ambiguous name` 并拒绝比较，而不是随便挑一张比。那个 bundle 现在 72 张里 2 张记为无法比较、70 张一致。
  - **第六处：UnityPy 纹理差分本身有三处让它测不到东西（2026-08-16）**。`tools/unitypy_texture_diff.py` 写好之后从没在这份语料上整跑过，一跑就发现：（1）它只比 `Texture2D`，却导出整个 bundle 的所有类，于是**两个 Live2D bundle 里那个"物化超出 type tree 上限 11 字节"的 MonoBehaviour 一失败，整个 bundle 的纹理全被丢掉**——为一个不是纹理的东西丢纹理；现已只导 class 28。（2）导不出东西的 bundle 被静默跳过，跟"比过了且一致"长得一样；现在汇总里报出跳过数（本语料 2,778 个里 1,763 个，绝大多数是压根没有纹理的 bundle，但这是一句陈述而不是一个空白）。（3）它把 `Alpha8` 归进"不需要解码、必须逐字节相同"那一类，于是报"两边解法相同、本应完全一致"——而 `Alpha8` 只存 alpha，RGB 填什么是约定：本项目照托管填白，UnityPy 填黑，那张字体图集 **16,777,216 个 alpha 字节两边完全相同**。现在按"该格式没存的通道可以不同、alpha 必须逐字节相同"归因并计数，alpha 错了照样是缺陷（已验证这道闸有效）。修完之后全量跑通：**RGBA32 143/143 逐字节相同；ASTC 4x4 139 张、ASTC 6x6 931 张与 ARM 参考解码器每通道差不超过 1；Alpha8 那张在该格式真正存储的每一个字节上都相同**。这条给出了纹理路径的第三方实现佐证，而且与上面 Alpha8 那条互相印证：托管说白、本项目跟托管，UnityPy 与另一份提取说黑。
  - 五处全部修完后的最终全量结果（2,778 个 bundle）：`.obj` **637 比对 / 632 逐值相同 / 5 未导出**（全为 Unity 写的空 mesh，本项目有意拒绝）；`.png` **2,609 比对 / 248 逐像素相同 / 2,147 在解码器容差内 / 212 有分歧 / 2 记为同名无法比较 / 0 未导出**；`.txt` **577 比对 / 6 逐字节相同 / 571 归因为 oracle 重编码 / 0 未导出 / 0 不一致**。那 212 张里，颜色分歧只剩 Alpha8 字体图集一张（已查明是提取那侧的约定，且已由新增的托管差分行长期把守），其余全部是遮罩边缘的 4,496 个像素。"没导出"那一列现在只剩 5 个，而且每一个都说得出理由。**零处指向本项目的解析或导出错误**——这句话在四处报告缺陷修掉之前是说不出口的，因为当时的数字里混着 41 个工具自己认不出名字的文件。
- **收口后的真实语料复跑与 schema 版本作用域实证（2026-08-15）**：这一批改动里有两处会碰真实数据的行为变化，收口时都单独验过，而不是靠"合成测试过了"推断。（1）**语料没有回归**：用临时只读 manifest 在 release 下重跑 6000.3.12f1 的 2,778 个 Addressables bundle，得到 **243,617 个对象、104,565 个有解析载荷**，与本文此前记录的数字**逐个相同**——纹理首图放宽与模型贴图预算/原子发布这两处改动没有让任何东西多解析或少解析。顺带印证了前几天那条错误信息的修复确实有用：第一次跑我自己把用例预算declare小了，报出来的是"materializing Live2D textures exhausted the 536870912 byte total budget with 7953295 bytes left"，直接指出是哪个预算见底、还剩多少，而不是像从前那样去怪一张纹理。（2）**schema 版本作用域是有效而不是有害的**：从这款游戏的 187 个 DummyDll 现生成一份带 `unity_version` 的 schema（826 个类，全部标 `6000.3.12f1`），跑 `tools/mono_schema_diff.py`：先取 120 个 bundle 抽查（1,288 个对象全对），随后**跑满全部 2,777 个 bundle——94,743 个经 schema 读出的对象取值与 Unity 自己的树逐一相同**（53,380 个连 JSON 都逐字节相同，41,363 个只差字段名，即已记录的 `UnityEngine.Rect` 那类命名分歧），零取值不符。对照本文此前记录的 94,713 个，加版本门之后**一个匹配都没少，反而多了 30 个**——也就是说这道门没有把任何本该读到的对象挡在外面。抽查和跑满都记在这里：这一天里被抽查坑过四次，抽查过了不等于跑满会过。反向验证同样重要：把同一份 826 个条目改标成 `2022.3.62f1` 再跑，**匹配数为 0**，工具以"nothing was read through a schema, so nothing was checked"退出码 1 失败。两条合起来才说明这个门是按预期咬合的：对得上就照常读，对不上就明确拒绝，而不是安静地套一份别的版本的布局。
- **收口后的完整本机复跑（2026-08-15）**：在最终提交的树上跑了 `tools/local_ci.py` 的**全部 13 组**（quality、rust、cli-package、oracle、audio、outputs、cross、linux、node、python、python314、typing、unitypy），**全部通过、零跳过**——也就是说 .NET 托管差分、vgmstream 音频差分、UnityPy 第三实现差分、输出格式的独立校验器、Linux amd64/arm64 容器里的运行与发布产物、Python 3.14 abi3 前向兼容、严格 mypy 消费端这些可选组这一轮都真的执行了，而不是因为缺工具被记成跳过。另外收口时带源码的提交逐个在独立 worktree 里用 `cargo check --workspace --all-targets --locked` 验证过能单独编译，没有哪个要靠后面的提交才能构建；只改仓库元数据或 Markdown 的提交不进构建。
- **收口门禁新增严格跳过策略（2026-08-16）**：`tools/local_ci.py --fail-on-skip ...` 在任何请求分组因缺少工具或语义前置条件而跳过时返回失败，日常不带该选项的兼容行为仍会报告并允许可选组跳过。七项回归分别锁定默认跳过成功、严格跳过失败、严格可运行成功、复合平台组缺第二工具必须跳过、未知组用法错误，以及锁定工具版本不符时必须跳过/精确相同时才可运行；测试本身已接入 `quality`。随后按路线图原样执行严格 `quality rust node python typing` 与 `linux`，本机和 Linux amd64/arm64 的全部步骤均通过、零跳过。
- **Python 主接口审计已闭环（2026-08-22）**：逐组对照 Core 的加载、枚举、资源、专用 reader、场景/模型、Live2D、导出和解包能力后，没有发现仍缺失的稳定 Python 主能力；Rust 的 borrowed view、`write_*` 与低层 ownership escape 分别由 Python 的拥有型元数据/键读取、bytes 或原子路径输出和内部所有权边界替代，映射记录在 `docs/python-api-audit.md`。原来的严格 mypy 消费端漏掉了分页/迭代、按路径资源、静态 binary FBX、ACL 原始输入和 TypeTree JSON/dump 等 11 个真实方法；现已全部调用。随后对托管对象构造分派的机械复核先发现 class 111 `Animation` 与 class 221 `AnimatorOverrideController` 已有完整 Core parser，却没有高层 Rust/Python 入口；现分别增加有界的 GameObject/default/ordered-clip reader 与 base-controller/ordered-substitution reader。继续复核又确认 class 142 `AssetBundle`、147 `ResourceManager`、150 `PreloadData` 的生产 loader 解析器虽已建立容器索引，却没有高层直接读取入口；现同样暴露 inherited/effective 名称、bundle dependencies 和有序 preload/container/reference 表，Python 可分别收紧条目、单字符串和累计字符串预算，兼容 effective-name 的额外字符串副本也计入累计预算。最后补齐了此前只被渲染链间接使用的 class 213 `Sprite` 和 class 687078895 `SpriteAtlas` 完整元数据：Sprite 返回 rect/pivot/border、复合 atlas key/tags/PPtr、resident texture/alpha/secondary references、settings/UV/downscale 和规范化 tight-mesh 三角形，SpriteAtlas 返回原始复合 GUID key、有序 render-data、颜色/alpha PPtr、裁剪/UV/settings、secondary textures、tag 与 variant 标志；版本范围仍严格限定在已有 parser、fixture 和差分证据覆盖的边界。release wheel 和 sdist 重建 wheel 用合成 v22 普通/tight Sprite 与 atlas 对象逐字段、顺序和预算验证这些 reader。审计器同时解析 `studio.rs` 的四个高层 impl，当前 **107 个公开 Core 方法**中 103 个必须指向实际存在的 Python symbol，`from_collection`/`collection`/`into_collection`/`object_by_index` 四个接受、借用或返回 Rust 内部类型的入口明确保留 Rust-only；Core 新增公开方法未分类、映射目标从 stub 消失都会失败。Python 方向则要求 **66 个公开方法和 4 个属性**全在严格消费端出现。六项正反回归覆盖当前计数、漏方法/属性、缺少类/消费函数、Core 新方法未分类和 Python target 消失；GitHub workflow 结构门禁还要求这组自测不能被移除。当前树重新执行严格 `quality typing`、release wheel、sdist、从 sdist 重建 wheel、两条安装后公开面和两条完整 API 测试，全部通过、零跳过。
- **可选 Node 主接口也有了独立机器审计（2026-08-23）**：`tools/check_node_api_surface.py` 不复用 Python 的映射结论，只复用 Core 方法解析器；同样对 107 个 Core 高层方法逐项分类，要求 103 个真实 Node target 同时存在于 `#[napi]` Rust 源和 `index.d.ts`，仅保留与 Python 相同的 4 个 Rust ownership/borrow 入口。反向再要求 Rust addon 与生成声明的 `UnityRs` 精确一致，并让当前 **85 个方法和 4 个属性**全部出现在严格 TypeScript 消费端；注释先被剥离，不能冒充调用。八项反向测试覆盖新 Core 方法、Rust/声明 target 消失、addon/声明漂移、漏消费、注释掉调用和对象字段消失，GitHub workflow 结构门禁又要求审计及其自测都不能被移除。盘点没有只产出表格：它发现 `fromBuffer`/`fromBuffers` 只能限制绑定复制字节，不能把 `OpenOptions` 交给 Core，导致内存 UnityCN、stripped version、skip policy 和路径预算与磁盘入口不等价；现把 options 作为旧字节上限之后的末尾可选参数，旧位置调用不变。JavaScript 回归真实证明单 buffer 的 `maximumPathBytes` 与多 buffer 的 `maximumInputFiles` 在遍历/解析前生效，生成声明与 pinned TypeScript 编译通过。详细映射和边界见 `docs/node-api-audit.md`；Node 仍是可选交付面，不会反过来成为 Rust/Python 完成的前置条件。变更后严格 `quality` 已增长为 15/15，Node 的 debug/release addon、JavaScript/TypeScript、包内容和 tarball 8/8，以及 workspace build/test 2/2 全部通过且零组跳过。
  同一轮继续核对 worker 入口，发现 `openAsync`/`fromBufferAsync` 仍丢失同一份 options；现同样在旧参数之后追加可选 `OpenOptions`，task 持有配置并在 worker 内调用 Core 的 `*_with_options`，实际 Promise 回归分别证明路径和内存标签预算生效，旧一/三参数调用保持通过。
  最后把发布包本身纳入同一约束：临时消费者从实际 `.tgz` 安装后，测试会解析安装后的
  `index.d.ts`，再与加载出来的 native `UnityRs` class 双向比较 85 个方法和 4 个 getter；
  保持成员总数不变、只重命名一个声明方法也必须失败。隔离 npm 缓存下该安装门禁已通过，
  因此当前证据不再依赖源码树中的 addon 或声明碰巧正确。随后重新执行完整
  `outputs quality rust python node typing security cross` 以及 `linux`：本机全部构建、测试、
  打包、类型、安全和交叉编译步骤，连同 Linux amd64/arm64 的 Core/CLI、release CLI、
  Python wheel、Node addon 与实际 npm tarball 安装测试均通过，零组跳过。
- **可选 Node 绑定同步了完整 `Sprite`/`SpriteAtlas` 元数据（2026-08-22）**：`readSpriteMetadata` 与 `readSpriteAtlas` 直接复用同一组 Core reader。Sprite 返回 object index/name/rect/pivot/border、原始 16-byte GUID `Buffer` 与 `bigint` key/PathID、atlas tags/PPtr、resident color/alpha/secondary references、解码后的 settings、UV/downscale 和规范化 tight-mesh triangles；条目、单字符串、累计字符串和 mesh bytes 预算可分别收紧。SpriteAtlas 返回有序 render-data、颜色/alpha PPtr、Rect/Vector/UV、settings、secondary textures、tag 和 variant 标志。JavaScript fixture 对普通 Sprite 逐字段核对并覆盖四个低预算拒绝分支，另一个有效 tight fixture 证明 triangle 跨过 napi 转换；atlas fixture 则故意把较大的原始 GUID key 写在前面并带一项 secondary texture，验证绑定保留 Core 的确定排序和全部嵌套字段。napi-rs 重生成的声明由严格 TypeScript 消费端实际调用，release addon build、JavaScript 和 TypeScript 测试全部通过。
- **Node 的 `AnimationClipInfo` 已从简略 shape 补到完整稳定元数据（2026-08-22）**：保留原方法名和旧字段，新增 PathID、high-quality 标志、PPtr 曲线、muscle size 与 streamed/dense/constant 计数、ACL frame/bone/rate/curve/track bytes/decoder/fast-sample 字段，以及外部 streaming offset/size/path。`maximumBytes` 现在同时收紧对象、字符串、packed/reference 与累计分配预算，两个 clip 入口共用同一限制构造。现有完整 Unity 2022.2 muscle fixture 验证无 ACL/streaming 时所有 optional 字段保持 `undefined`；Tuanjie 2022.3.55t4 fixture 验证 32-byte ACL 容器、decoder map、fast-sample 与空路径 StreamingInfo。生成声明中的 `bigint`/optional 字段由严格 TypeScript 消费端实际赋值，不再只做方法存在性 smoke。
- **Node 的 `Avatar` 已从四字段摘要补到完整稳定元数据（2026-08-22）**：`readAvatar` 现在返回 PathID、声明大小、普通/人形骨架节点数、保序 TOS `(hash,path)`、HumanDescription 的人形骨/骨架骨计数和 root-motion bone name；旧 `declaredSize` 保留为 `declaredAvatarSize` 的兼容别名。`maximumBytes` 同时收紧对象、单/累计字符串、引用与累计分配预算。由 Python 已通过的 Tuanjie 2022.3.55t4 fixture 逐字节移植到 JavaScript，完整对象解析后逐字段核对 `0xfeedbeef -> Root/Hips` 与 `Hips` root motion，并证明一字节预算拒绝；生成声明的 `AvatarPathEntry[]` 与 `bigint` PathID 进入严格 TypeScript 消费。
- **六平台发布配置有了自动结构门禁（2026-08-22，2026-08-24 加固）**：`tools/check_ci_matrix.py` 不把“workflow 里看起来有 matrix”当证据，而是逐 job 要求 Python wheel、CLI 和可选 Node 包各自恰好包含 Linux/Windows/macOS × x86-64/ARM64 六个 `(runner, artifact)` 对；Python 必须安装并跑 wheel API/mypy，CLI 必须执行精确 release 二进制的 `--help` 并带法律文件 staging，Node 必须测试 release addon、检查 npm 内容并真正 `npm pack`。门禁自身接入本地与 GitHub `quality`。公开 PR 评审随后指出，仅删除整行注释仍不足以证明命令真的执行：`env` 值或 `echo` 可以携带同样文本，CLI 的顺序比较也仍在原始 job 字符串上搜索，注释掉的 staging 能冒充提交点。现审计器只从步骤缩进下的 YAML `run` scalar/literal/folded block 提取可执行命令；命令必须是完整命令或受控前缀，环境字段、step 名称、注释与 `echo` 都不计证据；staging 与 staged-binary smoke 的顺序按实际命令位置比较，同一个 multiline block 内也不能逆序。20 项回归覆盖当前配置、缺平台、重复键、移除步骤、注释命令、`env`/`echo` 冒充、multiline `echo`、“注释 staging 在前、真实 staging 在 smoke 后”以及“同一 run block 内 smoke 在 staging 前”的绕过。收口审查还发现 GitHub `quality` 原先没有执行 `tools/test_local_ci.py`，所以 `--fail-on-skip` 的正反策略虽在本地跑、却可能在正式 CI 中退化；现已补上并由反向测试锁定。锁定的 `actionlint 1.7.12` 继续承担通用 YAML/Actions 表达式检查，专用脚本只验证项目声明的发布证据。外部状态也已按 API 核实：`81b02a5` 的 [run 31947620200](https://github.com/seiunx-dev/unity-rs/actions/runs/31947620200) 在 2026-08-16 启动后约六秒即失败，所有 16 个主 job 都是 `steps=[]` 且没有日志，两个依赖 job 被跳过，因此没有一条仓库命令实际执行；仓库级 Actions 当前为 enabled、允许全部 actions，默认 workflow 权限为 write，所以不是仓库禁用了 Actions。API 没有给出可进一步归因的错误文本；组织计费端点需要当前 token 没有的 `admin:org` 权限，不能越权把根因写成某一种计费状态，也不能把这次 run 写成代码失败或绿色证据。后续 2026-08-24 的正式六平台绿色记录见下方执行清单。
- **仓库公开后正式 runner 与发布矩阵全绿（2026-08-24）**：
  [`seiunx-dev/unity-rs`](https://github.com/seiunx-dev/unity-rs) 已从 Private 改为 Public，
  此前在分配 runner 前由账户账单策略拒绝的阻塞随之解除。收口分支位于
  [PR #1](https://github.com/seiunx-dev/unity-rs/pull/1)，上述绿色运行验证的代码 head 为
  `a07642e`。
  [PR run 32659993206](https://github.com/seiunx-dev/unity-rs/actions/runs/32659993206)
  的 16 个主 job 全部成功，覆盖三平台 Rust、三平台 Node、六平台 Python wheel、质量门禁和
  managed/UnityPy/vgmstream 三类差分 oracle；随后
  [workflow_dispatch run 32660298990](https://github.com/seiunx-dev/unity-rs/actions/runs/32660298990)
  28/28 成功，其中额外 12 项为 Linux/Windows/macOS × x64/arm64 的 CLI 与 Node 发布制品。
  公开 runner 首轮实际发现并修正了 vgmstream 安装目录、其信息命令的特殊退出码、Windows
  npm 启动方式、已加载 `.node` DLL 的 Windows 清理锁，以及 SILK fixture 在固定 r2117 Linux
  oracle 上的第二个实测 profile；这些都经过本地零跳过门禁和后续真实 runner 复验，不再只是
  workflow 结构检查。
- **旧无头 WorkMode 的入口名已补齐（2026-08-22）**：逐项核对托管 `WorkMode` 与 CLI alias 后，原生 CLI 现接受 Extract、Export、ExportRaw、Dump、Info、Live2D（`l2d`/`live2d`）、SplitObjects 和 Animator 的旧式 `-m` 写法。新补的两个 Live2D alias 进入完整 `live2d-package` 路径，而不是只写散件 MOC；仍要求显式 `-o`，并拒绝 overwrite 与无关的 extension flag，以保持当前原子 no-clobber 契约而不恢复隐式 `ASExport` 副作用。解析单测同时覆盖两个 alias、缺输出和两个拒绝分支；进程级测试逐个核对 MOC、model3、PNG 和临时文件清理。完整 CLI 共 **72 项测试**通过，严格 CLI Clippy 通过；同一工作树随后执行严格 `quality rust typing`，共 12 项快速质量步骤、workspace build/test 和 Python 3.9 严格消费端全部通过、零跳过。
- **锁定依赖图现有 RustSec 门禁（2026-08-22）**：`cargo-audit 0.22.2` 的 MSRV 正好是项目下限 Rust 1.88；当前数据库载入 1,225 条 advisory，扫描 `Cargo.lock` 的 103 个依赖后没有漏洞、unsound 或 yanked 包。唯一输出是 `paste 1.0.15` 的停止维护警告 RUSTSEC-2024-0436，它只经 `texture2ddecoder 0.1.2` 进入构建图；后者没有新版，而为去掉一个无已知漏洞的 proc-macro 完整 vendor 需要再维护约 8,685 行上游解码代码，因此现阶段明确允许 unmaintained 提示，但不忽略 advisory，也不放宽漏洞/unsound/yanked。GitHub `quality` 会安装锁定版本并执行该策略；本地 `security` 组不仅检查命令存在，还要求 `--version` 精确为同一个 `cargo-audit 0.22.2`，旧版或无法读取版本时按缺少前置条件处理，配合 `--fail-on-skip` 必然失败而不会产出较弱的假证据。对应正反测试与“注释掉 GitHub 执行行”的结构测试都已通过。
- **SerializedFile v1-v4 保持验收边界而不猜实现（2026-08-22）**：托管枚举定义 1、2、3 后直接跳到 5，格式 4 没有公开定义；v1-v3 又早于当前 TPK/真实语料能提供的最早 TypeTree。Rust 内部已有 v2/v3 recursive-node 字段门只说明代码为未来样本留了位置，不能当作兼容证据。生产入口继续在完整 header/layout 校验后返回带具体版本号的 `Unsupported`，新增回归逐一锁定 1、2、3、4 不得退化成 `InvalidData`、误解析或 panic。只有拿到真实文件和可独立核验的树/对象载荷后才扩展差分矩阵；在此之前不从 v5 树反推更老格式。
- **Python 门禁在断言被关掉时会整片变绿（2026-08-15）**：收口当前工作树时查出的一条自欺路径，而且正好压在刚补上的法律文件门禁上。仓库里当门禁跑的 Python 脚本——`check_delivery_scope.py`、`check_core_package.py`、`test_monoschema.py`、`sdist_contents.py`、`installed_wheel.py`、`python_api.py`——判定全部是裸 `assert`，而 `-O` 与 `PYTHONOPTIMIZE` 是把 assert 整条从字节码里删掉，不是跳过。实测而不是推演：造一个只带 README、三份法律文件和全部必需源码一个都没有的 sdist，`PYTHONOPTIMIZE=1 python3 tests/sdist_contents.py` 退出码 0、一个字都不说；正常模式下它会把缺的十二项逐个列出来。暴露面是具体的：CI 里 `python3 tools/*.py` 和 `python tests/sdist_contents.py` 这几步都不带 `-I`，环境变量能直接生效；带 `-I` 的那几步隐含 `-E`，只有命令行 `-O` 能触发。六个门禁与 `local_ci.py` 现在都在模块层拒绝在断言关闭时运行，`local_ci.py` 那道同时挡住它内嵌的四个 `-c` 断言和它派生的全部子进程（子进程继承同一份环境）。七处都反向验证过：`-O`/`PYTHONOPTIMIZE=1` 下一律以退出码 1 拒绝并说明原因，正常模式下逐个照常通过。
- **同一条路子又扫了三处，两处干净、一处有修（2026-08-15）**：既然"检查其实没在检查"这一类当天已经抓到两个，就把它扫完。（1）**`#[ignore]` 全部有归属**：12 个里 1 个是托管差分（有 CI job 与 `local_ci oracle`）、10 个是 vgmstream 音频差分（有 `audio-oracle` job 与 `local_ci audio`）、1 个是 `real_corpus`（按设计只能靠私有语料手动跑，`corpus/README.md` 写明）。没有第二个"写好了但没有任何东西会跑它"的音频差分那种情况。（2）**托管差分的每一行都已有反空洞断言**：ASTC/块格式解码为 null、Live2D 各文档没产出、split 组没对象、版本门没对象、容器没文件、曲线没 segment、关键帧行取值为零——都会直接失败并说明"比较证明不了任何东西"，不是靠两边同为空而通过。这一条是负结果，但值得记：这类扫描不写下来，下次还会有人重扫一遍。（3）**BC6H fixture 的位数检查用的是 `debug_assert_eq!`**，release 下整条消失，而那份提交入库的载荷正是这个函数产出的——重新生成时它是唯一一个会指出"块没填满 128 位"的东西，短了就会被写进文件然后永远跟自己比对。已改成 `assert_eq!`；反向验证：把一个十位端点写成九位，release 下报 `125` 对 `128`，而改之前一声不响。多填的情况本来就会被数组边界挡住，少填的只有这一条能抓。
- **UnityCN 没有第二实现这条已按版本核实（2026-08-15）**：此前写的是"本机这份 UnityPy 也没有 UnityCN"，属于陈述而非核对。实测本机 UnityPy **1.25.0** 的包内没有任何文件提到 UnityCN，托管 AssetStudio 仍然只检测不解密。结论不变，但现在是有版本号的核实结果，而不是印象。
- **顺带补上一处没有守卫的比较（2026-08-15）**：`unitypy_oracle.py` 的 `assert_double_precision` 对 15 个 fixture 每个都跑一遍，而只有一个 fixture 带那个 `Double` 字段——也就是说它今天确实比了一个值，但没有任何东西保证它明天还在比。旁边的 TypeTree 行早就有"比较了多少棵树、为零即失败"的计数器，这一条没有。现在按同样的形状把计数汇总到 `main`，全程为零即失败，摘要里也报出实际比了几个。反向验证：把字段名换成一个不存在的键（模拟 fixture 哪天不再携带它），差分立刻以"no double field was compared"失败，而在此之前那样改是全绿的。
- **生产目标 panic/占位审计补查（2026-08-23）**：项目生产代码没有遗留项目级 `TODO`、`todo!` 或 `unimplemented!`；vendored 纹理解码器的上游注释不冒充本项目功能。额外以 Clippy 的 `unwrap_used`/`expect_used`/`panic` 扫描 Core、CLI、Python 与 Node 生产 target 后，确认 Python 自身没有该类调用，Node 的命中均为受支持 64 位目标上的有界整数转换，CLI 临时文件命中由私有状态机约束，vendored ASTC panic 已在统一解码边界被 `catch_unwind` 转为错误。唯一仍把两个输入派生预算相加后 `expect` 的动画构建状态已改为 checked `InvalidData`；直接构造 `usize::MAX + 1` 的回归证明其返回错误而不 panic；
- Core 当前 554 项常规测试通过，12 项依赖可选 vgmstream oracle 的测试保持忽略并已在音频 oracle 门中另行通过；其中 SILK 项只接受两个精确实测 profile：固定 r2117 Linux amd64 的 `(shift=0, worst=0)`，或此前本地 oracle/build 的 `(shift=-2, worst=276)`；即使 shift 相同但差值为 275 也会被反向测试拒绝。CLI 全目标当前 89 项测试通过；
- C#→Rust 托管差分 oracle 通过；
- TypeTree dump 浮点文本对照 .NET 10 实测生成的 849 个取值（边界值 + 位模式扫描）逐字节一致，期望值以 fixture 形式入库；
- `cargo doc --workspace --no-deps` 在 `-D warnings` 下通过；
- `cargo package -p unity-rs-core` 的包内容和独立重建通过；
- Python 锁定 wheel、sdist 及从 sdist 重建的 wheel 均可安装并通过 API/类型桩测试；wheel 的运行时公开面与 `.pyi` 双向核对，主读取路径另由严格 Python 3.9 mypy 消费端编译；
- Node 原生 addon 测试、严格 TypeScript 消费端声明编译和实际 npm 包内容检查通过；
- `git diff --check` 通过。

**当前未提交工作树的真实语料复跑（2026-08-15）**：用临时、只读、无
expected snapshot 的 manifest 在 release 模式重新执行 `real_corpus`，215.53 秒
通过且零读取错误。当前本机的 Unity 2022.3.62f2 播放器目录是一个 21 文件
子集，得到 570,445 个对象、177,431 个有解析载荷；Unity 6000.3.12f1 的
2,778 个 Addressables bundle 仍得到 243,617 个对象、104,565 个有解析载荷，
与此前记录完全一致。前者不是此前 23 文件输入的同一份完整集合，所以这次
结果单列，不用较小数字覆盖原记录。两条都只证明当前工作树能完整遍历并解析
所支持载荷，不冒充托管 snapshot 差分；`real_corpus` 仍要求每个 read-only
用例至少包含一个文件、一个对象和一个解析载荷，无法靠空目录或未识别输入
假绿。

**当前未提交工作树的外部 oracle 复跑（2026-08-15）**：本机的 .NET 10、
独立 `Team-Haruki/AssetStudio` checkout 与 `vgmstream-cli` 均可用，
`tools/local_ci.py oracle audio` 的托管 manifest 差分、MonoBehaviour schema
生成器端到端和全部音频差分三步均通过，零组跳过。另从当前源码重新构建
`cp39-abi3` wheel，装入继承 Homebrew Python 3.14 的隔离环境后，以 UnityPy
1.25.0 运行第三实现差分：15 个 fixture 全部一致，其中 1 个内嵌 TypeTree
对象做了值比较；1 项名称比较因 UnityPy 自带数据库不覆盖该 class/version
而明确跳过。该结果验证提交中的合成矩阵，不替代上面的真实 corpus 覆盖；
两种证据分别记录，避免把“小而有 oracle”与“大而 read-only”混成一个完成
声明。

**Python 默认参数合同已加入安装后校验（2026-08-15）**：Core→Python
公开面逐项复核没有发现缺失的主要 reader，但查出 `read_model_obj` 与
`read_fbx_with_textures` 的 `.pyi` 把默认输出上限写成 1 GiB，实际 PyO3
方法使用的是全局 512 MiB 默认。原 wheel 门禁只比较名称，且 PyO3 对 Rust
const 形式的默认值在 `inspect.signature` 里显示为 `Ellipsis`，所以两边漂移
仍会全绿。现已统一为 512 MiB，并把所有 `UnityRs` 签名默认值展开成
运行时可观察的字面量；安装后门禁逐方法解析 `.pyi`，比较每一个可求值默认
参数，遇到 `Ellipsis`、缺失或数值不一致即失败。修复后的当前 abi3 wheel
已通过完整 API 测试、UnityPy 差分和严格 mypy 消费测试；workspace 测试、
Clippy、rustfmt 与 diff-check 同轮通过。

**Python 模型贴图格式不再被固定为 PNG（2026-08-15）**：
`read_model_obj` 与 `read_fbx_with_textures` 现在公开 keyword-only 的
`texture_format`，默认仍为 `png`，并复用 Core 的
JPEG/PNG/BMP/TGA/lossless-WebP/raw-RGBA 格式解析。端到端 fixture 包含
GameObject、Renderer、Material、Mesh 和真实 Texture2D 引用，分别断言
OBJ 返回 `.rgba`/`HARUKI_RGBAIR_V1`、FBX 返回并引用 `.tga`，未知格式稳定
报 `ValueError`；这也纠正了最初 fixture 把 2022 Renderer 的材质数组写在
lightmap 固定前缀之前、因而实际没有覆盖材质贴图的错误。当前 wheel 与
sdist 重建 wheel 的安装后 API 测试、stub surface 和严格 mypy 消费均通过。

**Python 模型贴图预算已从 Core 贯通到公开 API（2026-08-15）**：
新增 `ModelTextureLimits`，让 `read_model_obj` 与
`read_fbx_with_textures` 分别限制贴图数量、累计编码字节和单张贴图的源
payload、解码输出与 decoder 工作区；不传时保持原有
4096 张/2 GiB 累计/512 MiB 单张默认。数量预算在读取、解码和编码下一张
贴图之前收费，避免调用方设为零后仍被迫处理一张恶意大纹理；累计预算越界
使整个调用报 `ValueError`，编码缓冲按剩余总量封顶并用 `try_reserve` 增长，
不会先形成一份越界的完整 `Vec`；单张解析或预算失败则沿用模型导出的容错
合同记入 `skipped`。同级贴图写盘也已从直接创建最终文件改为同目录临时文件、
`sync_all` 和硬链接 no-clobber 发布，失败或放弃时由 Drop 清理临时文件，不会
留下一个以后被“已存在”误认成成功的截断图片。真实
Renderer→Material→Texture2D fixture 覆盖了三类预算，运行时类、包顶层导出、
`.pyi` 默认值和严格 mypy 消费端均由 wheel/sdist 门禁验证。返回绑定时也直接
把 Core 已拥有的编码贴图 `Vec<u8>` 移入 Python 对象，不再逐张复制出第二份
Rust 缓冲；Python `bytes` 仍在 getter 边界按 CPython 所有权规则安全创建。

**可选 Node 绑定已跟进相同的模型贴图格式合同（2026-08-15）**：
`readModelObj(materialLibraryName?, maximumBytes?, textureFormat?, textureLimits?)` 与
`readFbxWithTextures(maximumBytes?, textureFormat?, textureLimits?)` 把新参数放在末尾，旧的
位置调用保持兼容；格式名、大小写归一、默认 PNG 和拒绝未知值均与 Python
一致。两者现又在末尾接受可选 `textureLimits`，字段与 Python 的
`ModelTextureLimits` 对应，旧位置调用不变；转换结果时直接把 Core 持有的
编码 `Vec<u8>` 移交给 Node Buffer，不再为每张大贴图 clone 一份。Node 端另建
了真正包含 Renderer → Material → Texture2D 引用的多对象 fixture，验证默认
PNG、raw-RGBA OBJ、TGA FBX、三类预算及 FBX 内贴图文件名引用；napi-rs 生成
的声明再由严格 TypeScript 消费测试调用新签名。debug 与 release addon、
JavaScript/TypeScript 测试和 npm 包内容门禁均通过。

CI 在 Linux、Windows、macOS 上运行 Rust 测试，并分别验证 Python、Node、CLI 和托管差分任务。真实游戏文件仍通过私有 corpus manifest 接入，仓库不提交专有输入数据。

**证据强度提示**：上述大部分测试是 Rust 内部的合成往返，只能证明读写自洽。真正的跨实现证据只有托管差分 oracle 和 vgmstream 音频差分两处，覆盖面见下方 P0 第 1 项。

## 明确缺口

### P0：完成声明前必须补强

1. **托管差分 oracle 覆盖面仍不足（系统性根因）**
   - 这不是理论风险：2026-08-14 修掉的 FBX blend shape 增量、FBX 矩阵约定、Node 纹理行序、bundle 版本覆盖四个缺陷，全部被手写的测试期望值锁死，正是因为它们从未与 C# 对照过。
   - **已补齐**：serialized format v13-v22 全部版本门；UnityFS v6 内联 blocks-info、UnityFS v6 尾部 blocks-info、UnityFS v7 强制 16 字节对齐、LZ4/LZ4HC/Zstd 压缩块与压缩 blocks-info（含同时压缩 + 尾部布局）、legacy UnityRaw v6、gzip 流。容器差分首轮即发现两处命名分歧（bundle 条目标签、gzip/brotli 把可移植名变成字面量 `"gzip"`，后者会让压缩序列化文件永远无法被外部引用按名匹配），均已修复。
   - **v5-v12 已补齐（2026-08-14）**：这些格式必然带 TypeTree，13 之后那个可以关掉树的 flag 还不存在，所以 tree-less fixture 做不到；自己编一棵树等于让两个 reader 去比对 Unity 从没写过的形状。改用 `tools/generate_typetree_fixtures.py` 从 UnityPy 自带的 `lzma.tpk` 里取真实的 TextAsset 树，产出 JSON 入库（TPK 本身不 vendor，脚本也不进 CI，只在开发时跑一次；派生链在脚本头部写明）。差分矩阵现在覆盖 5-22（2026-08-15 补上 22：循环原先写的是 `5..22`，把最新那个格式排除在了唯一一个专门比格式的测试之外；补的时候还要给 fixture builder 加上 22 才有的两处变化——48 字节头，以及 large-file 支持把对象的 byte offset 从 32 位放宽到 64 位），首轮全对：9 以下头部在文件尾、7 起才有 Unity 版本串、8 起才有 target platform、11 起 destroyed 字段换成 script type index、树在 10 和 12 从递归编码换成 blob——这些门此前全靠 Rust writer 自己的假设。
   - **Cubism 物理已纳入托管差分（2026-08-15），并因此修掉两处真实的输出偏差**：这是差分里唯一一个布局不来自 Unity 内置类的资产——`CubismPhysicsController` 是 `MonoBehaviour`，形状由 Live2D SDK 自己的 C# 类型决定，两边都只能照文件自带的 TypeTree 走，然后各自用完全独立的代码投影成 physics3.json。fixture 的 TypeTree 按托管仓库 `CubismUnityClasses/CubismPhysics.cs` 里的字段顺序手写（不是本项目对形状的想象），对象字节则另写一遍、不由树驱动，这样树写错时托管侧会立刻炸而不是跟着一起错——第一版就是这么发现 `m_Enabled` 在 Unity 的树里是 `UInt8` 而不是 `bool` 的。查出的两处偏差：（1）`live2d_physics.rs` 把 Unity 的 float 存成 f64，导致 physics3.json 里 `0.8` 写成 `0.800000011920929`——`TypeValue::Float32` 的注释早就警告过加宽在数值上无损、在文本上有损，这里正好踩了；已全部改回 f32（Python 边界处显式加宽，Python 只有 double）。（2）数字格式：托管把每个 float 过一遍 .NET 的 `"0.###"`，本项目写的是 Rust 的最短往返形式，整数值会多出 `.0`、多于三位小数不会收敛。已实现同样的格式（先按 7 位有效数字收，再四舍五入到 3 位小数、逢半远离零，最后去掉尾零；且舍入作用在最短十进制形式上而不是二进制值上——`0.0025f` 实际是 `0.00249999994`，按二进制舍会得到 `0.002` 而 .NET 给 `0.003`），并用 .NET 10 实跑出来的 35 组数据做单元测试。fixture 里特意放了只有格式对得上才会相等的值，并单独断言住，防止字段哪天悄悄消失让两边"都没有所以相等"。顺带修正 oracle 的一处漏报：`Name` 只取 `NamedObject`，而 `MonoBehaviour` 挂在 `Behaviour` 下面却确实解析了 `m_Name`，等于托管侧少报了自己已经知道的东西。
   - **Cubism fade-motion（motion3.json）已纳入托管差分（2026-08-15），同样查出两处偏差**：走的是 `CubismFadeMotionData` 那条路——一个 MonoBehaviour 进、一个文档出，跟物理一样能单独成立，不需要围一整个模型组。fixture 的三条曲线分别落在 Parameter、PartOpacity、Model 三个 target 分支上（参数名/部件名两边喂同一份，真实流程里这份来自模型；不喂的话每条曲线都掉进未绑定回退，target 判定等于没测）。查出的偏差与物理同源：（1）f64 加宽，`FadeOutTime` 写成 `1.2345677614212036` 而托管是 `1.2345678`；（2）同一份文档里有**两种**数字格式——托管只给 `List<float>` 注册了 `0.###` 转换器，而 `Segments` 是唯一的该类型字段，其余标量走 Newtonsoft 默认 float 格式（整数值保留 `.0`，超出 1e9/低于 1e-4 转科学计数法）。两种格式现在都实现在新的 `live2d_number.rs` 里，各自用 .NET 10 / Newtonsoft 13 实跑出来的 32 组和 31 组数据做单元测试（其中一组期望值一开始是我自己写的、不是探针跑出来的，测试当场就把它否掉了）。顺带纠正一处旧单元测试：它把 `Segments` 断言成 `[0.0, ...]`，那是本项目自己的格式而不是托管的——手写期望值又一次只证明了实现跟自己一致。
   - **Cubism 数字格式已裁决（2026-08-15）：跟托管，不保留全精度**。托管的 `"0.###"` 会把值收到三位小数，看起来是有损的；之所以仍然照做，一是 Cubism 编辑器本身就按这个精度出数，真实数据几乎不会被截到；二是照做之后任意 rig 都能与托管逐字段精确比对，而保留全精度会让差分只能比那些"恰好短到两边打印一样"的值，等于把 oracle 的覆盖面换成一点点用不上的精度。这是有意的取舍，改回全精度只需换掉 `live2d_number.rs` 里的一个函数。
   - **Cubism expression（exp3.json）已纳入托管差分（2026-08-15）**：同样是单个 MonoBehaviour 进、单个文档出。这份文档序列化时**不挂**自定义转换器，因此全篇走 Newtonsoft 默认 float 格式，跟 physics（全篇 `0.###`）和 motion（两种混用）各不相同——三份都进差分之后，任何一份把格式搞反都会单独失败，这个区分才算钉住。查出的偏差还是 f64 加宽那一条，已修；反向验证过：把格式换回 serde_json 的 f64 输出，差分立刻失败。
   - **MOC3 头解析已纳入托管差分（2026-08-15）**：MOC 是 Live2D 里唯一不带 TypeTree 的资产——两边都按固定前缀跳过再走格式钉死的偏移（64 计数表、68 canvas、76 与 264 两张标识符表），它给出的参数名/部件名又是后面动作曲线绑定 target 的依据，所以这一条塌了后面全塌。版本字节、字节序标志、canvas 五个浮点、两张计数与标识符表全部一致。过程中修掉一个 fixture builder 的真实缺陷：`synthetic_plain_v22` 的类型记录对 class 114 少写了那 16 字节 script hash（Unity 只对 MonoBehaviour 写这一份），托管侧直接 EOF——这个 builder 此前从没被用在 114 上，所以一直没暴露。另外自己犯了一次前面刚批评过的错：Rust 侧一开始按"能不能解析出来"识别 MOC，而 MOC 布局没有任何 reader 会拒绝的 magic，于是一个 expression behaviour 被解析成了全零加"Unknown SDK version (50)"；改成跟托管一样按 MonoScript 类名判定。浮点在 manifest 里按位模式比较（跟关键帧那条一样），否则比的是两种语言的格式化器而不是值。
   - **整包差分已建立（2026-08-15），pose3/cdi3/model3 一并覆盖**：不再逐份文档比，而是造一个完整模型组（GameObject/Transform/MonoScript/多个带各自 TypeTree 的 behaviour），直接跑托管的 `Live2DExtractor.ExtractCubismModel`，把它写出来的每个文件跟本项目 materialize 出来的逐个对照。之所以要跑真的 extractor：pose3/cdi3 是遍历模型的 part/parameter 拼出来的，如果在 oracle 里把那段遍历重写一遍，比的就是本项目跟我自己对托管代码的理解，正是 sprite 那条已经纠正过的弱 oracle 模式。首轮 pose3（两个分组、组内顺序、Link 列表）与 cdi3（DisplayName 覆盖 Name）就完全一致；查出的偏差在 model3.json：托管的 `FileReferences` 五个成员无论有没有内容都会写出来（缺的引用是 `null`，空集合是 `[]`/`{}`），本项目是没有就整个省略——省略在 JSON 上合法，但拿到的不是同一份文档，按 `Motions` 取值的调用方会拿到"不存在"而不是空表。已改成照托管的声明顺序全写。顺带把三处手写期望值改对：CLI 和 core 各有一处 model3.json 的逐字符期望、还有一对断言明确断言 `Motions`/`Expressions` **不存在**——三处都只证明了实现跟自己一致，整包差分才把它们区分开。
     为避免这条差分变成"我自己驱动的东西"：只有文件里同时存在 `CubismMoc` 与 `CubismModel` 脚本时才跑 extractor（对应托管 CLI 只对模型发现配对成功的资产组走这条路），并按托管自己的做法用模型 GameObject 建 `CubismModel` 填进 `MocDict`，否则模型名会取成临时目录名。贴图刻意不放进 fixture：两个 PNG 编码器不可能逐字节一致，像素一致性已由纹理那条差分覆盖。
   - **Live2D 组件归属不再按组件反复向根扫描（2026-08-24）**：旧 planner 为 Renderer 和八类辅助组件各建一次模型表，然后让每个组件沿 `GameObject.parent` 一直走到最近的 `CubismModel`；在允许百万节点、百万组件的公开预算下，深链可以把这一阶段放大成 O(组件数 × 层级深度)。现在先以有界、可失败预留建立 GameObject→节点与 GameObject→模型索引，再从每个根迭代地向叶传播“最近祖先模型”，九类组件共用同一张结果表；祖先归属阶段为 O(层级节点+模型+组件)，后续为了稳定输出进行的分组排序仍单独是 O(组件 log 组件)。传播刻意先记录继承值、再把当前节点的模型传给子节点，因此模型自身 GameObject 上的 Renderer 不会错误归给自己，嵌套模型则会覆盖更远祖先，保持托管 `TryGetModelGameObject` 语义。新增 20,000 节点深链在中点嵌套模型的回归，逐节点断言这两个边界；现有 17 项整包/组件顺序/跨文件/schema/预算测试、严格 Clippy、托管差分以及 Rust/Python/Node 零跳过本地门禁均通过。
   - **容器、Live2D 与 Shader 差分状态**：UnityWebData（Unity/Tuanjie 两种签名）、ZIP、split 组、LZMA 块和 Live2D 整包均已补齐；LZMA 编码器来自仅开发期启用的 `lzma-rust2/encoder` feature，发布 crate 仍然只解压。Shader 5.3-5.4 的 subprogram blob 也已补齐；5.5+ 序列化程序无法接入托管 oracle，原因已查实：托管仓库 `AssetStudioUtility/ShaderConverter.cs` 的 `HeaderBytes`（第 15 行）用第 893 行才声明的 `header` 初始化，按 C# 静态初始化顺序必然得到 null 并使类型初始化抛 `ArgumentNullException`。5.5+ 所需转换方法又是该类型的私有静态方法，反射同样触发故障；在 oracle 里重写它只会变成另一份自实现，不能构成独立证据。上游修复只需把 `header` 改为 `const`，或移到 `HeaderBytes` 之前。
   - oracle harness 接受任意输入路径，上述补强全部不需要专有样本。
   - **第二 oracle 已就位，并于 2026-08-15 扩到 TypeTree 值**：`crates/unity-rs-python/tests/unitypy_oracle.py` 用 UnityPy（独立实现，不需要 .NET）对照对象顺序、PathID、classID、字节大小、名称和原始载荷哈希，14 个 fixture 首轮全对。新增第 15 个 fixture：一个自带 TypeTree 的 MonoBehaviour，专门覆盖 reader 容易出错的形状——单字节字段后的对齐、长度前缀字符串、基本类型数组、以及元素里同时含字符串和字节的结构体数组（对齐放错在这里会直接读出垃圾而不是读错一个数）。此前 TypeTree 解析只有托管一个 oracle 背书，现在有了第二份独立解析。两处刻意的处理：只比较文件自带树的对象（UnityPy 在没有内嵌树时会回落到它自带的数据库，那样比的是数据库而不是同一份字节的第二次解析），以及浮点两边都收窄到 f32 再比（UnityPy 的 Python float 是 double，会把 `0.8f` 变成 `0.800000011920929`）——收窄会让真正的 double 字段掩盖掉低位差异，因此 fixture 里放了一个 f32 表示不出来的 double 并单独直读断言。运行时统计真正比较了多少棵树，为零直接失败，避免两边都是 None 的假通过。
   - **顺带查出两处一直没跑到的失效测试（2026-08-15）**：本机第一次跑通 Python 侧全套（`maturin` 建 wheel 装进 venv）后发现 `python_api.py` 一直是失败的——sprite fixture 里 submesh 的 localAABB 仍然写 8 个 float，而 reader 早在 sprite AABB 缺陷修好时就改成了正确的 6 个，于是它后面每个字段都错位；这正是当初"fixture 照着同样错的布局手写"那条的残留，Rust 侧的 fixture 当时修了、Python 侧漏了。另一处是 physics3.json 的 `"Fps": 60.0` 断言，在数字格式改成 `0.###` 之后应当是 `60`。两处都是 CI 会拦下的，但 CI 自 LZMA 那次提交起就没跑过。刻意不比较解码后的像素与网格——UnityPy 走的是本项目已链接的同一个 `texture2ddecoder`，网格/shader 又是 AssetStudio 的转写，比了不构成独立证据。UnityPy 解析不出名字时（它的名称查找依赖自带的 TypeTree 数据库，不覆盖所有 class/版本）记为跳过并报数，而不是当成"双方都认为是空串"。

2. **真实游戏语料覆盖不足**
   - 当前合成 fixture 和差分 oracle 已覆盖大量版本门与格式分支，但不能替代跨游戏、跨平台、跨 Unity 版本的真实 corpus。
   - 需要持续扩充旧 Unity、Unity 5.x、2019/2020/2021/2022/2023、Unity 6、Tuanjie，以及大小端和平台资源样本。
   - 对象顺序、名称、container、PathID、原始 payload hash、像素/PCM/模型语义和错误分类都需要进入版本化快照。

3. **平台和版本长尾尚未闭合**
   - Unity 6000.2 `MeshLodInfo` 已由真实 Unity 6000.3 TypeTree 与 corpus 验证并实现；仍缺的是 Unity/Tuanjie 虚拟几何 cluster 的公开样本与解码；
   - Tuanjie 虚拟几何 cluster 尚未解码；
   - UnityArchive 没有样本验证的公开格式，当前仅识别并明确拒绝；
   - **UnityCN 加密已实现解密（2026-08-14）**：需调用方通过 `BundleOpenOptions`/`AssetLoadOptions` 或 Python `unity_cn_key=` 提供 16 字节密钥，仓库不内置任何密钥；无密钥时仍明确拒绝。检测改为 flag 驱动（不再用推测式解析探测），因此 blocks-info 本身被加密的常见情况会直接指出是 UnityCN，而不是报"LZ4 数据无效"。密钥校验与表派生所需的 AES-128 在本 crate 内实现并用 FIPS-197 向量验证；解密走 LZ4 token 流，literal 段不动，两条 0xFF 扩展链和每次偏移推进都做了边界检查。算法理解来自 UnityPy 与其致谢的 PGRStudio，代码为按行为重写。

4. **Tuanjie ACL 尚无内置纯 Rust 解码器**
   - ACL 容器、边界、hash、decoder map 和输出形状已验证；
   - Rust/Python 可注入安全 decoder；
   - 若希望完全开箱即用，仍需一个许可清晰、样本差分通过的纯 Rust ACL 2.x 解码实现。

### P1：主要功能长尾

1. **模型/FBX**
   - 当前场景可输出确定性的 ASCII 与 binary FBX 7.4；
   - **binary FBX 编码层已落地（2026-08-14）**：`fbx_binary.rs` 提供 FBX 7.4 的节点树、全部标准 scalar/array 属性类型与字节布局，写出与读回两条路径；`b` 布尔数组覆盖 raw/zlib 两种 encoding，读取时按规范把任意非零字节归一化为 `true`。`FbxBinaryWriteLimits` 对输出、节点、属性、非空深度和数组元素提供对称预算，默认 256 层先于递归拒绝过深的公共 `FbxNode`，而旧 API 继续包装默认值。写出走的是记录头里的绝对 end offset，因此每条记录先编码体再回填头；数组属性到阈值才 deflate，小数组压了反而更大。7.5+ 的 64 位 offset 明确拒绝而不是截断。读回的解析器是照格式写的、不共享写出侧代码，所以两者互为对照——但要说清楚它证明了什么：它证明编码自洽，不证明 FBX SDK 会接受。2026-08-15 补了一层独立校验：`tools/validate_fbx_binary.py` 是照格式规则另写的解析器（不看本项目的读取代码），会检查 23 字节 magic、版本字、每条记录头里的绝对结束偏移是否正好落在记录末尾、属性数与属性区长度是否与实际属性吻合、嵌套列表的 null 记录、以及 footer 的 id/对齐填充/重复版本/结尾 magic；额外的手工 fixture 不调用 Rust writer，直接构造包含 `b` 属性的 raw 布尔数组。`--cli` 模式会自己造模型、走 CLI 导出再校验，已接进 `tools/local_ci.py` 的 `outputs` 组。写这个校验器时它一上来就报 footer 不对，查下来是**校验器**漏了版本前面那 4 个零字节、写入端是对的——独立实现的价值正在于此：分歧会逼人去对格式，而不是对着代码自我确认。反向也验证过：改坏任一条记录的结束偏移或结尾 magic，校验器都会准确指出。ASCII 那条也补了同样性质的校验（`tools/validate_fbx_ascii.py`）：括号配平、7.4 必备的几个段是否齐全、`Definitions` 里每个 `ObjectType` 声明的 Count 是否与 `Objects` 里实际写出的对象数一致、每条 `C:` 连线引用的 id 是否真的存在（root 的 0 除外）、以及 `*N { a: ... }` 的值个数是否等于 N。这四条都是导入器会依赖、而写入端自己的测试不会注意到的东西；四种人为破坏都能被准确指出。本机没有 Blender/assimp/FBX SDK，因此导入器接受度这一条仍然只能由你那边验证。真正的差分做不了，因为托管侧是通过 FBX SDK 产出二进制的，字节结构本就不同。
   - **场景层已接上（2026-08-14）**：`fbx_binary_scene.rs` 把 `StaticScene` 的计划映射成节点树——Model、Geometry、Material 与 Connections，外加 header/GlobalSettings/Definitions。场景内容复用的就是 ASCII writer 的计划，所以几何、变换、材质颜色、连线都来自托管差分已经覆盖的代码；新增的只是记录布局。测试拿二进制解析回来的顶点数组跟 ASCII 文本里的同一个数组逐值比对，这样二进制的场景内容是靠 ASCII 那条已验证路径传递过来的，而不是只跟自己自洽。
   - 贴图的二进制布局也已接上：`Texture`/`Video` 记录与指向材质通道的 OP 连线，UV 变换按 FBX 的约定挂在 texture 上。蒙皮也已接上：Skin/Cluster deformer、Indexes/Weights 与两个 bind 矩阵，连线按 cluster→skin→geometry 与 cluster→bone model 建立。blend shape 也已接上：BlendShape/BlendShapeChannel deformer 与 Shape geometry，目标形状按 FBX 的约定写成相对基础控制点的偏移而不是绝对坐标。动画也已接上：AnimationStack/Layer/CurveNode/Curve 与相应的 OP 连线，key 时间按 FBX tick 写（1 秒 = 46186158000 tick，写成秒会让整个 clip 塌到第 0 帧却仍然能解析）。binary FBX 的场景层至此与 ASCII 覆盖同一组内容。**但直到 2026-08-15 之前，没有任何调用方能碰到它**——CLI、Node、Python 都只连着 ASCII writer，编码器写完了却是死代码。现已三个交付面全部接上：Core 补了 `Studio::read_static_fbx_binary`/`read_fbx_binary`（含 ACL decoder 注入变体），CLI 的 `fbx` 加了 `--binary`（`obj` 传这个标志会明确报错而不是默默忽略，因为 OBJ 只有一种编码），Node 加了 `readStaticFbxBinary`/`readFbxBinary`，Python 加了 `read_static_fbx_binary`/`read_fbx_binary`。测试都按格式本身验证（23 字节 magic、版本字 7400）而不是拿本项目产出的字节当基准；CLI 与 Python 还把同一个模型用文本 writer 也导一遍，确认两边描述的是同一个场景。Node 那套没有模型 fixture，因此只验证方法确实存在且行为与文本版一致（先断言是 function 再断言抛错，否则「没绑定」和「绑定了但报错」分不开）。顺带一提，Python 的 wheel 有一道 API 面守卫：运行时多出来的方法必须同时出现在类型 stub 里，这次正是它拦下来提醒补 `.pyi` 的。这些目前会明确报 Unsupported 而不是当普通几何写出去——写出去会得到一个看起来导出成功、实际丢了绑定的文件。
   - **新增 `obj` 命令（2026-08-14）**：整模型导出为 Wavefront OBJ + 同名 `.mtl` + 同级贴图。OBJ 没有层级，因此节点变换烘进世界空间、顶点索引跨 group 累加；面引用只写网格真正有的通道，与 `export` 写单个 Mesh 的 `.obj`（照抄托管 writer 无条件 `v/vt/vn`）刻意不同；
   - **贴图已写出（2026-08-14）**：`scene_textures.rs` 解析材质的贴图 PPtr、按对象去重解码一次、分配稳定文件名，writer 发射连线到 `DiffuseColor`/`NormalMap`/`SpecularColor`/`Bump` 的 `Texture`/`Video` 对，UV offset/scale 取自材质自己的 `TexEnv`，属性名映射沿用托管 reader 的 `_MainTex`/`_BumpMap`/`Specular`/`Normal` 规则。这确实改动了"单文件原子发布"契约：图片写在 FBX 同级目录，`--no-textures` 可退回纯几何，`--texture-format` 可换 PNG 以外的格式。贴图名来自资产因而不可信，一律削成单个路径分量，已存在的文件不覆盖；批量导出共用一个名字分配器，避免两张同名贴图互相顶掉。解析不到 `Texture2D` 或解码失败的引用记为 skip 并报数，不拖垮整个模型；
   - **`CompressedMesh` 已纳入托管差分（2026-08-14）**：加了两个 fixture——一个同时带顶点流和打包向量（验证叠加规则），一个是 Unity 实际写出的形态（顶点流为空）。首轮就查出实现是二选一分支：有打包数据就完全忽略顶点流，而托管是把打包结果按字段叠加到顶点流之上，每个块各自按 item count 判断。已改为叠加。另外空通道的表示也统一了：托管会分配零长数组，这边是 None，两者含义相同，manifest 两侧都归一为“没有这个通道”。
   - **`CompressedMesh` 打包几何已解码（2026-08-14）**：`packed_bits.rs` 提供共享的 `PackedFloatVector`/`PackedIntVector` 位流读取，顶点、八面体法线/切线加符号位、UV（读 packed channel descriptor）、31 量化蒙皮权重和索引缓冲全部还原；浮点刻意保持 f32，加宽会让 OBJ 文本与 oracle 分叉。Unity 6000.2 MeshLOD 和虚拟几何仍会明确报 Unsupported。

2. **纹理和音频**
   - Switch 更低 mip、stripped mip 和未进入受验证 GOB 表的格式仍缺；
   - **多 image / 非 `Tex2D` 的首图已对齐托管 converter（2026-08-15）**：托管实现虽然读取 `m_ImageCount` 与 `m_TextureDimension`，转换时完全忽略它们，只从 `image_data` 开头消费一个 `width × height` surface。普通线性载荷现同样只开放 mip0 首图；差分分别用 `imageCount=2`、`dimension=3` 和 cubemap `6/4`，并把后续 surface 写成不同像素，三条均与活的托管 converter 一致。后续 multi-surface mip 的排列、Crunch face framing 与 Switch block-linear face layout 没有同等证据，解码与公开 `mip_region` API 都会明确拒绝而不返回猜测的切片；PVRTC 仍要求 2 的幂尺寸与 16x8/8x8 下限；
   - **AnimationClip 关键帧值已纳入托管差分（2026-08-14）**：此前只比曲线条数、sample rate、wrap mode、ACL 头和 streaming 信息，关键帧本身（时间、值、两侧切线）只有本项目自己的期望背书。现在 rotation/euler/position/scale/float 五类曲线的路径与每个关键帧都按浮点位模式（不是十进制文本，避免舍入差异被掩盖）哈希对照。新增一个真的带关键帧的 fixture，并加了断言确保这五行都非空——否则曲线块解析失败时两边会一致地得到空哈希，看起来通过实则什么都没验证。列表缺失与列表为空统一按空处理，两者含义相同。
   - **tight-mesh sprite 已纳入托管差分（2026-08-14），并因此修掉一个真实缺陷**：oracle 之前的 sprite 载荷是在 C# 里另写了一遍矩形裁剪，等于拿本项目跟自己的假设比，而且根本到不了 tight 路径；现在直接调 AssetStudio 的 `SpriteHelper.GetImage`，图集 render-data、tight 裁剪、alpha mask、downscale 全走托管实现。首轮就查出 `sprite.rs` 读 submesh 的 localAABB 跳了 32 字节，实际是 6 个 float 24 字节（`mesh.rs` 三处都是对的）。凡是带 submesh 的 sprite——也就是所有 tight 打包的 sprite，而 tight 正是 Unity 的默认 mesh type——submesh 之后的字段（index buffer、vertex data、texture rect、packing settings）全部错位。单元测试没发现是因为 fixture 是照着同样错的布局手写的。修好之后 8x8 tight fixture 的 64 个像素与托管逐字节一致，说明 mask 光栅化本身也对得上。
   - **Switch GOB 反交织已纳入托管差分（2026-08-14，2026-08-15 补齐 crop 路径）**：原先 3 个 fixture 的尺寸都正好填满 GOB，等于绕开了裁剪；现在补了 3 个填不满的（64x40、20x12、BC7 的 6x6 块），padded 与可见尺寸必然不同（测试里直接断言这一点，防止哪天改表把它们悄悄变回对齐的），全部一致。载荷按 **padded** 尺寸给——真实的交织纹理存的就是补齐后的面，按可见矩形给等于造了个 Unity 不会写出来的纹理。顺带记一处有意的严格性差异：载荷被截断（不足 padded 大小）时本项目直接报错，托管则照读不误、解出一张部分是垃圾的图。3 个原始 fixture（RGBA32 两种 block height，加一个 BC7）端到端对照，全部一致。GOB 布局只取决于 texel 大小和 platform blob 里的 block height 指数，这三个就把两者都覆盖了。DXT5 和 ASTC 不放进来，理由和块格式矩阵一样：前者是已记录的 s3tc 偏离，后者随机字节会命中保留编码，都与交织无关。顺带确认了一件事：`texture2ddecoder` 的 ASTC 解码器在畸形输入上会 panic（减法下溢），但 Core 早已用 catch_unwind 包住外部解码器，所以对外仍然是报错而不是崩溃——不可信输入不崩溃这条不变量成立。
   - **Crunch 已纳入托管差分（2026-08-14）**：6 个真实 CRN 载荷（classic DXT1/DXT5 走 2017.2，UnityCrunch DXT1/DXT5/ETC1/ETC2A 走 2022.3）端到端对照，全部一致。单元测试此前只比解码器本身对着 C++ oracle 的哈希；这一条走的是调用方真正的路径：Texture2D 解析、头部嗅探、转码、mip0 解码，连选哪个 Crunch 方言的版本门也一并比了。fixture builder 现在按 revision 生成 Texture2D 布局（2017.3 的 fallback 块、2018.2 的 streaming 对、2019.3 的 mip limit、2020 的 stripped mip 与 64 位流偏移、2022.2 的 mip-limit group），因此老版本纹理布局本身也进了差分。
   - **块压缩纹理解码差分已建立（2026-08-14），并因此修掉一个真实缺陷**：oracle 之前只比原始 payload 字节（那是直接从盘上读的），所有块解码器都只有本项目自己的往返测试背书。现在比解码后的像素，覆盖 BC4/BC5/BC7、ETC_RGB4、ETC2_RGB/RGBA1/RGBA8、EAC_R/RG 及其 signed 变体共 11 种格式。首轮就查出 `texture2ddecoder` 0.1.2 的 EAC 入口把 48 位索引流按小端整数读，而格式是最高位在前——同一个 crate 自己的 ETC2 alpha 解码器读的是大端并且与托管解码器一致，等于 crate 跟自己不一致。受影响的是 EAC_R/EAC_R_SIGNED/EAC_RG/EAC_RG_SIGNED，即移动端常见的法线图和 mask 图，像素会整块错位。已在 `texture.rs` 内实现 EAC 解码取代 crate 的入口：只改字节序，算术仍按格式的 11 位空间做（multiplier 为 0 时代入的 1 是 11 位步长，不是 8 位；照 8 位算会让那一块的调制范围放大 8 倍——这一点也是差分查出来的）。
   - **ASTC 已全部纳入托管差分并修正上游舍入缺陷（2026-08-15）**：使用 ARM 官方 `astcenc`（经 `astc-encoder-py`）生成合法载荷，六种 block footprint × RGB/RGBA/HDR 共 18 个格式全部端到端对照，每个 fixture 为 2×2 个块。首轮确认 12 个 LDR 格式逐字节一致，6 个 HDR 格式因 `texture2ddecoder` 0.1.2 把参考实现的 `roundf(f * 255)` 移植为 `floor(f * 255)` 而有 8%–14% 的通道恒低 1。仓库现已 vendor ASTC 解码器并只修正该表达式，18 个格式均与托管解码器逐字节一致；源码差异、许可证和上游补丁记录在 `src/vendor/texture2ddecoder`、`THIRD_PARTY_NOTICES.md` 与 `docs/upstream-defects.md`，托管差分持续验证这份本地补丁。
   - **Live2D 六份文档改为逐字节比对，并修好一族布局偏差（2026-08-15）**：`live2d_number.rs` 的模块文档一直写着「匹配数字格式是为了让文档可以逐字节比较」，但差分实际比的是解析后的值——两份 parse 结果相同的文档并不是同一份文档，而这六份恰好都不是。托管侧六份文档全部走 Newtonsoft 的 `Formatting.Indented`（`MyJsonConverter`/`MyJsonConverter2` 只改数字拼写，不改布局），即每个对象成员、每个数组元素各占一行；本项目却在多处写成单行紧凑形式，且每份文件都多一个托管不写的结尾换行。把 manifest 里加上字节行（三份整包文档 + 三份单 behaviour 文档）之后，隐形的偏差立刻变成可读的 diff。已修：physics3 的 EffectiveForces、Input 的 Source、Output 的 Destination、Vertices 的 Position、Normalization 全是紧凑写法，且 Input/Output/Vertices 的数组元素缩进少了一级；motion3 的 Segments 数组写成一行；pose3 的 Link 数组写成一行；model3 的 motion 条目、expression 条目与参数组都是单行对象。现在六份文档与托管逐字节一致，并由差分长期把守。改成逐字节之后又做了一轮反向验证，结果发现「在比对范围内」不等于「被覆盖」：对两个数字格式做四种人为破坏，仍有三种能过——`0.###` 的有效位数从 7 改成 6 没被抓（fixture 里没有超过 7 位有效数字的值）、零写成 `0` 而不是 `0.0` 没被抓（走默认 float 格式的字段里没有零）、默认 float 转科学计数法的阈值挪一个数量级也没被抓（fixture 里的值离阈值差七个数量级）。补了三个值：physics 的一个 Radius 用 1234.5678（区分「先舍到 float 的 7 位再取三位小数」与「舍到 6 位」或「直接取三位小数」）、expression 加一个恰好为 0 的参数、再加一个 1.5e8（比阈值低一个数量级）。四种破坏现已全部被抓，其中两个值还在断言里按名字钉住——字节行报的是哈希，fixture 若悄悄不再携带这些值，差分照样通过且什么都不会说。另加 `ORACLE_DUMP_DIR`：字节行不一致时报的是两个哈希，据此改不了任何东西，把两侧 manifest 都 dump 出来才能逐份 diff——上述每一处都是这么定位的。
   - **Mesh OBJ 文本已接入差分，并修好一处静默偏差（2026-08-15）**：先前判断「差分接不进来」是找错了writer——托管的 `AssetStudioCore/Exporter.ExportMesh` 确实是 `internal sealed class` 里的私有方法且直接写文件，但 `AssetStudioCore/AssetStudioSession.WriteMeshObj` 是无头路径（托管库自己的 object payload 就走它，也正是本项目要替代的那条），可以反射调到。oracle 现在给 csproj 加了 `AssetStudioCore` 引用，用与托管 payload 完全一致的方式构造 `StreamWriter`（`NewLine = "\r\n"`，UTF8 无 BOM）驱动托管自己的 writer，manifest 里多出一行 `Obj`，比的是导出的文档本身而不只是它背后的几何。**这一步立刻暴露了一个存在已久的偏差**：托管每个坐标走 `string.Format(InvariantCulture, "{0}", v)`，即 .NET 的通用格式——指数落在 `[-4, 8]` 内写普通小数，否则转科学计数法；Rust 的 `Display` 永远不用科学计数法。两者只在那条带子内一致，而带子外的值一点也不罕见：法线里 4.3e-8 这种浮点噪声是常态，托管写 `4.3E-08`，本项目写 `0.000000043`。几何完全等价、导入器读到的东西一样，但字节不再可比，而可比正是差分的意义。修法是两趟渲染：最短往返形式给出有效位数，再按该位数重渲染一次拿到 .NET 的舍入——两者在末位打平时的方向不同（最短形式远离零，定精度形式取偶，.NET 取偶），-1298351.25 曾被写成 `-1298351.3` 而托管是 `-1298351.2`；两趟都不分配。提交的期望表由 .NET 10 探测得到，覆盖零与负零、两个阈值及其两侧、两个打平值、次正规范围与有限极值；此外拿 39,927 个值（20,000 个取自整个 f32 位空间、20,000 个取自网格实际会出现的量级）与 .NET 10 逐一比对，全部一致。fixture 的几何也一并改了：原来的 1.5/2/3 全落在那条带子内，把格式化改回旧实现差分照样通过——现在坐标横跨两个阈值、正好压在阈值上、含一个末位打平值并触到次正规，改回旧实现或去掉打平那一趟都会被抓住。另外用七种人为破坏确认这一行不是在比两份空文档：位置与法线的 X 不取负、绕序不反转、索引不加一、去掉子网格的 `g` 行、旧格式化、缺打平趟，全部被抓。没有顶点的网格产出空文档而不是报错，与托管一致（这道闸也要跑真实文件，而真实文件里有这种网格）。顺带牵出同一族的第二个缺陷：`type_tree_dump.rs` 早就实现了 .NET 的通用格式（连 float/double 两个精度常量 9/17 都是对的，已用 .NET 10 复核），但它同样只取 Rust 的最短往返形式，因此末位打平时与 .NET 反向——托管 dump 写 `1298351.2`，本项目写 `1298351.3`；已由差分实证。也就是说这套知识本来就在仓库里，只是 OBJ writer 没用它。现已收敛成一个共享实现 `managed_number.rs`（f32/f64 两个入口，栈上渲染不分配），OBJ 与 TypeTree dump 都改走它，各自保留自己的非有限值处理（OBJ 把 NaN 写成 0，dump 写托管拼法）。原有的 800+ 条 .NET 探测 fixture（`tests/fixtures/managed/float_format.txt`）一并接到新实现上——那份 fixture 之所以一直通过，是因为它的扫描恰好没扫出任何打平值，现补了 14 条打平条目，并加了一条反自欺断言：fixture 里若没有打平值，测试直接失败。差分侧的 dump fixture 也补了四个字段（打平的 float、小到转科学计数法的 float、落在「double 仍写定点而 float 已转科学计数法」那段区间的 double、以及三位指数的 double），因此去掉打平那一趟、或把 double 的精度常量写成 float 的，都会被差分抓住——后者正是加最后那个 double 之前抓不住的。至于此前记录的两处「有意偏离」，实际上是相对 GUI/CLI 那两个 writer 而言的；对无头 writer 而言本项目是逐字节一致的，文档与 README 已按此更正。原先核出的两处是：（1）托管的 `g` 行用 `StringBuilder.AppendLine`，走平台换行，而 `v/vt/vn/f` 全是显式 CRLF——也就是说它在 Linux/macOS 上产出的是混合换行，在 Windows 上才统一；本项目一律 CRLF，等于选了 Windows 那种，免得同一个网格的字节取决于导出时用的是哪台机器。（2）托管收尾做 `sb.Replace("NaN", "0")`，是对整份文本的替换，连名字里带 `NaN` 的网格也会被改掉；本项目只替换数值分量，名字保留。两处都写进了 `write_mesh_obj` 的文档与 README。
   - **BC6H 已纳入托管差分（2026-08-15），并修正与 ASTC HDR 同源的第二处上游缺陷**：手头没有 BC6H 编码器可借（`astcenc` 只管 ASTC），但差分需要的不是最优编码器、只是**定义明确的块**，因此直接构造单子集模式的块（5 位 mode + 三对 10 位端点 + 16 个 4 位索引），这个模式没有任何保留编码可踩。首轮查出 `texture2ddecoder` 0.1.2 的 `f32_to_u8` 把参考实现的 `roundf` 写成 `as u8`（截断）；本 fixture 256 字节里有 11 个通道因此恒低 1。vendored BC6H 解码器恢复舍入后与托管完全一致，这也反向证明构造块合法；`texture.rs` 的逐字节测试和托管差分现在都要求相等，把修复改回截断会立即失败。
   - **DXT5 与 DXT1 同属已记录的 s3tc 偏离**：托管的颜色调色板复刻 NV4x 硬件，本项目跟规范；DXT5 的 alpha 半边（BC4）能对上，颜色半边对不上，正好印证根因。
   - **DXT1 punch-through alpha 已裁决（2026-08-14）：跟 s3tc 规范**。`q0 <= q1` 模式下 index 3 解为透明黑 `(0,0,0,0)`，与独立解码器（Pillow）一致；AssetStudio 原生 `bcn.cpp` 给不透明黑 `(0,0,0,255)`，复刻的是 NV4x 时代硬件行为。这是对 oracle 的有意偏离——镂空贴图的遮罩区应当透明而非黑块——已在 `texture.rs` 注释、测试和兼容矩阵中记录。UnityPy 无法作为第三方仲裁：它与本项目共用同一个 `texture2ddecoder` 上游。（同批复核确认 DXT3/DXT5 调色板不是缺陷：Rust 符合 s3tc 规范，原生解码器复刻的是 NV4x 时代硬件行为，且 C# 侧根本没有 DXT3 解码器。）
   - 少数尚无可验证纯 Rust decoder 的平台音频 codec 仍保留原始数据；标准 1–8 声道 multistream Opus 已于 2026-08-24 完成；
   - 最初 8 个音频差分虽然写好却从未跑过（全部 `#[ignore]`，CI 也没有对应 job），现已加 `audio-oracle` job：按固定 release 拉 `vgmstream-cli` 再跑 `--ignored`；随着真实 MPEG/Opus/Vorbis 与 multistream fixture 补入，现为 12 条且全部通过；
   - **MPEG/Opus 的全零 fixture 已换成真实音频（2026-08-15）**：此前两边比的都是静音，解码器无论怎么处理比特两边都会一致地得到零，等于只验证了分帧。现在各自嵌入一段真实编码的正弦——MPEG 是 6 帧 MP3，Opus 是 libopus 编的 6 个包（FSB5 的 MPEG 帧按 4 字节对齐、Opus 包带 u16 长度前缀并以零长度收尾，这两条框架细节也因此第一次被真实数据验证）。MPEG 换成有内容之后仍然对得上，容差 1（实测得来，不是猜的）。
   - **FSB5 multistream MPEG 已完成并覆盖全部验证范围（2026-08-24）**：总声道数大于 2 时，FSB 不是把一个 MPEG frame 扩成任意多声道，而是把若干个独立的 1/2 声道码流按帧交织；每个 frame 的槽位从 4 字节对齐扩大到 16 字节。实现为每条码流保留独立的 Symphonia decoder 状态，逐帧同步后再按原声道顺序重组 PCM，最多 16 声道；输出和 scratch 都在写入前做 checked/bounded/fallible 分配。此前只有六声道的三组 stereo CBR MP3 证明这条路径，现改为 3–16 每个声道数各一份真实非静音 fixture：生成器直接写十六路互异的确定性整数三角波，相邻两路独立编码为 stereo，奇数布局的末路另编码为 mono，再按 FSB5 的 16 字节 frame span 交织。常规测试逐布局要求所有声道非静音且 hash 互异，同时覆盖奇数 mono 尾、6/8 紧凑声道码、其余显式声道 metadata 与 16 声道循环/预算上限；固定 `vgmstream r2117` 对十四份 fixture 的完整 PCM16 逐样本比较仍全部落在 Layer III 实测 1 单位上限内。另有回归拒绝截断 block、内部码流声道不一致、frame span 变化和超过验证上限，不会用补零伪装损坏流。
   - **FSB5 multistream Opus 已完成并逐布局验收（2026-08-24）**：FSB 省略标准 `OpusHead`，实现现按 mapping family 1 的标准表恢复 1–8 声道对应的 stream/coupled 数、self-delimited elementary stream framing 与 WAVE 声道顺序，mono/stereo 也统一走同一个纯 Rust multistream decoder。3、4、5、6、7、8 声道现在各有一对真实 Ogg/FSB fixture；生成器用 Python 标准库直接写非静音、互异的确定性整数三角波，在输入侧标记 WAVE 布局，并逐字节核对 libopus 写出的权威 `OpusHead`。把布局误放在 FFmpeg 输出侧的一版生成器会把三声道的第三路静默重混成零，新增逐声道 peak/hash 回归已经实际抓住并拒绝过它。FSB 对只复用对应 Ogg 的同一批音频包，因此 oracle 不是拿两份同一假设生成的 FSB 头互相印证。常规回归逐布局锁定 5,760 帧、全部声道非静音且互异，另继续覆盖输出预算、伪造普通 stereo packet、stream duration 不一致、截断与九声道拒绝；固定 `vgmstream r2117` 解每份原始 Ogg 后与 Rust 解 FSB 的完整输出逐样本比较，各布局按精确 fixture 固定实测舍入上限，最大为四声道的 9 个 PCM16 单位。Core 当前 554 项常规测试与 12 项音频 oracle 保持同一测试数，但 Opus oracle 已由单个六声道扩成全部六种 surround 布局。现剩余音频缺口是没有可验证纯 Rust decoder 的平台 codec，不再包括标准 multistream Opus。
   - **FSB5 multichannel Vorbis 的声道顺序已修正并逐布局验收（2026-08-24）**：reader 早已声明接受 1–8 声道，却只有 stereo fixture；`lewton` 返回 Vorbis I 的 speaker order，旧 writer 直接按该顺序交织进 WAVE。先加入的 6 声道 32 kHz fixture 使用 setup CRC `0x6aad13bc`（本来就在 161 项 FMOD 字典里，不为测试扩充生产 header 表），原始 Ogg 与 FSB 复用完全相同的 audio packets。首次把 Rust FSB 输出和固定 `vgmstream r2117` 的 Ogg 输出逐样本比较，长度正确但最大差 8,396，实证不是 decoder 精度而是 FC/FR/LFE/rear channel 排列错误。writer 按标准 1–8 声道 Vorbis→WAVE 表重排后，六声道最大差回到 1；随后又把 3、4、5、7、8 声道全部做成同样的真实 Ogg/FSB 对，每个布局都由独立 decoder 的 channel mask 定义顺序，完整 PCM16 最大差均不超过 1。生成器直接用 Python 标准库写确定性的整数 PCM，不依赖平台浮点正弦或 FFmpeg 的 channel remap；逐声道 hash 还实际抓住并拒绝过一版把四声道后两路复制成相同信号的 `amerge` 生成链。连续两次生成的整个 fixture 目录逐字节一致。Core 当前 554 项常规测试、12 项音频 oracle 全绿。
   - **Opus 偏离已定位到上游 `ruopus` 0.1.2 的 SILK 路径（2026-08-15）**：换成有内容 fixture 后先暴露出偏离，再把它从本项目的 FSB5 代码里摘出来复现——直接把同一批包喂给解码器、按流自己声明的 pre-skip 裁掉前导，不经过本项目任何一行代码，偏离照旧；而 `ffmpeg` 与 `vgmstream` 两个 libopus 实现彼此差 1 以内。分模式实测（峰值都在 4200 附近）：

     | 包模式 | 偏移 | 最大差 |
     |---|---|---|
     | CELT-only | 0 | 1 |
     | SILK/hybrid | -2 | 103 |
     | SILK 宽带 | -2 | 135 |
     | SILK 窄带 | -4 | 115 |

     CELT 路径是准确的，偏差只出在 SILK；偏移量还随 SILK 的内部采样率变化（窄带 8 kHz 偏 4 个、宽带 16 kHz 偏 2 个，折算下来是同一个固定的内部采样分数），这正是重采样器延迟补偿差一点点的形态。因此现在拆成两个差分：`fsb5_opus_celt_tone_matches_vgmstream` 要求 CELT 对齐精确、幅度差不超过 1（实测值）；`fsb5_opus_silk_tone_divergence_from_libopus_is_bounded` 把 SILK fixture 的实测值 276 钉住（**这不是容差，是对已知缺陷的记录**），超过就失败。2026-08-15 又用同一组真实包、同一个 312-sample pre-skip 和同一个 `vgmstream` oracle 评估了独立纯 Rust 候选 `opus-rs`：0.1.26 与当前 0.1.28 都把 SILK 从 `shift=-2/worst=276` 变成 `-5/164`，同时把本来正确的 CELT 从 `0/1` 退化成 `0/36`，因此不能替换；精确步骤和版本记录在 `docs/upstream-defects.md`。目前已知能通过现有 oracle 的替代仍只有 libopus FFI，它与纯 Rust 取向冲突，先记录不动。全零 fixture 把这一切都盖住了；
     **2026-08-24 的公开 runner 补充测量**：固定 `vgmstream` r2117 的 Linux x86-64
     release 对同一 SILK/hybrid fixture 给出 `shift=0, worst=0`，并在 Rust 1.88 Linux amd64
     隔离容器中独立复现。旧 profile 来自另一套本地 oracle/build；具体由版本、构建选项还是
     平台造成尚未隔离。门禁因此只接受“完全一致”或上述 `-2/276` profile，不使用一般 codec
     容差；完整说明见 `docs/upstream-defects.md`。
   - **音频 fixture 的生成过程已补上（2026-08-15）**：`tools/generate_audio_fixtures.py` 用记录在案的 ffmpeg/lame 命令重新生成三个编码 fixture，两次运行字节一致，连此前只写了文字描述的 MP3 也逐字节复现出来——原来的 Opus fixture 只有“从一次 ffmpeg 编码里提取”这句话，没有命令，等于是不可复现的二进制块，而这正是我一直在别处挑的毛病；
   - 新增 codec 必须先有真实样本和独立 oracle，不能只凭推测实现。

3. **MonoBehaviour schema 来源**
   - 内嵌 TypeTree、调用方提供的可信完整 schema、以及生成器写出的 JSON 文档均已支持，CLI/Core/Node/Python 四个面都可达；
   - 从 managed assembly/dummy DLL 生成 schema 仍是独立的离线可信工具（`tools/monoschema`），不会在解析进程中加载或执行 DLL；
   - 生成器现在默认给每个条目写入精确 `unity_version`，避免一份布局静默套到另一引擎版本；只有显式 `--unversioned` 才生成跨版本 fallback，并有真实临时 DLL 的端到端门禁。仍存在的已知分歧是重建树的字段命名（枚举、`UnityEngine.Rect` 等引擎结构），详见 `docs/mono-schema.md`。

4. **Node 专用 reader 完整度**
   - Node 公开面已覆盖主要 Python reader：读取面包括 `readAudio`、`readMonoScript`、`readMaterial`、`readBuildSettings`、`readPlayerSettings`、`readAvatar`、完整的 `readAnimationClipInfo`、`readLegacyAnimation`、`readAnimatorOverrideController`、`readAnimatorController`、`readAssetBundle`、`readResourceManager`、`readPreloadData`、`readSpriteMetadata`、`readSpriteAtlas`、`readAclTracks`（只读 ACL 头，够调用方判断自己的 decoder 能不能处理）、直接内嵌树的 `readMonoBehaviourJson` 与可信外部 schema 的 `readMonoBehaviourJsonWithSchemas`（schema 是纯数据，查找过程不执行任何资产控制的代码）、`readResourceRange`、`resourceIndexByPath`、`scene` 和 Cubism 单对象 reader；输出面包括 `readStaticFbx`、`readFbx`（含动画）、`readFbxWithTextures`（贴图随 FBX 一起返回，由调用方决定写哪）、`export`、静态 `extract`、`live2DPackages`、`readLive2DPackages`；加载面包括 `openWithVersion`、`fromBuffers` 与 `openWithOodle`。新增的直接 reader 共用一个 `ObjectReference { fileId, pathId }` 形状，JavaScript 有效 v22 fixture 逐字段、顺序、`bigint` 与预算验证，napi-rs 生成声明由严格 TypeScript 消费端调用。Material 属性值刻意只给名字不给值：它们按表分类型，硬摊平到 JS 只会丢信息。
   - Core 侧同时补上 `Studio::write_fbx_with_textures`：此前贴图输出只有 CLI 走得到，库调用方拿不到。它返回贴图集合而不是自己写盘——这个方法只持有一个输出流，没有目录可以写同级文件，由调用方决定落在哪里。
   - Live2D 包发现与落盘、FBX 静态几何/动画/贴图均已接；
   - Oodle decoder 注入已接（`openWithOodle`，只提供异步形式：解码回调要在事件循环上跑而 worker 在等它，同步调用会把该跑回调的那条线程堵死）；ACL decoder 注入覆盖 FBX、单对象 Cubism motion 与完整 Live2D 包，同样只提供异步形式，回落的曲线由 Core 校验形状、顺序与预算，不信调用方的承诺；完整包的 worker 可同时带入外部 schema，另有同步 `readLive2DPackagesWithSchemas` 给不含 ACL 的 stripped 包；
   - napi-rs 生成的声明由严格 TypeScript 消费端编译把守；ACL 与 Oodle 回调签名从 Rust 参数注解生成，避免原生测试通过但 `.d.ts` 留下未声明内部类型；
   - Node 是可选交付面，因此优先级低于 Core 和 Python 的真实语料兼容。

5. **Live2D 散件发现**
   - MOC3 标识表已接入参数组（与托管一致：MOC 的表覆盖组件推导出的名字），仅有 MOC、缺少活动组件的包不再得到空参数组。与托管的一处有意偏离：托管是无条件覆盖，因此 MOC 版本不带标识表时连组件名也会被清空；这里只在 MOC 确实带表时覆盖；
   - **散件发现回退已补（2026-08-14）**：模型组件图走不到时，回落到同一个序列化文件里的独立 `CubismExpressionData`/`CubismFadeMotionData`/`CubismPhysicsController`。语义跟托管一致——只在图路线什么都没拿到时才回落，因为表达式顺序由 `CubismExpressionList` 定义，扫文件复现不出来。作用域取序列化文件（托管取 container group），这是本 reader 最接近的等价物，也能防止一个 bundle 里的散件挂到另一个 bundle 的模型上。动作的回落顺序是：fade controller 的列表 → 散件 fade motion → AnimationClip。

### 上游缺陷

纹理解码器已证明与独立参考实现不符，并通过有许可证记录的 vendored 补丁在本仓库内修正。
`ruopus` SILK 则存在两个已实测 oracle profile：此前环境有固定时序/幅度偏离，固定 r2117
Linux amd64 完全一致；差异来源尚未隔离，因此暂不把任一 profile 外推为所有平台的结论。
`docs/upstream-defects.md` 记录了测量数据、不依赖本项目的复现步骤和门禁策略。

- `texture2ddecoder` 0.1.2 的 `f32_to_u8` 有两份移植（ASTC 一份、BC6H 一份），都把参考实现的 `roundf` 丢了；影响 6 个 ASTC HDR 格式加 BC6H，每个受影响通道恒低一格。上游 master 至今未修，仓库通过 vendored ASTC/BC6H 解码器修正并由托管差分把守。
- `ruopus` 0.1.2 的 SILK 路径在此前本地 oracle/build 中偏早且不精确（宽带早 2 个采样、窄带早 4 个，对齐后约差峰值的 3%）；固定 r2117 Linux amd64 对 CI fixture 完全一致。CELT 路径在两类测量中都准确。

纹理那处已于 2026-08-15 决定采用 vendor 修复：`crates/unity-rs-core/src/vendor/texture2ddecoder/` 收入 ASTC 与 BC6H 解码器，只改两个舍入表达式（就地标 `VENDOR FIX`），并删除一段本项目不用、依赖 `paste` 的宏（标 `VENDOR DELTA`），其余保持可与上游直接 diff。18 个 ASTC 格式加 BC6H 现在与托管解码器**完全一致**，测试也由“钉住已知偏差”改为要求相等。Opus 未采用同一路线：独立纯 Rust 候选 `opus-rs` 0.1.26/0.1.28 已实测仍有更大的时序偏差并使 CELT 精度回退，剩余已知替代只有 libopus 绑定，会为仅 SILK 路径的偏差引入新的原生运行时依赖并终结该解码链的纯 Rust 性质，而当前 CELT 路径本身已经准确；因此现阶段保留明确记录与有界偏差测试，等待可审计的纯 Rust 修复或上游版本。

公开 runner 的完全一致 profile 进一步降低了立即替换 decoder 的收益，但并未解释另一环境的
稳定偏差；在原因被隔离前，不能据此删除第二个已测 profile，也不能声称 SILK 已在所有平台修复。

### 设计上保留的外部适配器

以下能力不应通过在 Core 中静态链接不明来源或专有二进制来“补齐”：

- Oodle：由用户提供有授权的 decoder，Core 只接受安全的精确输入/输出接口；
- 外部 MonoBehaviour schema：由可信离线工具生成，运行时只消费数据结构；
- 在内置 ACL 解码器完成前，Tuanjie ACL 可由调用方提供 decoder。

这些是明确的安全和授权边界，不等同于旧 C ABI。

## 下一步执行清单

下表是后续工作的执行入口，按顺序推进。只有“完成证据”真实存在时才勾掉，
不能用缩小目标、删除失败样本或把未验证格式改名为已支持来结项。2026-08-15
整理出的主体提交已经推送；2026-08-25 最近一次绿色矩阵验证代码 head 为 `c661a39`。收口改动及公开 runner 修复已进入
[PR #1](https://github.com/seiunx-dev/unity-rs/pull/1)。仓库现为 Public；常规 PR 矩阵
[32859413104](https://github.com/seiunx-dev/unity-rs/actions/runs/32859413104) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败，
包含六平台 CLI/Node 制品的手工发布矩阵
[32660298990](https://github.com/seiunx-dev/unity-rs/actions/runs/32660298990) 28/28 全绿。

| 顺序 | 接下来要做的事 | 完成证据 |
| --- | --- | --- |
| 0 | **收口当前工作树**：逐模块审查现有 Core、Python、Node、CI、许可证和文档改动，确认所有新增文件都属于交付范围，再整理为可审查提交 | **已完成并同步远端（2026-08-24）**：`37e6ee0`、`ca0ea56`、`2c24e2f` 分别收口 Core/绑定、CI/交付范围和文档，均在独立 worktree 中验证可单独构建或通过对应严格门禁；分支 `codex/headless-rewrite-closure` 已推送并创建 PR #1。完整 `outputs quality rust python node typing security cross`、其余 `cli-package oracle audio python314 unitypy` 组，以及 Linux amd64/arm64 原生 Core/CLI、release CLI、Python wheel、Node addon/npm 均通过且零跳过；`git diff --check` 通过。CodeRabbit 指出的两处 CI 证据绕过已在 2026-08-24 修复：命令只从实际 `run` step 计数，CLI staging 顺序不再受注释文本影响；其日期提醒按当前上海日期无需改动 |
| 1 | **跑通正式六平台发布矩阵**：在 GitHub Actions 上执行 Linux、Windows、macOS × x86-64/ARM64 的 CLI、Python wheel 和可选 Node 包任务 | **已完成（2026-08-24）**：PR run 32659993206 的 16 个主 job 全绿；workflow_dispatch run 32660298990 为 28/28，全量包含六个 CLI artifact、六个 Node artifact 和六个 Python wheel。CLI staged 产物运行 `--help`，wheel 安装后通过公开 API/mypy，Node tarball 从临时消费者安装并核对 JS/TypeScript 运行时表面；法律文件随所有产物校验 |
| 2 | **扩充代表性真实 corpus**：在现有 Unity 2022.3 与 6000.3 之外，优先加入 Tuanjie 2022.3.x、Nintendo Switch、旧 Unity 4/5/2017 和带完整托管快照的样本 | 私有 manifest 在 release 模式稳定通过；每类至少有对象顺序、PathID/class、名称/container、原始载荷 hash、主要解码结果或明确错误族的版本化快照；专有样本不提交到仓库 |
| 3 | **按 corpus 命中补格式长尾**：只处理真实样本实际触发的 UnityArchive、Unity/Tuanjie 虚拟几何 cluster、Switch 低 mip/stripped mip 和平台纹理/音频 codec | 每项都有最小 fixture、边界/畸形输入测试和独立 oracle；没有可靠布局或 oracle 的格式在兼容矩阵中标记为 **Not tested**，实际命中时继续稳定返回 `Unsupported`，不猜字段、不静默产出 |
| 3A | **继续不依赖外部样本的 hostile-input 资源审计**：优先检查“只需一个字段却物化整棵对象”、跨对象重复遍历、累计输出/临时分配、不可失败集合增长和目录工作量放大；本轮已完成 `CubismModel._moc` 的完整校验式根字段投影、`SerializeReference` registry 的类型查找/校验二次方放大治理、模型动画 path/suffix 的 `tracks × nodes`、Avatar fallback 的 `tracks × avatar_paths`、Live2D 散件回退的 `models × roles`、clip fallback 的 `models × animators`、SceneHierarchy 两张百万级索引、AnimationGraph controller/clip/queued 三张百万级构建索引、ModelIr GameObject/Mesh/Material/Avatar 四类构建与最终索引、Loader collection-wide 对象名称/container/MonoScript class 元数据索引、递归 PendingInput/final collection 表、ASCII FBX 材质/贴图二次规划、Cubism clip motion 目标去重/后缀哈希、FadeMotion 曲线目标分类共享索引、ModelIr 目标资产对全局对象表重复线扫、TypeTree 子树边界的 `schema nodes × runtime values`、SpriteAtlas 回填的 `sprites × atlases × packedSprites`、legacy streamed AudioClip 的 `clips × serialized files`、FBX blend-shape 动画的 `tracks × morph channels`、Live2D 隐藏 lowercase 名称/显示信息索引、模型贴图共享名称索引的累计驻留预算，以及外部 MonoBehaviour schema 的 `objects × entries` 版本查找放大 | 每个确认问题必须有能在旧实现触发预算失败、超线性工作或不可恢复分配失败的合成回归；修复后仍完整消费/校验输入，并通过零跳过的 Rust/Python/Node/oracle 本地门禁和公开常规矩阵。`45e1194` 的低物化预算、截断尾部及 registry/reference-type 投影回归是第一项完成证据；`3f24ff0` 的 16,384 类型规模、重复首声明和两类预分配预算回归是第二项；`df42c67` 的 8,192 同名叶子、任意中段后缀、旧线性 oracle 等价和精确索引预算是第三项；`dac020e` 的 16,384 次 Avatar hash 重复查询、重复首声明、只索引所选 Avatar 及共享 count/byte 预算是第四项；`ee6f668` 的 16,384 条松散角色逆序/重复查询、发现顺序/首项语义和统一字节预算低一字节拒绝是第五项；`b8d0766` 的 16,384 条逆序 Animator、重复首项、重复二分查询和精确索引预算是第六项；`b77192a` 的 16,384 条逆序 GameObject 二分查询、重复身份拒绝、最终索引低一字节拒绝和 Transform-owner 预算前拒绝是第七项；`21ede89` 的 controller/clip 各 16,384 条逆序二分查询、重复身份拒绝、合计索引低一字节拒绝及正式 build 零预算路径是第八项；`01594a2` 的 16,384 条逆序 ModelIr 节点二分查询、重复身份拒绝、合计索引低一字节拒绝及真实共享资产唯一计费是第九项；`e53210a` 的 16,384 条逆序 Loader 元数据二分查询、entry/共享字节预算、公开命名对象拒绝路径及 metadata/MonoScript class last-wins 是第十项；`e5ee1a4` 的 16,384-entry WebData 精确/低一项 discovered-file 上限、队列拒绝前后 capacity 与最终资源表完整性是第十一项；`122a946` 的 16,384 条重复材质贴图绑定、唯一 Texture/Video plan 和首项 UV transform 语义是第十二项；`ca3171d` 的 16,384 个逆序唯一 Cubism 目标、重复首项、Parameter/Part 同名区分、嵌套 suffix 语义及六类目标索引预算是第十三项；`42fe0b2` 的 16,384 个逆序目标、16,384 次索引查询、4,096 条公开 writer 最坏位置曲线、Parameter/Part 同名优先级及四类目标索引预算是第十四项；`a646d15` 的 16,384 个逆序 Mesh 对象、逐一 pathID 查询、比较次数低于 `N × 20` 及现有 stale-index/重复首项语义是第十五项；`d9cff60` 的两棵 32,768 节点深/宽树、逐节点边界查询、构建 probe 不超过 `2 × N`、reference-type 按需缓存和分配前预算拒绝是第十六项；`2c9fb42` 的 16,384 条 assignment 逆序全查询、probe 低于 `N × 40`、master/variant 等价与 4/3-entry 分配前预算边界是第十七项；`9daca17` 的 16,384-file table、indexed 零 probe、兼容入口 16,384 probe、相同 payload 与越界拒绝是第十八项；`90f572d` 的 16,384 个逆序 morph channel、全量二分 probe 低于 `N × 20`、重复首 channel、精确/少一字节索引预算及公共 writer 拒绝路径是第十九项完成证据 |
| 3A-20 | **已完成 AnimationGraph 的 `Animators × bound clips` 派生复制治理**：共享 controller 的 clip 列表现在先累计计入 graph edge 预算，再逐 Animator 可失败分配，避免通过原始引用上限后再发生未计费的乘法驻留增长 | `5482d07`：16,384×16,384 预检在首个副本分配前拒绝，4×3 精确预算保序，公共 fixture 以 10/11 edge 边界证明生产 builder 已接线；零跳过本地总门禁及公开常规矩阵 32755098612 全绿。这是 hostile-input 审计的第二十项完成证据 |
| 3A-21 | **已完成模型动画同名 clip 的唯一名称放大治理**：唯一名称表现在把“下一个尚未检查的后缀”保存在既有名称键旁；每个 base 已经跨过的后缀不会在后续 clip 上从零重扫，也不为索引保留第三份输入字符串 | `98a722b`：`Walk`/`Walk_1`/空名交叉碰撞仍按首个空闲后缀命名，最终名称和索引副本继续精确计入累计字符串预算且少一字节拒绝；16,384 个同名 clip 的候选探测不超过 `2 × N`。完整 Core 611 项、畸形输入 6/6、Rust/Python/Node/oracle 零跳过本地门禁及公开常规矩阵 32758905875 全绿。这是 hostile-input 审计的第二十一项完成证据 |
| 3A-22 | **已完成递归解包叶子/父目录碰撞后缀的重复扫描治理**：叶子保存下一个未检查的 `~N`，冲突父目录保存已解析目录后缀；每次仍验证单调 claims 与当前文件系统状态，游标 key 通过可失败增长并纳入原累计路径预算 | `68fa9e5`：16,384 个同名叶子探测 `N+1` 次；4,096 个被文件占用的父目录后缀和 16,384 个子项合计探测 `4,096+16,384` 次；5/4 字节预算边界证明 claim、cursor、budget 事务性。完整 Core 614 项、畸形输入 6/6、Rust/Python/Node/typing/oracle 零跳过本地门禁及公开矩阵 32764678899 全绿。这是 hostile-input 审计的第二十二项完成证据 |
| 3A-23 | **已完成 Live2D 包目录/纹理/expression/motion 重名后缀的重复扫描治理**：规范化名称只保留一份并映射到稳定 ID；发生碰撞后按 `(base ID, SceneObjectKey)` 保存下一个未检查 ordinal，仍逐次复验全局 case-insensitive claim | `f88cb53`：原 `Face`/`face` 输出不变，显式 `_2` 交叉占位后稳定跳到 `_3`；不可能容量请求返回错误且不改变 claim/cursor；16,384 个相同 base/身份仅探测 `2 × N - 1` 次且末项为 `_16383`。Live2D 22/22、严格 Core Clippy、Rust/Python/Node/typing/oracle 零跳过本地门禁及公开矩阵 32768660736 全绿。这是 hostile-input 审计的第二十三项完成证据 |
| 3A-24 | **已完成 SplitObjects/Animator FBX 批量名称的错误 1,024 上限与重复后缀扫描治理**：case-folded 名称表同时保存 claim 和每个 base 的下一未检查 `~N`；临时文件尝试上限不再误用于合法模型数量，交叉占位仍逐项复验 | `a9b0665`：`Face`/`face~1` 与显式 `FACE~2` 后的 `face~3` 保持稳定；不可能容量请求不改变状态；16,384 个同名候选越过旧上限并仅探测 `2 × N - 1` 次，末项为 `Shared~16383`。CLI 定向测试、全目标严格 Clippy、Rust/Python/Node/typing/oracle 零跳过本地门禁及公开矩阵 32772105204 全绿。这是 hostile-input 审计的第二十四项完成证据 |
| 3A-25 | **已完成模型材质纹理引用的驻留放大治理**：`maximum_textures` 继续限制唯一解码纹理；新增独立 `maximum_texture_references`，在解析/解码前对每个非空引用计费，重复引用同一纹理和最终写入 skipped 诊断的失败引用都不能再绕过集合预算；Rust Core、Python `ModelTextureLimits` 与 Node `ModelTextureLimits` 使用同一语义 | `37ec43a`：有效与悬空两类引用各重复 16,384 次，在唯一纹理上限仍为 1 时均于 16,383 引用边界稳定拒绝；Python 真实模型调用验证零预算错误，Node 转换单测与 TypeScript 消费验证字段映射。完整 `quality rust python node typing oracle` 本地门禁零跳过，公开常规矩阵 32775923627 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第二十五项完成证据 |
| 3A-26 | **已完成公开 Rust 模型贴图集合的不可恢复分配入口治理**：`SceneTextureSet::push_texture` 由直接 `Vec::push` 改为 `Result<usize>`，手工构造与内部资产解析复用同一可失败容量预留；现有 `bind`、OBJ、ASCII/binary FBX 和 CLI 事务测试全部迁移到显式错误处理，Python/Node 没有暴露这个底层构造器，因而无需制造多余兼容包装 | `95ea542`：`usize::MAX` 容量请求稳定返回分配错误，集合长度与容量均保持不变，正常首项仍返回索引 0；Core 定向 14 项、CLI 全目标、workspace 严格 Clippy，以及完整 `quality rust python node typing oracle` 本地门禁零跳过。公开常规矩阵 32778657413 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第二十六项完成证据 |
| 3A-27 | **已完成 Live2D 隐藏 lowercase 名称索引的累计驻留预算治理**：包目录、纹理、expression、motion 的 collision claim key 与 display-info 去重 key 现在和来源/最终名称共用 `maximum_total_name_bytes`；Unicode lowercase 扩张在分配前按精确字节数检查，失败不改变 claim、suffix cursor 或预算 | `f8f630e`：已保留 4 字节后再申请 5 字节、总预算 8 字节时稳定拒绝并保持原状态；`İ` 的 2→3 字节 lowercase 扩张在分配前拒绝。Live2D 23/23、严格 Core Clippy及完整 `quality rust python node typing oracle` 本地门禁零跳过；公开常规矩阵 32781978836 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第二十七项完成证据 |
| 3A-28 | **已完成模型贴图共享名称索引的累计驻留与跨平台大小写碰撞治理**：`SceneTextureNames` 的实际对象文件名与 lowercase collision key 受独立累计字节预算约束，跨模型复用不会重置；同名不同大小写在发布前分配稳定后缀，而不是依赖目标文件系统是否区分大小写 | `31d1288`：`Body.rgba` 17/18 字节公开读取边界，`Body.png`/`body.png` 39/40 字节事务边界，`İ.png` 6+7 字节扩张；Python/Node 安装后测试按真实 fixture 名称验证少一字节拒绝，类型桩/声明显式消费 `maximum_name_index_bytes`/`maximumNameIndexBytes`。Core 16/16、严格 workspace Clippy及完整 `quality rust python node typing oracle` 本地门禁零跳过；公开常规矩阵 32785686979 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第二十八项完成证据 |
| 3A-29 | **已完成模型贴图 binding/skip 返回 metadata 的累计驻留治理**：`SceneTextureLimits.maximum_metadata_bytes`、Python `ModelTextureLimits.maximum_metadata_bytes` 与 Node `maximumMetadataBytes` 共同限制成功绑定 property 和跳过项 property/reason 的精确 UTF-8 字节；成功路径在解码和共享名称 claim 前预检，失败原因通过有界 formatter 构造，确定性超限不会消耗共享名称状态 | `7726bdb`：`_MainTex` 在 7/8 字节边界拒绝/成功，且 7 字节拒绝发生于损坏 Texture2D 解码之前并保持共享名称表为空；悬空 PPtr 在 property 恰好占满 8 字节后因 reason 无空间稳定拒绝。Core 17/17、workspace 严格 Clippy、Python wheel/sdist 真实安装、Node debug/release addon/npm、严格 typing 与完整 `quality rust python node typing oracle` 本地门禁均零跳过；公开常规矩阵 32788921665 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第二十九项完成证据 |
| 3A-30 | **已完成 SplitObjects/Animator FBX 批量隐藏 lowercase 名称索引的累计驻留治理**：候选数上限和 fallible HashMap 增长之外，新增精确 UTF-8 key 字节预算；现代 CLI 通过 `--maximum-name-index-bytes` 暴露 0–512 MiB 配置，默认 64 MiB，legacy WorkMode 使用同一默认值。按预算可容纳的最小 claim 数限制批次开始时的预留，低预算不会先按百万候选分配整张表 | `117f5a8`：首个 `Face` 的 3/4 字节边界；已有 `face` 后 `face~1` 的总计 9/10 字节事务边界，失败不改变 claim、retained bytes 或 suffix cursor；`İ` 的 2→3 字节 lowercase 扩张；真实 `split-objects` 进程在 3 字节上限下于发布 `root.fbx` 前拒绝且无临时文件。CLI 单元/进程测试、workspace 严格 Clippy 与完整 `quality rust python node typing oracle` 本地门禁均零跳过；公开常规矩阵 32790889498 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十项完成证据 |
| 3A-31 | **已完成通用 export 返回报告与隐藏 lowercase 路径索引的累计驻留治理**：`ExportOptions.maximum_metadata_bytes` 默认 256 MiB，精确计入成功记录的 source/编码后 output path、失败/unsupported 的 source/error，以及逐文件 collision claim key；预算错误在解码前可判定时提前拒绝，写入前的记录预算拒绝不会创建临时文件，失败原因用有界 formatter 构造且终止性预算错误不会继续被包装成更多失败记录。CLI `--maximum-metadata-bytes`、Python `ExportLimits.maximum_metadata_bytes` 与 Node `maximumMetadataBytes` 使用同一语义 | `bd297b6`：成功 TextAsset 报告按 source + output path + lowercase key 的精确/少一字节边界；unsupported MonoBehaviour 报告按 source + error 的精确/少一字节边界；`İ` 的 2→3 字节 lowercase 扩张在低预算下不改变 claim 或预算；Python wheel/sdist 与 Node addon 的真实导出调用以零预算拒绝且不落文件，CLI 接受 0 并拒绝负数/非数字。workspace 严格 Clippy 与完整 `quality rust python node typing oracle` 本地门禁零跳过；公开常规矩阵 [32794197624](https://github.com/seiunx-dev/unity-rs/actions/runs/32794197624) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十一项完成证据 |
| 3A-32 | **已完成独立 `live2d` MOC 导出隐藏 lowercase 名称索引的累计驻留治理**：输出名 claim 现在精确计入 UTF-8 key 字节，默认 64 MiB、可由 `--maximum-name-index-bytes` 在 0–512 MiB 内配置；重复 key 不重复计费，新 key 的预算或分配失败不会改变集合和累计值；名称预算在创建输出目录前检查。完整 `live2d-package` 继续使用 Core 已有、更广的包名称和 diagnostics 预算，不混用这个 CLI 专用索引 | `3ddcfc1`：`Face.moc3` 的 9/8 字节精确边界、已有 claim 后候选失败不改变状态、`İ.moc3` lowercase 扩张后的 8/7 字节边界；真实 CLI 进程用零预算拒绝第一份 MOC，退出码为运行错误且不创建输出目录。6 个 Live2D 进程测试、3 个相关 CLI 单测、严格 CLI Clippy，以及完整 `quality rust python node typing oracle` 本地门禁均零跳过；公开常规矩阵 [32796030129](https://github.com/seiunx-dev/unity-rs/actions/runs/32796030129) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十二项完成证据 |
| 3A-33 | **已完成递归 extraction 返回报告的累计驻留治理**：`ExtractionLimits.maximum_metadata_bytes` 默认 256 MiB，与遍历用 `maximum_total_path_bytes` 分离，精确累计 success/skip 的 source 与编码后 output path，以及 failure 的 source 与完整错误文本；所有检查在报告提交前事务性完成，成功记录预算不足不会发布文件，失败记录预算耗尽也不会继续无界增长。Python 运行时、类型桩和严格消费端暴露同一配置；Node 紧凑 `extract` 继续继承 Core 默认值 | `59bfb36`：成功记录按 source+output path 的精确/少一字节边界，少一字节时文件不发布而形成受限失败；两条失败记录恰好耗尽累计预算，第三条稳定拒绝且预算与报告长度不变；Python 真实安装后以零预算拒绝并不落文件。workspace 严格 Clippy，以及完整 `quality rust python node typing oracle` 本地门禁均零跳过；公开常规矩阵 [32798794256](https://github.com/seiunx-dev/unity-rs/actions/runs/32798794256) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十三项完成证据 |
| 3A-34 | **已完成逐输入容错诊断的累计驻留治理**：`AssetLoadLimits.maximum_diagnostic_bytes` 默认 256 MiB，与发现路径预算分离，精确累计 `LoadFailurePolicy::SkipInput` 最终保留的 path/message UTF-8 字节；每条错误通过有界 formatter 直接生成最多 4096 字节的合法 UTF-8 前缀，不再先物化任意长 `Display` 输出。Rust `Studio` 提供借用 slice，Python/Node 均暴露独立预算及 bounded count/page API | `62287c9`：单条 path+message 精确/少一字节边界，两条记录累计精确/少一字节边界，以及多字节 UTF-8 长错误的量测/格式化一致性；Python wheel/sdist 与 Node debug/release/npm 真实混合目录调用验证正常容错、分页和零预算稳定拒绝。workspace 严格 Clippy 与完整 `quality rust python node typing oracle` 本地门禁均零跳过；公开常规矩阵 [32802131674](https://github.com/seiunx-dev/unity-rs/actions/runs/32802131674) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十四项完成证据 |
| 3A-35 | **已完成 Live2D 包 diagnostics 的预算前临时分配治理**：26 条诊断路径统一接受借用 `Display`，其中 18 条动态消息由 `format!` 改为 `format_args!`；先 allocation-free 量测完整 UTF-8 长度并检查 `maximum_total_diagnostic_bytes`，再精确可失败分配和二次写入，最后与 diagnostics 容量一起事务性提交累计预算。原消息文本与公开 Rust/Python/Node 包结构不变 | `a789d45`：已有 2 字节诊断后，8 字节多字节 UTF-8 消息在累计 10 字节精确成功并只分配最终字符串；累计上限 9 字节时只执行量测遍历，第二次格式化不发生，报告长度和已消费预算保持不变。Core 定向回归、严格全目标 Clippy，以及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32804544428](https://github.com/seiunx-dev/unity-rs/actions/runs/32804544428) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十五项完成证据 |
| 3A-36 | **已完成外部 MonoBehaviour schema JSON 的整树中间物化治理**：`from_json_with_limits` 不再先构造一份无结构预算的 `serde_json::Value` 树再复制最终 registry；自定义 serde seed 直接、稳定地读取 document→entries→nodes，entry/node 上限在进入下一个元素前检查，所有 Vec 增长可失败，未知字段由 `IgnoredAny` 消费而不保留。对转义字符串另做 allocation-free JSON lexical preflight，按解码后 UTF-8 字节精确量测，在 `serde_json` 建立单字符串 unescape scratch 前拒绝；低于最长合法字段名的调用方 value limit 仍只给字段名保留 13 字节常量窗口，最终拥有字符串继续按原 per-string/total budget 精确计费 | `7dd88ab`：entry 上限 0 + string 上限 0 时在首个 entry 的字符串解析前拒绝；nodes-first 文档在 node 上限 0 时同样先拒绝；5 个 `\u4e16` 的 15 字节解码结果在 13 字节 preflight 边界拒绝。原有 8 项 schema 语义/错误/Unicode/累计预算回归加新用例为 9/9，Core 全目标严格 Clippy及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32822319496](https://github.com/seiunx-dev/unity-rs/actions/runs/32822319496) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十六项完成证据 |
| 3A-37 | **已完成外部 MonoBehaviour schema 的 `objects × entries` 查找放大治理**：注册表不再为每个 stripped 对象线扫最多 100,000 条 schema；以每个 registry 独立随机键对 portable assembly（ASCII-insensitive、忽略可选 `.dll` 后缀）、namespace、class 和 exact/fallback Unity version 建索引，hash collision 仍逐项复验完整 identity，不会选错树。查询先取首个 exact version，再取首个 unversioned fallback；后载文档和重复条目仍不能覆盖首条。索引只保留 entry index，不复制第二份身份字符串；`push` 与多文档 `extend` 的 map/bucket/entries 增长全部可失败，`extend` 先在临时索引完成重建再事务性提交 | `0888585`：20,000 条同 assembly/class、不同 version 的 schema 逆序逐一查询，candidate probe 不超过 `2 × N`；同 key 的 exact/fallback 后置重复项仍选首条，跨文档 `extend` 也保持第一份文档优先。schema 定向测试 10/10、Core 全目标严格 Clippy，以及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32826196935](https://github.com/seiunx-dev/unity-rs/actions/runs/32826196935) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十七项完成证据 |
| 3A-38 | **已完成 Python 外部 MonoBehaviour schema 集合的 `objects × schemas` 查找放大治理**：Python 不再把每个可见 schema 包成一个单项 provider，再对整张列表执行 exact/fallback 两轮线扫；Core 新增不可变共享 registry set，以每个集合独立随机键索引 `(registry, entry)`，通过 `Arc` 保留原 registry，只保存位置而不复制 TypeTree 或 identity。Python API 形状不变，并在转换首个元素前拒绝超过 100,000 项的集合 | `98f7069`：20,000 个独立 registry 逆序逐一查询，candidate probe 不超过 `2 × N`；指针身份断言证明返回原 registry 的 TypeTree，重复 exact/fallback 保持首项优先；安装后 Python 测试用 100,001 个 `None` 证明计数上限先于元素转换。schema 定向测试 11/11、Core/Python 严格 Clippy，以及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32830773426](https://github.com/seiunx-dev/unity-rs/actions/runs/32830773426) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十八项完成证据 |
| 3A-39 | **已完成 Python schema 集合索引构建的 GIL 独占治理**：Python 列表计数与元素类型检查仍在 GIL 内完成，随后最多 100,000 个已验证 registry 的纯 Rust random-keyed 索引构建通过 `Python::detach` 在锁外执行；公开构造器、first-match、exact/fallback 和错误语义不变 | `a78a8a9`：安装后 wheel 与 sdist 测试分别构造 100,000-node 单 schema 和 100,000-entry schema collection，把 Python 线程切换间隔设为 1,000 秒，并要求辅助线程只能在 Rust 构造窗口内获得运行机会；不释放 GIL 的集合构造无法通过。Python 严格 Clippy、真实 wheel/sdist 两套 API 测试，以及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32832857909](https://github.com/seiunx-dev/unity-rs/actions/runs/32832857909) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第三十九项完成证据 |
| 3A-40 | **已完成 Node Promise schema 索引的 event-loop 独占治理，并统一程序化版本校验**：两个接收外部 schema 的 Promise API 仍在主线程对 JavaScript 数组做计数、节点/字符串预算检查和必要的值复制，但 Unity 版本身份校验、random-keyed Core registry 构建、对象解析与 JSON/Live2D 物化全部移到 worker。同步 API 保持立即报错；Core 程序化 `push` 现在与 JSON loader 一样事务性拒绝非法 Unity 版本，Rust/Python/Node 不再产生永远无法命中的无效条目 | `0c291d0`：Node 行为回归证明同步调用对非法版本立即抛错，而 `readMonoBehaviourJsonWithSchemasAsync` 与带 ACL decoder 的 Live2D Promise 均先正常返回、再在 worker 中 reject，且 decoder 不会被调用；Core 验证失败后 registry 仍为空，安装后 Python 对同一非法版本稳定给出 `ValueError`。完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32837018171](https://github.com/seiunx-dev/unity-rs/actions/runs/32837018171) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十项完成证据 |
| 3A-41 | **已完成 Node 异步 Texture2D/Texture2DArray 行翻转的 event-loop 治理**：解码原本在 libuv worker，但把 bottom-up RGBA 转为 JavaScript top-down 行序的 O(pixel bytes) 原地翻转留在 `Task::resolve`；大纹理会在 worker 完成后再次阻塞事件循环。任务输出现在由 `DisplayRowImage(s)` 类型保证已在 `compute` 完成行序转换，`resolve` 只做 Node `Buffer`/数组包装；同步 API 继续复用同一转换语义 | `13ee8f7`：Rust 单测分别对单图和两层数组断言 worker 输出已经翻转；安装后 Node addon 用真实 1×2 RGBA32 Texture2DArray 同时调用同步与 Promise API，逐层核对 top-down 像素完全一致，既有 2×2 Texture2D Promise 行序回归继续通过。Node debug/release addon、npm tarball、严格 TypeScript 及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32839865245](https://github.com/seiunx-dev/unity-rs/actions/runs/32839865245) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十一项完成证据 |
| 3A-42 | **已完成 Python Texture2D/Texture2DArray 行翻转的 GIL 治理**：两个读取入口原本只把解码放进 `Python::detach`，随后重新持有 GIL 执行最多 512 MiB 的 O(pixel bytes) bottom-up-to-display 原地翻转；其他 Python 线程会在解码结束后再次停顿。现在 `DisplayRowPyImage(s)` 作为 detach 闭包输出，保证完整行序转换已在锁外完成，attached 路径只按层可失败预留 wrapper 列表并搬移像素所有权 | `dd44509`：源码审计同时要求单图与数组转换位于各自 `py.detach` 闭包内，正反测试逐一把调用移到闭包外并确认门禁失败；真实 wheel 与 sdist 安装后测试继续逐像素验证 2×2 Texture2D 和两层 1×2 Texture2DArray 的 top-down 结果，严格 mypy 公开消费保持通过。Python 严格 Clippy、格式、API/GIL 审计及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32842974806](https://github.com/seiunx-dev/unity-rs/actions/runs/32842974806) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十二项完成证据 |
| 3A-43 | **已完成 Python AudioClip 与简单二进制资产的 GIL 治理**：`read_audio_clip` 原本只在 `Python::detach` 内解析对象，随后持有 GIL 读取 raw Region 或估算、分配并写出 WAV；Font、MovieTexture、VideoClip 也在 attached 路径才把 source-bound Region 复制成最多 512 MiB 的完整载荷。现在四个入口均在同一 detach 闭包内完成解析和最终有界载荷物化，attached 路径只搬移已完成的 Rust wrapper | `f061b90`：源码审计要求 `materialize_audio_clip` 及三个 `materialize_binary_asset` 调用位于对应 `py.detach` 内，四个负向变体逐一把调用移出闭包并确认门禁失败。真实 wheel/sdist 测试覆盖 AudioClip raw/PCM 以及 FSB5 IMA/DSP/VAG/HEVAG/FADPCM/MPEG/Opus/Vorbis 的 WAV 路径，Font/MovieTexture/VideoClip 字节和预算拒绝保持一致；Python 严格 Clippy、格式、API/GIL 审计及完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过。公开常规矩阵 [32845001314](https://github.com/seiunx-dev/unity-rs/actions/runs/32845001314) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十三项完成证据 |
| 3A-44 | **已完成 Python Cubism JSON 的 GIL 治理**：expression、physics、fade-motion、标准 `AnimationClip` motion 与 Tuanjie ACL motion 原本都在 detach 内解析 Core 对象，却在重新持有 GIL 后才写出最多 256 MiB JSON并扫描曲线/关键帧计数。现在五个入口都在同一 detach 闭包内完成有界 writer 和派生计数；expression 只有必须调用 `Py::new` 的参数 wrapper 留在 attached 路径 | `03e490b`：源码审计要求五个 writer helper 位于各自 `py.detach` 内，五个负向变体逐一移出调用并确认门禁失败。真实 wheel/sdist 测试除了既有 expression/physics/fade 文档外，新增标准 Unity clip 和带单 Cubism binding 的 Tuanjie ACL clip，完整解析 motion3 JSON，并逐路径验证精确输出预算成功、少一字节稳定 `ValueError`；完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过。公开常规矩阵 [32848377924](https://github.com/seiunx-dev/unity-rs/actions/runs/32848377924) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十四项完成证据 |
| 3A-45 | **已完成 Python 大型元数据表的 GIL 治理**：legacy `Animation` clips、`AnimatorOverrideController` overrides、`AssetBundle` preload/container、`ResourceManager` container、`PreloadData` assets、`AnimatorController` TOS/clips 与 `Avatar` paths 原本都在 detach 内完成 Core 解析，却在重新持有 GIL 后才预留并遍历最多百万项的第二张 Python-facing Rust 表。现在七个入口均在原 detach 闭包内完成可失败投影，attached 路径只接收最终 wrapper，公开 API 与字段语义不变 | `734d931`：源码审计要求七个 preparation helper 位于各自 `py.detach` 内，七个负向变体逐一移出调用并确认门禁失败；严格 Python Clippy、真实 wheel/sdist 重建与安装后 API 套件、严格 Python 3.9 mypy 均通过，完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过。公开常规矩阵 [32850532392](https://github.com/seiunx-dev/unity-rs/actions/runs/32850532392) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十五项完成证据 |
| 3A-46 | **已完成 Python 集合/场景/报告表的 GIL 治理**：file/object/resource convenience lists 与 pages、容错加载 diagnostics、SceneHierarchy nodes、SplitObjects/Animator FBX candidates、Material properties、模型贴图 skip 文本以及 export/extraction reports 原本都会在 Core detach 返回后再次持有 GIL 执行最多百万项的纯 Rust 预留、字符串复制和 tuple/wrapper 投影。现在 15 个 preparation 路径都在对应 Core 调用的 detach 内完成；模型唯一纹理文件仍只把必须调用 `Py::new` 的包装留在 attached 路径，公开 Python 签名和返回字段不变 | `a9f09d7`：call-aware 源码审计同时覆盖 block 和 single-expression detach closure，要求 15 个 helper 位于对应调用内，并以 15 个负向变体逐一改写后确认门禁失败；严格 Python Clippy、真实 wheel/sdist 重建与安装后 API 套件、严格 Python 3.9 mypy 均通过，完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过。公开常规矩阵 [32853842338](https://github.com/seiunx-dev/unity-rs/actions/runs/32853842338) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十六项完成证据 |
| 3A-47 | **已完成 Node ACL-backed Live2D 包表的 event-loop 治理**：完整包的 MOC、manifest、贴图、expression/motion/physics/pose/display-info 与 diagnostics 虽已在 libuv worker 物化，旧任务却在 `resolve` 才按包重新计算文件数、可失败分配并展开最终文件/诊断表、格式化诊断类型；大包会在 worker 返回后再次占用事件循环。任务输出现在就是最终 `Live2dPackageSet`，全部 O(packages + files + diagnostics) 的纯 Rust 投影在 `compute` 完成，`resolve` 只把已完成表交给 napi-rs，JavaScript API 与字段不变 | `4cbdbce`：源码门禁要求 task output 为最终表、转换 helper 只出现在 `compute`，两条负向变体分别恢复 Core 输出和把转换移回 `resolve` 后均稳定失败。Node debug/release addon、运行时 API、严格 TypeScript、包内容、安装后临时消费者与 npm tarball 全部通过；完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过。公开常规矩阵 [32856570477](https://github.com/seiunx-dev/unity-rs/actions/runs/32856570477) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十七项完成证据 |
| 3A-48 | **已完成 Node 异步 Texture2DArray 最终层表的 event-loop 治理**：此前 O(pixel bytes) 行翻转已在 worker，但最多 4,096 层、累计 1 GiB 像素的最终 Vec 仍在 `resolve` 才可失败预留，并逐层把 Core image 转为 napi-facing `RgbaImage`/`Buffer`。`DisplayRowImages` 现在直接持有 worker 构造完成的最终层表；`compute` 完成行翻转、表预留和每层投影，`resolve` 的 `into_nodes` 只移动一个现成 Vec，同步 API 和 JavaScript 像素/字段顺序不变 | `bb2eb98`：源码门禁要求 async task 继续输出 `DisplayRowImages`、worker helper 必须预留最终表并调用 `convert_image`，同时禁止 `into_nodes` 出现分配、循环或 image projection；两条负向变体分别改回未投影输出和把转换塞回 event-loop helper 后均稳定失败。Rust worker 单测确认两层 Buffer 已按 top-down 行序完成；Node debug/release addon、运行时 API、严格 TypeScript、包内容、安装后临时消费者与 npm tarball 全部通过。完整 `quality rust python node typing oracle` 本地门禁全部通过且零组跳过；公开常规矩阵 [32859413104](https://github.com/seiunx-dev/unity-rs/actions/runs/32859413104) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十八项完成证据 |
| 3A-49 | **已完成 Python `SpriteAtlas` 大型元数据表的 GIL 治理**：对象解析原本在 `Python::detach` 内完成，但随后重新持有 GIL 才遍历最多 1,000,000 条 packed-sprite/render-data/secondary-texture 记录，转换 PPtr、展开 rect/vector/settings tuple 并移动名称表。现在 `prepare_sprite_atlas` 在同一 detach 闭包内完成全部纯 Rust 可失败表投影；attached 路径只为最终 key/render-data/secondary-texture 调用必须由 PyO3 执行的 `Py::new`，公开字段、顺序和预算语义不变 | `e8c9365`：源码门禁要求 preparation 位于 `py.detach` 且 Python wrapping 位于闭包外，负向变体把 preparation 移走后稳定失败。Python crate check/严格 Clippy、真实 wheel 与 sdist 重建和安装后 SpriteAtlas 行为测试、公开面检查、严格 Python 3.9 mypy，以及完整 `quality rust python node typing oracle` 本地门禁均全部通过、零组跳过；公开常规矩阵 [32862849606](https://github.com/seiunx-dev/unity-rs/actions/runs/32862849606) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。这是 hostile-input 审计的第四十九项完成证据 |
| 4 | **完成 Python 主接口审计**：以 Rust Core 的稳定高层能力为源，逐项核对 Python 的加载、读取、导出、预算、错误类型和类型桩；Node 只作为可选绑定跟进稳定接口，不作为 Python 完成的前置条件 | **本地完成（2026-08-22；2026-08-25 随诊断分页扩展）**：107 个高层 Core 方法均被机器检查为 103 个真实 Python 映射或 4 个明确 Rust-only ownership/borrow 入口；66 个公开 Python 方法和 4 个属性全部进入严格 Python 3.9 mypy 消费端并由源码门禁防漂移；安装后的 release wheel、sdist 与重建 wheel 公开面和 `.pyi` 双向一致，完整 API 测试通过；大结果继续使用有界、可失败分配，Rust/Python 路径不经过 C ABI 或 .NET |
| 5 | **做 1.0 退役审计**：重新逐条核对本文“完成判定”，把 C# 从日常运行链彻底降为可选 oracle | **无头运行时退役完成（2026-08-25）**：`check_delivery_scope.py` 与 8 项反向自测确认 workspace 只有 Core/CLI/Python/可选 Node，三个前端均直接依赖 Core，源码、公开 API 和发布物均无 GUI、旧 C ABI 或 context handle；`cargo metadata` 独立复核一致。最终 HEAD `0c23ac1` 的公开矩阵 [32863442940](https://github.com/seiunx-dev/unity-rs/actions/runs/32863442940) 为 16 个实际 job 全绿、2 个手工发布条件 job 正常跳过、0 失败。兼容性 1.0 仍等待本文明确列出的外部 corpus，不把未验证格式冒充完成 |
| 6 | **处理非阻断上游事项**：有可审计方案时向上游提交 `ruopus` SILK 与 vendored 纹理解码器修复；拿到可验证 ACL 样本后再评估纯 Rust Tuanjie ACL decoder | 上游 issue/PR 或本仓库可复现记录可独立运行；任何替换不得使已精确通过的 CELT/纹理路径回退，也不得引入未授权专有二进制 |

贯穿所有步骤的范围约束不变：**不做 GUI，不恢复或扩展旧自定义 C ABI，
不把 Oodle 等专有库静态打进 Core**。Rust 和 Python 是正式目标，Node 是可选
交付面；C# 只承担历史参考和可选差分 oracle。

## 后续优先顺序

上表用于安排实际工作；本节保留各项背景、限制和已完成调查。前一版的九条里有四条已在 2026-08-15 完成或走到尽头，按本文维护规则移出：托管差分已扩到容器/版本门/已实现的解码路径（含整包 Live2D 与全部块格式）；binary FBX 已在 Core/CLI/Node/Python 全部可达；MOC3 标识表与散件发现回退已接；`ruopus` SILK 已定位为上游缺陷并写进 `docs/upstream-defects.md`，CELT 路径有精确差分把守。剩下的按能否自主推进排序：

> 2026-08-24 更新：下方历史清单的第 1 项“让 CI 重新跑起来”已经完成；仓库公开后，
> PR 常规矩阵与手工六平台发布矩阵分别 16/16、28/28 全绿。当前真正依赖外部输入的工作
> 从第 2 项真实 corpus 开始；第 1 项正文保留用于解释本地门禁为何建立。

**需要外部输入，我这边无法推进：**

1. 让 CI 重新跑起来（GitHub Actions 计费）。本机已把 CI 的常规步骤复跑过一遍且全绿；Linux 运行面现已在 amd64/arm64 容器中覆盖，仍无法本机复现的是 Windows 运行与 GitHub 托管发布矩阵。为降低这条的代价，新增 `tools/local_ci.py`：一条命令跑完 CI 的全部步骤（现为 62 步、14 组）（格式、Python/Node API、六平台矩阵与交付范围审计及其反向自测、RustSec、Clippy、rustdoc、打包、许可证聚合、无 GUI/旧 C ABI 的交付范围与产物校验、workspace 构建/测试、托管差分、MonoBehaviour schema 生成器、音频差分、输出格式校验、release CLI 的构建/smoke/staging、Node debug/release 构建/测试/打包、Python wheel 与 sdist 的构建/安装/两套测试、Python 3.14 abi3 前向兼容、严格 Python 类型消费、UnityPy 差分，以及可选交叉/Linux 容器门禁），缺哪个工具就把那一组记为跳过而不是失败。2026-08-15 修了一处会让证据凭空消失的问题：wheel 原先直接装进当前解释器，而 Homebrew/发行版的 Python 现在按 PEP 668 直接拒绝安装——于是 `install wheel` 失败，依赖它的 `wheel surface`、`python api` 与 UnityPy 差分一并失败，也就是说 Python 那一侧的公开面守卫和 API 套件其实根本没在跑。现在 `python` 组自己建一个 `--clear` 的临时 venv（带 `--system-site-packages`，好让宿主已装的 UnityPy 仍可导入），宿主解释器只作为建 venv 的底座和 maturin 的目标。修好之后第一次跑就抓出一条陈旧断言：`python_api.py` 里还在断言 physics3.json 的紧凑写法，而两次提交前刚把 Cubism 文档改成与托管一致的展开写法——正是「没在跑的测试」会掩盖的那类回归。随后类型桩审计又抓到两项幽灵 API：`.pyi` 允许导入 `AclDecoder`/`OodleDecoder`，运行时模块却没有这两个名字；现在二者是真正的运行时导出，wheel 守卫也改成双向比较。本轮逐项对照 workflow 时又发现本地脚本漏掉了 workspace build、sdist 重建、release CLI/Node 产物和 Python 3.14 安装；这些步骤现已接入并实际通过。交付范围检查也已实际通过，会把 workspace、依赖方向或产物内容的漂移直接变成失败；schema 生成器步骤则在本轮以真实临时 DLL 单独验证，并和托管差分一起通过 `oracle` 组。这不能替代 CI——CI 是在三个平台上跑这套矩阵——但它让拿得到 Linux/Windows 的人能自己产出同样的证据，而不必先从 workflow 文件里把步骤拼出来。另外补了一个 `cross` 组：Linux x86-64 继续编译完整 workspace 与全部测试目标；Windows x86-64 现在由同一门禁用 MinGW 编译 Core/CLI/Python，并在开始前同时检查两套 C 工具链，缺任一个时严格模式都会失败而不是先产出一半证据。Node addon 要链接 `libnode.dll`，仍由真实 Windows runner 验证，不用不完整的交叉环境冒充。这只覆盖「能不能编译」——路径假设、漏掉的 `cfg`、只在某个平台成立的 `Send` 之类——行为层面则由新增的 `linux` 组覆盖：用 CI 钉住的同一个 Rust 1.88 镜像，在容器里跑 core+CLI 的完整常规测试、6 项畸形输入扫描和全部 CLI 套件，并构建、执行和 stage release CLI；Python wheel 也完成 release 构建与两套安装后测试，x86-64 与 arm64 两个架构都已实跑通过（CI 的 wheel 矩阵正是这两个）——这是 LZMA 那次提交之后第一次拿到 Linux 上的真实运行结果，而 Python 那两套正是之前坏了很久没人发现的。Node release addon 也在两种 Linux 架构的干净容器副本里跑通了，且实际生成并检查了 npm tarball（镜像没有 Node，因此按 CI 用的版本拉官方构建），10 组 API 测试全过，整个构建发生在只读源码的临时副本中，容器销毁后不会污染宿主构建。Windows 目前仍只有编译层面的验证。顺带记一笔：workspace 唯一的 C 依赖是 `zstd`（`zstd-sys` 会编 C 源码），交叉编译因此需要目标平台的 C 工具链；`docs/upstream-defects.md` 里原先把「纯 Rust」当成本 crate 已有的性质来论证，那是不准确的，已改成「再加一个原生依赖的代价」；
2. 扩充真实 corpus，按实际命中率排序缺口（需要真实游戏文件）。为降低这条的门槛，corpus 用例的 `expected` 快照改成可选：不给快照时依然会把每个对象都读一遍、出错即失败，并报出读到了多少文件/对象/可解析载荷，只是没法断言取值对不对。这样「有游戏文件」就够跑，不再同时要求 .NET SDK 与 AssetStudio checkout 来生成快照。另加了一条防自欺断言：没有快照的用例必须至少解析出一个对象——reader 认不出来的输入会被当成资源文件、解析出零个对象，否则那种用例会安静地通过；
3. 获取样本并实现 Unity 虚拟几何、Tuanjie 虚拟几何 cluster、UnityArchive，而不是猜测布局
   （Unity 6000.2 的 MeshLOD 尾部已凭真实 6000.3 TypeTree 实现）；
4. 是否把 `ruopus` 与 `texture2ddecoder` 的缺陷提到上游（补丁已备好，见 `docs/upstream-defects.md`）。

**可以自主推进，但代价与收益需要先掂量：**

5. 纯 Rust Tuanjie ACL 2.x 解码。crates.io 上没有现成实现，属于从零写；而且没有可对照的样本，写出来无法验证，因此在拿到样本之前不宜动手；
6. 补齐高命中率的平台纹理/音频长尾（新 codec 必须先有真实样本和独立 oracle）；
7. 继续提升 Node 专用 API 覆盖。2026-08-15 补了此前完全没有绑定的 `readFont`/`readMovieTexture`/`readVideoClip`，按 GameObject 粒度的 FBX 规划（`splitObjectFbxCandidates`/`animatorFbxCandidates`/`readGameObjectFbx`），以及 Cubism 单对象读取器。后者先接入 physics/expression/fade-motion，又补齐 `readCubismPosePart`、`readCubismDisplayInfo`、标准 Unity `AnimationClip` 的 `readCubismClipMotion` 和 Tuanjie ACL 的异步 `readCubismClipMotionWithAclDecoder`；2026-08-22 又把完整 Sprite/SpriteAtlas、AnimationClip 和 Avatar 稳定元数据接上，并让新 `readAudioClip` 与 Core/Python 共用 `auto`/`raw`/`wav` 选择和输出预算，旧 `readAudio` 保持 raw-only 兼容；同轮 `exportWithOptions` 把 Core 的 mode、filename format、image/audio、JSON、overwrite 以及 16 项对象/累计/单 payload 预算完整带到 JavaScript，旧 `export(outputRoot, overwrite?)` 不变；`readFbxBinaryWithAclDecoder` 与 `readGameObjectFbxWithAclDecoder` 又补齐 Python 已有的 Binary/selected-model ACL 导出面。测试不是只断言方法存在：pose/display 由真实嵌入 TypeTree 的 class 114 fixture 驱动，标准 clip fixture 完整走 2022.2 muscle/binding/event 布局，Tuanjie fixture 走 2022.3.55t4 `m_AnimData`、ACL 容器/hash/decoder map/StreamingInfo 与 Core 回传校验，Avatar 则完整走 Tuanjie skeleton/TOS/HumanDescription；音频 fixture 走 legacy PCM16 到精确 48-byte RIFF/WAVE、raw 保真、格式拒绝与一字节输出预算；导出 fixture 逐项拒绝全部负预算，并真实写出 path-ID raw、lossless WebP、raw AudioClip 和受限 WAV；ACL 场景 fixture 真实绑定 GameObject→Animator→Controller→Tuanjie clip，分别证明 ASCII、binary 和 selected-model worker 各调用 decoder，`includeAnimations=false` 则不得调用。两类 clip 生成的 motion3 JSON 都交给 JavaScript JSON parser，完整 clip/avatar 元数据另逐字段核对。至此 Node 的直接 motion、稳定 clip、Avatar、decoder-free AudioClip、通用导出配置和模型 ACL 面与 Python/Core 对齐；TypeScript 消费测试另外验证 ACL/Oodle callback、`Buffer`、`bigint`、路径表和全部 export optional 字段能由发布声明直接编译。最新又补上完整包的两个入口：同步 `readLive2DPackagesWithSchemas` 恢复 stripped 模型/渲染器字段，Promise `readLive2DPackagesWithAclDecoder` 可同时接 schema 与 decoder。回归集合真实走 `GameObject → CubismModel → CubismMoc`、跨文件 `CubismRenderer → Texture2D` 和 `Animator → Controller → ACL AnimationClip`；没有 schema 时包不存在，没有 decoder 时保留包并给出 `MotionReadFailed`，两者都有时产出可解析的非空 motion3，错误 decoder 则降为带原因的包诊断。

## 完成判定

只有同时满足以下条件，才可把 Rust 重写标记为完成：

- Rust Core 和 Python 主流程不依赖 .NET、GUI 或旧 C ABI；
- 支持矩阵中的“Implemented”项均有相应单测、边界测试或差分证据；
- 代表性真实 corpus 在支持的 Unity/Tuanjie/平台矩阵上稳定通过；
- 未实现格式均有明确、稳定的 Unsupported 行为，不产生静默错误输出；高于已验证
  上限的标准 Unity 版本按默认宽松策略尝试最新已知布局，失败时同样稳定归入
  `Unsupported` 并注明该次尝试；
- Rust crate、Python wheel/sdist、可选 Node 包和 CLI 的跨平台发布任务通过；
- 导出/解包保持有界、拒绝路径穿越和符号链接目标，并采用安全原子发布；
- C# 只需作为历史参考或可选 oracle，不再承担用户运行时功能。

### 本轮进展（2026-08-15 晚）

**MonoBehaviour schema 从「接口存在」做成了「能用且被验证过」**。此前 Core 有 provider trait 和 registry，但没有任何东西能把一份真实的 schema 送进去：CLI 没有开关，`export` 根本不接 provider。现在补齐了三段：`MonoBehaviourSchemaRegistry::from_json` 读生成器写出的文档（对每一种「描述不了任何东西」的形状都明确拒绝，而不是收下之后静默读错）；`export` 走 `ExportPlan` 拿到 provider 与文件下标（解析跨文件 `m_Script` 需要后者）；CLI 加 `--mono-schema <path>`（可重复）与 `--mono-schema-override`。经由 schema 读出的对象在导出报告里叫 `typetree_json_schema` 而不是 `typetree_json`——这是更弱的一句话，报告不该把两者混为一谈。

- **一个真实缺陷**：`MonoScript` 里的程序集名是 `Fwk`，而按目录生成的 schema 写的是 `Fwk.dll`，registry 逐字比较，于是每一次查找都落空——调用方可以送进一整套 schema，什么都不发生，也没有任何提示。一款 Unity 6000.3 游戏里 115 个不同的 MonoScript，修之前命中 0 个，修之后命中 81 个（其余是这份 dummy DLL 集里确实没有的类）。
- **生成器 `tools/monoschema`**：读游戏的托管程序集（Mono 的 `Managed` 目录或 IL2CPP dump 的 DummyDll），产出 JSON。读程序集与读数据文件是两种不同的信任判断，因此它是独立程序：Rust 侧不链接任何托管 reader，也不打开 DLL，只消费这份 JSON。
- **dummy DLL 的两处失真**，都安静到值得单独写出来。其一：Il2CppDumper 写出的 enum 基类是底层类型（`System.Int32`）而不是 `System.Enum`，Cecil 因此看不见 enum，Unity 的序列化逻辑答「不是可序列化值类型」，字段被丢掉且没有任何提示——schema 短四个字节，唯一症状是很久之后读越界；光 `UnityEngine.UI.ScrollRect` 就丢三个字段。生成器按形状把它认回来（一个名为 `value__` 的整型实例字段），这份语料修回了 1,680 个 enum。其二：字段类型所在的程序集不在目录里时，`Resolve()` 失败，字段同样被安静丢掉——生成器把这些类和字段打到 stderr（下游无从得知 schema 为什么短），而 Rust 侧仍然拒收这种对象，因为它的树覆盖不了对象的全部字节。
- **验证**：schema 对着它所来自的构建是无法验证的——reader 没有可以反对的东西，布局错了就会读出自信的胡话。`tools/mono_schema_diff.py` 用唯一有意义的方式验：拿到**仍然带 TypeTree 的构建**，同一个对象读两遍再比。全部 2,777 个 bundle、94,713 个经 schema 读出的对象，取值与 Unity 自己的树逐一相同（53,350 个连 JSON 都逐字节相同，其余 41,363 个只差字段名——重建的树按 C# 源码命名，Unity 不总是同意，比如 `UnityEngine.Rect` 序列化成 `x, y, width, height` 而字段叫 `m_XMin, m_YMin, m_Width, m_Height`）。细节见 `docs/mono-schema.md`。

**`SerializeReference` 托管引用注册表已实现**。此前那段余数被当成「读不了的对象」拒收，注释里还断言它「几乎总是」注册表——两份语料都不支持这个说法，措辞已改。真正的实现是：注册表自己的形状在 type tree 里，每条记录里存的对象的形状不在，要从文件声明的 reference types 按 class/namespace/assembly 取。三处细节各由一个「去掉就挂」的测试钉住：不命名类的记录是 Unity 的 null 引用，不存任何东西，因此字段不写出来而不是编一个；命名了文件未声明的类型的记录直接拒收，因为它的长度只能从不存在的布局得知，没有东西可以跳过；只读最外层的注册表声明，reference type 自己的树里可以再声明一个，读它会吃掉不属于它的字节。一款 6000.3 游戏的 CriWare bundle 里 93 个对象现在能读，且与 UnityPy 逐字节相同（其中一个是 176 字节有类型字段加一个 `rid` 后面 712,288 字节的载荷）；托管 reader 根本没实现注册表，所以这条没有 oracle 行可比。

**Node 补上三处真实缺口**。此前 Node 是「一个选项一个工厂」：路径、路径加 Unity 版本、路径加 Oodle decoder——这些组合不起来，而组合是常事（一个 UnityCN 加密、同时头部版本被剥掉的档案就是同一个文件的两件事）；UnityCN key 与 skip-unreadable 则根本没有入口。现在 `openWith(path, options)` 全部接受，`openWithOodle` 也接同一份 options。`readLive2DPackages` 一直在丢掉它自己文档里承诺的 physics/pose/display-info 三份文档（它们跟包一起物化，只是没被拷出来），而且只返回包不返回 diagnostics——于是一个「因为动作的 clip 在没加载的 bundle 里」而不完整的模型，看起来和完整的一样。两处都已修。

**两处线性搜索改掉**（都是「在已经做过的事情里再线性找一遍」）：`ZipContainer::read_entry` 每次调用都重开档案，于是中央目录被解析 N 次；解包器的已占用路径集合按路径建键却靠扫描每个键来查（因为大小写不同的两个名字在目标平台上是同一个文件）。一万条目的档案原本花 29 秒 CPU、且条目数翻倍代价约翻四倍，现在是 0.54 秒且线性。墙钟时间不变，全部落在每文件一次的 `sync_all` 上——那是耐久性取舍，不是缺陷。

**`SpriteAtlas` 补上托管差分**。此前完全没有：sprite 行比的是渲染结果，只在恰好经由图集解析时才碰到它，图集这张表本身（键相等、版本门字段、以及依赖它们的每一个偏移）没有任何东西在比。四个 fixture 各守一道版本门，2020.1 那个是变异测试逼出来的——没有它，把 secondary texture 的门往前挪一年，任何 fixture 都看不出来。两处细节决定这条差分有没有意义：GUID 按构造它的字节输出（.NET `Guid` 会把前三个字段按显示顺序倒过来，拿那个拼写去比 Rust 保留的原始键，失败的原因与两边的 reader 都无关），以及两条记录特意选成「按原始字节排序」与「按显示顺序排序」结果不同，这样顺序错了的比较不可能通过。浮点按位模式输出，跟 material 行一样：两个序列化器对整数值浮点的拼写不同（`8` 与 `8.0`），真正的舍入差异会藏在这个分歧后面。

**语料自带的另一份提取，被当成第二个 oracle 用了起来**。`sirius_assets/extracted/` 是另一个工具对同一批 bundle 的完整提取（96,773 个 JSON、2,609 个 PNG、637 个 OBJ、577 个 txt、58 个 shader）。这比任何手写 fixture 都宽——覆盖的是这款游戏真实出货的东西、真实的版本、以及手写 oracle 到不了的规模。`tools/extracted_corpus_diff.py` 按每类文件能被要求的标准去比：

- **OBJ 比数值不比文本**。两边写的是同一份几何，但那份提取把浮点写成九位有效数字，本项目复现的是当代托管 writer 的最短往返形式；两边都 parse 回 `f32` 再比，比的是 reader 读到了什么而不是两个工具选择怎么打印。
- **txt 逐字节比**，没有可归一化的东西。
- **PNG 比"画出来是什么"而不是比原始通道**。尺寸必须完全相同，alpha 最多差两级，合成值 `rgb * alpha / 255` 最多差二——后者是"颜色和 alpha 各差一级"复合出来的上界（`|r1a1−r2a2| ≤ 255+255`），不是为了让语料通过而挑的数。这个形式是必要的而不是图方便：两个正确的块解码器会落在半格两边（一张 768×1536 的贴图在 4,718,592 个字节里差 4 个、每个恰好差 1），而在接近 0 的 alpha 下它们分歧大得多而画出来的结果并没有——某张 sprite 在 alpha ≤ 8 处原始通道差到 26，合成值全图不超过 1。按原始通道比要么在看不见的颜色上失败，要么需要一个手挑的 alpha 阈值，那就是调到绿为止。
- **`Texture2D` 与从它裁出的 `Sprite` 常常同名**，那份提取用 `_sprite` 后缀区分，本项目的导出用 path ID 区分，两套对不上；所以每个 bundle 导两次（一次是名字不带后缀的那几类，一次是 sprite），各自跟提取的对应那一半比。一次比完会把 sprite 拿去跟它自己的源贴图比，凡是裁过的地方都不同。

**这条差分立刻抓到一个大缺口**：Unity 6000.2 起 `Mesh` 尾部多了 `MeshLodInfo`，本项目的版本门停在 6000.1，于是那款 6000.3 游戏里**每一个 mesh 都被拒**（一个 bundle 里 152 个 mesh 一个都没导出来）。形状不用猜——那份构建带 TypeTree，Unity 自己对 `Mesh` 的描述就在文件里：一条选择曲线、一个层级数、以及每个 sub-mesh 的索引区间。修完之后那个 bundle 的 151 个 mesh 全部读出，逐个顶点/法线/UV/面索引与那份提取相同。顺带把"空 mesh"从 failed 改成 unsupported：Unity 会写空 mesh，托管导出器也跳过它们，算成失败会让一次本来干净的导出以非零码退出。这条尾巴也补了托管差分 fixture（托管侧把 `MeshLodInfo` 的读取注释掉了且不要求对象被完整消费，因此它停在尾巴之前仍报出相同几何——这正是它能当 oracle 的原因：本项目要是走错了这段就会拒收该 mesh，两边随即不一致）。

跑完前 400 个 bundle：301 张 PNG 里 24 张逐像素相同、271 张在解码器容差内，1 个 OBJ 逐值相同。唯一一次"本项目没导出"其实是文件名差异——sprite atlas 的贴图 Unity 就叫 `sactx-0-1024x512-ASTC 4x4-...`（带空格），本项目保留资产自己的名字，那份提取把空格换成下划线；工具现在两种拼法都试，因为"没导出"是个远比"名字不同"更该警觉的报告。

**同一条路子又抓到第二个版本门**：Renderer 的"已验证范围"停在 6000.2，于是这份 6000.3 语料上的 scene/FBX/OBJ 导出在开始之前就被拒。实际什么都没变——那份 6000.3.12f1 构建自己的 `MeshRenderer` type tree 列出的字段、顺序，与本项目读的逐一相同，6000.2 的 `m_ForceMeshLod`/`m_MeshLodSelectionBias` 之后再没有新字段。这份语料里没有 `SkinnedMeshRenderer`，它的树取自 UnityPy 的 type-tree 数据库（开发期查阅，不入库），而同一个数据库里的 `MeshRenderer` 与真实构建逐字段相同——这正是它在这里可以当依据的理由。放宽之后立刻露出下一处：一个 bundle 里 152 个 mesh 中有一个是空的，整个场景就失败；本项目拒收的 mesh 所属的 renderer 不贡献任何几何，丢掉它才是对的（`PPtr.TryGet` 对解不开的指针本来就是这么做的），而畸形的 mesh 仍然失败，因为那是关于字节的陈述而不是关于资产的。那个 bundle 现在能导出 152 个材质的 OBJ 与 FBX。

**动画三件套的上限也补齐到 6000.3（2026-08-26）**：AnimationClip、AnimatorController、Avatar 此前仍停在 6000.2——正是 Mesh、Renderer 两次教训里那类"区间没跟上"的遗留项。三个类的分支结构在 6000.2 之后没有任何新字段（AnimatorController 最高的门是 6000.2 的 entity IDs，AnimationClip 最高是 2023.2 的 streamed-curve 拆分，Avatar 最高是 2019.1 的 HumanDescription），托管 reader 同样没有更新的分支；按 sprite-atlas 前例的做法，三个类各带一个 `6000.3.12f1` 差分 fixture 一起放宽，托管 oracle 首轮全对。

**版本上限策略整体转向默认宽松（2026-08-26，维护者裁决）**：Mesh、Renderer 的两次教训说明硬上限的实际效果是"新引擎版本发布 → 真实游戏整体解析失败 → 等一次带 fixture 的放宽"，而 UnityPy/AssetStudio 在同样场景下直接可用。现在**高于已验证上限的标准 Unity 版本默认按最新已知布局解析**；解析不匹配时错误仍归 `Unsupported` 族并注明"高于已验证区间、按最新已知布局尝试未成功"，保留内层诊断（含经由不可信 count 触达的 EOF 与预算诊断——超上限时解出的 count 本就不可信，中途的预算命中按布局错位对待）。`strict_unity_versions` 选项（CLI `--strict-unity-versions`、Python `strict_unity_versions=True`、Node `strictUnityVersions: true`）恢复原有的直接拒绝。**不放宽的部分**：容器格式门（SerializedFile >22、UnityFS v9+）、各类的版本下限（Mesh/Renderer/动画三件套的 2017.3 等）、stripped 版本、Tuanjie 构建（逐 build 布局波动太大）。宽松解析不是验证——文档化的已验证上限仍只随 fixture 移动，机制统一在 `version_gate.rs`，七个类门（Mesh、Renderer、SpriteAtlas、Shader、AnimationClip、AnimatorController、Avatar）共用同一套三态语义与错误映射，各自的拒绝消息逐字节保留。

**两份语料一起过了 corpus 闸门（2026-08-15 晚）**：`real_corpus` 用同一份 manifest 同时跑两个用例——2022.3 播放器目录 23 个文件 / 610,552 个对象 / 190,300 个有解析载荷，6000.3 Addressables 2,778 个文件 / 243,617 个对象 / 104,565 个有解析载荷，release 下 240 秒全过。第一次跑是失败的，而失败的原因值得记一笔：报的是"Texture2D RGBA output is 16777216 bytes, exceeding limit 7953295"，看起来像贴图缺陷，实际是用例声明的预算不够——Live2D 物化的总预算是**整个用例所有包**共用的，交给解码器的上限就是它剩下的余额，于是错误里出现了一个调用方从没选过的数字。已改成明说是哪个预算见底、还剩多少，`corpus/README.md` 也补上"总量是整个用例的、不是每个包的"。

**PNG 那边剩下的分歧是 sprite 紧密网格的边界**：最坏的一张里 1,143,000 个像素差 4 个，而且是双向的——一个 texel 本项目保留、提取那边遮掉，另外三个反过来。这是光栅化的边规则，而本项目的光栅器由托管差分逐字节钉住，所以分歧在提取那一侧。这些情况按"报告"处理而不是"容忍"：把容差放宽到它们能过，同样会盖住真正的遮罩缺陷。

### 对照结论（2026-08-15）

逐条对上面七条自评，其中六条已满足、一条部分满足：

| 条件 | 状态 |
|---|---|
| Core/Python 主流程不依赖 .NET、GUI 或旧 C ABI | 满足，且**从 2026-08-15 起有门禁把守而不再只是约定**；2026-08-23 进一步删除了此前仅排除在 workspace 外的旧 C ABI/context crate。`tools/check_delivery_scope.py` 读 `cargo metadata` 的解析结果（不是读清单文本）核对：workspace 恰好四个成员、各自的 target kind 正确、清单都在本仓库内、三个前端都直接依赖 Core，任何一个的普通依赖里都不许出现 GUI/旧 FFI 包名；同时拒绝旧 FFI 源文件或 Rust/Python/Node 公开 `Context`/`context_id` handle 回流。产物侧另有 Core `.crate`、npm tarball、wheel 与 sdist 的内容检查，拒收 `.cs`/`.csproj`/GUI 目录 |
| 「Implemented」项均有单测、边界测试或差分证据 | 基本满足；纹理/Sprite/Mesh/AnimationClip/Live2D/容器/版本门均有托管差分，TypeTree 另有 UnityPy 第二 oracle，畸形输入另有专门扫描。唯一没有差分的是 5.5+ 序列化 shader，原因是托管侧自身的初始化缺陷；2021+/2022+ 的 shader 已实现（见下），其结构正确性由 46 个真实 shader 与 UnityPy 的逐一比对背书；托管差分覆盖的仍是 5.2/5.3，因为托管侧对 2021+ 根本不产出对象 |
| 代表性真实 corpus 稳定通过 | **部分满足，且已扩到第二款游戏与第二个引擎世代（2026-08-15）**；(1) 一份完整的 Unity 2022.3.62f2 播放器目录（23 文件 / 610,552 对象 / 190,300 个有解析载荷）；(2) 四个真实 UnityFS v8 bundle；(3) **2,778 个 Unity 6000.3.12f1 的 Addressables bundle（926 MB / 243,617 对象）**，全部零错误通过。仍缺的是 Tuanjie / Switch / 更老版本，以及带托管快照的取值比对 |
| 未实现格式有明确稳定的 Unsupported 行为 | 满足；且畸形输入扫描验证了不会 panic。2026-08-26 起高于已验证上限的标准 Unity 版本默认按最新已知布局尝试，失败仍稳定归入 `Unsupported`（见上文策略转向段落） |
| 跨平台发布任务通过 | **满足（2026-08-24）**；PR run 32659993206 的 16 个主 job 全绿，真实覆盖 Linux/Windows/macOS Rust 运行、三平台 Node、六平台 Python wheel、质量和差分 oracle。workflow_dispatch run 32660298990 进一步 28/28 全绿，额外执行并上传 Linux/Windows/macOS × x86-64/ARM64 的六个 CLI 与六个 Node 制品；CLI staged 二进制、wheel 安装后 API/mypy、Node tarball 临时消费者安装及 JS/TypeScript smoke、许可证/notices 均在对应 runner 验证。专用静态门禁继续防止任何一路或必要产物测试被静默删除 |
| 导出/解包有界、拒绝穿越与符号链接、原子发布 | **现在才算满足（2026-08-15 修）**；此前这一行写"满足"是**说大了**：主导出与解包路径确实是临时文件加原子发布，但模型同级贴图那条不是——它直接以最终文件名 `create_new` 然后往里写，写到一半失败就留下一张截断的图片，而下一次导出会因为"已存在"跳过它，于是一张坏图会被当成成功的结果永远留在那里。现已改成同目录临时文件、`sync_all`、硬链接 no-clobber 发布，放弃时由 Drop 清掉临时文件。这一行的教训与本文其他几处同源：断言覆盖面时要按路径逐条数，而不是按"这个模块做过这件事"泛化 |
| C# 仅作历史参考或可选 oracle | 满足 |

**第二款游戏（Unity 6000.3.12f1）接入，又抓到六处（2026-08-15）**：`~/ida/outnoteida/sirius_assets/bundles` 是一份 2,778 个 Addressables bundle 的完整导出（926 MB、243,617 对象、Unity 6000.3.12f1）。容器层一次通过——v8 之前已支持——对象层则一个接一个地挡住后面所有文件：

1. **shader 的 pass 布局又变了两次**。Unity 2023.1 把 `m_EditorDataHash` 与 `m_Platforms` 从 `SerializedPass` 里删了，而本项目只按「2020.2 加入」设了下界，于是读 Unity 6 的 shader 时会多读两个数组——一个 pass 报出 1,952,671,082 个 platform 就是这么来的。6000.3 还在 asset GUID 之后追加了 `m_AssetLocalIdentifierInFile`。两个边界同样取自序列化布局本身，语料里 58 个 shader 现已全部解析。
2. **空的贴图引用被当成损坏**。Addressables 会把 sprite 与它的图集拆到不同 bundle，逐个 bundle 读时天天遇到；这是关于资产的陈述，不是关于字节的，现已归为「声明性拒绝」。
3. **type tree 描述的字节少于对象本身**时报的是字节数不符。现代 Unity 里那段余数几乎总是 `SerializeReference` 的托管引用注册表——这里一个 CriWare 音频组件是 176 字节的有类型字段加 712,292 字节的注册表，挂在一个 `rid` 后面。注册表未实现，现改为明确说明而不是描述读取器自己的困惑。
4. **三处上限低于普通内容**，每一处现在都由实测定：一个 Live2D 模型的单个数组有 3,892,672 个元素（上限 100 万）；同一个模型物化需要 439 MB（上限 256 MB，corpus 闸门现按用例声明的预算来放宽，这本就是该闸门写明的契约）；一张 lane-skin sprite 需要 536,921,219 次紧密网格像素测试（上限 536,870,912，只超了 0.01%）。

**顺带纠正一处我自己写错的归因**：早些时候我把「真实游戏里 ASTC 与 UnityPy 差 1」归因于 UnityPy 绑了未修补的 crate。这是错的——那些贴图是 LDR，而 vendored 的舍入修复在 HDR 路径上；而且 UnityPy 解 ASTC 用的根本不是那个 crate，是 `astc_encoder`（ARM 参考实现，`USE_DECODE_UNORM8`）。测量本身站得住，且说明了更好的一件事：本项目的 LDR ASTC 解码与托管原生解码器逐字节相同，同时与 ARM 参考实现每通道相差不超过 1。新增 `tools/unitypy_texture_diff.py` 把这条界限变成可检查的：不需要解码的格式必须逐字节相同，ASTC 最多差 1（含 alpha——ASTC 块四个通道走同一套端点运算，`ASTC_RGB_*` 也可能带变化的 alpha），其余一律报出；把某个 ASTC 通道挪 3 会被抓。

**用 88 个独立 agent 对着仓库审计了一遍七条完成判定（2026-08-15）**：不看本文的自述、只看代码与测试，每条「缺口」还要另一个 agent 去反驳，反驳不掉才留下。结果 52 条留存（21 条 material、31 条 minor、0 条 blocking），两条判定为满足、一条未满足（跨平台发布）、六条部分满足。当场按结论修掉的：

1. **packed float 的浮点运算与托管不一致**。差分里的压缩网格一直用 `bit_size=8, range=255`，量化 scale 恰好 1.0、offset 恰好 0，反量化退化成恒等式——把 offset 删掉都看不出来。改成 12 位、range=100、offset=-25 之后差分立刻挂：托管是 `x / ((1/range) * (2^bits-1)) + start`，三次舍入；本项目算一个 f64 scale 再用 `mul_add` 融合，只舍一次。同一套算术、不同的比特，凡是 range 不等于量化上限的压缩网格与打包动画曲线都会差。现按托管的运算顺序逐步复现；去掉打平那一趟、或把 offset 删掉，都会被抓。
2. **ATC 解码是上游的第三个缺陷，而且是最严重的一个**。审计指出 `ATC_RGB4`/`ATC_RGBA8` 连一个测试都没有；一加进托管差分就挂了。`texture2ddecoder` 的备用模式调色板本应是 `max(0, c0 - c1/4)`，上游把除法用在了差上、减法用 `overflowing_sub`、再对无符号数做 `max(0, _)`（永远不会 clamp）——只要某个通道 c0 < c1 就회绕成 65530 左右，取低字节。这不是舍入：通道能偏 200/255。判定过程不是靠猜：fixture 里 mode=0 的块两边本来就逐字节相同，mode=1 的两个块只在选中那个调色板项的 texel 上不同，而托管的值恰好等于 `max(0, c0 - c1/4)`、本项目的值恰好等于回绕表达式——所以托管是对的。现已把 ATC 解码器也 vendored 进来并修正那一处（`ATC_RGBA8` 还需要 BC3 的 alpha 块，crate 没导出，一并原样 vendored），两个格式现在与托管逐字节一致。
3. **三条 ADPCM 差分的输入是退化的**。IMA 块每个字节都是 `0x10`，16 个 nibble 只用到 0 和 1，符号位从没置过，step-index 表只走到最前面几项；DSP 每帧头都是 0（predictor 恒为第 0 对、scale 恒为 0），而且每声道 16 个系数里有 15 个是 0。现已覆盖全部 nibble、逐声道变化帧头、16 个系数各不相同；解码器仍与 vgmstream 一致，而这次这句话有意义了：把 IMA 的符号位忽略掉、或改坏 step-index 表的一项，都会被抓，而用旧的块前者能过。
4. **README 把纹理差分说大了**。原文写「所有列出的格式都与托管解码器逐字节一致」，但 DXT1/DXT5 是**刻意**偏离（同一段前面三十个字就写着），DXT3 托管侧根本没有解码器。两处出现这句话的地方都已改成如实表述。
5. **2021+ shader 的容错过宽**。实现 2021+ 之后，`Err(_)` 把 2022+ 上的**任何**错误都吞掉并在导出文本里一律归因于「2022 布局变了」——预算超限、记录损坏、未知 GPU program type、未知记录版本全被说成同一件事，而后两者是本项目有意声明的拒绝。现在只吞解析失配（含读到记录末尾之外，那是同一件事从读取端看的样子，且记录是内存切片、不可能是文件系统故障），声明性的拒绝仍然让整个 shader 失败，导出文本里写的是它**实际**失败的原因。

这份 material 清单已在同日晚些时候逐条处理（见上文「本轮进展」）：Node 的加载选项、Live2D 三份文档与 diagnostics、三个交付面的模型导出、两处线性搜索、SpriteAtlas 与 Texture2DArray 的托管差分均已完成。剩下两条，各有各的原因：

- **场景与 FBX 缺差分，而且比原先以为的更难补**。原来的判断是"导出走原生库，但它上面的 `ModelConverter` 是普通托管代码，可以拿来当 oracle"——2026-08-15 晚实际试了一遍，不成立：`ModelConverter` 的构造函数里就会调 `Fbx.QuaternionToEuler`，而那是 `AssetStudioFBXWrapper/Fbx.PInvoke.cs` 里对 `AssetStudioFBXNative` 的 P/Invoke。也就是说连"四元数转欧拉角"都在原生库里，托管侧的模型转换整条路径都拿不到（本机跑起来直接 `dlopen` 失败）。要补这条，要么把那个原生库连同它的构建一起接进 oracle（跨平台代价与授权都得先想清楚），要么换一个第二实现——UnityPy 没有 FBX 导出，所以也不是现成的。本项目这一侧的证据仍然是 `scene_hierarchy`/`model_ir`/`fbx_scene_ascii` 的单测加真实语料能导出，这比"有差分"弱，如实记在这里。
- **UnityCN 缺差分，而且短期内补不了**。托管 AssetStudio 只检测不解密（`BundleFile.cs` 直接抛 `NotSupportedException`），本机这份 UnityPy 也没有 UnityCN，两份语料都不是 UnityCN 加密的——也就是说没有第二个实现可比。当前证据分两层：AES-128 本身由 FIPS-197 向量验证，这是真正的外部 oracle；它上面的 token/counter/掩码层只有往返测试，而那个测试里的"加密"是测试自己写的，与解密器共享同一份理解——两边一起错就一起过。这一层现在只能算"结构上被检查过"，不能算"被验证过"，要真的验证需要一个 UnityCN 加密的真实 bundle 和它的密钥。

**真实 AssetBundle 也接上了，并抓到一个更大的缺口（2026-08-15）**：上面那份是播放器目录（`.assets`），不是 AssetBundle；`~/Downloads` 里有四个真实的 PJSK bundle，一跑就发现 **UnityFS v8 被整个拒掉了**——本项目只认 v6/v7，而 v8 正是当代 Unity 写出来的版本，也就是说现在任何一款新游戏的 bundle 都打不开。查下来 v8 根本不是新格式：它的 header 与 blocks info 就是 v7 的，托管侧压根不区分（只判 `>= 7`），本项目在闸门以下也早就是同样的写法；拿真实 v8 header 按 v7 布局解，declared size 与文件大小逐字节吻合。现已接受 v8（更高版本仍然拒绝而不是假定兼容），并且用两个 oracle 分别验证：四个真实 bundle 的对象数与 class 分布与 UnityPy 完全一致（101/23/4/26，逐类相同），容器差分也加了一条 v8 用例交给托管侧比对。另外两个 bundle 的 header 版本被抹成 `5.x.x`，需要 `--unity-version`——corpus manifest 因此加了 `unity_version` 字段（CLI 一直有这个选项，只是清单没法表达）。四个 bundle 现在都能完整导出（101/23/4/26 全部成功、0 失败），其中的贴图是真实的 ASTC_RGB_6x6，导出的 PNG 用独立解码器逐个验过。`extract` 那条路径也在真实 bundle 上跑通并做了闭环：解出 CAB 与同名 `.resS` 之后，直接读解出来的 CAB 得到的 22 张 PNG 与直接读 bundle 得到的逐字节相同——这同时也验证了「单文件输入要连带加载同名伴随文件」那个修复。顺带拿到一条对上游缺陷的实证：那张 600×576 的 ASTC_RGB_6x6 贴图，本项目解出来的 1,382,400 字节与托管原生解码器逐字节一致（FNV `c6687283ffa9acde`），而绑定未修补 crate 的 UnityPy 与两者都不同；二十张 sprite 的差异全部是 R/G/B 恰好差 1、alpha 一字节不差——正是「该四舍五入的地方做了截断」的形状，已写进 `docs/upstream-defects.md`。

**首次跑真实 corpus 抓到的四个缺陷（2026-08-15）**：这道门一直没跑过——每次的结论都是「没有游戏文件」，而本机 Desktop 上一直躺着一份完整的 2022.3 播放器目录。指上去之后，一个接一个地挡住了后面所有文件：

1. **Shader 2021+ 在解析出垃圾**。Unity 2021 改了序列化布局，托管侧根本没跟进——它的对象表对 `version >= 2021` 直接跳过 class 48，所以 AssetStudio 导一个 2022 游戏是「一个 shader 都没有」而不是「有一堆坏的」。本项目却照 2021 前的布局硬解：游戏里 372 个 shader 全部在头几个字段内就跑偏，然后把后续字节当结构读——9888 字节的对象报出 1869762655 这种个数，那是四个字节的 shader 源码被当成长度。根因也定位了：真实的 `SerializedProgram` 有五个字段，托管的定义只有三个，缺 `m_PlayerSubPrograms` 与 `m_ParameterBlobIndices`（用 UnityPy 的 TypeTree 对出来的）。**当天晚些时候把它实现掉了**，而不是停在拒绝：缺的字段一共三个，版本门全部取自序列化布局本身（UnityPy 的 TypeTree 数据库，只在开发期查阅，不入库）——`stageCounts`（2022.2 起，位于压缩 blob 与依赖表之间；这也解释了为什么连一个 pass 都没有的 shader 也会失败：它上面的字段都不依赖 pass 数，那四个字节被当成依赖表长度读了）、`m_PlayerSubPrograms` 与 `m_ParameterBlobIndices`（2022.2 起，两个嵌套 vector，夹在 sub-programs 与 common parameters 之间）、以及 Unity 6 追加的 `m_AssetGUID`（四个 u32）。现在 372 个真实 shader 全部解析成功，且导出内容经过核对而不是假定：`unity_builtin_extra` 里 46 个 shader 的导出名与 pass 数与 UnityPy 从同一对象读出来的逐一相符；随手挑一个出来是 `Legacy Shaders/VertexLit`，五个属性、LOD、RenderType、LIGHTMODE 标签、Fog 模式、GpuProgramID 一应俱全。blob 里的**编译产物记录**是另一回事：Unity 2022 在保留 202012090 版本戳的前提下改了它的形状，两个实现都没有跟进，而本机手上的样本又全是被剥离的桩，照着猜等于拿没有对照的东西去验证自己——所以解不动的记录不再让整个 shader 失败，而是保留 shader 结构、并在本该是程序清单的位置写明"这条没解出来"以及它实际失败的原因；2022 以下的解不动仍然是硬错误，因为那里的布局是已知的。fixture 也补上了这三个字段——此前那份正是「照着与 reader 相同的假设造出来、Unity 不会写」的文件，这也是三个字段长期没被发现的原因。
2. **Texture2D 的零尺寸被当成损坏**。Unity 的动态字体图集就是 0×0、`m_ImageCount` 为 0，运行时再填——这份游戏里 810 个纹理有 12 个是这样，UnityPy 全部能读。现在解析接受，解码仍然拒绝（该拒的地方拒）。
3. **AnimatorController 的 16 字节尾巴**。托管读完 animation clips 就停手，本项目照抄了字段但又额外要求「对象必须被完整消费」，于是游戏里 212 个控制器全部被拒——而它们无一例外正好多 16 字节：两个空集合、一个空 behaviour 向量、一个线程标志加对齐。现在把这段读掉（严格性因此仍然有效），非空集合报 unsupported 而不是猜元素布局。四处 fixture 构造器一并补上这段——它们此前造的是 Unity 不会写出来的文件。
4. **导出把「不支持」算成「失败」**。一个 2022 游戏有几百个 shader 读不了，全算进 failures 的话，每次导出都以非零码退出，调用方分不清「这版实现不认这些格式」和「导出真的坏了」。`ExportReport` 因此多了一类 `unsupported`，Core/CLI/Node/Python 四个面都分开报，CLI 还会逐条列出拒绝了什么、为什么。

也就是说，剩下的两条恰好就是上面「需要外部输入」里的前两项。在达到这些条件前，项目可以作为 Beta 使用，但不应把「测试通过」误写成「所有 Unity 游戏都已兼容」。

## 维护规则

每次更新本文件时：

- 更新顶部日期；
- 只把有代码和验证证据的能力移入“已完成”；
- 发现新格式时先记录样本来源、版本门和预期行为；
- 不用缩小目标或删除失败 fixture 的方式提高完成度；
- GUI 和旧 C ABI 始终列为非目标，而不是待办缺口。
