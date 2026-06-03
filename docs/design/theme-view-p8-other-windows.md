<!-- 用途: P8 其它窗口几何 View 化（盒模型推广）设计 + 切片计划 -->

# WindInput 主题 v2.6 — P8：其它窗口几何 View 化

> 状态：**已完成（2026-06-03，toolbar 切片4 延后）**。切片 0/1/2/3/5/6 全部合入 main 并经 `dev.ps1` 实测；toolbar 单列为独立重构工程（见切片4 说明）。
> 完成提交：`e6193be5`(切片0-2) · `c7efe8cd`(切片3) · `51e1fab4`(切片5) · `8e9b5be3`(切片6) · `c490ae9d`(切片6 圆角修复：菜单边框透明 + status/tooltip/toast 深色毛边)。
> 关联：`theme-v26-freeze-report.md`（候选窗冻结契约 + 第八节钦定 P8 为非破坏扩展）、`theme-view-architecture.md`（P0 设计 + 冻结声明）、`pkg/theme/AGENTS.md`、`internal/ui/AGENTS.md`。
>
> **切片6 收尾补记（圆角渲染两坑，勿重踩）**：① 菜单圆角边框半透明透出下方——`popup_menu_render` 内圆角 clip 须裁到**完整 radius**（非 `radius-borderWidth`），让边框抗锯齿像素有不透明底垫；② status/tooltip/toast 圆角深色毛边——**深色半透明底 + 浅色背景图**时 `blendOver` 用底色 alpha 门控图，边缘透出底色，须让底色**不透明且与图同色调**（候选窗用不透明白底故无此问题）。带图半透明窗口的配法：底色与图同调 + 底色 alpha>0（`transparent` 会令图消失）。

## 一、背景与目标

P7 已把**候选窗** schema 升级为完整盒模型（几何 + 字体 + 背景图 + layers + states）并**冻结**（2026-06-02）。但其它 5 类窗口仍停在 P4 时代：

| 窗口 | 当前状态 |
|---|---|
| toolbar / menu / tooltip / status | 颜色已 View 化（扁平 `ResolvedXxxViews` 色集），**几何仍 hardcode** |
| toast | **完全未 View 化**（仅 palette 预留，连颜色 schema 都没有） |

**P8 目标**：把候选窗那套完整盒模型能力**非破坏地**推广到全部 5 类窗口，使其也能配置几何/字体/背景图/layers/states，并最终能被 P3 编辑器统一编辑。冻结报告第八节已钦定本扩展为「新增可选字段、非破坏、无需迁移」。

## 二、设计决策（已与用户确认）

1. **粒度 = 完整盒模型**：几何 + 字体 + 背景图 + layers + selected/hover/disabled states，与候选窗对齐（不止 margin/padding/border）。
2. **批次 = 分批垂直切片**：先单节点窗口跑通全管线，再推广到多元素窗口，最后 toast。
3. **零回归 = 不追过程零回归**：过程中不做逐像素 golden 守护；重心放在引擎/schema 正确，**最终通过调 `default`/`msime` 主题 YAML + 内置默认把视觉拉回"和之前差不多"**。（用户原则：差异最终可用更新主题解决，引擎正确性优先。）
4. **架构 = 统一 ViewNode 骨架**：其它窗口复用候选窗的通用 `ViewNode` 节点类型，各自定义"具名 View 集"；引擎与编辑器一套逻辑通吃。现有专用扁平结构（`ToolbarViews`/`MenuViews`）重构为 ViewNode 组合。
5. **范围 = 5 窗口全做，toast 排最后**。

## 三、架构

**核心事实**：渲染核心早已统一——status/tooltip/menu/toolbar 全部已用包级 `Layout`/`PaintTree` + `newSharedDrawContext`（候选窗同款引擎）。**P8 不碰渲染核心**，只改"schema → View 树"的构建 + 解析层。

每个窗口的改动都是同样的三层：

1. **schema 层（`pkg/theme/views.go`）**：每窗口定义一组"具名 ViewNode 集"。
   - status / tooltip / toast：单节点，直接是 `*ViewNode`（status/tooltip 现成；toast 新增）。
   - menu / toolbar：从扁平色集结构重构为具名 ViewNode 集。
2. **解析层（`pkg/theme`）**：抽通用解析器
   ```
   resolveViewNode(n ViewNode, resolveColor func(token string) color.Color,
                   defBg, defBorder, defText color.Color) RVNode
   ```
   复用 `resolveState`（泛化为接受 `resolveColor`）+ `toRVImage`。候选窗 `ResolveCandidateViews` 改为调它（零行为变化，作为重构验证）。各窗口新增 `ResolveXxxViews` 注入自己的 palette token resolver。
