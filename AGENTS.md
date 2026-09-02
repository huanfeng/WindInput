# AGENTS.md — WindInput 协作约定

> 给在本仓工作的 AI/人类协作者。Rust 核心在 `wind_input/`（cargo workspace）。

## 仓库地图

本仓含三大组件，另有若干独立仓库与本仓协同（相对路径以本仓根为基准）：

| 位置 | 内容 | 文档 |
|---|---|---|
| `wind_input/` | Rust 核心服务（cargo workspace：19 个 crate + `apps/service`） | 本文档 + crate 级 AGENTS.md |
| `wind_tsf/` | C++17 TSF DLL：Windows 输入法接口层，经 Named Pipe 与 Rust 服务通信 | [AGENTS.md](wind_tsf/AGENTS.md) |
| `wind_macos/` | macOS IMKit `.app`（与 `wind_tsf/` 对位，开发中） | [AGENTS.md](wind_macos/AGENTS.md) |
| `../wind-setting` | 设置 UI（**独立仓库**，经 JSON-RPC 与核心通信）；改设置界面去那边 | — |
| `../wind-portable` | 绿色版启动器（独立仓库，不存在时构建脚本自动跳过） | — |
| `../WindInput-Go` | Go 旧版源码（只读参考；docs 旧文档里的 `../WindInput` 指的是它） | — |

## Crate 索引

> workspace 共 20 个 crate（均在 `wind_input/crates/`）。复杂 crate 已配 crate 级 `AGENTS.md`，改动前先读对应文档；新增/重构 crate 时参照同结构补文档。

| Crate | 职责 | crate 文档 |
|---|---|---|
| `wind-coordinator` | 输入法“大脑”：按键路由、状态机、候选与模式切换的中央协调器 | [AGENTS.md](wind_input/crates/wind-coordinator/AGENTS.md) |
| `wind-engine` | Schema 驱动的引擎工厂：拼音/码表/混输/英文四类引擎的构建、切换与候选分发 | [AGENTS.md](wind_input/crates/wind-engine/AGENTS.md) |
| `wind-ui` | 所有浮层窗口（候选窗/工具栏/菜单/状态泡/Toast/Tooltip）的渲染与鼠标交互 | [AGENTS.md](wind_input/crates/wind-ui/AGENTS.md) |
| `wind-ui-types` | 表现层协议（纯数据）：`UiCommand`/`UiEvent` 及载荷，协调器 ↔ 任意前端（桌面/macOS/Android）共用 | [AGENTS.md](wind_input/crates/wind-ui-types/AGENTS.md) |
| `wind-cmdbar` | 命令直通车：短语解析 → AST 求值 → 动作执行（纯逻辑） | [AGENTS.md](wind_input/crates/wind-cmdbar/AGENTS.md) |
| `wind-dict` | 多层复合词典引擎：DictLayer/CompositeDict 查询 + wdat mmap 二进制词库 | [AGENTS.md](wind_input/crates/wind-dict/AGENTS.md) |
| `wind-store` | 基于 redb 的用户数据持久化：按方案隔离用户词/词频/Shadow，全局存短语 | [AGENTS.md](wind_input/crates/wind-store/AGENTS.md) |
| `wind-rpc` | core ↔ 设置端 JSON-RPC IPC 双通道（ctrl 请求-响应 + events 广播） | [AGENTS.md](wind_input/crates/wind-rpc/AGENTS.md) |
| `wind-config` | 配置系统：TOML 三层合并、字段注册表 SSOT、热键编译、变体探测、运行时状态 | [AGENTS.md](wind_input/crates/wind-config/AGENTS.md) |
| `wind-theme` | 加载并求值 v3 主题，输出调色板 + 盒模型树供 wind-ui 渲染 | [AGENTS.md](wind_input/crates/wind-theme/AGENTS.md) |
| `wind-bridge` | Named Pipe 服务器 + Push 管道，桥接 Rust 服务与 C++ TSF DLL | [AGENTS.md](wind_input/crates/wind-bridge/AGENTS.md) |
| `wind-ipc` | IPC 协议定义与编解码（TSF 二进制协议 + JSON-RPC） | — |
| `wind-keys` | 键名/VK 映射、导航键分类（纯逻辑）+ 按键注入（平台层）；**VK 常量 SSOT** | — |
| `wind-candidate` | 候选词数据类型、排序与过滤 | — |
| `wind-phrase` | 短语系统：静态/动态模板展开 + cmdbar 双路径 | — |
| `wind-transfer` | 导入导出/备份还原底座：Bundle（manifest + zip）聚合打包与 Merge 合并策略（编解码在 wind-store） | — |
| `wind-quick-input` | 快捷输入的内置候选来源（纯逻辑）：`quick_input.calc` 算式（含幂 `^`）/ `.date` 日期年月 / `.number` 数字金额；另定义 `.repeat`（重复上屏，由协调器实现）的成员 id。开关与优先级 = `mix_modes.members` 的有无与顺序 | — |
| `wind-reverse` | 候选反查：五笔编码/拆字/拼音读音（悬停 tooltip） | — |
| `wind-aux-code` | 辅助码过滤：拼音后追加字形辅助码，按字形裁减候选字词（**出厂关闭**，`schema.pinyin.aux_code.enabled`） | [AGENTS.md](wind_input/crates/wind-aux-code/AGENTS.md) |
| `wind-punct` | 标点转换纯逻辑（中英标点/全半角/数字后智能） | — |
| `wind-transform` | 文本变换：标点、全角、自动配对、简繁 | — |

