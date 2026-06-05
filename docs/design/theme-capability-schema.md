<!-- 用途: 主题能力声明 schema 设计 + 前后端统一契约 -->

# WindInput 主题能力声明 Schema（Capability Manifest）

> 状态：设计已定（前后端共享单一数据源 / Go 权威 + 导出 JSON / 三态模型）。
> 关联：`theme-v3-freeze-report.md`（v3 冻结契约）、`pkg/theme/AGENTS.md`、`internal/ui/AGENTS.md`。

## 一、动机

v3 schema 冻结后，字段散落各处、且渲染消费不均：有的字段已渲染（真能力）、有的 schema 占位但渲染未实现（**假字段**，如 `shadow.blur`/`spread`）、有的字段对某 view 概念上无意义（如 `status` 无交互状态）。Web 编辑器（独立仓 `WindInputThemeEditor`）若按 schema 全量暴露控件，会误导用户配置不生效的字段。

**能力声明 schema** 给出一份权威清单：每个 view 对每个能力维度的支持状态。前端编辑器据此显示/灰显/隐藏控件，后端引擎据此明确渲染/忽略，文档从它生成。三者**单一数据源**，根治"声明 vs 实现"漂移。

## 二、核心模型

### 三态

```go
type CapabilityStatus string
const (
    CapSupported   = "supported"   // 已渲染消费（真能力）
    CapReserved    = "reserved"    // schema 有、渲染未实现（假字段，如 shadow.blur/spread）
    CapUnsupported = "unsupported" // 该 view 概念上不支持（如 status 无状态）
)
```

编辑器映射：`supported`→正常控件；`reserved`→灰显 + "未生效"角标；`unsupported`→不渲染。
引擎映射：`reserved`/`unsupported`→忽略（不渲染，可选 warn）。

### 维度粒度

按**用户可感知的能力单元**划分（非每个 schema 叶子字段，避免矩阵爆炸）。能力键白名单：

| 分组 | capability key |
|---|---|
| 盒模型 | `padding` `margin` `border` |
| 颜色 | `background_color` `text_color` |
| 背景填充 | `background_image` `background_gradient` `background_shape` |
| 字体 | `font`（size/weight/family 合一） |
| 状态 | `state_selected` `state_hover` `state_disabled` · `state_geometry`（状态态能否覆盖几何） |
| 层/阴影 | `layers` `shadow_offset` `shadow_blur_spread` |
| 间距 | `line_spacing` `col_gap` `title_gap`（提示窗）· `item_spacing` `band_gap` `row_gap`（候选列表：横向 / band / 纵向） |
| 节点专有 | `index_shape` `index_labels` `accent_bar` `footer_arrow_image` `pager` `mode_states` |

某 view 的 caps 中未列出的键 = 隐式 `unsupported`（该能力对该 view 不适用）。

### view 主体（subjects）

候选窗：`window` `preedit_bar` `candidate_list` `item` `index` `text` `comment` `accent_bar` `footer_bar` `mode_label`
其它窗口：`status` `tooltip` `toast` `menu.root` `menu.item` `menu.separator` `toolbar`

## 三、真相源与数据流

- 权威定义：`pkg/theme/capability.go` 的 `ThemeCapabilities []ViewCapability`（Go，引擎可直接读）。
- 导出：`MarshalCapabilities()` → `docs/design/theme-capabilities.json`（仓内可见、随 PR diff）。
  golden 测试 `TestCapabilitiesJSON` 同时落盘 + 守护"矩阵改了必须重落"。
- 跨仓到达前端：编辑器把 `theme-capabilities.json` 复制/内置消费（同步一个 JSON 文件，远比同步整套 Go 类型轻）。
- 防漂移：矩阵人工维护（类 AGENTS.md 纪律），改渲染时同步对应格子；`TestCapabilities_WellFormed` 校验状态值合法、能力键在白名单、view 名在白名单（防 typo），但"是否真被 paint"靠 code review。

JSON 形态（稳定、语言无关）：

```json
{
  "version": 1,
  "views": [
    { "view": "window",  "caps": { "padding": "supported", "shadow_blur_spread": "reserved", "background_gradient": "supported" } },
    { "view": "item",    "caps": { "padding": "supported", "state_disabled": "unsupported", "background_gradient": "supported" } },
    { "view": "tooltip", "caps": { "line_spacing": "supported", "state_hover": "unsupported" } }
  ]
}
```

（`encoding/json` 对 map 键按字母序输出，确保确定性。）

## 四、本次范围（一次性交付 A–D）

| 阶段 | 内容 |
|---|---|
| A | 能力矩阵 Go 结构 + 全 view 三态填表 + JSON 导出 + golden 守护 + well-formed 单测 |
| B | 渲染补全·间距字段：`ViewNode` 加 `LineSpacing`/`ColGap`/`TitleGap *Dimension`；消费层 `viewbox_tooltip`/`toast_renderer`/footer 读取（nil=现状兜底，零回归）；矩阵转 `supported` |
| C | 渲染补全·菜单 padding：`viewbox_menu` 让 root 消费左右 padding、item 上下 padding 独立生效（仿候选项 `FixedH=itemH+padT+padB`）；矩阵转 `supported` |
| D | AGENTS.md（pkg/theme + internal/ui）+ 冻结报告补能力声明一节 |