3. **构建层（`internal/ui/viewbox_xxx.go`）**：`buildXxxTree` 从 hardcode 改为读解析出的 `RVNode` 集（padding/border/font/bg image/layers/states 全部来自 schema）。

**收益**：一份盒模型解析逻辑全窗口复用；背景图/layers/states 能力自动对所有窗口生效；编辑器一套 ViewNode 渲染/编辑逻辑覆盖 6 类窗口。

## 四、切片计划

每切片独立 commit、可独立 `dev.ps1` 验收。每切片标准动作：**schema 扩展 → 泛化解析 → build 消费 → 引擎正确性单测 → YAML 默认/示例**。

| 切片 | 窗口 | 要点 |
|---|---|---|
| **0** | 泛化解析器地基 | 抽 `resolveViewNode` 通用 helper + `resolveState` 泛化 `resolveColor`；候选窗 `ResolveCandidateViews` 改调它。**零行为变化**，纯重构，跑候选窗既有单测验证。 |
| **1** | status（试点） | 单节点 + YAML 已是 `*ViewNode`，改动最小。`buildStatusTree` 从 hardcode 改读解析的 RVNode（padding/border/font/bg image/layers）。`ResolvedStatusViews` 由"2 色"升级为"持 RVNode"。确立模式。 |
| **2** | tooltip | 单节点 + 多列对齐，复用切片1模式。root 几何/外观 views 化；多列文本仍 hardcode 列算法。 |
| **3** | menu | 多元素 + hover/disabled 态 → `ViewNode.Hover`/`Disabled` patch。`MenuViews` 重构为具名 ViewNode 集（root/item/separator）。 |
| **4** | toolbar | **延后（2026-06-03 用户决定）**：toolbar 需更全面的重构——不止几何 views 化，还要按钮尺寸/布局**动态化**；且其几何（gripWidth/buttonWidth/buttonPadding）与 `HitTest`/`GetButtonBounds` 命中测试**强耦合**，渲染改几何来源时命中必须同步。单列为独立工程，不在本次 P8 范围。当前 toolbar 已有颜色 View 化（P4-C，`ResolvedToolbarViews` 扁平色集 + `views.toolbar` token 覆盖 + 中/英 mode 态），几何/外观盒模型化待该独立重构。 |
| **5** | toast | 从零补颜色 schema（palette.Toast 已有）+ 几何 + 盒模型。新增 `Views.Toast *ViewNode` + `ResolveToastViews`。 |
| **6** | image/layers 横切 | 提取共享 `imageResolver`（从 `Renderer.imageForRef`/`fillFor`/`appendThemeLayers` 抽出位图缓存基础设施，候选窗委派、零行为变化），给 5 个独立窗口统一接入 background image + layers。**切片 1–5 只做几何/border/font/颜色**（不碰候选窗 `Renderer`、零耦合低风险）；本切片统一补位图能力——复刻候选窗自身「P2 几何 → P7 位图」的节奏。 |
| **收尾** | 主题补偿 | 调 `default`/`msime` 的 YAML views 块 + 内置默认，把视觉拉回"和之前差不多"；更新 `pkg/theme/AGENTS.md` + `internal/ui/AGENTS.md`；冻结报告补「P8 完成」。 |

## 五、零回归策略

- **过程**：不做逐像素 golden。每切片只保证：①引擎正确性单测（解析/几何计算正确）②`go build`/`go vet`/`go fmt` 干净 ③该窗口 `dev.ps1` 能正常显示（不崩、布局合理）。
- **最终**：收尾切片统一调 `default`/`msime` 主题 YAML + 各 `buildXxxTree` 的内置默认值，使视觉"和之前差不多"。允许中间切片视觉与旧版有偏差。

## 六、契约对齐（必须遵守候选窗冻结契约）

- ViewNode 字段语义与候选窗完全一致（margin/padding/border/background/font/layers/states + 显式 0 语义 + `${token}` 颜色 + `transparent`）。
- 各窗口 palette token 表沿用 `pkg/theme` palette 既有组件色（`toolbar`/`popup_menu`/`tooltip`/`status`/`toast`）。
- 新增字段一律 `omitempty` 可选指针/零值回退，不破坏既有主题解析。
- 编辑器约束（冻结报告第八节）：方向当维度、透明当常规色值、容忍未知扩展字段——P8 schema 不得写死单方向/不透明假设。

## 七、验收

- 全部 6 切片完成；`go build`（主 + wind_setting）+ `go test ./...` + `go vet` 干净。
- `default`/`msime` 两主题下 5 类窗口视觉与 P8 前"差不多"（用户 `dev.ps1` 核对）。
- 至少一个窗口端到端验证背景图/layers/states 可配（沿用候选窗 `jidian-classic` testdata 模式，或新增其它窗口样例）。
- AGENTS.md + 冻结报告同步。