`—` = 暂无独立 `AGENTS.md`（多为纯逻辑/工具 crate，职责单一，看 `src/lib.rs` 顶部模块注释即可）。

核心输入链路（词库 → 五类引擎 → 候选后处理）的**现状架构文档**见
[docs/architecture/engine-candidate-pipeline.md](docs/architecture/engine-candidate-pipeline.md)
（含混输拼音否决、顶码/满码一致性、各模式流程对比）；改引擎/候选逻辑时同步更新该文档。

## 平台分层策略——平台/系统依赖放哪

Android 复用（进程内直调 headless coordinator）落地后，「平台代码放哪」是立约级问题。
新写任何 `cfg(windows)` / `cfg(target_os = …)` / 平台 crate 依赖之前，按下表定归属：

| 你要加的东西 | 归属 | 先例 |
|---|---|---|
| 协调器 ↔ 前端之间流转的**数据类型** | `wind-ui-types`（三条红线见其 AGENTS.md：仅纯数据、依赖过 android check、平台载荷走 target-specific + cfg 变体） | `UiCommand`；唯一平台载荷 `SetHostRender` |
| 引擎/词库/候选/配置等**纯逻辑** | 对应纯逻辑 crate，**零平台依赖**（引入前先问「Android 上编得过吗」） | wind-engine / wind-dict / wind-candidate 等全员 |
| 协调器逻辑中途要**同步 pull** 的平台能力，且 cfg 兜底值是语义错误 | `HostServices` trait 注入面（coordinator/src/host_services.rs，收录判据三条见模块文档） | 剪贴板三方法 |
| 平台状态有**事件推送通路**可注入 | 用注入路，**不要**造第二个 pull 真相源 | 宿主进程名走焦点事件喂 `pid_names` |
| cfg 兜底值在缺失平台**可接受**的探测函数 | 同名函数三平台并列 cfg + 兜底，不进 trait | `theme_style.rs::system_prefers_dark`（三实现范例）、`is_foreground_fullscreen`→false |
| **整模块**平台专属的功能 | `lib.rs` 处整模块 cfg（聚类，勿散落函数内） | `direct_switch`（windows）、`handle_cmdbar_macos`、wind-bridge 的 `*_unix`/`*_windows` 模块族 |
| **桌面渲染路径**（窗口/UI 线程/渲染注入） | wind-coordinator 的 `desktop-ui` feature（default 含）；`--no-default-features` 即 headless/Android 形态 | 生产构造器 `new`、`UiManager` |

