<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-06-10 -->

# pkg/config

## Purpose
应用配置的完整定义、加载/保存逻辑、路径管理和运行时状态持久化。配置文件为 **TOML 格式**（`config.toml` / `state.toml` / `compat.toml` / `schema_overrides.toml`），存储在用户数据目录（Windows `%APPDATA%\WindInput\`、macOS `~/Library/Application Support/WindInput/`，由 `os.UserConfigDir()` 解析）。采用**桥接式编解码**（见 `codec.go`）：TOML 仅作为磁盘表面格式，struct ↔ map 转换统一走 yaml tag 管线，**无 toml tag**。旧版 `.yaml` 文件双读回退 + 首次加载/保存时一次性迁移（旧文件改名 `*.migrated.bak`）。`GetConfigDirDisplay`/`GetLogsDirDisplay` 返回平台友好的显示串（Windows 用 `%APPDATA%`/`%LOCALAPPDATA%` 占位串，其余平台显示真实路径并把 home 缩写为 `~`）。支持三层配置合并机制。**自定义数据目录（`datadir.conf` / `ReadUserDataDirOverride`）仅 Windows 支持；macOS 约定固定用 `~/Library/Application Support/WindInput`，`ReadUserDataDirOverride` 在 darwin 始终返回空（忽略残留 conf），设置端也禁用「更改数据目录」入口。**

## Key Files
| File | Description |
|------|-------------|
| `config.go` | `Config` 结构体（含所有子配置）、`Load()`/`LoadFrom()`/`Save()`/`SaveTo()`/`DefaultConfig()`，三层加载逻辑，yaml 序列化标签（TOML 桥接复用） |
| `codec.go` | TOML 桥接编解码层：`IsTOMLPath`/`LegacyYAMLPath`/`normalizeToYAML`（TOML→map→YAML）/`marshalTOML`/`marshalForPath`/`readFileWithLegacyFallback`/`renameLegacyFile`、`MigratedBackupSuffix` 常量 |
| `clone.go` | `Config.Clone()` 反射式深拷贝（自动覆盖全部导出字段，新增字段无需改此文件）。**红线：异步持久化（`go config.Save(...)`）必须先 Clone，禁止 `cfgCopy := *cfg` 浅拷贝**——浅拷贝共享底层 map/slice，与前台并发修改会触发 concurrent map 硬 panic。防漂移由 `clone_test.go` 反射别名检查守护 |
| `paths.go` | 路径常量（`AppName`、`DataSubDir`、`ConfigFileName`、`Legacy*FileName` 等）和辅助函数（`GetConfigDir`、`GetDataDir`、`GetSystemConfigPath`（toml 优先、回退旧版 yaml）、`EnsureConfigDir` 等） |
| `config_hotkey.go` | `HotkeyConfig`：热键字符串配置（`ToggleModeKeys`、`SwitchEngine`、`DeleteCandidate`、`PinCandidate`、`ToggleToolbar`、`OpenSettings`、`AddWord` 等） |
| `state.go` | `RuntimeState`：运行时状态持久化（中英文模式、全角、标点、工具栏位置 `ToolbarPositions`、候选窗固定位置 `CandidatePinPositions`），`LoadRuntimeState`/`SaveRuntimeState` |
| `compat.go` | `AppCompat`/`AppCompatRule`：按进程名匹配的兼容性规则（`caret_use_top`、`skip_caret_pending`、`pin_candidate_position`）；`LoadAppCompat`（系统预置 + 用户层合并）、`ToggleUserSkipCaretPending`、`ToggleUserPinCandidatePosition` |

## For AI Agents

### Working In This Directory
- `Config` 顶层字段：`Startup`、`Schema`、`Hotkeys`、`UI`、`Toolbar`、`Input`、`Advanced`
- **TOML 桥接（重要设计约束，见 codec.go 包注释）**：读 = `toml→map→yaml.Marshal→yaml.Unmarshal(已填充 struct)`，写 = `ComputeYAMLDiff 产出 map→toml.Marshal`。这样保留了两个关键语义：① yaml.v3 部分覆盖（三层合并的根基）；② `yaml.TypeError` 部分解码（自愈分支依赖，TOML 阶段只会产生语法错误 → 走"损坏"分支）。**不要给配置 struct 添加 toml tag，不要绕过桥接直接 toml.Unmarshal 进 struct**
- **三态字段约定**：`*bool`/`*string` + `omitempty`，"未设置"以**键缺失**表达（TOML 无 null，这是唯一可行编码）；`marshalTOML` 会防御性剔除 nil 值
- **旧版 YAML 迁移**：`config/state` 在加载时迁移（`readFileWithLegacyFallback` 回退读 + 成功写出 TOML 后 `renameLegacyFile`）；`compat` 在 toggle 写出时迁移；`schema_overrides` 读时回退、保存/删除时迁移。旧文件改名 `*.migrated.bak`，写出失败则保留旧文件下次重试
- **三层配置加载**（`Load()` / `LoadFrom()`）：
  1. 代码默认值（`DefaultConfig()`）
  2. 系统预置配置（`data/config.toml`，通过 `GetSystemConfigPath()` 定位，缺失时回退旧版 `data/config.yaml`）覆盖
  3. 用户配置（`%APPDATA%\WindInput\config.toml`）覆盖
- **Schema 方案系统**：`SchemaConfig`（`Active` + `Available` 字段），用于多方案切换（`wubi86`/`pinyin`）
- **新增 HotkeyConfig 字段**：`DeleteCandidate`（删除候选）、`PinCandidate`（置顶候选）、`ToggleToolbar`（切换工具栏）、`OpenSettings`（打开设置）、`AddWord`（快捷加词，默认 `ctrl+equal`）
- **新增 UIConfig 字段**：`TextRenderMode`（`directwrite`/`gdi`/`freetype`）、`GDIFontWeight`、`GDIFontScale`、`MenuFontWeight`、`MenuFontSize`
- **新增枚举**：`PagerBarDisplay`（`"" | "always" | "auto" | "hide"`），控制翻页栏显示方式的用户级覆盖；`PageNumberDisplay`（`"" | "show" | "hide"`），控制页码文字显示方式；空字符串（Default）均表示跟随主题配置
- **新增 UIConfig 字段**：`PagerBarDisplay`（`pager_bar_display`），空值=主题配置，always=总是显示，auto=大于一页时显示，hide=完全隐藏翻页栏（含箭头）；`PageNumberDisplay`（`page_number_display`），空值=主题配置，show=显示页码，hide=隐藏页码
- **新增 AdvancedConfig 字段**：`HostRenderProcesses`（Band 窗口宿主进程白名单，默认 `["SearchHost.exe"]`）
- **新增 UIConfig 字段**：`CmdbarCandidatePrefix *string`（`cmdbar_candidate_prefix`），副作用命令直通车候选的渲染前缀；nil=默认 "⚡"，""=完全不显示，其他字符串=自定义符号。使用 `UIConfig.GetCmdbarCandidatePrefix()` 取值。
- **新增 UIConfig 字段**：`FontSizeFollowTheme bool`（`font_size_follow_theme`），候选字号是否跟随主题 `behavior.font_size`：true=跟随（忽略 `FontSize`），false=用 `FontSize` 自定义。**yaml omitempty + json 不带 omitempty**（前端需总收到显式 bool）。**保守迁移**：`LoadFrom` 用探针检测用户文件是否含该字段，缺失（老配置）→ 置 false 自定义保留现字号；`DefaultConfig()` 设 true（新装无用户文件、提前返回默认，故跟随主题）。
- **新增 UIConfig 字段（V3-D 主题 behavior 用户覆盖层，哲学Y）**：为主题 `behavior` 三字段补全用户覆盖通道，与 `FontSize`/`FontSizeFollowTheme` 同模式——`AlwaysShowPager` + `AlwaysShowPagerFollowTheme`、`ShowPageNumber` + `ShowPageNumberFollowTheme`、`VerticalMaxWidth` + `VerticalMaxWidthFollowTheme`。最终值 = `*FollowTheme ? 主题 behavior : config.UI 用户值`。所有 `*FollowTheme` **默认 true**（新装跟随主题），**不可加 omitempty**（默认 true 的 bool 会破坏 diff-save/merge-on-default 闭环——同 FontSizeFollowTheme）。消费：`coordinator` 经 `uiManager.SetBehaviorOverrides(...)` 下发到候选窗渲染器（`renderer.applyBehaviorOverrides` 应用 pager/page_number、`viewbox_render` 每帧据 follow 标志选 vertical_max_width 源）。round-trip 守护见 `behavior_override_test.go`。
- **新增 InputConfig 字段（2026-06-09）**：`SpecialModes []SpecialModeConfig`（`special_modes`，引导键自定义码表特殊模式实例列表，空=关闭，无默认值）。`SpecialModeConfig` 字段：`ID`/`Name`/`TriggerKeys`/`Table`(相对 schemas 目录)/`AutoCommit`(常量 `SpecialAutoCommitPrefixFree`/`FixedLength`/`Manual`)/`FixedLength`/`ForceVertical`/`AccentColor`/`ShowAllOnEntry`(进入即列全表，默认 false)，预留 `CodeCharset`/`Schemes`/`Engines`；`Validate()` 校验单实例（id/trigger_keys/table 非空、auto_commit 合法、fixed_length 档要求 >0）。消费见 `internal/coordinator/special_mode_registry.go` 与 docs/design/special-mode-codetable.md
- 新增配置项时：在对应子结构体添加字段，设置 **yaml 标签**（TOML 桥接复用 yaml tag，无需 toml tag），在 `DefaultConfig()` 中提供默认值，在 `applyConfigFallbacks()` 中处理兜底
- `RuntimeState` 与 `Config` 分开存储（`state.toml`），避免用户编辑配置时覆盖运行时状态
- 数据根目录通过 `GetDataDir(exeDir)` 获取（`exeDir/data`），词库和 Schema 文件均位于此目录下
- 配置热重载通过 `control` 管道触发，`coordinator.UpdateHotkeyConfig` 等方法应用变更

### Testing Requirements
- YAML/TOML 序列化/反序列化可做单元测试；TOML 桥接、旧版迁移、损坏自愈、三态键缺失语义的回归用例见 `codec_test.go`
- 路径函数在 Windows 环境测试（依赖 `os.UserConfigDir()`）；用 `setTestConfigDir(t)`（`schema_overrides_test.go`）把配置目录重定向到隔离临时目录

### Common Patterns
- 路径函数返回 `(string, error)`，调用方在错误时回退到 exeDir
- `GetDataDir()` 直接返回 `string`（无错误，相对于 exeDir 的绝对路径）
- `FuzzyPinyinConfig` 包含 11 个独立开关（含 `IanIang`、`UanUang`），都可独立启用
- `applyConfigFallbacks()` 处理旧格式迁移（如 `theme:"dark"` 迁移到 `theme_style:"dark"`）

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3` — struct ↔ map 编解码管线（yaml tag 驱动）+ 旧版 YAML 文件解析
- `github.com/pelletier/go-toml/v2` — TOML 磁盘格式编解码（仅 map 级，经桥接使用）

<!-- MANUAL: -->
