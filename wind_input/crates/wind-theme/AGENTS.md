<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-06-29 -->

# wind-theme

## Purpose
主题系统（schema v3）。从 TOML 文件加载主题，经扁平→嵌套归一化、base 单链继承深合并、类型化解析，最终求值为 `Resolved`（调色板 + `RvNode` 盒模型树 + 行为配置 + 图片资源），供 wind-ui 渲染消费。无 Windows 平台依赖，可在任意 host 完整测试。

## Key Files
| File | Description |
|------|-------------|
| `src/lib.rs` | 对外导出：`Resolved`/`ResolvedBehavior`、`RvNode`/`RvViews`/`RvImage`/`RvGradient`、`load_resolved`/`load_resolved_dirs`/`resolve`、`Meta` 等顶层入口 |
| `src/schema.rs` | 类型化 v3 schema：`Dim`（dp/px/%）、`Ld`（light/dark 解析中间形态）、`ViewNode`/`Views`/`Theme`/`Behavior` 等，base 深合并后的 serde 解析目标 |
| `src/theme.rs` | TOML 加载 + base 单链继承深合并：`load_typed_dirs`（Value 层合并→normalize→类型化）、`find_theme_dir`、`theme_chain_dirs`（资产目录链）、`validate_text`（导入前校验） |
| `src/normalize.rs` | 扁平人写 TOML → 规范嵌套形态：顶层视图表收入 `views.*`、`radius`→`border.radius`、`margin/padding/slice` 简写展开、`shadow.offset=[x,y]` 拆分为 `offset_x/y`、`position_offset=[x,y]`→`{x,y}`、toolbar button `chinese/english`→`mode.*` |
| `src/palette.rs` | 调色板解析：colors 段 → `HashMap<String, Rgba>`，支持 `${var}` 多跳递归引用 + 环检测 + `{light,dark}` 变体选取 |
| `src/resolve.rs` | 求值入口：typed `Theme` → `Resolved`；`resolve_view_node`/`resolve_state` 通用节点/状态 patch 求值；`load_resolved_dirs` 便捷入口 |
| `src/skin.rs` | 异形皮肤 `[skin]`：schema、求值为 `RvSkin`（横 / 竖 / 嵌入变体，竖排不回退横排）、base 链上变体整表替换；设计见 docs/design/theme-skin-animation.md |
| `src/rvnode.rs` | 渲染消费形态：`RvNode`（颜色已解析为 `Rgba`、几何保持 `Dim` 符号态）、`RvViews`（全节点集合 + 列表级几何）、`RvImage`/`RvGradient` |

## For AI Agents

### Working In This Directory
- **存储格式是 TOML，不是 YAML**：Go 版用 YAML，Rust 版改为 TOML（主题文件名 `theme.toml`）。schema v3 结构大体一致，但有以下**刻意精简**：
  - **不做 derive**：Go 版有 `primary`→语义色自动派生；Rust 版要求主题显式给全语义色，`resolve_palette` 直接忽略 `colors.derive` 块（`Ld::Variant` 无 light/dark 时 `select()` 返回 `None`，被跳过）。
  - **validate 降级为 warn**：未解析的 `${token}` / 缺失 ref 只记 `tracing::warn` 并返回 `None`，不 fail fast；外部坏主题不黑屏，渲染器内置默认兜底。
- **数据流顺序不可逆，改任一层须保持顺序**：原始 TOML → `normalize_theme`（归一化 `toml::Value`）→ `Value::try_into::<Theme>`（类型化）。base 深合并在 `toml::Value` 层完成（`merge(base, over)`），合并后才归一化+类型化——这样 base 扁平写法也能被正确归一化。不得在合并前对子主题单独类型化。
- **`Ld` 只是解析中间形态**：`Ld::select(is_dark)` 在 `resolve_color` 中坍缩为单值后进入 `RvNode`。`RvNode` 中颜色字段类型是 `Option<Rgba>`，不存 `Ld`。新增颜色字段时遵循此分层，不得把 `Ld` 带入 `RvNode`。
- **几何字段 `Dim` 进 `RvNode` 仍保持符号态**（`Option<Dim>`），paint 期 `Dim::resolve(scale, host)` 才换算为像素。不得在 `resolve` 层提前换算。
- **state patch 不合并几何**：`resolve_state` 判定"有覆盖"的条件是色/图/渐变/层/字重等，纯几何 patch（只改 padding 等）返回 `None` 丢弃（防止候选框状态切换时跳动，`state_geometry unsupported`）。新增状态字段前确认是否属于此豁免范围。
- **「默认值」不能靠 `RvViews` 的 derive `Default`**：数值型字段（非 `Option`）的零值等于「算成 0」，不等于「用出厂默认」。典型是 `accent_bar_height_ratio`——derive 给 0.0 会让条高算成 0（钳到 2px 细线），观感是强调条消失。此类字段须在 `resolve` 期显式写默认值（常量放 `rvnode.rs`，如 `DEFAULT_ACCENT_BAR_HEIGHT_RATIO`），**且消费端再判零兜底**，两处缺一都会让未配该项的主题（含全部内置主题）出问题。
- **给 `views.*` 加字段后要检查渲染层是否真消费**：本 crate 只负责「解析 + 求值」，字段进了 `RvViews` 不代表生效。历史上 `accent_bar.height_ratio`/`.offset`、`toolbar.border.radius`/`.width` 都曾停在这一层，schema 有、resolve 有、渲染硬编码，主题写了毫无变化，而编辑器照 schema 开放了控件 —— 用户看到的是「配了不生效」。加字段的最后一步是 grep `wind-ui` 确认消费点存在。
- **资产目录链 `Resolved.asset_dirs`**：`asset_dirs[0]` 为 self 主题目录（resources 相对路径基准），后续为 base 链目录（`theme_chain_dirs` 返回）。字面 image ref（如 `_base` 的 `chevron.svg`）需到 base 目录查找。加载路径逻辑在 `theme.rs::find_theme_dir`，靠前目录优先（用户目录可覆盖内置）。

### Testing Requirements
- `cargo test -p wind-theme` 可在任意 host（包括非 Windows）直接运行，crate 无平台依赖。
- 内置主题测试依赖 `data/themes/`（由 `env!("CARGO_MANIFEST_DIR")` 相对定位，在仓根 workspace 目录下执行即可找到）；jidian-classic 等额外主题在 `crates/wind-theme/testdata/themes/`。
- 测试覆盖：`Dim`/`Ld` 解析与求值语义、normalize 各变换规则（扁平视图表、radius/shape 下沉、edges 简写、toolbar button mode 展开）、颜色解析（hex/`${var}` 多跳/`{light,dark}`/环检测）、base 多目录继承端到端、`resolve` 端到端（default/jidian-classic × light/dark）、state gating（纯几何 patch 应丢弃）。

## Dependencies

### Internal
- 无（wind-theme 无内部 crate 依赖）

### External
- `serde`（反序列化）、`toml`（TOML 解析 + `toml::Value` 中间层）
- `anyhow`/`thiserror`（错误处理）
- `tracing`（warn 降级，不 panic）
- `image`/`resvg`（SVG 栅格化能力，位图解码不在 wind-theme 路径内，由 wind-ui 按 ref 缓存）

## 全局约束
- INFO 日志不含颜色 token 原始值（可含 token 名），不含用户输入；见根 `AGENTS.md`。
- 改完跑 `cargo fmt --package wind-theme`。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