编译门与防走样（命令即 `wind_input/.cargo/config.toml` 的 alias，CI 强制）：
`cargo check-headless`（host 无 wind-ui 形态）、`cargo check-android`（依赖闭包过
aarch64-linux-android；⚠ zstd-sys 等 C 依赖的 build script 需 NDK clang，本机无 NDK 时由
CI 承接）、CI 另有 `cargo tree` 断言 coordinator 无 wind-ui 依赖边。

已知 Android target 坑：bionic 无 POSIX SHM（`shared_memory_posix` 已 cfg 排除）；
「check 不链接故无需 NDK」对 `-sys` crate **不成立**（build script 要编 C 源）。

## 虚拟键码（VK）—— 用常量，禁止裸十六进制

所有 Windows 虚拟键码统一在 `wind_input/crates/wind-keys/src/keymap.rs` 定义为
`pub const VK_*`（`VK_ESCAPE`/`VK_BACK`/`VK_SPACE`/`VK_RETURN`/`VK_PRIOR`/`VK_NEXT`/
`VK_UP`/`VK_DOWN`/`VK_A..VK_Z`/`VK_0..VK_9`/`VK_SEMICOLON` 等）。

- **禁止**在 `match data.key_code` / 比较中写裸 `0x1B`、`0x21` 之类字面量；用 `keymap::VK_*`。
- 触发键名（配置里的 `"backslash"`/`"semicolon"`）→ VK：`keymap::key_name_to_vk(_with_letters)`，
  单一真相源 `KEY_TABLE`，新增键只改一处。
- **⚠️ 触发键名跨仓一致性**：设置界面（独立仓库 `../wind-setting`，见上方仓库列表）的
  `src/assets/settings_manifest.toml` 里各触发键选项的 `value` 必须与本表 `KEY_TABLE.names`
  字符串**逐字相同**——两仓无编译期/运行期校验，写错会静默失效（UI 显示"已选中"、保存不报错，
  内核 `key_name_to_vk` 返回 `None` 后被 `filter_map` 悄悄丢弃）。曾因 `wind-setting` 把方括号
  选项写成 `open_bracket`/`close_bracket`（本表实际是 `lbracket`/`rbracket`）导致临时英文/临时
  拼音/快捷输入的方括号触发键全部失效。改本表新增键或改名时，**必须同步 grep 检查
  `wind-setting/src/assets/settings_manifest.toml` 与 `wind-setting/src/key_conflict.rs` 的
  `key_symbol()`** 有没有过时或不一致的字符串。
- **注意类型**：`KeyEventData.prev_char`、`CommitRequestData.trigger_key` 是 **u16**
  （UTF-16 码元 / 协议字段），与 VK(`u32`) 比较前需 `as u32` 转换；prev_char 是字符码点不是 VK，
  别套 VK 常量（用数值区间，如 `(0x30..=0x39).contains(&prev_char)` 判数字字符）。

## 候选导航键（翻页 / 高亮 / 二三候选）—— 配置驱动 + 统一逻辑

这些键**都可配置**，且必须走**统一**入口，禁止各 handler 各写一套硬编码判断：

- 翻页 / 高亮：`keymap::NavKeys`（从 `keys.page_keys` / `keys.highlight_keys` 组名编译）+
  `Coordinator::apply_nav_key(state, data, include_printable)`。普通模式与所有候选模式共用。
  - `include_printable=true`（码表型：普通/特殊/mix/临拼）：`-`/`=`/`[`/`]` 可作翻页；
  - `include_printable=false`（文本/表达式型：临英/快捷输入）：上述键作输入，不当导航。
  - overlay handler 用 `handle_candidate_nav`（按 `state.active` 自判 `include_printable`）。
- 二三候选键：`select_key_offset`（读 `keys.select_key_groups`，经 `hotkey::select_key_vks`）。
  - **可打印键组**（`;'` / `,.`）走 keydown：各模式 handler 自己调 `select_key_offset`。
  - **修饰键组**（`lrshift` / `lrctrl`）走 **keyup**：纯修饰键的 keydown 不能吃（宿主要看得见
    修饰键），且在 keydown 上判定会让 `Ctrl+A` 的第一下 Ctrl 误选候选。故它们注册进
    `key_up`（action=`select_candidate`），由 TSF 的轻敲机制（<500ms + 中途无别的键）转发
    keyup，协调器入口的 `handle_select_key_up` → `select_page_candidate` 按模式派发。
    与切换键撞键时：**有候选选词、无候选切换**；越界吞键（修饰键没有字符，不套 overflow）。
  - 因此 `is_toggle_mode_keycode` 必须按 action 过滤——key_up 表里不只有切换键了。
