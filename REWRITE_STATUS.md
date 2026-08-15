# AssetStudio Rust 重写进度与缺口

最后更新：2026-08-15

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
| Node.js | 可选 Beta | 同步与 Promise worker API 已覆盖加载、枚举及主要读取路径；保留 `scene(maximumGameObjects?)`，并通过 `sceneWithLimits` 暴露与 Core/Python 相同的六类场景预算；加载选项（Unity 版本、UnityCN 密钥、失败容忍策略、输入上限）可组合传入，Live2D 包连同 diagnostics 一起返回，并可直接读取 pose/display-info、标准与调用方解码的 Tuanjie ACL AnimationClip motion，模型导出（场景 OBJ 与带贴图的 FBX）与 Python 等价 |
| .NET 运行时退役 | 尚未完成 | 公开 Rust/Python/Node 不依赖 .NET；C# 仍用于差分 oracle 和格式核验 |

这些状态是风险分级，不是按代码行数计算的完成百分比。

## 已完成能力

### 输入、容器和资源

- SerializedFile v5-v22，支持大小端、32/64 位 PathID、TypeTree、外部引用和有界对象区间；
- UnityFS v6-v8，支持 None、LZ4/LZ4HC、LZMA、Zstd；
- UnityWeb/UnityRaw v1-v6、UnityWebData/TuanjieWebData；
- gzip、Brotli、ZIP Stored/Deflate；
- `.split0`...`.splitN` 惰性拼接、递归容器发现、外部资源和跨文件 PPtr；
- UnityCN 加密包检测与调用方密钥解密（无密钥时明确拒绝），以及 UnityArchive 签名识别后的明确拒绝；
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
- 通用层级 ASCII/Binary FBX 7.4，覆盖普通/蒙皮网格、材质与贴图、骨骼、静态 blend shape 和已验证动画采样；
- Live2D MOC、model3、纹理 PNG、expression、motion、physics、pose、display-info 和参数组；
- 有界递归解包，以及拒绝符号链接、同目录临时文件和原子发布的安全导出。

### 公开 API 和发布