**全 view 一次性填表**（声明轻量，全覆盖才有统一标准价值）；渲染补全只做 B/C 明确的格子。编辑器侧改动属独立仓，本次只交付 Go 权威 + JSON + 契约，不改编辑器代码。

### 间距字段归属（B）

- `ViewNode.LineSpacing *Dimension`：tooltip 行距（兜底 2）、toast 行距（兜底 4）。
- `ViewNode.ColGap *Dimension`：tooltip 多列列距（兜底 16）。
- `ViewNode.TitleGap *Dimension`：toast 标题与正文距（兜底 6）。
- footer 翻页箭头左右 padding：**复用 `footer_bar.Padding.Left/Right`**（已有字段，pager 读取，兜底 6），不新增专有字段。
- toast `accent_bar` 宽度/内缩等视觉签名常量保持 hardcode（非间距，不纳入）。

新增字段进入 `RVNode`（`LineSpacing`/`ColGap`/`TitleGap Dimension`），由 `resolveViewNode` 填充；**不纳入 golden dump**（默认零值不影响既有渲染，新值由独立单测守护，golden 零变化）。

### 候选项 disabled 的处理（澄清）

候选项无禁用业务语义（`Candidate` 无 disabled 字段、无触发源），故 `item/index/comment` 的 `state_disabled` 在矩阵中标 `unsupported`，**不新增无触发源的假字段**。菜单项 `state_disabled` 已完整实现（`MenuItem.Disabled`），标 `supported`。

### 状态态覆盖范围的处理（澄清）

状态态 patch（`selected`/`hover`/`disabled`）schema 上是完整 `ViewNode`，但**渲染只消费颜色/边框/字重覆盖**——`resolveState` 的"有无覆盖"判定不看几何、`effectiveNode` 合并时也不碰 `padding`/`margin`/`font_size`。即"选中态改间距/字号"是声明可写、渲染必忽略的假字段。

故新增单一能力键 `state_geometry`（粒度="状态态能否改几何"，非每个几何叶子 × 每个状态，避免矩阵爆炸），在所有有状态的 view（`item`/`index`/`text`/`comment`/`menu.item`）标 `unsupported`。编辑器据此在状态态编辑器里隐藏/灰显几何控件，只留颜色/边框/字重。转 `supported` 须先补齐 `resolveState`+`effectiveNode` 的几何消费并重做 golden。详见 `theme-dimension-inheritance.md`。

## 五、回归判据

- `TestV3GoldenSnapshot` 全程逐字节绿（本次改动设计为 golden 零影响）。
- 新增 `TestCapabilities_WellFormed` / `TestCapabilitiesJSON`（矩阵合法性 + JSON 落盘守护）。
- 新增间距字段 + menu padding 的独立单测（tooltip/toast 行列距、menu root/item padding 生效且默认零回归）。
- 双 module `go build/vet/test` + `gofmt` 全绿。

## 六、变更规则

- 能力维度键白名单、view 主体白名单变更须同步本文档 + `capability.go` 注释。
- 渲染补全把格子从 `reserved`/`unsupported` 转 `supported` 时，必须同时落地真实渲染消费（不得空转声明）。

## 七、编辑器消费契约：能力三态 × 继承三态（正交）

编辑器（独立仓 `WindInputThemeEditor`；`wind_setting` 只编辑 `ui.*` 高层覆盖，不碰 `views` 盒模型）消费两套正交的三态，组合决定每个尺寸控件的呈现：

| 维度 | 三态 | 决定 | 数据源 |
|---|---|---|---|
| 能力声明 | supported / reserved / unsupported | 控件**显不显示**（reserved 灰显角标、unsupported 隐藏） | `theme-capabilities.json` |
| 继承（值是否填写） | 空 / 0 / N | 控件**填没填值**（空=继承占位） | 主题 JSON 的 key 存在性 |

### 继承三态 ↔ `*Dimension` 序列化（round-trip 铁律）

后端尺寸字段是 `*Dimension`（nil=继承、`&{0}`=显式 0），`omitempty` 使 nil 省略、`0` 写出。继承语义只有在序列化保留时才成立，故编辑器须遵守：

| 控件状态 | 含义 | 序列化 |
|---|---|---|
| 空（placeholder 显示**继承来的有效值**，灰字） | 继承 base/默认 | **省略该 key** |
| 填 `0` | 显式覆盖为 0 | 写 `0` |
| 填 `N` / 切 px | 显式覆盖 | 写 `N` 或 `"Npx"` |

**铁律：清空控件 ⇒ 删除 JSON key（回到继承），绝不写 0**——否则把"继承"悄悄变成"显式 0 覆盖"，污染 base 单链继承。加载主题时缺省的 key 渲染为"空 + placeholder（有效值）"，而非"0"。round-trip 测试：加载→不动→保存的 diff 必须为空；清空字段→对应 key 消失。

### 单位（dp/px）是正交第三维

`Dimension` 支持 `8`（dp，随 DPI 缩放）/ `"8px"`（设备像素，不缩放）。数值控件宜配单位切换，分别序列化为裸数字 / `"Npx"`。这是"值本身的单位"，**不要与"空=继承"混淆**。

详见 `theme-dimension-inheritance.md`（继承语义现状审计 + 三个残留缺口）。