- 新增模式/按键时**复用**以上，不要再写 `0x21|0x22 =>` 之类分支。

## 跨组件硬约定（违反即复现历史 bug）

跨 crate / 跨语言边界的立约级不变量集中在此；crate 内部细节归 crate 级 `AGENTS.md`。

- **C++ 吃键集必须 ⊆ Rust 出字集**：`wind_tsf` 在 `OnTestKeyDown` 就决定是否吃键，早于 IPC
  往返；Rust 侧事后回 PassThrough 已来不及。凡 C++ 吃掉而 Rust 最终不出字的键，在严格 TSF
  宿主上直接丢失（历史案例：全角模式丢键、密码框丢键；指纹＝「有些应用打不出、有些出半角」）。
  给 C++ 侧新增吃键条件前，先确认 Rust 侧在同条件下必定产出。
- **候选排序必须落到 weight**：协调器会按 weight 统一重排候选；引擎内部只调顺序、不改
  weight 的排序会被重排冲掉。顶码上屏取首选与候选窗展示必须共用同一排序函数。
- **用户短语数据只存 `user_data.db`**（wind-store，全局不分方案）：yaml 短语文件是系统种子，
  **不是**用户覆盖入口——旧设计文档里「yaml 用户目录覆盖」的说法已过时，勿据此实现。
- **自带数据文件一律经覆盖解析函数定位，禁止直接 `data_dir.join(...)`**：用户目录同名文件
  整体替代安装目录那份（`Config::resolve_data_file` / `resolve_schema_resource` /
  `EngineManager::resolve_schema_file` / `resolve_dict_file`）。绕过解析函数是这套机制历史上
  **全部**缺陷的唯一形态（`common_chars.txt`、`pinyin_map.txt`、`unigram_path` 均栽于此），
  且失败静默——找不到就退化，不报错。键级合并只有 `config.toml` 与 `compat.toml` 两处。
  完整矩阵与新增数据文件的约定见 `docs/architecture/user-override.md`。
- **出厂预置的绑定/开关，落点必须是用户能清空的载体**：配置四层（默认 < data < data_custom
  < user）与方案两层（`.schema.toml` < `schema_overrides`）用的都是**逐键深合并**
  （`merge_value` / `merge_toml`）——这个算子只能新增/覆盖，**表达不了删除**。所以把出厂值
  写进 map 类字段（`keys.key_actions`、`punct.custom_mappings` 那类）的某个子键，用户在设置页
  删掉后每次 `load()` 都会被合并回来，**永远关不掉**：软键盘 `ctrl+shift+k` 写在 L2 的
  `keys.key_actions` 里就是这么在 v0.120.0 报障的；更早一例是五处 `trigger_keys` 折算进
  `key_actions`，靠一次性物化（`Config::materialize_key_actions`）才收场。三条出路，按优先级：
  1. **出厂值放标量/数组字段**（`keys.softkeyboard`、`keys.toggle_toolbar` 那类）：清空即禁用，
     上层整体覆盖，天然可关。**新增内置功能的开关键一律先考虑这条。**
  2. 形态上进不了专用字段（「一键一功能」的 `key_actions` 就是）时，那张表的值域**必须**含一个
     表示禁用的显式值（本仓是 `"none"`），且它的**每条通路**都要认——组合键、单键、修饰键三条
     少认一条，那一档就还是关不掉。
  3. 整表替换（`merge_toml` 对 `custom_mappings` 的特判）：只在「这张表是一整份、不与任何层
     逐行混」时才成立。跨层叠加的表（`key_actions`）不能这么改，那会让方案升级后作者新增的
     条目透不过来。
  ⇒ 加任何出厂预置前先答这一句：**用户在 UI 上做的删除落到哪个键？那个键能否压制出厂值所在
  的那个键？** 答不出「能」，就是上面这个 bug。