- Rust 高层 `Studio` API 可直接从路径、单个内存区域或多文件内存集合加载；
- Python 提供惰性/分页枚举、资源读取、主要专用 reader、schema/ACL/Oodle 适配器、导出和解包；`SceneLimits` 可独立收紧 GameObject、组件、Transform 子项、材质、骨骼和层级边预算；Core 的 I/O、无效数据和未支持功能分别保留为标准 `OSError` 子类、`ValueError` 和 `NotImplementedError`，所有 Rust→Python 字节复制以及可能很大的场景、候选、图片层和报告转换都使用可失败分配，并以 `MemoryError` 报告内存不足；
- Python wheel 使用 `cp39-abi3`，CI 构建 Linux、Windows、macOS 的 x86-64/ARM64 组合，并在构建解释器和 Python 3.14 上安装测试；
- Python sdist 会被重新构建成 wheel 并执行完整 API 测试；
- Node 提供本地模块、TypeScript 声明、同步 API 和 libuv worker Promise API；旧的 `scene(maximumGameObjects?)` 调用保持兼容，`sceneWithLimits` 可分别限制 GameObject、组件、Transform 子项、材质、骨骼和层级边；
- Rust Core crate、Python wheel/sdist、Node 包和原生 CLI 都有独立发布校验。
- 四类发布产物都携带项目 `LICENSE`、第三方归属摘要和由锁定依赖图生成的完整许可证文本；生成器当前覆盖 97 个非开发依赖，遇到缺少许可证文件、依赖更新或分发副本漂移会直接失败。
- 交付范围由 `tools/check_delivery_scope.py` 与各产物内容检查共同锁定：workspace 只能包含 Core、CLI、Python 和可选 Node，三个前端必须直接依赖 Core；发布 crate、wheel/sdist 与 npm 包拒绝 GUI、旧 `assetstudio-ffi` 和 C# 工程文件。托管 oracle 仍可作为仓库测试输入存在，但不会进入运行时依赖或二进制交付面。

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
- **CI 的常规作业步骤已于 2026-08-15 在本机完整复跑一遍**（`cargo fmt --check`、`clippy -D warnings`、`cargo doc -D warnings`、`cargo package -p assetstudio-core`、workspace 测试、Node 的 build/test/pack、Python 的 release wheel 与 sdist 两条路各跑 `installed_wheel.py` 与 `python_api.py`、UnityPy 差分、托管差分、vgmstream 音频差分），全部通过；当前主机上的 release CLI 与 Node tarball 也按发布作业的命令构建、直接执行/测试并检查了包内容。工作流另经锁定的 `actionlint` 1.7.12 验证，六个平台使用的 runner label 也逐项对过 [GitHub 当前官方清单](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。六平台发布矩阵本身尚未在 GitHub runner 上跑过，无法在本机复现的仍是 Windows 运行及其他架构的 runner 环境。之所以要手工跑一遍：CI 自 LZMA 那次提交起就没跑过，而这一跑就发现 Python 侧其实已经坏了一段时间（见上面 sprite fixture 那条）。
- **Linux 双架构交付面已于 2026-08-15 实跑**：`python3 tools/local_ci.py linux` 在 `linux/amd64` 与 `linux/arm64` 容器中分别完成 Core+CLI 全量测试、release CLI 构建/执行/法律文件 staging、`cp39-abi3` Python wheel 的 release 构建/安装/公开 API 测试，以及 Node 24 release addon 的构建/加载、JavaScript 与严格 TypeScript 消费测试、包内容检查和真实 npm tarball 构建。Node 在只读源码的干净临时副本中打包，宿主机已有的其他平台 addon 不可能混入产物；六个当前步骤均已实跑通过。该结果证明当前工作树的 Linux x86-64/ARM64 运行与发布产物面，不替代仍待 GitHub runner 验证的 Windows/macOS 六路发布矩阵。
- **发布法律文件已按实际产物验证（2026-08-15）**：根目录补齐项目 MIT `LICENSE`，`tools/generate_dependency_licenses.py` 从 `Cargo.lock` 与 `cargo metadata` 的非开发依赖闭包生成 97 个依赖的许可证全文聚合，并同步 Core、Python、Node 的分发副本。Core `.crate`、实际 npm tarball、Python wheel、Python sdist 及由 sdist 重建的 wheel、release CLI staging 目录均检查三份法律文件存在且与根副本逐字节一致；门禁第一次运行就抓到了 Core 内陈旧的 NOTICE，而不是只确认“有同名文件”。同轮还发现并修复 Python 3.9 测试文件使用 `int | str` 却未延迟求值的支持下限回归；3.9 wheel/API 已重新通过。
- **输出格式的独立校验已建立（2026-08-15）**：本项目产出的几种文件此前只有自己校自己——binary FBX 的读写是一起写的（记录头里存的是绝对结束偏移，写入端事后回填，读取端若与写入端理解一致，错的偏移一样会被接受），ASCII FBX 根本没有 oracle（托管走 FBX SDK），WAV 头由本项目写、又由本项目的 helper 读回去比对采样；PNG 的 chunk 分帧、CRC、IHDR 与逐行 filter 全是本项目手写的，而它的单元测试用本项目自己的 `png_crc32` 去核对 CRC——CRC 表要是错了，测试照样过，每个看图软件都打不开。现在四者都有了照格式另写的第二实现：`tools/validate_fbx_binary.py`（magic、版本、每条记录的结束偏移、属性数与属性区长度、嵌套列表的 null 记录、footer 的 id/对齐/重复版本/结尾 magic）、`tools/validate_fbx_ascii.py`（括号配平、必备段、`Definitions` 声明的 Count 与实际对象数、`C:` 连线引用的 id 是否存在、`*N` 数组的值个数）、`tools/validate_wav_output.py`（直接交给 Python 标准库的 `wave` 模块读，它不是为本项目写的，字段自相矛盾就会拒绝）、`tools/validate_png_output.py`（用标准库 `zlib` 提供独立的 CRC-32 与 inflate：逐 chunk 核对 CRC、校验 IHDR 为 8 位色彩类型 6 且非隔行、解压 IDAT、还原 0–4 号 filter、要求 IHDR 在首 IEND 在尾且其后无残留，最后把像素与源数据逐字节比对，包含 Unity 自下而上到 PNG 自上而下的翻行）。另外 `json.rs`（TypeTree dump 的 JSON emitter）也是手写分帧，而它原有的测试同样是拿手写字符串对比——两边同一套理解，天然一致，且覆盖的形状都很浅。现补了随机树的往返测试：确定性 xorshift 生成任意 `TypeValue` 树，pretty 与 compact 两种形式都交给 serde_json 的 parser（与 writer 不共享任何代码）解析，再与本模块声明的映射逐值比对；对象键序不比（解析后的对象无序），那部分仍由原有手写测试覆盖。末尾的覆盖断言不是装饰：十一个 variant、空容器、容器套容器、非有限浮点、未配对代理项各至少出现一次，把生成器限制成只出叶子会让 variant 断言失败。五种人为破坏均被抓住（裸 `NaN`、对象字段间少逗号、typeless 元数据少逗号、map 的 key/value 互换、float 加宽成 f64），其中前三种只有真正的 parser 才看得见。BMP 与 TGA 同理：两者都是本项目手写的二进制布局，而且是那种「写错了也照样能看」的布局——32 位 BMP 的通道掩码写错、TGA 的 descriptor 把行序写反，任何按惯例假定 BGRA/top-down 的读取器都会照样显示正确的图，直到遇上一个真按文件头来读的读取器；file size、pixel offset、image size 这类冗余字段更是错了也不影响解码。`tools/validate_image_output.py` 按两个格式的规则解码：行序取自文件头而不是假定，BMP 的通道排列取自文件声明的掩码而不是假定 BGRA，再与源像素逐字节比对。六种人为破坏均被抓住（BMP 红蓝掩码互换、BMP 高度写成正数即声称 bottom-up、BMP file size 少算文件头、BMP image size 清零、TGA descriptor 声称 bottom-up、行数据按 RGBA 而非 BGRA 写）。五者都接进了 `tools/local_ci.py` 的 `outputs` 组，并且都反向验证过：人为改坏各自检查的那一项都会被准确指出（PNG 这条试了改 IDAT 的 CRC 与截断 IEND 两种，分别报出 CRC 不符的具体两个值、和 chunk 声明的长度越过文件末尾）。
  值得记一笔的是，写这三个校验器的过程中有两次是**校验器自己错了**——binary FBX 的 footer 漏了版本前的 4 个零字节，WAV 那边把 legacy AudioClip 的第一个字段当成了声道数（实际是 `format`，声道数是 `format >> 1`）。这恰恰是第二实现的价值：分歧会逼人去对格式或对读取端的实现，而不是让代码自我确认。
- **畸形输入扫描已建立（2026-08-15）**：`crates/assetstudio-core/tests/malformed_input.rs`。"不可信输入不崩溃"此前只有各模块零散的拒绝用例背书——那些覆盖的是有人想到的情况。现在拿有效输入批量损坏（单比特翻转、截断、把长度字段改成 `0xFFFFFFFF`/`0x80000000` 一类的毒值），每个结果只允许是解析成功或 `Err`，panic 即失败并报出确切的偏移量以便复现。两处防自欺的断言：种子本身必须能解析（否则损坏的是本来就坏的东西，全是空洞的 Err），以及损坏后仍能解析的比例必须够高（否则全被文件头挡掉，根本进不到对象表）。另有三条把损坏的数据送到真正的解码器：一条把真实压缩载荷（ASTC LDR/HDR、BC6H、Crunch）包进 Texture2D 再损坏后解码——第一版直接喂原始载荷，而那些不是序列化文件，reader 在嗅探阶段就退掉了，解码器一次都没跑到，正是这份文件要防的那种测试。这条验证的是外部解码器的 catch_unwind 护栏确实生效（已反向验证：把护栏外面插一个 panic，扫描立刻失败）。另一条损坏真实 FSB5 音频后走 `detect_direct_wav`/`write_direct_wav`，覆盖 codec 分发与 Vorbis 解码——Vorbis 尤其值得测，它的 setup header 是从表里重建的而不是从流里读的，损坏的流可以配上一个解析得干干净净的 setup。第三条损坏 MOC3 头：那是本项目里唯一一处由载荷自己决定 reader 下一步去哪儿看的地方（四个表偏移在固定位置，然后按定宽切标识符记录），因此偏移或计数被改就是直接的越界邀请，翻转位置也刻意偏向头部。三条都要求"损坏后仍有成功解出的样本"，否则说明解码器压根没跑到。
- **收口后的完整本机复跑（2026-08-15）**：把工作树整理成 13 个提交之后，在最终提交的树上跑了 `tools/local_ci.py` 的**全部 12 组**（quality、rust、cli-package、oracle、audio、outputs、cross、linux、node、python、python314、typing、unitypy），**全部通过、零跳过**——也就是说 .NET 托管差分、vgmstream 音频差分、UnityPy 第三实现差分、输出格式的独立校验器、Linux amd64/arm64 容器里的运行与发布产物、Python 3.14 abi3 前向兼容、严格 mypy 消费端这些可选组这一轮都真的执行了，而不是因为缺工具被记成跳过。另外这批提交里**带源码的那 12 个**逐个在独立 worktree 里用 `cargo check --workspace --all-targets --locked` 验证过能单独编译，没有哪个要靠后面的提交才能构建；其余两个只动 `.gitattributes` 与 Markdown，不进构建。
- **Python 门禁在断言被关掉时会整片变绿（2026-08-15）**：收口当前工作树时查出的一条自欺路径，而且正好压在刚补上的法律文件门禁上。仓库里当门禁跑的 Python 脚本——`check_delivery_scope.py`、`check_core_package.py`、`test_monoschema.py`、`sdist_contents.py`、`installed_wheel.py`、`python_api.py`——判定全部是裸 `assert`，而 `-O` 与 `PYTHONOPTIMIZE` 是把 assert 整条从字节码里删掉，不是跳过。实测而不是推演：造一个只带 README、三份法律文件和全部必需源码一个都没有的 sdist，`PYTHONOPTIMIZE=1 python3 tests/sdist_contents.py` 退出码 0、一个字都不说；正常模式下它会把缺的十二项逐个列出来。暴露面是具体的：CI 里 `python3 tools/*.py` 和 `python tests/sdist_contents.py` 这几步都不带 `-I`，环境变量能直接生效；带 `-I` 的那几步隐含 `-E`，只有命令行 `-O` 能触发。六个门禁与 `local_ci.py` 现在都在模块层拒绝在断言关闭时运行，`local_ci.py` 那道同时挡住它内嵌的四个 `-c` 断言和它派生的全部子进程（子进程继承同一份环境）。七处都反向验证过：`-O`/`PYTHONOPTIMIZE=1` 下一律以退出码 1 拒绝并说明原因，正常模式下逐个照常通过。
- **顺带补上一处没有守卫的比较（2026-08-15）**：`unitypy_oracle.py` 的 `assert_double_precision` 对 15 个 fixture 每个都跑一遍，而只有一个 fixture 带那个 `Double` 字段——也就是说它今天确实比了一个值，但没有任何东西保证它明天还在比。旁边的 TypeTree 行早就有"比较了多少棵树、为零即失败"的计数器，这一条没有。现在按同样的形状把计数汇总到 `main`，全程为零即失败，摘要里也报出实际比了几个。反向验证：把字段名换成一个不存在的键（模拟 fixture 哪天不再携带它），差分立刻以"no double field was compared"失败，而在此之前那样改是全绿的。
- Core 497 项普通测试通过，10 项依赖可选 vgmstream oracle 的测试在本机额外执行并全部通过（其中 1 项钉住的是已记录的上游 Opus 偏离，不是一致性）；
- C#→Rust 托管差分 oracle 通过；
- TypeTree dump 浮点文本对照 .NET 10 实测生成的 849 个取值（边界值 + 位模式扫描）逐字节一致，期望值以 fixture 形式入库；
- `cargo doc --workspace --no-deps` 在 `-D warnings` 下通过；
- `cargo package -p assetstudio-core` 的包内容和独立重建通过；
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
仍会全绿。现已统一为 512 MiB，并把所有 `AssetStudio` 签名默认值展开成
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
   - **容器、Live2D 与 Shader 差分状态**：UnityWebData（Unity/Tuanjie 两种签名）、ZIP、split 组、LZMA 块和 Live2D 整包均已补齐；LZMA 编码器来自仅开发期启用的 `lzma-rust2/encoder` feature，发布 crate 仍然只解压。Shader 5.3-5.4 的 subprogram blob 也已补齐；5.5+ 序列化程序无法接入托管 oracle，原因已查实：托管仓库 `AssetStudioUtility/ShaderConverter.cs` 的 `HeaderBytes`（第 15 行）用第 893 行才声明的 `header` 初始化，按 C# 静态初始化顺序必然得到 null 并使类型初始化抛 `ArgumentNullException`。5.5+ 所需转换方法又是该类型的私有静态方法，反射同样触发故障；在 oracle 里重写它只会变成另一份自实现，不能构成独立证据。上游修复只需把 `header` 改为 `const`，或移到 `HeaderBytes` 之前。
   - oracle harness 接受任意输入路径，上述补强全部不需要专有样本。
   - **第二 oracle 已就位，并于 2026-08-15 扩到 TypeTree 值**：`crates/assetstudio-python/tests/unitypy_oracle.py` 用 UnityPy（独立实现，不需要 .NET）对照对象顺序、PathID、classID、字节大小、名称和原始载荷哈希，14 个 fixture 首轮全对。新增第 15 个 fixture：一个自带 TypeTree 的 MonoBehaviour，专门覆盖 reader 容易出错的形状——单字节字段后的对齐、长度前缀字符串、基本类型数组、以及元素里同时含字符串和字节的结构体数组（对齐放错在这里会直接读出垃圾而不是读错一个数）。此前 TypeTree 解析只有托管一个 oracle 背书，现在有了第二份独立解析。两处刻意的处理：只比较文件自带树的对象（UnityPy 在没有内嵌树时会回落到它自带的数据库，那样比的是数据库而不是同一份字节的第二次解析），以及浮点两边都收窄到 f32 再比（UnityPy 的 Python float 是 double，会把 `0.8f` 变成 `0.800000011920929`）——收窄会让真正的 double 字段掩盖掉低位差异，因此 fixture 里放了一个 f32 表示不出来的 double 并单独直读断言。运行时统计真正比较了多少棵树，为零直接失败，避免两边都是 None 的假通过。
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
   - **binary FBX 编码层已落地（2026-08-14）**：`fbx_binary.rs` 提供 FBX 7.4 的节点树、属性类型与字节布局，写出与读回两条路径。写出走的是记录头里的绝对 end offset，因此每条记录先编码体再回填头；数组属性到阈值才 deflate，小数组压了反而更大。7.5+ 的 64 位 offset 明确拒绝而不是截断。读回的解析器是照格式写的、不共享写出侧代码，所以两者互为对照——但要说清楚它证明了什么：它证明编码自洽，不证明 FBX SDK 会接受。2026-08-15 补了一层独立校验：`tools/validate_fbx_binary.py` 是照格式规则另写的解析器（不看本项目的读取代码），会检查 23 字节 magic、版本字、每条记录头里的绝对结束偏移是否正好落在记录末尾、属性数与属性区长度是否与实际属性吻合、嵌套列表的 null 记录、以及 footer 的 id/对齐填充/重复版本/结尾 magic；`--cli` 模式会自己造模型、走 CLI 导出再校验，已接进 `tools/local_ci.py` 的 `fbx` 组。写这个校验器时它一上来就报 footer 不对，查下来是**校验器**漏了版本前面那 4 个零字节、写入端是对的——独立实现的价值正在于此：分歧会逼人去对格式，而不是对着代码自我确认。反向也验证过：改坏任一条记录的结束偏移或结尾 magic，校验器都会准确指出。ASCII 那条也补了同样性质的校验（`tools/validate_fbx_ascii.py`）：括号配平、7.4 必备的几个段是否齐全、`Definitions` 里每个 `ObjectType` 声明的 Count 是否与 `Objects` 里实际写出的对象数一致、每条 `C:` 连线引用的 id 是否真的存在（root 的 0 除外）、以及 `*N { a: ... }` 的值个数是否等于 N。这四条都是导入器会依赖、而写入端自己的测试不会注意到的东西；四种人为破坏都能被准确指出。本机没有 Blender/assimp/FBX SDK，因此导入器接受度这一条仍然只能由你那边验证。真正的差分做不了，因为托管侧是通过 FBX SDK 产出二进制的，字节结构本就不同。
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
   - multistream MPEG/Opus 和少数平台音频 codec 仍保留原始数据；
   - 8 个音频差分此前虽然写好却从未跑过（全部 `#[ignore]`，CI 也没有对应 job），现已加 `audio-oracle` job：按固定 release 拉 `vgmstream-cli` 再跑 `--ignored`，8 条首次执行即全部通过；
   - **MPEG/Opus 的全零 fixture 已换成真实音频（2026-08-15）**：此前两边比的都是静音，解码器无论怎么处理比特两边都会一致地得到零，等于只验证了分帧。现在各自嵌入一段真实编码的正弦——MPEG 是 6 帧 MP3，Opus 是 libopus 编的 6 个包（FSB5 的 MPEG 帧按 4 字节对齐、Opus 包带 u16 长度前缀并以零长度收尾，这两条框架细节也因此第一次被真实数据验证）。MPEG 换成有内容之后仍然对得上，容差 1（实测得来，不是猜的）。
   - **Opus 偏离已定位到上游 `ruopus` 0.1.2 的 SILK 路径（2026-08-15）**：换成有内容 fixture 后先暴露出偏离，再把它从本项目的 FSB5 代码里摘出来复现——直接把同一批包喂给解码器、按流自己声明的 pre-skip 裁掉前导，不经过本项目任何一行代码，偏离照旧；而 `ffmpeg` 与 `vgmstream` 两个 libopus 实现彼此差 1 以内。分模式实测（峰值都在 4200 附近）：

     | 包模式 | 偏移 | 最大差 |
     |---|---|---|
     | CELT-only | 0 | 1 |
     | SILK/hybrid | -2 | 103 |
     | SILK 宽带 | -2 | 135 |
     | SILK 窄带 | -4 | 115 |

     CELT 路径是准确的，偏差只出在 SILK；偏移量还随 SILK 的内部采样率变化（窄带 8 kHz 偏 4 个、宽带 16 kHz 偏 2 个，折算下来是同一个固定的内部采样分数），这正是重采样器延迟补偿差一点点的形态。因此现在拆成两个差分：`fsb5_opus_celt_tone_matches_vgmstream` 要求 CELT 对齐精确、幅度差不超过 1（实测值）；`fsb5_opus_silk_tone_divergence_from_libopus_is_bounded` 把 SILK fixture 的实测值 276 钉住（**这不是容差，是对已知缺陷的记录**），超过就失败。2026-08-15 又用同一组真实包、同一个 312-sample pre-skip 和同一个 `vgmstream` oracle 评估了独立纯 Rust 候选 `opus-rs`：0.1.26 与当前 0.1.28 都把 SILK 从 `shift=-2/worst=276` 变成 `-5/164`，同时把本来正确的 CELT 从 `0/1` 退化成 `0/36`，因此不能替换；精确步骤和版本记录在 `docs/upstream-defects.md`。目前已知能通过现有 oracle 的替代仍只有 libopus FFI，它与纯 Rust 取向冲突，先记录不动。全零 fixture 把这一切都盖住了；
   - **音频 fixture 的生成过程已补上（2026-08-15）**：`tools/generate_audio_fixtures.py` 用记录在案的 ffmpeg/lame 命令重新生成三个编码 fixture，两次运行字节一致，连此前只写了文字描述的 MP3 也逐字节复现出来——原来的 Opus fixture 只有“从一次 ffmpeg 编码里提取”这句话，没有命令，等于是不可复现的二进制块，而这正是我一直在别处挑的毛病；
   - 新增 codec 必须先有真实样本和独立 oracle，不能只凭推测实现。

3. **MonoBehaviour schema 来源**
   - 内嵌 TypeTree、调用方提供的可信完整 schema、以及生成器写出的 JSON 文档均已支持，CLI/Core/Node/Python 四个面都可达；
   - 从 managed assembly/dummy DLL 生成 schema 仍是独立的离线可信工具（`tools/monoschema`），不会在解析进程中加载或执行 DLL；
   - 生成器现在默认给每个条目写入精确 `unity_version`，避免一份布局静默套到另一引擎版本；只有显式 `--unversioned` 才生成跨版本 fallback，并有真实临时 DLL 的端到端门禁。仍存在的已知分歧是重建树的字段命名（枚举、`UnityEngine.Rect` 等引擎结构），详见 `docs/mono-schema.md`。

4. **Node 专用 reader 完整度**
   - Node 公开面已覆盖主要 Python reader：读取面包括 `readAudio`、`readMonoScript`、`readMaterial`、`readBuildSettings`、`readPlayerSettings`、`readAvatar`、`readAnimationClipInfo`、`readAnimatorController`、`readAclTracks`（只读 ACL 头，够调用方判断自己的 decoder 能不能处理）、`readMonoBehaviourJsonWithSchemas`（用调用方提供的可信 schema 还原被剥掉的托管字段；schema 是纯数据，查找过程不执行任何资产控制的代码）、`readResourceRange`、`resourceIndexByPath`、`scene` 和 Cubism 单对象 reader；输出面包括 `readStaticFbx`、`readFbx`（含动画）、`readFbxWithTextures`（贴图随 FBX 一起返回，由调用方决定写哪）、`export`、静态 `extract`、`live2DPackages`、`readLive2DPackages`；加载面包括 `openWithVersion`、`fromBuffers` 与 `openWithOodle`。Material 属性值刻意只给名字不给值：它们按表分类型，硬摊平到 JS 只会丢信息。
   - Core 侧同时补上 `Studio::write_fbx_with_textures`：此前贴图输出只有 CLI 走得到，库调用方拿不到。它返回贴图集合而不是自己写盘——这个方法只持有一个输出流，没有目录可以写同级文件，由调用方决定落在哪里。
   - Live2D 包发现与落盘、FBX 静态几何/动画/贴图均已接；
   - Oodle decoder 注入已接（`openWithOodle`，只提供异步形式：解码回调要在事件循环上跑而 worker 在等它，同步调用会把该跑回调的那条线程堵死）；ACL decoder 注入同时覆盖 `readFbxWithAclDecoder` 与单对象 `readCubismClipMotionWithAclDecoder`，同样只提供异步形式，回落的曲线由 Core 校验形状、顺序与预算，不信调用方的承诺；
   - napi-rs 生成的声明由严格 TypeScript 消费端编译把守；ACL 与 Oodle 回调签名从 Rust 参数注解生成，避免原生测试通过但 `.d.ts` 留下未声明内部类型；
   - Node 是可选交付面，因此优先级低于 Core 和 Python 的真实语料兼容。

5. **Live2D 散件发现**
   - MOC3 标识表已接入参数组（与托管一致：MOC 的表覆盖组件推导出的名字），仅有 MOC、缺少活动组件的包不再得到空参数组。与托管的一处有意偏离：托管是无条件覆盖，因此 MOC 版本不带标识表时连组件名也会被清空；这里只在 MOC 确实带表时覆盖；
   - **散件发现回退已补（2026-08-14）**：模型组件图走不到时，回落到同一个序列化文件里的独立 `CubismExpressionData`/`CubismFadeMotionData`/`CubismPhysicsController`。语义跟托管一致——只在图路线什么都没拿到时才回落，因为表达式顺序由 `CubismExpressionList` 定义，扫文件复现不出来。作用域取序列化文件（托管取 container group），这是本 reader 最接近的等价物，也能防止一个 bundle 里的散件挂到另一个 bundle 的模型上。动作的回落顺序是：fade controller 的列表 → 散件 fade motion → AnimationClip。

### 上游缺陷

两处依赖的输出都已证明与独立参考实现不符：纹理解码器已通过有许可证记录的 vendored 补丁在本仓库内修正，`ruopus` SILK 仍未解决。`docs/upstream-defects.md` 记录了测量数据、不依赖本项目的复现步骤和可提交上游的补丁。

- `texture2ddecoder` 0.1.2 的 `f32_to_u8` 有两份移植（ASTC 一份、BC6H 一份），都把参考实现的 `roundf` 丢了；影响 6 个 ASTC HDR 格式加 BC6H，每个受影响通道恒低一格。上游 master 至今未修，仓库通过 vendored ASTC/BC6H 解码器修正并由托管差分把守。
- `ruopus` 0.1.2 的 SILK 路径输出偏早且不精确（宽带早 2 个采样、窄带早 4 个，对齐后约差峰值的 3%），CELT 路径准确。

纹理那处已于 2026-08-15 决定采用 vendor 修复：`crates/assetstudio-core/src/vendor/texture2ddecoder/` 收入 ASTC 与 BC6H 解码器，只改两个舍入表达式（就地标 `VENDOR FIX`），并删除一段本项目不用、依赖 `paste` 的宏（标 `VENDOR DELTA`），其余保持可与上游直接 diff。18 个 ASTC 格式加 BC6H 现在与托管解码器**完全一致**，测试也由“钉住已知偏差”改为要求相等。Opus 未采用同一路线：独立纯 Rust 候选 `opus-rs` 0.1.26/0.1.28 已实测仍有更大的时序偏差并使 CELT 精度回退，剩余已知替代只有 libopus 绑定，会为仅 SILK 路径的偏差引入新的原生运行时依赖并终结该解码链的纯 Rust 性质，而当前 CELT 路径本身已经准确；因此现阶段保留明确记录与有界偏差测试，等待可审计的纯 Rust 修复或上游版本。

### 设计上保留的外部适配器

以下能力不应通过在 Core 中静态链接不明来源或专有二进制来“补齐”：

- Oodle：由用户提供有授权的 decoder，Core 只接受安全的精确输入/输出接口；
- 外部 MonoBehaviour schema：由可信离线工具生成，运行时只消费数据结构；
- 在内置 ACL 解码器完成前，Tuanjie ACL 可由调用方提供 decoder。

这些是明确的安全和授权边界，不等同于旧 C ABI。

## 下一步执行清单

下表是后续工作的执行入口，按顺序推进。只有“完成证据”真实存在时才勾掉，
不能用缩小目标、删除失败样本或把未验证格式改名为已支持来结项。工作树已于
2026-08-15 收口（见第 0 项），本地 `main` 比远端超前 46 个提交且**尚未推送**；
远端发布矩阵之前必须先把这些提交推上去。提交和推送属于显式的仓库操作，
需要得到维护者授权后执行——本轮已获授权在本地提交，推送尚未获授权。

| 顺序 | 接下来要做的事 | 完成证据 |
| --- | --- | --- |
| 0 | **收口当前工作树**：逐模块审查现有 Core、Python、Node、CI、许可证和文档改动，确认所有新增文件都属于交付范围，再整理为可审查提交 | **本地部分已完成（2026-08-15）**：`python3 tools/local_ci.py quality rust node python typing` 28 步全绿零跳过、`python3 tools/local_ci.py linux` 六步全绿、`git diff --check` 干净；工作树已整理成 14 个按主题的提交，其中带源码的 12 个每个都单独用 `cargo check --workspace --all-targets --locked` 在独立 worktree 里验证过能编译、不依赖后续提交（另两个只动 `.gitattributes` 与 Markdown）。**未完成的是远端那一半**：这些提交还没推送，所以还不存在一个远端可验证分支 |
| 1 | **跑通正式六平台发布矩阵**：在 GitHub Actions 上执行 Linux、Windows、macOS × x86-64/ARM64 的 CLI、Python wheel 和可选 Node 包任务 | 六路 job 全绿；每个 CLI 产物能运行 `--help`，每个 wheel 能安装并通过公开 API 测试，每个 Node tarball 只含目标平台 addon 并通过 JS/TypeScript smoke；产物包含一致的许可证和 notices |
| 2 | **扩充代表性真实 corpus**：在现有 Unity 2022.3 与 6000.3 之外，优先加入 Tuanjie 2022.3.x、Nintendo Switch、旧 Unity 4/5/2017 和带完整托管快照的样本 | 私有 manifest 在 release 模式稳定通过；每类至少有对象顺序、PathID/class、名称/container、原始载荷 hash、主要解码结果或明确错误族的版本化快照；专有样本不提交到仓库 |
| 3 | **按 corpus 命中补格式长尾**：只处理真实样本实际触发的 UnityArchive、Unity/Tuanjie 虚拟几何 cluster、Switch 低 mip/stripped mip 和平台纹理/音频 codec | 每项都有最小 fixture、边界/畸形输入测试和独立 oracle；没有可靠布局或 oracle 的格式继续稳定返回 `Unsupported`，不猜字段、不静默产出 |
| 4 | **完成 Python 主接口审计**：以 Rust Core 的稳定高层能力为源，逐项核对 Python 的加载、读取、导出、预算、错误类型和类型桩；Node 只作为可选绑定跟进稳定接口，不作为 Python 完成的前置条件 | 安装后的 wheel 公开面与 `.pyi` 双向一致，Python 3.9 严格 mypy 消费端和完整 API 测试通过；大结果采用有界、可失败分配；Rust/Python 路径不经过 C ABI 或 .NET |
| 5 | **做 1.0 退役审计**：重新逐条核对本文“完成判定”，把 C# 从日常运行链彻底降为可选 oracle | 默认构建、测试、安装和用户工作流均不需要 .NET；没有 GUI 或旧 C ABI 发布物；七项完成条件均有当前证据，未满足项不得被标成完成 |
| 6 | **处理非阻断上游事项**：有可审计方案时向上游提交 `ruopus` SILK 与 vendored 纹理解码器修复；拿到可验证 ACL 样本后再评估纯 Rust Tuanjie ACL decoder | 上游 issue/PR 或本仓库可复现记录可独立运行；任何替换不得使已精确通过的 CELT/纹理路径回退，也不得引入未授权专有二进制 |

贯穿所有步骤的范围约束不变：**不做 GUI，不恢复或扩展旧自定义 C ABI，
不把 Oodle 等专有库静态打进 Core**。Rust 和 Python 是正式目标，Node 是可选
交付面；C# 只承担历史参考和可选差分 oracle。

## 后续优先顺序

上表用于安排实际工作；本节保留各项背景、限制和已完成调查。前一版的九条里有四条已在 2026-08-15 完成或走到尽头，按本文维护规则移出：托管差分已扩到容器/版本门/已实现的解码路径（含整包 Live2D 与全部块格式）；binary FBX 已在 Core/CLI/Node/Python 全部可达；MOC3 标识表与散件发现回退已接；`ruopus` SILK 已定位为上游缺陷并写进 `docs/upstream-defects.md`，CELT 路径有精确差分把守。剩下的按能否自主推进排序：

**需要外部输入，我这边无法推进：**

1. 让 CI 重新跑起来（GitHub Actions 计费）。本机已把 CI 的常规步骤复跑过一遍且全绿；Linux 运行面现已在 amd64/arm64 容器中覆盖，仍无法本机复现的是 Windows 运行与 GitHub 托管发布矩阵。为降低这条的代价，新增 `tools/local_ci.py`：一条命令跑完 CI 的全部步骤（现为 51 步）（格式、Clippy、rustdoc、打包、许可证聚合、无 GUI/旧 C ABI 的交付范围与产物校验、workspace 构建/测试、托管差分、MonoBehaviour schema 生成器、音频差分、输出格式校验、release CLI 的构建/smoke/staging、Node debug/release 构建/测试/打包、Python wheel 与 sdist 的构建/安装/两套测试、Python 3.14 abi3 前向兼容、严格 Python 类型消费、UnityPy 差分，以及可选交叉/Linux 容器门禁），缺哪个工具就把那一组记为跳过而不是失败。2026-08-15 修了一处会让证据凭空消失的问题：wheel 原先直接装进当前解释器，而 Homebrew/发行版的 Python 现在按 PEP 668 直接拒绝安装——于是 `install wheel` 失败，依赖它的 `wheel surface`、`python api` 与 UnityPy 差分一并失败，也就是说 Python 那一侧的公开面守卫和 API 套件其实根本没在跑。现在 `python` 组自己建一个 `--clear` 的临时 venv（带 `--system-site-packages`，好让宿主已装的 UnityPy 仍可导入），宿主解释器只作为建 venv 的底座和 maturin 的目标。修好之后第一次跑就抓出一条陈旧断言：`python_api.py` 里还在断言 physics3.json 的紧凑写法，而两次提交前刚把 Cubism 文档改成与托管一致的展开写法——正是「没在跑的测试」会掩盖的那类回归。随后类型桩审计又抓到两项幽灵 API：`.pyi` 允许导入 `AclDecoder`/`OodleDecoder`，运行时模块却没有这两个名字；现在二者是真正的运行时导出，wheel 守卫也改成双向比较。本轮逐项对照 workflow 时又发现本地脚本漏掉了 workspace build、sdist 重建、release CLI/Node 产物和 Python 3.14 安装；这些步骤现已接入并实际通过。交付范围检查也已实际通过，会把 workspace、依赖方向或产物内容的漂移直接变成失败；schema 生成器步骤则在本轮以真实临时 DLL 单独验证，并和托管差分一起通过 `oracle` 组。这不能替代 CI——CI 是在三个平台上跑这套矩阵——但它让拿得到 Linux/Windows 的人能自己产出同样的证据，而不必先从 workflow 文件里把步骤拼出来。另外补了一个 `cross` 组：本机交叉编译到 `x86_64-unknown-linux-gnu`（含全部测试目标）已验证通过，Windows 侧用 `zig cc` 也验证了 core/CLI/Python 三个 crate 能编过（Node addon 要链 libnode.dll，交叉环境下不适用）。这只覆盖「能不能编译」——路径假设、漏掉的 `cfg`、只在某个平台成立的 `Send` 之类——行为层面则由新增的 `linux` 组覆盖：用 CI 钉住的同一个 Rust 1.88 镜像，在容器里跑 core+CLI 的完整测试（当前 497 项 core、6 项畸形输入扫描、全部 CLI 套件），并构建、执行和 stage release CLI；Python wheel 也完成 release 构建与两套安装后测试，x86-64 与 arm64 两个架构都已实跑通过（CI 的 wheel 矩阵正是这两个）——这是 LZMA 那次提交之后第一次拿到 Linux 上的真实运行结果，而 Python 那两套正是之前坏了很久没人发现的。Node release addon 也在两种 Linux 架构的干净容器副本里跑通了，且实际生成并检查了 npm tarball（镜像没有 Node，因此按 CI 用的版本拉官方构建），10 组 API 测试全过，整个构建发生在只读源码的临时副本中，容器销毁后不会污染宿主构建。Windows 目前仍只有编译层面的验证。顺带记一笔：workspace 唯一的 C 依赖是 `zstd`（`zstd-sys` 会编 C 源码），交叉编译因此需要目标平台的 C 工具链；`docs/upstream-defects.md` 里原先把「纯 Rust」当成本 crate 已有的性质来论证，那是不准确的，已改成「再加一个原生依赖的代价」；
2. 扩充真实 corpus，按实际命中率排序缺口（需要真实游戏文件）。为降低这条的门槛，corpus 用例的 `expected` 快照改成可选：不给快照时依然会把每个对象都读一遍、出错即失败，并报出读到了多少文件/对象/可解析载荷，只是没法断言取值对不对。这样「有游戏文件」就够跑，不再同时要求 .NET SDK 与 AssetStudio checkout 来生成快照。另加了一条防自欺断言：没有快照的用例必须至少解析出一个对象——reader 认不出来的输入会被当成资源文件、解析出零个对象，否则那种用例会安静地通过；
3. 获取样本并实现 Unity 6000.2 MeshLOD/虚拟几何、Tuanjie 虚拟几何 cluster、UnityArchive，而不是猜测布局；
4. 是否把 `ruopus` 与 `texture2ddecoder` 的缺陷提到上游（补丁已备好，见 `docs/upstream-defects.md`）。

**可以自主推进，但代价与收益需要先掂量：**

5. 纯 Rust Tuanjie ACL 2.x 解码。crates.io 上没有现成实现，属于从零写；而且没有可对照的样本，写出来无法验证，因此在拿到样本之前不宜动手；
6. 补齐高命中率的平台纹理/音频长尾（新 codec 必须先有真实样本和独立 oracle）；
7. 继续提升 Node 专用 API 覆盖。2026-08-15 补了此前完全没有绑定的 `readFont`/`readMovieTexture`/`readVideoClip`，按 GameObject 粒度的 FBX 规划（`splitObjectFbxCandidates`/`animatorFbxCandidates`/`readGameObjectFbx`），以及 Cubism 单对象读取器。后者先接入 physics/expression/fade-motion，又补齐 `readCubismPosePart`、`readCubismDisplayInfo`、标准 Unity `AnimationClip` 的 `readCubismClipMotion` 和 Tuanjie ACL 的异步 `readCubismClipMotionWithAclDecoder`。测试不是只断言方法存在：pose/display 由真实嵌入 TypeTree 的 class 114 fixture 驱动，标准 clip fixture 完整走 2022.2 muscle/binding/event 布局，Tuanjie fixture 走 2022.3.55t4 `m_AnimData`、ACL 容器/hash/decoder map 与 Core 回传校验，两者生成的 motion3 JSON 都交给 JavaScript JSON parser；错误类型、坏 decoder 输出和一字节输出预算分别拒绝。至此 Node 的直接 motion 读取面与 Python 对齐；TypeScript 消费测试另外验证 ACL/Oodle callback 与 `Buffer` 类型能由发布声明直接编译。

## 完成判定

只有同时满足以下条件，才可把 Rust 重写标记为完成：

- Rust Core 和 Python 主流程不依赖 .NET、GUI 或旧 C ABI；
- 支持矩阵中的“Implemented”项均有相应单测、边界测试或差分证据；
- 代表性真实 corpus 在支持的 Unity/Tuanjie/平台矩阵上稳定通过；
- 未实现格式均有明确、稳定的 Unsupported 行为，不产生静默错误输出；
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

**两份语料一起过了 corpus 闸门（2026-08-15 晚）**：`real_corpus` 用同一份 manifest 同时跑两个用例——2022.3 播放器目录 23 个文件 / 610,552 个对象 / 190,300 个有解析载荷，6000.3 Addressables 2,778 个文件 / 243,617 个对象 / 104,565 个有解析载荷，release 下 240 秒全过。第一次跑是失败的，而失败的原因值得记一笔：报的是"Texture2D RGBA output is 16777216 bytes, exceeding limit 7953295"，看起来像贴图缺陷，实际是用例声明的预算不够——Live2D 物化的总预算是**整个用例所有包**共用的，交给解码器的上限就是它剩下的余额，于是错误里出现了一个调用方从没选过的数字。已改成明说是哪个预算见底、还剩多少，`corpus/README.md` 也补上"总量是整个用例的、不是每个包的"。

**PNG 那边剩下的分歧是 sprite 紧密网格的边界**：最坏的一张里 1,143,000 个像素差 4 个，而且是双向的——一个 texel 本项目保留、提取那边遮掉，另外三个反过来。这是光栅化的边规则，而本项目的光栅器由托管差分逐字节钉住，所以分歧在提取那一侧。这些情况按"报告"处理而不是"容忍"：把容差放宽到它们能过，同样会盖住真正的遮罩缺陷。

### 对照结论（2026-08-15）

逐条对上面七条自评，其中五条已满足、两条未满足：

| 条件 | 状态 |
|---|---|
| Core/Python 主流程不依赖 .NET、GUI 或旧 C ABI | 满足；C ABI crate 已排除在 workspace 外，只作历史参考 |
| 「Implemented」项均有单测、边界测试或差分证据 | 基本满足；纹理/Sprite/Mesh/AnimationClip/Live2D/容器/版本门均有托管差分，TypeTree 另有 UnityPy 第二 oracle，畸形输入另有专门扫描。唯一没有差分的是 5.5+ 序列化 shader，原因是托管侧自身的初始化缺陷；2021+/2022+ 的 shader 已实现（见下），其结构正确性由 46 个真实 shader 与 UnityPy 的逐一比对背书；托管差分覆盖的仍是 5.2/5.3，因为托管侧对 2021+ 根本不产出对象 |
| 代表性真实 corpus 稳定通过 | **部分满足，且已扩到第二款游戏与第二个引擎世代（2026-08-15）**；(1) 一份完整的 Unity 2022.3.62f2 播放器目录（23 文件 / 610,552 对象 / 190,300 个有解析载荷）；(2) 四个真实 UnityFS v8 bundle；(3) **2,778 个 Unity 6000.3.12f1 的 Addressables bundle（926 MB / 243,617 对象）**，全部零错误通过。仍缺的是 Tuanjie / Switch / 更老版本，以及带托管快照的取值比对 |
| 未实现格式有明确稳定的 Unsupported 行为 | 满足；且畸形输入扫描验证了不会 panic |
| 跨平台发布任务通过 | **仍未满足，但已大幅缩小**；Linux 的 x86-64 与 arm64 两个架构上，core+CLI 测试及 release CLI 构建/执行/staging、Python release wheel 构建与两套安装后测试、Node release addon 与 npm tarball 构建/测试均已在容器里实跑通过；Windows 侧验证了编译。CI 已配置 Python wheel、CLI 和可选 Node 包的 Linux/Windows/macOS × x86-64/ARM64 六路发布矩阵，并对 CLI 二进制和 Node release addon 做产物自身 smoke，但该发布矩阵尚未在 GitHub runner 上实际跑通。缺的仍是 Windows 上的真实运行——本机查过 wine（未安装）、Windows 容器（macOS 上不支持）与 Parallels（只有一个无效的 Debian 虚拟机），都不具备条件，装 wine 属于往你机器上装东西，没有自作主张——以及 CI 发布作业的实际绿色记录 |
| 导出/解包有界、拒绝穿越与符号链接、原子发布 | 满足 |
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