## 提交纪律（多会话共仓）

可能有多个 AI 会话同时在本仓工作。**提交只用显式路径**（`git add <具体文件>`），
**禁止 `git add -A` / `git add .`**——会把其它会话未提交的文件一起卷入提交。
提交前 `git status` 确认暂存区只含自己改的文件。

提交信息保持常规工程风格（`type(scope): 摘要`，中文正文）：**不要**添加
`Co-Authored-By`、`Generated with` 以及 `Constraint:` / `Confidence:` / `Tested:` 等
AI 附加 trailer。

## 格式化（强制）

仓库自带 `.githooks/pre-commit`（提交前自动跑 `cargo fmt --check`），默认未激活，
一次性执行 `./scripts/dev.sh hooks`（或 `.\scripts\dev.ps1 hooks`）激活；
纯本地 git config，不随仓库自动传播，每个 worktree/clone 都需单独激活一次。

**每次修改 Rust 文件后，验证通过前必须运行 `cargo fmt`**（在 `wind_input/` 目录下），
再把格式化结果作为独立提交：

```bash
cd wind_input
cargo fmt
# 确认只有格式改动，无逻辑变更
git add <修改过的 .rs 文件>
git commit -m "style(fmt): cargo fmt 统一格式化"
```

- **逻辑修改** 和 **fmt 修改** 必须分开提交，不能混在同一个 commit。
- 不要用 `git add -A`：只 stage 本次逻辑改动涉及的文件 + 对应 fmt 文件。
- `cargo fmt` 对整个 workspace 生效，若其他 crate 也被格式化，一并纳入 fmt 提交。
- 多会话协作下格式漂移容易累积（上一会话改完忘记提交 fmt 结果）：开始新一轮工作前，
  先跑一次 `git status` + `cargo fmt`，确认没有遗留的纯格式改动混入本次工作区，
  避免和自己本次的逻辑改动绞在一起难以拆分提交。

## 日志规范

### 级别策略

| 级别 | 用途 | 隐私要求 |
|---|---|---|
| `error` | 不可恢复错误，影响功能 | 无用户数据 |
| `warn` | 可恢复异常，值得关注 | 无用户数据 |
| `info` | **生产默认级别**，关键生命周期事件 | **严禁**包含用户输入、词库词条、候选词等隐私数据 |
| `debug` | 诊断细节，开发时手动开启 | 可含调试上下文，部署时不应开启 |
| `trace` | 极细粒度追踪 | 仅本地调试 |

`info` 是正式部署时的唯一文件输出级别，开发者需在 `config.toml` 手动配置才能开启更详细级别：

```toml
[debug]
log_level = "debug"   # 或 "trace"
```

### 日志文件

- 滚动策略：**每次服务启动滚动一次**（`log_rotate::rotate_on_startup`，上次运行整体搬入
  历史文件），另按大小兜底（默认 10 MB/文件）；历史文件默认保留 10 个（`debug.log_max_files`）
- 文件命名：`wind_input.log`（本次运行）、`wind_input.1.log`（上次）… `wind_input.10.log`。
  **序号在扩展名之前**，滚动后仍是 `.log`（编辑器可双击、按 `*.log` 可搜）；勿改回 `.log.N` 旧式
- 时间戳为**本地时区**，格式与 `wind_tsf` 的 FileLogger 完全一致，两份日志按时间直接对齐排查；
  勿退回 tracing 默认的 UTC SystemTime timer
- 路径（变体感知）：
  - 正常安装 release：`%LOCALAPPDATA%\WindInput\logs\`
  - 正常安装 dev：`%LOCALAPPDATA%\WindInputDev\logs\`
  - 便携模式：`<exe目录>\userdata\logs\`（以 exe 同目录存在 `portable_mode` 文件为标记；
    旧名 `wind_portable_mode` 仅保留读取兼容，新写入一律用新名）
- 可通过 `RUST_LOG` 环境变量覆盖级别（优先级最高，仅用于开发排查）

### 写日志准则

- `info!` 只记录系统事件（启动/关闭/加载/错误），**不得**记录用户键入的字符、候选词、词库内容
- `debug!` / `trace!` 可含诊断数据，但部署包中不应默认开启

## 构建 / 测试

两套开发脚本命令菜单对齐，按主机选择：

### Windows 本机（MSVC，`scripts\dev.ps1` / `dev.bat`）

- host 即 Windows 目标，无交叉编译限制：`cargo check` / `cargo test` 可直接跑全 workspace
  （含 `wind-coordinator`）。脚本快捷键：`k`=check、`l`=clippy、`t`=test、`f`=fmt、
  `ci`=fmt+clippy+test。
- 全构建：`1`（release → `build/`）/ `d1`（dev → `build_dev/`）；单模块 `m1..m4`（tsf/核心/
  setting/portable，前缀 `d` 为 dev）。系统安装：`p1`/`pd1`；安装包：`8`/`d8` → `dist\*-Setup.exe`。
- 部署目标默认 `C:\Program Files\WindInput[Dev]`，可在 `scripts\deploy.local.ps1` 覆盖。

### 远程编译机（可选，默认不生效）

本机编译吃满 CPU 时，可把构建整体转到另一台 **Windows** 机器，产物回传本机，**部署仍在本机**。
照 `scripts\build.local.ps1.example` 建出 `scripts\build.local.ps1` 即启用（该文件含内网地址与
账号，**不入库**）；不建则 `dev.ps1` 行为逐字不变，直接调 `remote-build.ps1` 也只会回落本机执行。

- **照常用 `dev.ps1`**：`dm1` / `d1` / `t` / `ci` 等构建检查类命令自动转发，产物落回 `build[_dev]\`。
- **细粒度 cargo**：`.\scripts\rc.ps1 test -p wind-coordinator`（直接敲 `cargo` 是在本机跑）。
- **任意命令**：`.\scripts\remote-build.ps1 -Raw "<命令>"`，在编译机的 `wind_input\` 下执行。
- **临时回落本机**：`$env:WIND_NO_REMOTE = "1"`（编译机关机 / 不在内网 / 要做 A-B 对照）。

⚠️ **编译机必须是 Windows + 原生 MSVC**：clang / cargo-xwin 交叉编译出的 `wind_tsf.dll` 在带
安全加固的宿主（企业微信 / TIM / UU 浏览器）里 COM 激活失败，已 A/B 实测锁定在工具链上
（`6dbc8595` 因此把发布链从 ubuntu 交叉编译改回 windows-latest）。Linux 编译机与 sccache-dist
（其 build server 官方只支持 Linux）都因此出局。

#### 同一台编译机同时只跑一个构建

看到 `[锁] 编译机正被另一个构建占用 (...), 等待...` 是正常的，它会自动排队并在对方结束后继续。
这道锁不是为了效率而是**正确性**：`Do-Full` 开头会清空 `build[_dev]/`，两个构建并发时后者
会把前者的产物整个删掉，而前者浑然不觉地走到打包——产出一个空安装包并报告「打包完成」，
全程零报错（实测：本该 20 MB 的包出了 2.3 MB）。锁按槽位隔离，不同 worktree 互不排队。
只读的 `-Raw` 查询可用 `-NoLock` 跳过排队；凡是会写 `target/` 或 `build/` 的命令都不要加。

⚠️ 并发还会让**任何性能测量失效**——同一条命令实测跑出过 83.8 / 96.5 / 103.4 / 135.9 s。
调优前先确认没有别人在用这台机器。

**Ctrl+C 中断构建后锁卡死怎么办**：陈旧判据仍是「锁文件创建时刻起 30 分钟未变」，Ctrl+C/断线
中断时锁不一定能立刻释放（本地进程如何退出不受脚本控制）。不想等 30 分钟：
`.\scripts\dev.ps1 runlock` 或 `.\scripts\remote-build.ps1 -Unlock`——只删锁文件，不碰源码/产物。
⚠️ 用前先确认没有其他会话正在用这台机器（见上一段），否则会打断真实在跑的构建。
（曾做过一版远程心跳自动续期/判陈旧的方案，机制验证有效，但对单人使用这台编译机而言过于
复杂，已撤回改回手动 `-Unlock`；设计与实测记录见项目记忆 `project_remote_build_machine`，
以后如果这台机器变成多人共用、手动清锁不够用了，可以按那份记录抄回来。）

#### 远程侧的构建是并行的

`remote-build.ps1` 会注入 `WIND_PARALLEL_BUILD=1`，让全构建的 core / tsf / setting / portable
四步并行（它们各写各的产物，且 setting 与 portable 用各自仓库的 `target/`）。**本机直接跑
`dev.ps1` 时默认关闭**——12 核机器上同时跑四个构建会把机器压死。想在本机试可自行设该环境变量。

实测并行度约 1.9x 而非 4x：三个 cargo 进程会在**全局 package cache 锁**上排队
（输出里的 `Blocking waiting for file lock on package cache`），这是 cargo 的固有行为。

#### worktree 必须各占一个槽位

多个 worktree 共用一台编译机时，若都同步到同一个远程目录会**无声地互相覆盖**——后一次解压
盖掉前一次的源码，产物属于哪个分支全看谁最后跑完。`remote-build.ps1` 因此按 **worktree 目录名
自动派生槽位**（主树派生不出槽位，行为与从前一致），也可用环境变量覆盖：

```powershell
$env:WIND_REMOTE_SLOT = "fx"     # → C:\build-fx\WindInput
$env:WIND_REMOTE_SLOT = "off"    # 关掉槽位，与主树共用目录（串台风险自负）
```

⚠️ 槽位换的是**父目录**，不是主仓的目录名。伴生仓与主仓平级，而 `wind-setting\Cargo.toml` 写死了
三条相对 path 依赖（`../WindInput/wind_input/crates/` 下的 `wind-ipc` / `wind-rpc` / `wind-config`）：
只改主仓目录名的话，wind-setting 仍会去 `..\WindInput` 取那三个 crate，**取到的是主树代码、
编译照样成功、错得毫无提示**。

⚠️ 每个槽位各带一份 `target\`（几十 GB），用完清掉：`remote-build.ps1 -Command <x> -Clean`。

#### 同步语义（三条容易误判的）

- 同步的是**工作树快照**（tar 打包文件系统），不是 git 提交。本地改了没提交的文件照样会过去
  ——包括被设了 `skip-worktree` 的 `docs/VERSION`。
- 反过来，**多会话并发改同一棵树时会抓拍到别人的半成品**。远程构建报的错若指向不在你改动集里
  的文件、且 `git status` 显示它干净，那就是撞上了中间态：别去改那份代码，也别反复重试（结果
  随机），等它收敛。开跑前可用 `rc.ps1 check --workspace --all-targets` 当门——不做 codegen，
  半分钟出结果。⚠️ `check -p <单个 crate>` **不能**当这个门：给枚举加变体时，报错的是所有
  `match` 它的下游 crate，`-p` 只覆盖其中一个。
- 同步是**镜像**：解压后会清掉编译机上多余的文件。这道清理不是洁癖——cargo 把 `tests/`、
  `benches/`、`examples/` 下的每个 `.rs` **自动发现**为独立编译目标，**不需要任何 `mod` 引用**，
  于是本机早已删除的测试文件会带着对已删字段的引用一起炸，而报错指向一个 `git` 和工作区里
  都找不到的文件。清理范围严格等于同步范围（同一份排除清单），`target/`、`build[_dev]/`、
  `dist/`、`.cache/` 一律不碰；只删文件不删目录。逃生口 `-NoPrune`，彻底重来 `-Clean`。

### Linux 交叉（MinGW，`scripts/dev.sh`）

- 编译检查：`cargo check --target x86_64-pc-windows-gnu -p <crate>`（`wind_input/` 下）。
- host 单测：`windows` crate 全员是 `cfg(windows)` 依赖，Linux host 上不参与编译——
  `wind-coordinator` 也可直接 `cargo test -p wind-coordinator`（旧说法「传递依赖 windows
  不能跑」已随 wind-ui 解耦作废；CI 的 test 正是跑在 ubuntu 上）。注意集成测试的
  `build_dev/data` 假绿判据（见下方「注意」）。
- 部署调试版到 Windows：`scripts/dev.sh push debug`（配置见 `scripts/deploy.local`）。

### macOS 本机（`scripts/mac/dev.sh`）

- host 即目标：`cargo check/test --workspace` 全 workspace 可直接跑（含 `wind-coordinator`
  ——`windows` crate 是 `cfg(windows)` 依赖，非 Windows 上不参与编译）。脚本快捷键与
  `dev.ps1` 对齐：`k`/`l`/`t`/`f`/`fmt-check`/`ci`/`hooks`/`clean`/`gd`/`r`。
- 模块映射：Win 的 `m1`(TSF DLL) ↔ mac 的 `m1`(`WindInput.app`，Swift/IMKit)；
  Win 的 `m2`(核心 exe) ↔ mac 的 `m2`(Rust 服务)。全构建 `1`/`d1`，系统安装 `p1`/`pd1`，
  安装包 `8`/`d8` → `.pkg`。
- `.app` 侧单测：`cd wind_macos && swift test`。
- 与 Windows 的功能差距（宿主层）登记在 `wind_macos/AGENTS.md`「与 Windows 的功能差距」。

### 注意

- 部分集成测试依赖 `build_dev/` 下的数据（junction/词库）；**数据缺失时测试族静默跳过，
  0.0x 秒全绿＝假绿**，在 worktree 里跑测试尤其要核对耗时是否合理。

## 版本 / 发布

- 产品版本**唯一真源 = `docs/VERSION`**。构建脚本读取后分发到 5 类产物
  （`wind_input.exe` / `wind_tsf.dll` / `wind_setting.exe` / `wind_portable.exe` / 安装包），
  跨仓经环境变量 `WIND_APP_VERSION` 注入（不经脚本独立构建时各仓自行回退）。
  发版只改 `docs/VERSION` 一处，**不要**手改各仓 `Cargo.toml` 的 `version`。
- CI（release.yml）为 tag-first：以 tag 覆盖 `docs/VERSION` 再构建；仓库里的 `docs/VERSION`
  是开发占位。**切勿添加 `tag == docs/VERSION` 一致性校验**——会破坏手动触发的 `-dev` 占位流程。
- 草稿 Release 的正文由 `scripts/gen-release-notes.sh` 生成（模板在 `docs/release-notes/`）：
  基础信息 + 人工填写区 + 折叠的提交记录。人工填写区由 `<!-- user-facing:start/end -->`
  圈定，**两个下游按此标记取内容**——文档仓 `scripts/sync_release_notes.py`（官网更新记录）
  与 wind-setting `src/update/notes.rs`（应用内升级提示）。占位文本必须恰好是 `暂未填写`
  （Rust 侧按全等判定），前面加 `>` 之类修饰会让占位符被当成正文弹给用户。

## Agent skills

> 供 Matt Pocock 系列工程技能（`to-tickets` / `triage` / `to-spec` / `qa` / `wayfinder` 等）读取的仓级约定。改动这些约定改对应 `docs/agents/*.md`，无需重跑安装技能。

### Issue tracker

本地 markdown：issue 与 spec 存于 `.scratch/<feature>/`（一 feature 一目录，`issues/NN-<slug>.md` 一票一文件）。详见 `docs/agents/issue-tracker.md`。

### Triage labels

五个规范角色标签，标签名与角色名一致（`needs-triage` / `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix`），本地追踪器下写作 issue 文件顶部的 `Status:` 行。详见 `docs/agents/triage-labels.md`。

### Domain docs

单上下文：根 `CONTEXT.md` + `docs/adr/`（均按需惰性生成，缺失时静默跳过）；本仓另有 `AGENTS.md` 与 `docs/architecture/` 作为现状架构文档。详见 `docs/agents/domain.md`。
