# 渲染文字的分段着色：主题角色色 + 模板内联颜色

> 2026-09-27 设计；§16 各项均已于同日确认，无待确认项。同日按设计审查意见修订（I1～I9 与 16 条次要项）。
> 前身：`candidate-comment-layering.md`（注释模板语法）、`candidate-tooltip-sections.md`
> （气泡段列表与 `TooltipDoc`；其 §9「段标题的独立样式……属主题能力扩展，另立」的**颜色**部分
> 由本设计承接，加粗仍不做）。

## 1 问题

候选注释与悬停提示的文字，现在整段只有一个颜色：

- View 的文字叶子只有一个 `text_color`（`wind-ui/src/view.rs` `View`），
  `TextRenderer::draw` 也只收一个 `color`（DirectWrite / CoreText / mock 三个后端同契约）。
- 主题只能给整个节点配色：`[comment] color`、`[comment.selected] color`、`[tooltip] color`。

而模板恰恰是把**多种信息拼进一行**的工具：

| 场景 | 渲染结果 | 看不出来的 |
|---|---|---|
| 出厂注释 `${code_hint\|code_rev\|shuangpin}` | `kao` | 这是剩余编码、反查编码还是双拼 |
| `(拼: ${pinyin} ${chaizi})` | `(拼: nǐ 亻尔)` | 读音与字根的分界 |
| 气泡合并段 `${char}：{${chaizi}{ [${chaizi_code}]}\t}${readings}` | `好：女子 [vbg]	hǎo` | 字根、编码、读音三列 |
| 气泡段名 | `[编码(五笔)]` | 段名与内容同色同字（文档站 candidate-tooltip「已知局限」写明了这一条） |

### 1.1 调研核实的现状（2026-09-27）

| 事实 | 位置 | 对本设计的影响 |
|---|---|---|
| 文字叶子是「整串一次排版」，插入符是覆盖层、刻意不拆节点 | `view.rs` `View::caret_at` 字段文档 | 着色同理：片段挂在叶子上，不拆节点（§7.1） |
| DirectWrite 测量缓存键 = 文本 + 字号 + 字重 + 字族（`measure_key`）；**layout 不缓存**，measure 未命中与每次 draw 各建一个 | `dwrite.rs` `measure` / `draw` | 颜色只进 draw，不进 measure，缓存天然不被污染（§7.2） |
| `DrawGlyphRun` 的 drawing effect 参数（`_effect`）至今未用；颜色经 `clientDrawingContext` 整段透传 | `dwrite.rs` `GlyphRenderer` | 按区间上色走 `SetDrawingEffect` + 读 effect（§7.2） |
| 文字 alpha 不交给 DirectWrite，而是 draw 第 3 步按**整段一个** `fa` 后混 | `dwrite.rs` `draw` 步骤 3 | 片段颜色 alpha 不同时要分组多遍（§7.2） |
| CoreText 按属性串排版，`make_line` 已给全区间设 `kCTForegroundColorAttributeName` | `coretext.rs` | 按区间改属性会切分 CTRun，**不能假定度量不变**；首选「整形一次、按 run 子区间换填充色」（§7.3） |
| 状态 patch 只对 `item`/`text`/`index`/`comment`/`footer_bar.disabled`/菜单项求值；**`tooltip` 没有状态 patch** | `resolve.rs` `resolve_views` | 角色表：注释节点有常态/选中/悬停三份，气泡只有常态（§5.3） |
| `resolve_state` 的 nil 门控只看色/图/边框/字重，状态 patch 只写了别的字段时整体为 `None` | `resolve.rs` `resolve_state` | 只写 `[comment.selected.roles]` 的 patch 会被丢掉，门控要加一条（§5.2） |
| 注释的选中/悬停正文色只看 `comment.selected/hover`，**不继承 `item.selected` 的色** | `candidate_window.rs` `eff_text` | 规则 3 的「该状态正文色」= `eff_text(&v.comment, …)`（§6.3） |
| 直立态（`upright`）注释本就逐字切成多个叶子 | `candidate_window.rs` `upright_text` | 片段按格切分（§7.1），不是本设计引入的拆分 |
| 宿主渲染（TSF 带窗口）候选窗与气泡都由 UI 进程画进 SHM；macOS 候选窗同样是 Rust 画进 SHM | `manager.rs` `try_host_render_candidates`、`manager_macos.rs` | 自动覆盖，无需另改（§7.5） |
| macOS 气泡是 .app 原生绘制，只收纯文本 + 前景/背景两个 hex | `wind-ipc` `encode_tooltip_show`、`TooltipPanel.swift` | 唯一要改线协议的地方（§7.6） |
| `CandidateItem` 经进程内 mpsc（`UiCommand`）交 UI，不过 IPC；TSF UIElement 与 Android 拉取面都不带注释 | `coordinator.rs`、`handle_uielement.rs`、`candidate_pull.rs` | IPC / bridge 不涉及（§8.4） |
| **`parse_hex` 只认 6/8 位**，而文档站 themes.mdx 与主题编辑器（`lib/color.ts`）都声称/支持 `#RGB` | `palette.rs` | 现存不一致：编辑器预览出色、引擎回落。本设计顺手补 3 位（§5.5） |
| 出厂配置、测试、文档站、设置端**无一处**出现 `$[` | grep 全仓 | 新语法与老模板冲突概率极低（§13.1） |
| 软键盘「激活键文字」按 `softkb_active_text` → `accent_text` → … 取色 | `soft_keyboard.rs` `set_theme`（约 489 行） | `_base` 一旦有 `accent_text = ${accent}`，`_base`/`msime` 的激活键会变成强调色字压强调色底；P2 顺手把这一级改为 `on_accent`（§15） |
| `wind-ui/src/text/mock.rs` 不在模块树里（`text/mod.rs` 未声明），是死文件 | `text/mod.rs` | Linux mock 以 `dwrite.rs` 里的 `imp` 为准（§7.4） |
| `parse_hex` 也用于语言栏配置（`[ui.langbar] text_color_*` 等），且对 `#` 可有可无 | `palette.rs`、wind-config | 补 3 位时必须要求带 `#`（§5.5） |

## 2 目标

- 注释与气泡的文字可以**分段着色**，两条通道：
  1. **主题角色色**：每个变量渲染出的文字自动带同名**角色**，主题按角色名配色，模板不用改；
  2. **模板内联色** `$[颜色]{内容}`：用户在模板里直接指定颜色。
- 选中/悬停态有明确、可预期的回落规则（§6.3）。
- 定义一组**所有主题都保证可解析**的标准色名，内联色写名字就跟随主题与明暗。
- **出厂零变化**：出厂主题不配角色、出厂模板不含内联色，外观逐像素不变；且由构造保证而非靠测试兜底（§6.4）。
  （首发时的约束。2026-09-28 起 `_base` 有意配了气泡的角色色，见 §18；注释仍不配，候选窗外观仍逐像素不变。）
- 颜色**不影响度量**：上色前后宽度、行高、换行逐像素一致。

### 非目标

- 候选正文 `text` 不纳入（§8.5）。
- 不做字重、字号、下划线等**非颜色**的分段样式——它们会改度量，破坏「颜色不影响布局」这条地基。
- 不做用户级（`config.toml`）角色色覆盖（§9）。
- 编码栏、状态提示、Toast、菜单不纳入：它们没有模板，没有分段的来源。

## 3 模型：片段串

### 3.1 数据结构（wind-ui-types）

```rust
/// 带分段样式的文字：纯文本 + 挂在旁边的区间表。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct StyledText {
    /// 私有：只经构建器写入，保证区间与文字同步（见下）。对外 `as_str()`。
    text: String,
    /// 按 `start` 升序、互不重叠；区间以字节计、落在字符边界上。空 = 整段节点正文色。
    /// 未被任何区间覆盖的文字 = 节点正文色。内联存放前 3 段（`SmallVec<[Span; 3]>`，§13.3）。
    spans: SmallVec<[Span; 3]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    /// 角色 = **归一后的变量名**（`code_rev`、`pinyin` …），或结构角色 `title` / `literal`；
    /// `None` = 无角色（未知变量名的回显，以及不在 `TEXT_ROLES` 里的变量）。
    /// 取自 `TEXT_ROLES` 的常驻字符串，模板解析时就算好（§13.3：逐片段分配 / 引用计数吃掉性能预算）。
    pub role: Option<&'static str>,
    /// 在段名（气泡 label）里产出的片段：角色未配色时回落 `title` 角色，而非正文色（§3.2）。
    pub in_title: bool,
    /// 变量值里的分项分隔符（`${chaizi_all:／}` 的 `／`）：先查 `separator` 角色，没配按 `role`（§3.2）。
    pub separator: bool,
    /// 内联色（`$[…]{}`）。有值即优先于角色（§6.2）。同一个 `$[…]{}` 内的片段共享一份。
    pub color: Option<Arc<InlineColor>>,
}

impl StyledText {
    pub fn as_str(&self) -> &str;
    pub fn is_empty(&self) -> bool;
    pub fn spans(&self) -> &[Span];
}
impl From<String> for StyledText { /* 无区间 */ }
```

- **为什么是「纯文本 + 区间表」而不是 `Vec<(String, Style)>`**：下游绝大多数代码只关心文字
  （测量、截断判定、复制、上屏、指纹、右键菜单标签），它们读 `as_str()` 即可；只有画字的那一处
  读区间。两种表示信息等价，但前者让「颜色只进 draw」在类型上就成立。
- **为什么捆成一个类型、且 `text` 私有**：并列字段或公开 `text` 都会出现「文字改了、区间没跟着改」
  ——截断、trim、吞空白都在改文字。文字只能经构建器（§8.1）写入，构造时校验（升序、不重叠、在界内、
  在字符边界），不变量只守一处。
- `InlineColor` / `ColorRef` 的定义在 wind-theme（§4.2），它依赖调色板的解析；wind-ui-types 已依赖 wind-theme。

**角色名的来源与别名**：角色存归一后的变量名，不另编号。归一只靠一张别名表
（`code` → `code_rev`、`code_all` → `code_rev_all`），放在 `comment.rs` 模板引擎里，模板解析时查一次。

`TEXT_ROLES`（wind-ui-types 的 `pub const`）是**对外契约清单**：文档站角色表、主题编辑器严格模式照它
列出与校验。**引擎也查它**（2026-09-27 P2 为性能预算改定，§13.3）：片段的角色取自这张表里的常驻字符串，
**清单外的变量不产角色**——主题给它配的色不生效，模板引擎对每个这样的名字 warn 一次。所以新增模板变量
必须同时进清单。两个方向都有测试守：

- 清单 ⊆ 求值入口：清单里每个变量名都能被某个求值入口求出值（`Some`，空串也算），走真实
  `notify_ui_update`，写进模板不得回显 `${…}`；
- 求值入口 ⊆ 清单：扫下表各入口的源码，抽出 `match` 里 `"名字" =>` / `"名字" |` / `"名字" if` 的分支，
  经别名表归一后必须都在清单里（`every_evaluable_variable_is_a_listed_role`）。

入口逐一列明：

| 入口 | 位置 | 覆盖的变量 |
|---|---|---|
| 候选上下文 | `Coordinator::eval_var`（`comment.rs`） | `code_hint`、`emoji`、`code_rev(_all)`、`shuangpin`、`pinyin`、`chaizi*`、`dict` |
| 气泡候选上下文附加 | `coordinator.rs` 气泡组装处的 `cand_eval`；`CompiledTooltip::render` 内层的 `cand_eval` | `word_code`、`code_source`、`debug`；`full_text`、`unicode_all` |
| 逐字上下文 | `tooltip::char_var`；`Coordinator::eval_text_var` | `readings`、`unicode`；`char` 及与候选上下文同名的变量 |

结构角色 `title`、`literal`、`separator` 不是变量，测试单列排除；别名表的每个键也必须能求值（今天
`legacy_names_are_aliases` 已守）。新增求值入口时把它加进源码扫描测试的入口表。

### 3.2 角色从哪来（协调器产出规则）

| 文字来源 | 角色 | 说明 |
|---|---|---|
| `${name}` 的值 | `name`（经别名表归一） | `${code}` → `code_rev`，`${code_all}` → `code_rev_all`；主题只需认规范名 |
| `${a\|b\|c}` 的值 | **实际取到值的那个变量** | `${code_hint\|code_rev}` 取到反查码时是 `code_rev` |
| `${name:arg}` 的值 | `name` | 参数不影响角色 |
| `${name:分隔符}` 的分隔符（`chaizi_all`、`chaizi_code_all`、`code_rev_all`、`dict_all`、`unicode_all`，含缺省分隔符） | `name`，`separator = true` | 主题配了 `separator` 用它，没配随变量同色（老主题零变化）。分项由变量求值时记下（`VarValue`），不在渲染时按参数切串——分隔符是空格时会误切值内部的空格 |
| 模板字面文字（正文） | `literal` | `(拼: `、`：`、`\t`、` [`、`]` |
| 模板字面文字（段名里） | `title` | |
| 气泡段名的装饰 `[` `]` 与 inline 段的 `: ` | `title` | 由 `TooltipDoc` 拼出，不是模板字面 |
| 段名里的变量值（`编码{(${code_source})}` 的 `五笔`） | `code_source`，`in_title = true` | 主题没配 `code_source` 时回落 `title` 色，整个段名同色 |
| 未知变量名的原样回显 `${pinyn}` | 无（`None`） | 它是错误提示，用正文色，不借任何角色的色；在段名里也不回落 `title`。写在 `$[…]{}` 里时回显照样带该内联色（内联色标在其内的全部片段上） |
| 截断标记 `…`（`comment_max_chars` / 气泡 `max_chars`） | 与被截处前一个字同样式 | 不单设角色：它不承载信息 |
| 非模板来源的注释（快捷加词预览行的提示等） | 无片段 | 整段正文色 |

**结构角色设 `title`、`literal`、`separator` 三个**：前两个是模板里仅有的两类「不是变量值」的文字，
主题要「淡化装饰、突出信息」或「段名用强调色」时离不开；`separator` 是变量值里的分隔符，有用户要它与
分项分色。它不占片段的 `role`（那仍是变量名），而是 `Span::separator` 标记，求色时先查它、
没配再查变量——「不配则继承」只有这样表达得出来。再细分（括号、制表符各一个角色）没有真实需求。
`char` 是逐字段的变量，按「变量即角色」自然得到，不算结构角色。

`emoji` 角色对彩色 emoji 基本无效：彩色字形各层用字体自带调色板，只有「前景色哨兵层」
（`paletteIndex == 0xFFFF`）才用文字色。文档站注明，不特殊处理。

## 4 内联语法 `$[颜色]{内容}`

### 4.1 语法

```
$[SPEC]{BODY}

SPEC  := COLOR ( ',' KEY '=' COLOR )*        KEY 目前只认 selected
COLOR := ATOM ( '/' ATOM )?                  单值 = 亮暗共用；a/b = 亮/暗
ATOM  := '#' HEX{3|6|8} | NAME               NAME = [a-z0-9_]+，调色板 token 名
```

- 上面的文法只决定**颜色是否合法**（§4.5），不决定语法是否成立（§4.4）。
- `SPEC` 内各记号两侧空白忽略。
- `BODY` 是普通模板（变量、可选段、嵌套的 `$[…]{}` 都可以写），按层数配对到对应的 `}`。
- 例：`$[accent]{${code_rev}}`、`$[#C00000/#FF8080]{(${pinyin})}`、
  `$[text_dim,selected=on_accent]{${chaizi}}`、`{ $[success]{[${dict}]}}`。

### 4.2 颜色值

| 写法 | 含义 |
|---|---|
| `#RGB` / `#RRGGBB` / `#RRGGBBAA` | 字面色（3 位扩写为 6 位） |
| `accent`、`text_dim` … | 当前主题调色板里的 token；**标准色**（§5.4）保证任何主题都有 |
| `#C00000/#FF8080`、`text/accent` | 亮/暗各一；两侧各自可以是字面色或名字 |
| `,selected=X` | 候选处于选中态时改用 X；只在注释里有意义，气泡里忽略 |
| 其它 `key=value` | 忽略（前向兼容：将来加 `hover=` 时，老版本不至于把整段判非法）；设置页预览提示 |

wind-theme 新增（`palette.rs`）：

```rust
pub struct InlineColor { pub normal: ColorRef, pub selected: Option<ColorRef> }
pub struct ColorRef { pub light: Atom, pub dark: Atom }      // 单值时两侧相同
pub enum Atom { Rgba(Rgba), Name(Arc<str>), Invalid }
impl InlineColor { pub fn parse(spec: &str) -> Self }           // 不失败，坏值落 Invalid
```

十六进制在**解析时**就校验（坏值即 `Atom::Invalid`）；名字只能在**求色时**按当前主题查（§6.2）。

### 4.3 与可选段的关系

`$[…]{…}` **只管上色，不是可选段**：

- 内部变量照常计入外层 `{…}` 与整个模板的「有值」判定；
- 内部变量全空时，它自己**不会**消失（`$[accent]{(${pinyin})}` 在无读音时仍留 `()`）——
  要「空则消失」就套一层可选段：`{$[accent]{(${pinyin})}}` 或 `$[accent]{{(${pinyin})}}`，两种等价；
- 「空变量吞掉紧邻的一个空白」照旧按**输出文字**判断，跨不跨颜色边界都一样吞。

这样定义是为了让「加颜色」对已有模板是**纯加法**：给任意一段**花括号配平**的片段包上 `$[…]{}`，
文字输出逐字节不变（包住半个 `{…}` 会改变配对，那不是加颜色而是改结构）。

### 4.4 何时仍是字面文字

语法是否成立只看两条：

1. `$[` 之后到第一个 `]` 之间的 `SPEC` 不含 `{` `}` 换行（遇到它们即不成立），长度 ≤ 64 字节；
2. `]` 紧跟 `{`，且 `BODY` 能按层数配对闭合。

任一条不满足，`$` 按字面输出、从 `[` 继续扫描，与现有 `${` 未闭合即退化为字面的宽容取向一致。
`SPEC` 里写的是什么**不影响**语法是否成立：`$[红]{x}`、`$[]{x}`、`$[#GG]{x}` 都是语法成立、颜色非法，
按 §4.5 处理（显示 `x`、正文色、预览提示）。这样用户写错颜色时不会突然看到一串字面 `$[…]`。

### 4.5 非法或查不到的颜色

语法完整、颜色值不对（`#GG0000`、当前主题没有的名字、`transparent` 与 alpha 为 0 的 `#RRGGBB00`——它们占宽却不可见，只会是误用）：
**该段按节点正文色显示，不回显字面**。设置页预览在该段下标出原因（§11）。

与「未知变量名原样回显」的取向不同，理由：拼错变量丢的是**信息**，必须让人看见；
颜色写错丢的只是**装饰**，文字本身完整，回显一串 `$[#GG0000]` 反而把信息淹没。

### 4.6 纯文本出口剥色

模板引擎另有两个出口，产物是**要上屏的文字**而非显示：`alt_commit_text`（上屏注释）与
`reverse_render`（cmdbar `dict.rev`）。它们继续调 `render()` 得 `String`，`$[…]{}` 只留 `BODY` 的文字。
复制 / 上屏 / 「复制全部」同理，一律取 `StyledText::as_str()` 或气泡的原始行（本来就是纯文本）。

### 4.7 分隔符参数里的内联色

`${name:分隔符}` 的参数**整个**写成 `$[SPEC]{BODY}` 时（`${chaizi_all:$[text_dim]{／}}`、
`${dict_all:$[#888]{ · }}`），变量收到的分隔符是 `BODY`，`SPEC` 只标在值里的分隔符片段上。

- `SPEC` 的成立条件、颜色写法与回落同 §4.2–4.5（复用 `InlineColor::parse`）；`BODY` 不嵌套，到其后
  第一个 `}`。变量的收尾 `}` 是第一个**不在**这样的 `$[…]{…}` 里的 `}`；`|` 回退链同样不在其中切。
- 优先级：分隔符自己的内联色 > 外层 `$[…]{}` > 主题 `separator` 角色 > 变量角色。
- 降级同 §4.4：语法不成立（`]` 后不是 `{`、`SPEC` 过长或含 `{` `}` 换行、只占参数一部分）参数按字面；
  `BODY` 未闭合即变量未闭合，其后全按字面；颜色写错照样是内联色，分隔符按正文色显示。
- 纯文本出口只有 `BODY` 的文字。设置页预览把这个 `SPEC` 与模板内联色一起列进 `colors` 做诊断。

## 5 主题：角色表

### 5.1 TOML 写法

在视图节点下加 `roles` 子表，状态照现有 patch 形态写：

```toml
[comment.roles]
pinyin   = "${text_dim}"
code_rev = "${accent}"
literal  = { light = "#B0B0B0", dark = "#606060" }

[comment.selected]
color = "${on_accent}"         # 选中态改了注释正文色 ⇒ 未单列的角色在选中态回落到它（§6.3）

[comment.selected.roles]
code_rev = "#FFE08A"           # 单列：选中态仍保留编码高亮

[tooltip.roles]
title    = "${accent}"
readings = "#9AD0FF"
```

- 值的形态与节点 `color` 相同：`${token}` / `#hex` / `{ light, dark }`；`""` = 未设置（跟随正文色），
  派生主题借此撤销 base 配的某个角色。唯一例外：`transparent` 在角色表里按未设置处理并 warn，
  与内联色口径一致（§4.5）——文字占着宽度却看不见，只会是误用。
- 只有 `comment`（含 `selected` / `hover`）与 `tooltip` 消费；写在其它节点下的 `roles` 被忽略。
- 引擎**不校验**角色名：更新版本的主题（用到了新变量的角色）在旧引擎上应静默忽略而非报错。
  拼写检查交给主题编辑器的严格模式（§10）。

### 5.2 内存模型改动（wind-theme）

| 位置 | 改动 |
|---|---|
| `schema.rs` `ViewNode` | `#[serde(default)] pub roles: HashMap<String, Ld>`；状态 patch 递归同一类型，`selected.roles` 自然可用 |
| `normalize.rs` | **不用改**：`roles` 是普通子表，`normalize_node` 不认识的键原样保留，`selected`/`hover` 已递归；加一条测试钉住扁平写法 `[comment.selected.roles]` 落到 `views.comment.selected.roles` |
| `theme.rs` `merge` | **不用改**：表对表逐键深合并，派生主题可只改一个角色；`{light,dark}` 对标量按既有「整体重置」语义 |
| `rvnode.rs` `RvNode` | `pub roles: HashMap<String, Rgba>`（已解析；空 = 未配） |
| `resolve.rs` `resolve_view_node` | 逐项 `resolve_color`，`None`（空串、未解析的 token）与 `transparent` 不入表——未解析 token 沿用现有 warn |
| `resolve.rs` `resolve_state` | nil 门控加 `|| !resolved_roles.is_empty()`；否则只写了 `[comment.selected.roles]` 的 patch 被整体丢弃 |

气泡节点 `rv.tooltip` 与注释节点走同一个 `resolve_view_node`，无需分别处理。

### 5.3 各节点覆盖的状态

| 节点 | 常态 | selected | hover | 说明 |
|---|---|---|---|---|
| `comment` | ✓ | ✓ | ✓ | `resolve_views` 已求值 `comment.selected` / `comment.hover` |
| `tooltip` | ✓ | — | — | 气泡没有状态；`[tooltip.selected.roles]` 不生效，编辑器不提供 |

`[comment.disabled]` 当前不求值（`resolve_views` 未建），本设计不新增。

### 5.4 标准色契约

内联色写名字才能「跟随主题 + 明暗」。下列名字由 `_base` 给默认，所有出厂主题都继承 `_base`，
故**保证可解析**；没继承 `_base` 的第三方主题由引擎兜底补齐（见本节末「引擎兜底」），同样保证。
契约写进文档站（§12），主题编辑器按此给中文名与分组。清单于 2026-09-27 确认（不加 `link`）。

#### 已有、纳入契约

| 名字 | 用途 | `_base` 亮 | `_base` 暗 |
|---|---|---|---|
| `text` | 主文字 | `#1E1E1E` | `#E0E0E0` |
| `text_dim` | 次要文字 | `#646464` | `#B0B0B0` |
| `text_hint` | 提示文字（注释默认色） | `#969696` | `#808080` |
| `accent` | 强调色 | `${primary}`（`#4285F4`） | 同左 |
| `on_accent` | 强调底上的前景 | `#FFFFFF` | 同左 |
| `selection_text` | 选中候选文字 | `${text}` | 同左 |
| `tooltip_text` | 气泡文字 | `#FFFFFF` | `${text}` |

#### 新增：候选窗用（为 `bg` 调）

`_base` 底色：亮/白底 `#FFFFFF`、亮/选中底 `#E6F0FF`、暗/`#2D2D2D`、暗/选中底 `#3D4A5C`。
`_qingfeng` 系（`default`、`amber`、`jade`、`violet`）底色：亮/白底 `#FFFFFF`、亮/选中底 `accent_soft`
叠白底 ≈ `#EBF2FE`、暗/`#121826`、暗/选中底 ≈ `#192B4C`。

| 名字 | 用途 | 亮 | 暗 | `_base` 对比度（亮白 · 亮选中 · 暗底 · 暗选中） | `_qingfeng` 对比度（同序） |
|---|---|---|---|---|---|
| `accent_text` | 可读的强调**文字**色 | `${accent}` | `${accent}` | 3.56 · 3.10 · 3.86 · 2.53 | 用其自有值 `#2563eb`/`#60a5fa`：5.17 · 4.59 · 6.97 · 5.54 |
| `success` | 成功 / 肯定 | `#1E8E3E` | `#81C995` | 4.21 · 3.66 · 7.03 · 4.60 | 4.21 · 3.74 · 9.05 · 7.19 |
| `warning` | 提醒 | `#B06000` | `#FDD663` | 4.65 · 4.04 · 9.82 · 6.42 | 4.65 · 4.13 · 12.64 · 10.04 |
| `error` | 错误 / 否定 | `#D93025` | `#F28B82` | 4.77 · 4.15 · 5.77 · 3.77 | 4.77 · 4.24 · 7.42 · 5.89 |
| `info` | 说明 / 补充 | `#1A73E8` | `#8AB4F8` | 4.51 · 3.92 · 6.53 · 4.27 | 4.51 · 4.00 · 8.41 · 6.68 |

#### 气泡用：契约里每个名字都有 `tooltip_<名>`（为深色气泡底调，§6.2 作用域查找）

**契约名在气泡里都有可读担保**：上两张表里的每个名字 `x`，`_base` 都定义了 `tooltip_x`，内联色在气泡里
写 `x` 取到的是 `tooltip_x`。气泡在语境上没有意义的名字（`on_accent`、`selection_text`）等于
`tooltip_text`，保证写了也可读。

| 名字 | 亮 | 暗 | 对比度：`_base` 亮档气泡 `#3C3C3C` · 暗档 `#1E1E1E` · `_qingfeng` 亮档 `#2A2F3E` · 暗档 `#12151F` | 状态 |
|---|---|---|---|---|
| `tooltip_text` | `#FFFFFF` | `${text}` | 11.03 · 12.63 · 13.34 · 16.43（暗档按各自 `text`：`_base` `#E0E0E0`、`_qingfeng` `#F2F3F7`） | 已有 |
| `tooltip_text_dim` | `#BDBDBD` | `#BDBDBD` | 5.87 · 8.87 · 7.10 · 9.70 | 新增 |
| `tooltip_text_hint` | `#A0A0A0` | `#A0A0A0` | 4.22 · 6.38 · 5.10 · 6.97 | 新增 |
| `tooltip_accent` | `#8AB4F8` | `#8AB4F8` | 5.23 · 7.91 · 6.33 · 8.64 | 新增；`_qingfeng` 系各自覆盖，见下 |
| `tooltip_accent_text` | `${tooltip_accent}` | 同左 | 同 `tooltip_accent` | 新增 |
| `tooltip_on_accent` | `${tooltip_text}` | 同左 | 同 `tooltip_text` | 新增 |
| `tooltip_selection_text` | `${tooltip_text}` | 同左 | 同 `tooltip_text` | 新增 |
| `tooltip_success` | `#81C995` | `#81C995` | 5.63 · 8.51 · 6.81 · 9.30 | 新增 |
| `tooltip_warning` | `#FDD663` | `#FDD663` | 7.87 · 11.89 · 9.51 · 12.99 | 新增 |
| `tooltip_error` | `#F28B82` | `#F28B82` | 4.62 · 6.98 · 5.58 · 7.63 | 新增 |
| `tooltip_info` | `#8AB4F8` | `#8AB4F8` | 5.23 · 7.91 · 6.33 · 8.64 | 新增 |

`tooltip_text_dim` / `tooltip_text_hint` 为什么要补：直接用候选窗的值，`text_hint` 暗档 `#808080` 在
`_base` 亮档气泡上只有 2.79，`text_dim` 亮档 `#646464` 压在深色气泡上更低。补的两色保持「主文字 > 次要 >
提示」的明度阶梯（`#FFFFFF` > `#BDBDBD` > `#A0A0A0`），最低一档仍 ≥ 4.22。

**`tooltip_accent` 要随主题换色**：`_base` 给的 `#8AB4F8` 是蓝。`_qingfeng` 系的强调色各不相同，故
`_qingfeng` 与其派生主题各自覆盖 `tooltip_accent` 为本主题 `accent_text` 的**暗档值**（为深底调过）：

| 主题 | `tooltip_accent` | 对比度（`_qingfeng` 亮档气泡 · 暗档） |
|---|---|---|
| `_qingfeng`（蓝，`default` 继承） | `#60a5fa` | 5.25 · 7.17 |
| `amber`（橙） | `#fbbf24` | 7.99 · 10.91 |
| `jade`（绿） | `#34d399` | 6.94 · 9.48 |
| `violet`（紫） | `#a78bfa` | 4.90 · 6.69 |

不能写成 `${accent_text}`：它的亮档是为白底调的深色，在亮档（深色）气泡里会看不清，而调色板引用没法
「取另一档的值」。于是「清风·XX 只覆盖 primary / accent_text / accent_soft 三色即整体换色」这条约定
（`_qingfeng/theme.toml` 文件头）变成四色，文件头注释同步改。`msime` 继承 `_base`、主色是微软蓝，
`_base` 的蓝色 `tooltip_accent` 与之协调，不覆盖。第三方主题改了 `primary` 而没覆盖 `tooltip_accent` 时，
气泡里的强调色仍是蓝——可读但不同色相；主题编辑器在「改了 primary、未改 tooltip_accent」时给提示（§10）。

（对比度按 WCAG 相对亮度公式计算；气泡底色带 alpha（`F0`/`F5`），按不透明近似。）

**取值依据**：

- **对比度**：候选窗四色在所属明暗档的窗口底色上 ≥ 4.2，在选中底色上 ≥ 3.66（`_qingfeng` 系 ≥ 3.74）。
  作为参照，出厂注释色 `text_hint` 亮档在白底只有 2.96——新色都比现有注释更易读，不会出现
  「标了色反而看不清」。气泡各色在四种出厂气泡底上 ≥ 4.22。
- **与现有色板协调**：`_base` 的品牌蓝 `#4285F4` 是 Google 蓝，四个语义色取同一套 Google 配色的
  深浅配对——亮档取深阶（`#1E8E3E` 绿、`#D93025` 红、`#1A73E8` 蓝，`warning` 取深橙 `#B06000`：
  黄色系在白底上够不到 4.5），暗档取浅阶（`#81C995` / `#FDD663` / `#F28B82` / `#8AB4F8`）。
  同源的明度阶梯让它们与品牌蓝并排出现不打架，亮暗两档的「同一个颜色」也认得出是同一个。
  `info` 与 `accent` 同色相而更深（亮档）/更浅（暗档），用于「补充说明」这类比强调弱一级的信息。
- **`accent_text` 两档都 `${accent}`**：`_base` 里没有「强调色不可读」的问题需要另调，给别名只为让名字
  在所有主题里存在；`_qingfeng` 系（含 `default`、`amber`、`jade`、`violet`）已有自己调好的
  `accent_text`，照旧覆盖。它在暗档选中底上只有 2.53，这与 `accent` 本身一致，是 `_base` 既有取值。
- **气泡色亮暗两档同值**：`_base` 与 `_qingfeng` 的气泡底色亮暗两档都是深色，暗档语义色（200～300 阶）
  在两档上都够亮，没有分档的理由；仍写成亮暗两键（值相同），主题作者覆盖时照常分设。

**气泡为什么要单独一组、以及查找规则**：为白底调的亮档色放进深色气泡，`#D93025` 对比度从白底的 4.77
掉到 2.31，`#1E8E3E` 从 4.21 掉到 2.62。故内联色在**气泡里**按名字 `x` 求色时先查 `tooltip_x`，查不到
再查 `x`（2026-09-27 确认，§6.2）。于是气泡里的 `$[text]{…}` 取到 `tooltip_text`——在气泡里「正文色」
本就该是气泡文字色，与直觉一致。

新增 token 不被任何出厂节点引用（首发时；2026-09-28 起 `_base` 的气泡角色表引用其中的 `tooltip_*`，§18），外观零变化——**唯一例外是软键盘**：它的激活键文字按
`softkb_active_text` → `accent_text` → … 取色（`soft_keyboard.rs`），`_base` 有了 `accent_text` 后
`_base` / `msime` 的激活键会变成强调色字压强调色底。P2 把这一级改为 `on_accent`（§15）；`_qingfeng` 系
今天就已是 `accent_text` 字压 `accent` 底，改后变为白字，属顺带修正，靶机核对一眼。`Resolved.palette`
多出十几项，Android 拉取调色板（`theme_palette`）会多拿到这些键，无害。

#### 引擎兜底：没继承 `_base` 的主题（2026-09-28）

**起因**：用户靶机上的第三方主题「Switch风格」没写 `base`（主题继承只认显式 `base`，`load_merged_dirs_at`），
契约名一个也没有，`$[error]{…}` 查不到名字、回落正文色，看起来像「颜色失效」。

**规则**：调色板解析后，把主题 `[colors]` **没写**的契约名补进 `Resolved.palette`；写了的一个不动，
契约外的名字一个不加。不是隐式继承 `_base`——那会连带改掉第三方主题的布局与其它颜色（已否决）。

- 补值来源是编译期嵌入的 `_base/theme.toml` 本身（`wind_theme::contract`，`include_str!`），不是手抄的常量表：
  两处写同一组值迟早只改一处，而这里漂移了没有任何症状（只有无 base 主题看得到）。嵌入而不是运行时读盘：
  兜底不该依赖搜索链里恰好有 `_base`（测试刻意不把 `data/themes` 放进搜索链）。
- 引用的解析口径：**契约名之间的引用按本主题解析**——`selection_text = ${text}`、`tooltip_text` 暗档
  `${text}` 取本主题的 `text`，`accent_text = ${accent}` 取本主题的强调色（「Switch风格」得到它的红
  `#FF4554`），`tooltip_accent_text` / `tooltip_on_accent` / `tooltip_selection_text` 同理；本主题也没写，
  再取兜底值。**契约外的名字只取 `_base` 自己的值**：主题连 `accent` 也没写时，`accent = ${primary}` 取
  `_base` 的 `#4285F4`，不借主题的 `primary`——它不在契约里，第三方主题的同名色未必是同一个意思。
- **例外（2026-09-28 确认）**：主题没有可用的 `tooltip_bg`（没写，或写了解析不出——渲染层取底色也按解析结果）时，`tooltip_text` 取渲染层原常量
  `fallback::TOOLTIP_TEXT`（亮暗同值），引用它的 `tooltip_on_accent` / `tooltip_selection_text` 随之。气泡底是
  渲染层的深灰常量，`_base` 暗档的 `${text}` 是配它自己深色气泡底的；借过来，只调了浅色、`text` 写成单个
  深色值的主题在暗色下就是深字压深底。这样这类主题的气泡外观与没有兜底时完全一致。写了 `tooltip_bg`
  才按 `_base` 规则。
- 「写没写」按 `[colors]` 的键判，不按解析结果：主题写了却解析不出（坏值、`transparent`、断链）是作者自己的
  选择，不替它换成 `_base` 的色；引用它的兜底（如 `selection_text`）也随之解析不出，与主题里写 `${它}` 一致。
- 补在 views 求值**之后**：节点的调色板默认色（`tk("text")`、注释默认 `text_hint` 等）仍只看主题自己写的，
  候选窗节点外观不变。按名取色的渲染层会看到补上的名字，见下「有意的外观变化」。同理，主题 `[colors]`
  内部的 `${契约名}` 引用也看不到兜底（它在兜底之前按主题自己的表求值）——兜底只保证模板内联色查得到，
  文档站照此措辞。
- 两个出口同一口径：桌面 `wind_theme::resolve`（候选窗、气泡、设置页预览 `previewTemplate`）与移动端拉取的
  `Coordinator::theme_palette`（它不走 `resolve`，单独接一次）。出厂主题都继承 `_base`，兜底是空操作，
  调色板逐项不变（有用例对拍）。

**有意的外观变化**（只影响没写对应名字的无 base 主题）：

- 气泡正文色：`theme.color("tooltip_text", 兜底)` 原先落渲染层兜底 `[240,240,245]`。没写 `tooltip_bg` 的主题
  兜底仍是这个值（见上「例外」），不变；写了 `tooltip_bg`、没写 `tooltip_text` 的，现在是契约值——亮档
  `#FFFFFF`，暗档本主题的 `text`。
- 候选窗翻页箭头的禁用色（`text_hint`）与页码色（`text_dim`，翻页栏没配文字色时）原先落渲染层兜底，现在取契约值。

**已知局限**：`tooltip_*` 兜底按深色气泡底调（`_base` 的气泡底亮暗两档都是深色）。第三方主题若把气泡底
配成浅色（`tooltip_bg`），兜底补的 `tooltip_*` 在上面看不清，需自己定义 `tooltip_*`。同理，兜底的
`tooltip_accent` 是 `_base` 的蓝，不随主题强调色变（与 §5.4 上文「改了 primary 未改 tooltip_accent」同一情形）。

### 5.5 `#RGB`

`parse_hex` 补 3 位（`#F80` → `#FF8800`）。这同时修掉 §1.1 那条现存不一致：文档站与编辑器都说
支持 `#RGB`，引擎却回落。

- **3 位形式必须带 `#`**：`parse_hex` 今天对 `#` 可有可无（6/8 位裸写也认）。3 位若也可裸写，`bad`、
  `fed`、`ace` 这类英文单词会被当成颜色——而 `parse_hex` 的调用方不止主题，还有语言栏配置
  （`[ui.langbar] text_color_*` 等用户手填的字符串）与本设计的内联色名字。6/8 位的裸写保持现状。
- 作用面：所有主题颜色字面值、语言栏颜色配置、内联色。此前写了 3 位色的主题在引擎里是「没配」，
  修后开始生效——这是文档承诺的行为，不算回归，发版说明写一句。
- 主题编辑器 `lib/color.ts` 还接受 `rgb()` / `rgba()`，引擎不认（整个都不认，不止 3 位）；编辑器严格模式
  对此给提示（§10），不在引擎侧扩。

## 6 求色（UI 侧）

### 6.1 为什么在 UI 侧、而不是协调器里求好颜色再下发

1. **悬停态只有 UI 知道**：选中下标协调器有，悬停由 UI 自己跟踪、不回协调器重算；
2. **明暗切换**：系统切明暗时协调器推一份新的 `Resolved`（`UiCommand::SetTheme`），UI 收到后**用手里的
   候选重绘**（`manager.rs` `SetTheme` 分支）。片段带的是「角色 / 颜色引用」而非 RGBA，重绘即取到新明暗
   下的颜色——内联亮暗对 `#C00000/#FF8080` 的取值时机就是**每次画**，不需要协调器重算候选；
3. 协调器不持有 `Resolved`（它只在切主题时加载一次推给 UI），在候选循环里查主题是新耦合。

**求色函数是 wind-theme 里的纯函数**（如 `wind_theme::span_color`），不依赖 wind-ui-types 的 `Span`：

```rust
pub fn span_color(
    theme: &Resolved,
    node: &RvNode,          // views.comment 或 views.tooltip
    is_tooltip: bool,       // 决定内联色名字的 tooltip_ 作用域查找
    state: TextState,       // Normal | Selected | Hover
    body_fallback: Rgba,    // 节点未配正文色时的渲染层兜底（候选窗今天是 [150,150,150,255]）
    role: Option<&str>,
    in_title: bool,
    inline: Option<&InlineColor>,
) -> Rgba
```

四处共用：wind-ui 的候选窗与自绘气泡、`manager_macos.rs`（算 macOS 气泡的 runs）、wind-webdata
（设置页预览，§11）。放 wind-theme 而不是 wind-ui 的理由是第四个调用方：wind-webdata 不该为求个颜色
依赖整套 UI crate；且它只依赖 `Resolved` / `RvNode` / `InlineColor` 三个 wind-theme 自己的类型，
可在 Linux 上直接单测全部分支。候选窗现有的 `eff_text` 闭包随之改调这里的正文色计算，保证两处同一口径。

### 6.2 顺序

给定节点 `n`（`comment` 或 `tooltip`）、当前状态 `st`（常态 / 选中 / 悬停，选中优先，与 `eff_text` 同）、
一个片段 `s`：

```
body      = eff_text(n, st)                     // 该状态的正文色（最终 RGBA），现有函数
// 规则 3 的判据：比**有效色值**，不看主题写没写 `[comment.selected] color`（§6.3）
state_changed_body = st ≠ 常态 && body ≠ eff_text(n, 常态)

if let Some(ic) = s.color:                      // ① 内联色优先
    if st == 选中 && ic.selected 有值 → return resolve(ic.selected) ?? body
    if state_changed_body              → return body        // 规则 3
    return resolve(ic.normal) ?? body                        // 非法 / 查不到 → 正文色
// ②③ 的「角色」：分隔符片段先查 `separator` 再查 s.role，其余只查 s.role
if st ≠ 常态 && n.<st>.roles[s.role] 有值  → return 它         // ② 状态态单列的角色色
if state_changed_body                      → return body      // 规则 3
if n.roles[s.role] 有值                     → return 它         // ③ 常态角色色
if s.in_title && s.role 非 None && n.roles["title"] 有值 → return 它   // ④ 段名里的变量回落 title（未知变量回显除外）
return body                                                   // ⑤ 正文色
```

`resolve(ColorRef)`：按 `Resolved.is_dark` 取亮/暗侧的 `Atom`，再：

```
Rgba(c)        → c
Name(x)        → n 是 tooltip 且 palette 有 "tooltip_" + x → 它      // 气泡作用域，§5.4
               → palette 有 x                               → 它
               → None
Invalid        → None
```

作用域查找只作用于**内联色的名字**；主题 `[tooltip.roles]` 里写的 `${token}` 在 resolve 期已按字面
解析，不改写。`#hex` 字面色也不受影响——用户写死的颜色就是他要的颜色。

### 6.3 状态回落（规则 3）

> 某状态若改了该节点正文色，而未给某角色在该状态下的颜色 → 该角色（以及内联色）回落到**该状态的
> 正文色**；状态没改正文色 → 沿用常态角色色 / 内联色。

理由：主题把选中底换成深色实心块时，一定会同时把选中态注释色改成浅色（否则灰字压在蓝底上看不见）。
此时常态为浅底调的角色色直接搬到深底上多半不可读；回落到主题作者为这个状态**亲自选过**的正文色，
是唯一有可读性担保的颜色。想在选中态也保留分色，就在 `[comment.selected.roles]` 单列，
或在内联色里写 `selected=`。

**「改了」按有效色值判，不按「写没写」**（2026-09-27 确认）：`state_changed_body` 比较的是该状态与
常态的**最终正文色**（`eff_text` 的 RGBA，已按当前明暗解析），不等才回落。理由：派生主题常把 base 配的
选中注释色「写回」常态色（撤销 base 的深色选中），按「写没写」判会让这类主题在选中态无端丢掉全部分色；
而值相等时常态角色色的可读性前提（底色之外的正文色没变）也原样成立。亮暗两档各自比较——同一主题可能
只在暗色下改了选中正文色，那就只有暗色回落。

出厂 `_base` / `_qingfeng` 都没写 `[comment.selected] color`，选中态注释正文色 = 常态色，
故选中时角色色 / 内联色照常显示。

### 6.4 出厂零变化：构造保证

UI 把片段解析成颜色后，**丢掉颜色等于正文色的区间**；一个区间都不剩时，叶子不带颜色区间，走与今天
**同一条**绘制路径（§7.2 的早返回）。出厂主题不配角色、出厂模板不含内联色 ⇒ 每个片段都解析到正文色
⇒ 全被丢弃 ⇒ View 树与今天逐字段相同。零回归因此是结构性质，对拍只是确认（§13.2）。

> 2026-09-28 起 `_base` 配了气泡角色色（§18），气泡外观随之有意改变；注释不配，候选窗仍满足上面的构造保证。
> golden 于同日重录，候选窗部分与分段着色之前逐字节相同，气泡只多出颜色区间。

## 7 渲染层

### 7.1 View 叶子

```rust
pub struct View {
    // …
    pub text: Option<String>,
    pub text_color: [u8; 4],
    /// 分段颜色：按字节区间覆盖 `text_color`。空 = 整段 `text_color`（今天的行为）。
    /// **只在 paint 消费，不参与 measure/布局**，理由同 `caret_at`。
    pub color_runs: Vec<ColorRun>,   // ColorRun { start: u32, end: u32, rgba: [u8; 4] }
}
```

**不拆节点**：把 `亻尔 [wq]` 拆成三片各自测量再拼接，`measure(a+b) != measure(a)+measure(b)`
（拆分边界丢字距、每段各自亚像素舍入），宽度随着色边界变化而抖动——与 `View::caret_at` 字段文档
记录的插入符问题是同一个问题，同一个答案：整串一次整形，颜色只作用于绘制。

直立态（`upright_text`）本就把注释逐格切成多个叶子（这个拆分今天就存在，与着色无关）：
每格取落在它字节范围内的区间、平移到格内偏移。为此 `upright_text` 的 leaf 回调要扩参，把**该格在整串里的
字节偏移**传出来（今天只传格文本与 caret），回调据此切区间；不切格时偏移为 0、区间原样。

### 7.2 DirectWrite

- `TextRenderer` 新增 `draw_runs(buf, …, text, ts, base_color, runs)`；`runs` 为空直接调现有 `draw`
  ——出厂路径一个字节都不变。
- 建 layout 后对每个区间 `SetDrawingEffect(effect, range)`（区间换算成 UTF-16 码元，同 `pua_runs` 口径）。
  `DrawGlyphRun` 读 `_effect`：有则用它对应的色，无则用 `clientDrawingContext` 里的基色。彩色 emoji 的
  前景哨兵层同样取这个色。
  - 实现提示：**不必自定义 COM 接口**。每次 draw 为每种颜色建一个最简的 `IUnknown` 对象，连同颜色放进
    一张表（经 `clientDrawingContext` 传给回调）；`DrawGlyphRun` 拿到的 `_effect` 按**指针身份**
    （`as_raw()`）在表里反查颜色即可。对象只活到本次 draw 结束。
- **alpha 分组**：draw 第 3 步把文字 alpha `fa` 在回写时**统一**混入，而「哪个像素属于哪个片段」
  在那一步已经不可知。故按 alpha 把区间分组，每组一遍 Draw（只画本组，其余 glyph run 在
  `DrawGlyphRun` 里跳过）。不透明色（绝大多数）与基色同组，常见情形仍是一遍。
  - ⚠️ 多遍时**不能**每遍都「拷底 → 回写预乘值」：第 3 步回写的是按窗口 alpha **预乘**后的值，下一遍
    第 1 步又把它当直通值拷进 DIB，半透明底（窗口 alpha < 255，如 `_base` 气泡 `F0`）上的字会被二次预乘、
    发暗。做法：包围盒内先把底拷进一块**直通值草稿区**，各遍都在草稿区上「画 → 按本组 `fa` 混合」累积，
    最后统一按窗口 alpha 预乘、回写缓冲**一次**。单遍时与现行逻辑逐字节等价。
  - （P1 实测修正）二次预乘**只在两段墨迹相交的像素上**发生：朴素做法下一遍比对 DIB 与缓冲时，
    前一遍已画、本遍没碰的像素保持不变而被跳过；只有后一遍的字形（含抗锯齿边）落在前一遍已画像素上时，
    那几个像素才被当直通值再混一次。故它表现为交界处的一圈暗边，不是整片字发暗；验证它的用例必须先
    保证两段墨迹确有交叠（§13.2 P1 ④）。
- **缓存不受污染**：
  - `measure_cache` 键是 `measure_key(text, size, weight, family)`，measure 路径根本拿不到区间；
  - layout 不缓存：measure 未命中与每次 draw 都新建（`create_layout`），effect 只设在 draw 那个一次性
    layout 上，用完即弃；
  - `formats`（按字号）与 `line_heights`（按字号/字重/字族）与颜色无关。
  - ⚠️ 将来若给 draw 加 layout 缓存，effect 必须每次取出后重设、用后清掉，或带 effect 的 layout 不入缓存
    ——在 `create_layout_with` 的文档里写明这条约束。
- drawing effect 是格式属性，不参与整形与度量，故带 effect 的 layout 与 measure 用的 layout 字形位置相同。
  这一点由 P1 的像素测试钉住（§15），不只靠 API 文档。
- **P1 实现细节**（设计未写、实现时定下的）：
  - effect 对象借 `IDWriteFactory::CreateTypography` 造（任何活着的 `IUnknown` 都行，这个最轻）；
    wine 9.0 与真 Windows 都原样（同一指针）把它传进 `DrawGlyphRun`，并在 effect 边界切开字形段。
  - 反查不到的 effect 按基色、在**第一遍**画（`DrawCtx::fallback_on`）。区间全覆盖时基色那组不存在，
    兜底若只挂在基色组上，反查一旦全失败整段字都不画——审查实测坐实后修正。
  - alpha 为 0 的组直接跳过，也不算「改动过」，免得把底色白白预乘一次。
  - 回写按 touched 标记（任一遍改动过的像素），单遍时与 `draw` 的判据、算术逐字节相同。
  - 区间重叠时后者覆盖前者（后设的 `SetDrawingEffect` 生效）；CoreText 侧按同一口径反查。
  - 非法区间（空、越界、不在字符边界）在后端入口丢弃，全部非法即退回 `draw`；mock 同口径。

### 7.3 CoreText

「颜色不影响度量」在 CoreText 上**不能假定成立**：属性不同的区间会被切成不同的 CTRun，跨边界的字距
与连字可能因此变化——带色 line 的宽度可能不等于 `measure` 用的无色 line。

- **首选**：整形只做一次。整串设 `kCTForegroundColorFromContextAttributeName = true`（字色取上下文填充色），
  line 与 `measure` 用的完全同构；绘制时遍历 `CTLineGetGlyphRuns`，对每个 CTRun 用
  `CTRunGetStringIndices` 把颜色区间换算成**字形子区间**，逐段 `CGContextSetFillColor` + `CTRunDraw(run,
  ctx, subrange)`。字形位置来自同一次整形，度量一致由构造保证。CG 原生处理 alpha，无需分组。
- **备选**：按区间设 `kCTForegroundColorAttributeName`（`CFRange` 以 UTF-16 计）。实现最短，但风险即上所述，
  只在首选方案遇到阻碍（例如彩色 emoji 在 `FromContext` 下取色异常）时采用，且必须过 P1 的宽度用例。
- P1 加 macOS 用例：同一文本，带色 line 与无色 line 的 typographic width 相等（含跨颜色边界的连字 / 字距
  样例，如 `fi`、`AV` 恰好落在边界上）。只能在 macOS CI 上跑。

### 7.4 mock（Linux，`dwrite.rs` 的 `imp`）

Linux mock 以 `dwrite.rs` 里的 `imp` 为准；`wind-ui/src/text/mock.rs` 不在模块树里（`text/mod.rs`
未声明），是死文件，不要在那里改（可顺手删除）。

- `draw_runs` 同 `draw` 不出像素。
- **新增绘制调用记录**：`imp::TextRenderer` 记下每次绘制调用 `(text, x, y, ts, color, 是否走 runs 及 runs)`，
  供 §13.2 的 golden 对拍。记录只在测试构建开启，不影响生产路径（生产的 Linux 构建本就没有真实渲染）。

### 7.5 宿主渲染

- **Windows 宿主渲染**（TSF 带窗口）：候选窗与气泡都由 UI 进程 `render_frame` / `render_tooltip_frame`
  画成 BGRA 写进 SHM，C++ 侧 `HostWindow` 只贴图——走的就是 View + `TextRenderer`，自动覆盖。
- **macOS 候选窗**：同样由 Rust 用 CoreText 画进 SHM（`manager_macos.rs` `encode_host_render_frame`），
  自动覆盖。

### 7.6 macOS 原生气泡

`.app` 的 `TooltipPanel` 用 `NSAttributedString` 自绘，收的是 `CmdTooltipShow(text, bg, fg, font_path)`。

- 协议：`encode_tooltip_show` 末尾**追加**第五段 `runs`：`count: u32` + `count × (start_u16: u32, len_u16: u32,
  rgba: u32)`，区间以 **UTF-16** 计（直接对应 `NSRange`），颜色已在 Rust 侧按 §6.2 解析好。
- Swift `decodeTooltipPayload`：沿用 `fontPath` 那条「`off < buf.count` 才读」的先例容忍缺省。
  旧 `.app` + 新服务：多出的尾段被忽略，退化为单色；新 `.app` + 旧服务：没有尾段，同样单色。
- `TooltipPanel.show` 在 `.foregroundColor: fg` 之后逐区间 `addAttribute(.foregroundColor, …)`。
- `rgba: u32` 的字节序：按小端 u32 写出，字节依次为 **R、G、B、A**（即 `R | G<<8 | B<<16 | A<<24`）；
  Swift 侧按 **sRGB** 建色：`NSColor(srgbRed: r/255, green: g/255, blue: b/255, alpha: a/255)`，
  与 Rust 侧 CoreText 渲染候选窗所用的色彩空间一致，同一颜色在候选窗与气泡里看起来相同。
- `manager_macos.rs` 需要在 `SetTheme` 时留一份当前 `Resolved`（现在只留 `tooltip_bg/fg` 两个 hex），
  供算 `runs`。
- **`Forwarder.last_tip` 改存结构化的 `TooltipDoc`**（现在是 `Option<String>`），每次 push 时按当前主题
  **现算** runs。原因：换主题 / 换明暗时 `handle` 会用 `last_tip` 重推当前帧（`affects_appearance` 分支），
  若存的是算好颜色的结果，重推出去的气泡仍是旧主题的颜色。

## 8 链路：从模板到像素

### 8.1 模板引擎（`wind-coordinator/src/comment.rs`）

- `Node` 新增 `Color(InlineColor, Vec<Node>)`；`parse` 在 `$` 分支里先判 `${`、再判 `$[`（§4.4）；
  `find_group_end` 不用改——`SPEC` 里不许出现花括号，`$[…]{…}` 贡献的花括号天然配平。
- `render_nodes` 的输出从 `String` 改为一个片段构建器：`push(&str, role, in_title)` 合并相邻同样式；
  `pop_whitespace()` 取代现在的 `out.pop()`（吞空白时同步收缩末尾区间、删掉变空的区间）。
- **`Color` 节点在当前构建器里原地渲染**：进入时把颜色压入构建器的颜色栈，渲染子节点，退出时弹出；
  `push` 用栈顶颜色标记片段。它**不另开子构建器**，子节点的 `any_var_filled` 直接累加到父层。
  只有 `Group` 用子构建器（它要先渲染再决定整段要不要），保留时把子构建器的文字与区间按偏移并回父层。
  这样「空变量吞掉紧邻的一个空白」看的永远是**同一个**输出缓冲的末尾，跨颜色边界照吞——若 `Color` 也开
  子构建器，`${pinyin} $[accent]{${chaizi}}` 里 `${chaizi}` 为空时，它看到的是空的子缓冲，吞不到外面那个空格。
  用例：`(${pinyin} $[accent]{${chaizi}})` 在拆字为空时得 `(nǐ)`；同一模板经气泡的 `Template::render`
  （不 trim）渲染，行尾也不能多出空格。
- `render()`（纯文本，供 §4.6 两个出口）保留签名，内部取构建器的 `text`；新增 `render_styled()` 给
  `comment_for` 用。`trim()` 与 `comment_max_chars` 截断改在构建器上做：截断按 `char` 计（现口径不变），
  `…` 继承被截处前一个字的样式。
- ⚠️ `Template::references` 与 `has_literal` 的遍历**必须下钻 `Color` 节点**。漏了的后果不显眼：
  `$[x]{${debug}}` 不被认为引用了 `debug` → 调试上下文不准备 → 段静默为空；`$[x]{…\t…}` 不被认为是
  分列段 → 被折行。两个 walker 各加一条用例。

### 8.2 气泡（`wind-coordinator/src/tooltip.rs`）

`CompiledTooltip::render` 里文字经过的每一步都要带着样式走：

| 步骤 | 现状 | 改后 |
|---|---|---|
| 行求值 `sec.template.render` | `(String, bool)` | `(StyledText, bool)`；逐字段每行一个 |
| `promote` 稳定分组 | 按行排序 | 不变（行是整体） |
| `rows → raw`（按 `\n` 拆、去首尾空白行） | `Vec<String>` | 原始行仍存**纯文本**（复制 / 上屏取值），另存同形的带样式行供显示 |
| `display_lines`：换行符归一 | `replace` | 按字素簇走：`\r\n` 本就是一个字素簇，逐簇映射成 `\n`，样式跟随 |
| 单行截断 `max_chars` + `…` | 字素簇 | 同；`…` 继承前一簇样式 |
| 折行 `wrap` | 已是 `Vec<(&str, 宽度)>` | 元素加样式下标；`trim_end`、续行跳过行首空白都按簇做 |
| 段名 `sec.label.render` + `trim` | `String` | `StyledText`（`in_title = true`，字面文字角色 `title`） |

`wind-ui-types`：

```rust
pub struct TooltipSection { pub title: Option<StyledText>, pub inline: bool, pub lines: Vec<TooltipLine> }
pub struct TooltipLine    { pub text: StyledText, pub raw: u16 }
```

- `plain_lines()` 改为产出带样式的行（`[`、`]`、`: ` 以 `title` 角色拼进去），`to_plain_text()` 与新增的
  `to_styled()`（整块文字 + 区间，给自绘气泡与 macOS 下发）都从它取——画出来的行、命中换算的行、
  下发的行仍是同一份排列。
- `hit_at_line`、行等高命中：不受影响。
- **指纹拆成两个**：
  - `fingerprint()`（右键菜单核对用，`doc_fingerprint`）只 hash **文字与 `raw` 下标**（段名文字、inline、
    每行文字与 `raw`），不含片段；
  - 需要「内容含颜色都相同」判断的地方（如是否要重画）直接比 `TooltipDoc` 的 `PartialEq`，不另设指纹。

  理由：右键核对要防的是「段下标错位、取到别的段」，那只与文字结构有关。若颜色也进指纹，只改了颜色的
  刷新（用户在设置页改了模板里的 `$[…]`、或片段因角色归一变化）会让菜单弹出前后指纹不等，菜单退化成
  只剩「截图此窗口」，而此时取值其实完全正确。
- 右键菜单标签（`handle_tooltip.rs` `section_label`）、复制 / 上屏段与行、「复制全部」（`raw_plain_text`）
  全部取纯文本。

### 8.3 类型改动清单

| 类型 | 改动 |
|---|---|
| `CandidateItem.comment` | `String` → `StyledText`（`From<String>` 给无样式来源用；现有调用点改读 `as_str()` / `is_empty()`） |
| `TooltipSection.title` | `Option<String>` → `Option<StyledText>` |
| `TooltipLine.text` | `String` → `StyledText` |
| `View` | 加 `color_runs`；`upright_text` 的 leaf 回调加格的字节偏移 |
| `TextRenderer`（三后端） | 加 `draw_runs` |
| `wind-ipc` `encode_tooltip_show` | 加尾段 `runs`（Swift 解码同步） |
| `manager_macos.rs` `Forwarder.last_tip` | `Option<String>` → `Option<TooltipDoc>` |
| `TooltipDoc::fingerprint` | 只 hash 文字与 `raw`（§8.2） |

### 8.4 IPC / bridge

不涉及。`CandidateItem` 经进程内 `UiCommand` 通道交给 UI；TSF UIElement（游戏兼容）与 Android
拉取面（`candidate_pull`）都不带注释；宿主渲染传的是像素。唯一的线协议改动是 §7.6 的 macOS 气泡。

### 8.5 候选正文 `text` 为什么本期不纳入

1. **没有来源**：正文不是模板产物，没有变量可成为角色，也没有地方写内联色；
2. **UI 会改它**：候选窗按可用宽度截断（`truncate_candidate_text`）、协调器按 `ui.candidate.max_chars` 截断、
   命令 / 放宽检索的前缀是拼上去的——每一处都要做区间映射，收益却还没有需求支撑；
3. **可读性最敏感**：正文选中色（`text.selected`）是主题最着力调的一处，分色在这里最容易出事故。

将来有真实需求（例如给「补来的候选」前缀单独着色）时，本设计的 `StyledText` + View 区间能力可直接复用，
只需补正文这条链路的映射。

## 9 用户级角色色：本期不做（config-design-rules R3）

R3「外观覆盖主题」要求用户值逐格回落、只在一处合并。角色色放进 `config.toml` 会是
「角色 × 节点 × 状态 × 亮暗」的开放映射，既不是 REGISTRY 能登记的标量，也没有 R8 那种「勾选行」
能呈现的形态。而用户想要的两件事已有通道：

- **按自己的意思给某段上色** → 模板内联色。它本身就是用户值，优先于主题角色（符合 R3.1 的「用户 > 主题」）；
  写标准色名还能跟随主题与明暗；颜色非法只让该段回落正文色，不牵连别的段（符合 R3.4）。
- **整体换一套角色配色** → 派生主题：`base = "default"` + `[comment.roles]` 几行。文档站给出配方。

真出现「不想碰模板、也不想建主题」的需求时，再按 R3 加 `ui.candidate.comment_role_colors` 一类映射键。

## 10 主题编辑器（WindInputThemeEditor）同步点

编辑器对外的契约在面板文案里，以下几处要与引擎同步（路径相对编辑器仓根）：

| 文件 | 改动 |
|---|---|
| `src/lib/theme3/types.ts` `ViewNodeV3` | 加 `roles?: Record<string, Color>` |
| `src/lib/theme3/toml.ts` | `NODE_KNOWN` 加 `roles`；读写 `[comment.roles]`、`[comment.selected.roles]`、`[comment.hover.roles]`、`[tooltip.roles]` |
| `src/lib/theme3/strict.ts` | `roles` 的键**不是**开放命名空间：按公开角色表（从 `TEXT_ROLES` 抄一份，附来源注释）报未知角色，抓 `pinyn` 这类错字；角色值写 `transparent` 报提示；任何颜色值写成 `rgb()` / `rgba()` 报提示（编辑器 `src/lib/color.ts` 认、引擎不认，导出后在引擎里等于没配） |
| `src/lib/theme3/resolve.ts` + `engineParity.test.ts` | 求值角色表（含状态），与引擎对拍 |
| `src/lib/theme3/tokenMeta.ts` | 新 token 中文名：`accent_text` 强调文字、`success` 成功、`warning` 提醒、`error` 错误、`info` 说明、`tooltip_*` 同名加「（提示框）」；`TOKEN_GROUPS` 新增「文字标注色」「提示框标注色」两组 |
| `src/lib/theme3/presets/baseChain.ts` | `_base` / `_qingfeng` 改动后跑 `pnpm bake:theme` 重新烘焙（勿手改） |
| `src/lib/color.ts` | 已支持 `#RGB`，无需改；§5.5 修的是引擎一侧 |
| `src/components/form/ViewsEditorV3.vue` | 注释、提示框节点加「文字角色色」面板：常态 / 选中 / 悬停三栏（提示框只有常态）；文案写明回落规则 §6.3；配色页在「改了 `primary`、未改 `tooltip_accent`」时提示（§5.4） |
| `src/lib/theme3/contract.ts`（2026-09-28） | 标准色契约兜底（§5.4「引擎兜底」）：`contractLookup` 按名取色含兜底，源头是烘焙的 `BASE_CHAIN_V3._base`；提示框正文色按名取 `tooltip_text` 时同样含兜底；与引擎共用期望表 `contract-nobase{,-nobg}/expected.json` 对拍 |
| `src/stores/theme3.ts`、`contract.ts`、`VariantColorField.vue`、`ViewsEditorV3.vue`（2026-09-28） | 预置与保护（范围见 §15「预置范围」：新建全补，打开已有主题只补不改外观的 16 个，另 6 个列为「未写入（由引擎兜底）」可手动写入）：方案进编辑器（导入 / 切换 / 载入 / 新建 / 重置 / 撤销）时，继承合并后仍缺的契约名按兜底同一规则写进本层 [colors]（`fillContract`，保留契约名之间的 `${引用}`），标「已按默认补齐」，保存（写回工作区 / 推送 / 导出 .wtheme）才落盘；base 链有未知一环时不补。契约名删不掉（`setField` 拒绝、整块替换与 TOML 面板删掉的补回）；颜色面板「标准色」徽标 + 说明、置灰删除按钮；「恢复默认」= 本层不写时生效的值（继承来的或兜底值，内置方案仍是源预设值）；导入时缺色给一条诊断（toast / TOML 面板 / 工作区 report.md） |
| `src/lib/preview/candidateBox.ts`、`src/lib/preview/otherWindows.ts` | 预览样例带片段（如注释 `kao` 标 `code_hint`、气泡段名标 `title`），按 §6.2 求色，否则配了角色在预览里看不出来 |

## 11 设置端（wind-setting）

两处编辑模板的界面：候选注释对话框（`manifest.rs` `build_dialog_button_comment` →
`field_dialogs.rs` `build_comment_dialog`）与提示内容对话框的段表单（`dialogs/tooltip_sections_dialog.rs`）。

- **预览行**（两处都加）：模板输入框下方一行，显示当前主题下的渲染效果（常态；注释另显一行选中态）。
  - 值来自 core 新 RPC（如 `appearance.previewTemplate`），**由 wind-webdata 分发**：`WebDataHost`
    （`wind-coordinator/src/web_host.rs`）加一个「模板样例求值」入口，协调器实现它（它够得着变量求值）；
    主题按 `current_theme_name()` / `current_theme_is_dark()` 经 `wind_theme::load_resolved_dirs`
    （目录取 `theme_search_dirs()`）现取，颜色用 §6.1 的 `wind_theme::span_color` 求。设置端不依赖 wind-theme，
    求色必须在 core。
  - 入参：模板 + 场景（注释 / 段名 / 段内容 + `each`）。core 用固定样例（`你好`，候选专属变量给样例值）求值。
  - 回包：`{ text, fg, bg, runs: [{start, end, rgba}], selected: {fg, bg, runs} | null, problems: [{start, end, message}], swatches }`
    （`swatches` 是入参 `swatches: [名字]` 各自按内联色求法求出的颜色，给「插入颜色」的色块着色）。两类区间的坐标系
    不同，协议里写明：
    - `runs` 是**输出偏移**（`text` 里的字节区间，给预览上色）；
    - `problems` 是**模板偏移**（入参模板里的字节区间，给输入框标位置）。设置端的输入框显示的是**转义后**的
      模板（`\t`、`\n` 以两个字符显示，见 `tooltip_sections_dialog.rs` 的 `escape_template`），故设置端要把
      `problems` 的区间经同一张转义映射换算到显示文本上再标注。
  - 渲染用 windui 的 `RichText`（`src/ui/rich.rs`，支持逐 span 固定色），底色取回包的 `bg`（主题窗口底色 /
    气泡底色）。
  - `problems` 覆盖：颜色非法、名字当前主题查不到、未知 `key=`、未知变量名（后者今天只在候选栏里能看到）。
  - **新设置端配旧 core**：方法不存在即回错误，设置端静默隐藏预览行（不弹错、不占位），其余编辑功能照常。
  - 设置端 mock 回包补一条（`src/rpc.rs` 的 mock 表）。
- **调色板点选插入**：输入框旁「插入颜色」按钮，弹出标准色色块（§5.4，按当前主题着色、带中文名）
  + 「自定义 #…」。有选区（windui core `selection_of`）则包住选区成 `$[名]{选区}`，无选区插入 `$[名]{}`
  并把光标放进花括号。
  - 实现（`dialogs/template_assist.rs`）：触发器与色块用富文本可点 span（不请求焦点），外套
    `preserves_focus` 容器，插完输入框仍持焦点。光标用合成按键（Home + →/Shift+→）摆，不用
    `TextInput::set_selection`——它记成预置选区，对话框每次重开都会再兑现一次。输入框节点靠
    `on_click` 记下（windui 没有按 Element 查节点的入口，也没有聚焦通知）；没点过（键盘 Tab 进来）的，
    用时从触发控件往上找最近的、正文相同的输入框。「自定义 #hex」例外：焦点在那个小输入框里，插完回不到
    模板输入框（windui 无跨节点移交焦点的入口）。
- 参考区（可复制的变量表 / 示例）补一行 `$[颜色]{内容}` 与一个示例。**未做**：两处对话框现在都没有
  参考区（注释对话框改成了「查看文档」链接），语法说明在文档站。
- 新文案过设置项 label 的拼音检索表（「插入颜色」「预览」若进检索索引）。

## 12 文档站（WindInputDocs）

| 页面 | 改动 |
|---|---|
| `settings/appearance/candidate-comment.mdx` | 「语法」加 `$[颜色]{内容}` 小节（§4 的颜色值表、与可选段的关系、写错的表现、选中态回落）；加 `<Since>` |
| `settings/appearance/candidate-tooltip.mdx` | 同上一句带过并链到注释页；「平台支持与已知局限」里「段名没有单独的样式（加粗、颜色）」改为「段名可由主题配色（`title` 角色），不能加粗」；macOS 旧版 `.app` 单色的说明 |
| `settings/appearance/themes.mdx` | 新增「文字角色色」节：`[comment.roles]` / `[tooltip.roles]`、状态写法、回落规则、角色名全表（§3.2 + `TEXT_ROLES`）；新增「标准色」节：§5.4 表（没继承 `_base` 的主题由引擎兜底补齐，写明兜底规则与已知局限，2026-09-28 改）；「颜色格式」处 `#RGB` 与实现对齐 |
| 配方 | 「选中时保留编码高亮」「淡化装饰、突出信息」「派生主题只改角色色」 |

## 13 兼容与零回归

### 13.1 老模板里的 `$[`

出厂 `data/config.toml`、wind-coordinator / wind-ui 测试、文档站、设置端均为 0 处。用户模板要撞上，
得恰好写出「`$[` + 64 字节内的 `]` + 紧跟 `{…}` 配平」——注释模板里几乎不可能；撞上时表现为
`$[…]` 消失、内容照常显示（颜色非法按正文色），不丢信息。

### 13.2 对拍

| 期 | 对拍 |
|---|---|
| P1（前置） | **先录 golden，再改渲染层**：在**改动前的提交**上给 mock `imp` 加绘制调用记录（§7.4，这一步本身不改任何行为），录出「出厂主题 × 出厂模板 × 常态/选中/悬停 × 横排/竖排/直立」的 View 树 Debug dump + 绘制调用日志，入库作参照。参照必须来自改动前——事后用新代码录的 golden 只能证明「和自己一样」 |
| P1 | （执行记录见表下「P1 执行记录」）Windows 目标：**第一步先验证 wine 的 DirectWrite 会不会把 drawing effect 传进 `DrawGlyphRun`**（写个最小用例看 `_effect` 是否非空）；传不进就改由 Windows CI 或编译机跑，不要让下面几条在 wine 上「空转全绿」。① 非空断言：绘制后被改动的像素数 > 0（否则后两条可能在什么都没画时平凡成立）；② 多个 effect 全取基色 vs 无 effect → 缓冲逐字节相同；③ 色相断言：两色文本（如红 `ab` + 蓝 `cd`），在各 run 字形的 x 区间内，被改动像素按色相统计以该 run 的颜色为主；④ 半透明底 + 两种 alpha 的多遍绘制，与逐 run 单独绘制再合成的结果一致（守 §7.2 的二次预乘）；⑤ `measure` 结果与缓存条目数不受 `draw_runs` 影响。macOS CI：带色 line 与无色 line 的 typographic width 相等（§7.3）；golden 对拍（Linux）逐字节相同 |
| P2 | golden 对拍仍逐字节相同（P2 起协调器产出片段，出厂下必须全部解析到正文色而被丢弃，§6.4）。旧 `render()` 作为测试内参照实现保留：出厂模板 + 现有全部注释用例，`render_styled().text` 与参照逐字节相同；气泡 2⁵ 开关组合对拍（`tooltip.rs` 现有）改用 `doc.to_plain_text()` 断言，照旧全绿；每个 `$[…]{}` 用例同时断言「去掉 `$[…]{` `}` 后的模板输出相同文字」（§4.3 纯加法） |
| P3 | 复用 P1 前置的 golden：出厂主题 × 出厂模板 × 三态 × 三种排布，View 树 dump 与绘制调用日志与改动前**逐字节相同**（含「没有任何调用走 runs」）。另：全部出厂主题（`_base`、`_qingfeng`、`default`、`amber`、`jade`、`violet`、`msime`）× 亮暗，`RvViews` 各节点 `roles` 为空、§5.4 契约名（含全部 `tooltip_*`）可解析 |

**P1 执行记录（2026-09-27）**：

- wine 9.0 的 DirectWrite 会传 effect（`effect_passthrough_tests`），①～⑤ 在本机经 `cargo xwin test` +
  wine（须 `--test-threads=1`）与 Windows 编译机原生 MSVC 两处都跑过、全绿。wine **不做抗锯齿**、字形按
  整像素落位，交叠像素极少，故 ④ 的鉴别力以真 Windows 结果为准。
- ② 的适用边界：effect 把一个字形段切成几段分别光栅化，相邻两字的抗锯齿边若落进同一像素，「一段一次
  混合」与「两段先后混合」可以差 1——那是 DirectWrite 自身的分段行为。用例把区间边界放在空格上（墨迹
  互不相邻）；紧挨着的边界上「逐字节相同」不作承诺。
- ④ 的前提是两段墨迹确有交叠：用例取 `_j`（`j` 的下伸部回勾到下划线底下）并先断言共同覆盖像素存在，
  再断言「每遍各自预乘回写」的错误做法会被判出不同。两个全块字紧挨着从半像素起画，在 wine 下一个共同
  像素都没有。
- golden 在 **wind-ui 层**：输入是手工构造的「出厂模板渲染结果」样例（注释串、`TooltipDoc`），录
  View 树转储（只列非默认字段，新增字段取默认值时不出现）、绘制调用日志、缓冲 FNV-1a 摘要；矩阵为
  7 个出厂主题 × 亮暗 × 横排/竖排/旋转/直立 + 横排/直立两种内联编码，另加气泡。它**覆盖不到协调器**
  ——P2 的协调器侧对拍（`render_styled().text` 对参照 `render()`）是另一道，缺一不可。

**P2 执行记录（2026-09-27）**：

- 协调器侧对拍的参照是改动前模板引擎的**逐字副本**（`comment_legacy_ref.rs`，在动引擎之前单独提交），
  出厂与文档里的模板 × 取值夹具 × 截断上限 × 两种计数口径逐一比 `render` / `Template::render`；
  模板表不含 `$[…]{…}` 成立的写法（它们在改动前是字面文字，正是有意改变的输出）。
- wind-ui 的 golden 输入改为带协调器真实产出的片段角色（注释 `code_hint` / `code_rev` / `shuangpin`，
  气泡 `title` / `code_source` / `char` / `literal` / `readings`），golden 文件一字未改、仍逐字节相同，
  且日志里没有 `draw_runs`；把「丢弃等于正文色的区间」变异掉后 golden 变红。
- 实现与本文的偏差（均为实现细节，契约不变）：
  - `Span::role` 是 `Option<&'static str>`（取自 `TEXT_ROLES`），不是 `Option<Arc<str>>`；**清单外的
    变量名不产角色**、按正文色。§3.1 正文已按此改写；审查后补了两道守卫：运行期 warn-once、源码扫描测试
    （求值入口 ⊆ 清单）。
  - `Span::color` 是 `Option<Arc<InlineColor>>`；`CandidateItem::tooltip` 是 `Arc<TooltipDoc>`（§13.3）。
  - `InlineColor` / `span_color` / `body_color` 放在 wind-theme 新模块 `span.rs`，不在 `palette.rs`。
  - `RvNode::roles` 字段与 `span_color` 的角色分支在 P2 就位并有全分支单测；主题 `[*.roles]` 的
    schema / resolve / 状态门控仍是 P3，在那之前该字段恒空。
  - 可求值测试走真实 `notify_ui_update`：每个契约变量写进注释与气泡模板都不得回显 `${…}`。它当场发现
    `${emoji}` 在功能未开 / 非 comment 档 / 未命中时原样回显字面（`eval_var` 用了 `?`），已另行修复。

**P3 执行记录（2026-09-27）**：

- 引擎：`ViewNode.roles` → `RvNode.roles`；`""` / 未解析 token 不入表，`transparent` 与 alpha 0 拒收
  并 warn；`resolve_state` 的 nil 门控加上「有角色色」。出厂主题一律不配 roles（全部出厂主题 × 亮暗断言
  各节点与状态 patch 的 roles 为空），渲染 golden 逐字节不变。
- 非出厂测试主题 `wind-theme/testdata/themes/span-roles`（+ 派生 `span-roles-child`）：token 与亮暗、
  拒收、只写 roles 的 hover patch、派生逐键深合并与 `""` 撤销；wind-ui 层注释三态区间与气泡段名回落。
- 主题编辑器（WindInputThemeEditor `feat/span-colors`）：roles 读写往返、求值与门控、`spanColor.ts`
  （span_color 角色分支移植）、`TEXT_ROLES` 副本（主仓在场时逐项对拍）、严格模式报未知角色 / 不消费的
  节点与状态 / 全透明 / `rgb()`、新 token 中文名与两组、面板与预览、「改了 primary 未改 tooltip_accent」
  提示；`bake:theme` 后 `check:base` / `check:theme` / `check:engine` 绿。`wind-theme-kit` 的示例与
  `schema.md` 不在本仓，未同步。

**P4 执行记录（2026-09-28）**：

- core：`wind-coordinator` 的 `template_preview`（固定样例「你好」求值 + 模板偏移诊断）与
  `WebDataHost::template_sample`；wind-webdata `appearance.previewTemplate`（回包见 §11，另收 `swatches`
  给「插入颜色」的色块着色）。
  - **变量按场景放行**：注释只认 `eval_var` 的变量，段名 / 整段再加气泡候选级的 `word_code`、`code_source`、
    `debug`、`full_text`、`unicode_all`，逐字段再加 `char`、`readings`、`unicode` 与裸文本那两层。各组取自
    求值入口旁的常量表（`EVAL_VAR_NAMES` 等），测试扫源码逐表核对 match 分支；场景外的变量与真实渲染一样
    原样回显，并报「此处不可用」。
  - **入参防护**：模板解析加嵌套上限 32 层（超出的 `{` / `$[…]{` 按字面文字），花括号配对改为一次线性
    扫描（`Pairs`）——此前 5000 层嵌套即栈溢出、20 万个未闭合 `{` 要 37 秒，且这条路径在**配置**上就存在
    （这样的模板存进配置，候选渲染即让服务崩溃），不只是预览。配对与旧的逐次重扫按随机模板对拍。
    RPC 层另限模板 4KB、`swatches` 32 个。
  - 诊断与引擎口径对齐：气泡里的 `selected=` 提示不生效（不查名字）；名字解析出 alpha 0 报全透明；
    没写 `=` 的 `selected`、多余逗号与未知 key 逐条说明；主题加载失败只给一条总提示。兜底色（注释文字、
    窗口底、选中底、气泡底 / 字）收进 `wind_theme::fallback`，wind-ui 与预览共用。
  - 共用期望表（`span-roles/expected.json`）补 `inline` 一节：内联色的 `tooltip_` 作用域、`selected=`、
    没写 `=` 的 selected、查不到 / transparent / alpha 0 的回落；编辑器 `inlineColor` 逐项对拍。
- 设置端（wind-setting `dialogs/template_assist.rs`）：注释对话框（横排、竖排）与提示段表单的预览行、
  「插入颜色」；旧 core 静默隐藏预览。有意的外观变化：横排注释输入框宽 240 → 200（旁边要放「插入颜色」）；
  竖排注释改由专用 builder 建（行外观经 `field_row` 与通用字段一致，占位文字沿用清单 hint，不变）。
- **已知局限**：
  - 每次预览都重新从磁盘解析一次当前主题（设置页低频操作，不挂缓存）。
  - 自定义 `#hex` 插入后焦点留在那个小输入框，回不到模板输入框：windui 的 `request_focus` 只能给回调
    自己所在的节点，没有移交焦点的入口。光标位置照样摆好。
  - 输入框节点靠点击记下，键盘 Tab 进来的在用时从触发控件往上找最近的、正文相同的输入框。

**契约兜底执行记录（2026-09-28）**（§5.4「引擎兜底」）：

- wind-theme 新增 `contract` 模块：`NAMES`（11 个候选窗契约名，另各有 `tooltip_*`，共 22 个）与
  `fill_missing`；`resolve` 在 views 求值之后调用，`Coordinator::theme_palette` 同样接上。补值来自嵌入的
  `_base/theme.toml`，契约表测试（`span.rs`）的 `NAMES` 改为引用同一常量。
- 测试：空主题补齐全部 22 名且值等于 `_base`、自有值不被覆盖、契约外不加、写了却解析不出的不补、引用按
  本主题解析、缺 `accent` 时不借主题 `primary`；出厂七个主题 × 亮暗兜底前后调色板逐项相等；无 base 测试
  主题 `testdata/themes/contract-nobase`（仿「Switch风格」，另含写了却解析不出的 `text_dim = "transparent"`、
  `info = "${nowhere}"`）的内联色求色；移动端拉取面用例（夹具主题本就
  没写 base）断言拿到 `_base` 的 `error`。
- 与编辑器共用的期望表 `contract-nobase/expected.json`：22 个契约名与两个契约外名字的调色板终值，及其
  在注释 / 气泡里的内联色求色；编辑器按同一规则补齐后逐项对拍，另断言两边契约名清单相同。已知差异（早于
  本改动）：编辑器 `resolveColor` 对 `{ light, dark = "" }` 缺侧回退、不认裸写 hex，引擎调色板层反之，这类
  写法下「写了却解析不出」的判定两边不同，未纳入期望表。
- 追加（同日）：没写 `tooltip_bg` 时 `tooltip_text` 取渲染层常量（上文「例外」），新增测试主题
  `contract-nobase-nobg` 与其期望表，`contract-nobase` 去掉自己的 `tooltip_text` 以覆盖「写了 `tooltip_bg`」一支；
  去掉这条例外 → 3 条红；判据由「解析得出」退回「写没写」→ 1 条红。编辑器预置与保护见 §10。
- 编辑器预置的审查修补（同日）：「恢复默认」对来自预设的方案仍回到源预设值（一律回兜底值会把
  `${primary}` 联动换成字面色，新建方案凭空冒出一排「恢复默认」），只有外部导入的方案回兜底值；整体替换
  方案（TOML 面板应用、版本检出）保留值没变的「已补齐」标记；工作区载入缺色主题后显示「未写回」（文件里
  没有补齐的值），而判断「覆盖了本地改动」另按载入后编辑器那一份比，不误报。
- **预置范围（2026-09-28 用户拍板，混合方案）**：`text` / `text_dim` / `text_hint` / `accent` / `on_accent` /
  `selection_text` 这 6 个是视图节点的调色板默认色，写进本层 [colors] 会改候选窗外观（序号底、强调条、注释、
  编码栏、选中字），而引擎兜底补在 views 之后、不改。于是：**新建主题** 22 个全补（没有旧外观可破坏）；
  **打开已有主题** 只自动预置其余 16 个（`tooltip_text` 预置值等于兜底后气泡实际用的值，外观不变），那 6 个
  在配色页列为「未写入（由引擎兜底）」，带亮 / 暗兜底色块与「写入」（单个 / 全部，先确认「写入后序号、注释、
  强调条等默认色会按此生效，候选窗外观会变」，可撤销）。预置的 16 个里引用到未写入名字的（如主题没写
  accent 时的 `accent_text = ${accent}`）写成此刻的兜底值，否则文件里是指向不存在名字的引用、引擎按「写了却
  解析不出」处理；那个名字之后被写入（「写入」、复制新建全补），未改过的补齐项重算回 `${引用}`，联动恢复。
  主题自己用 `${名}` 引用了、却没写的契约名（如 `[comment] color = "${error}"`）打开时同样不自动写：今天那处
  引用解析不出，写了就解析得出、外观改变；它们也列入「未写入」。已写入的契约色都删不掉（含 TOML 面板删掉再应用、整块替换 colors，一律原样放回）；导入
  外部新版本时文件说了算、不放回。
- **已知取舍**：「未写入」的 6 个导出 / 保存时不强制补齐——文件里可能没有它们，内联色靠引擎兜底照样取得到；
  要它们进文件，用户在配色页点「写入」（外观随之改变）。
- 变异：去掉 `resolve` 里的兜底 → 2 条红；`_base` 垫在主题之上（盖掉自有值）→ 7 条红；`accent` 不按本
  主题解析（`accent_text` 不借主题强调色）→ 3 条红；去掉 `theme_palette` 里的兜底 → 拉取面用例红。
  另试过「最后一步对全部契约名无条件写入」：因为主题写了的名字在兜底表里就是本主题的终值，写回去值不变，
  是等价变异，不算漏测。

**靶机验证判据**（部署含 P1～P3 的构建后，由用户人工核对；同时核 Windows 候选窗与 TSF 宿主渲染窗口）：

1. **出厂零变化**：出厂主题（清风蓝、msime 各亮暗）下，候选窗注释、悬停提示的颜色、位置、宽度与升级前
   截图逐像素一致（横排、竖排各看一次）。（2026-09-28 起 `_base` 配了气泡角色色，§18：候选窗注释仍应逐像素一致，气泡颜色按 §18 核；
   位置、宽度仍应逐像素一致。）
2. **软键盘激活键**（唯一有意的出厂变化）：`_base` / msime / 清风系下，软键盘激活键（如 Shift 锁定）的
   文字是**白色**压强调色底，不再同色看不见（清风系此前是深蓝字，也变为白字）。
3. **内联色**：把全局注释模板临时改成 `$[error]{${code_hint|code_rev|shuangpin}}`：注释编码显示为红色
   （亮色 `#D93025`、暗色 `#F28B82`）；切明暗后不打字、直接看候选窗，颜色即随之切换。改成
   `$[#F80]{…}` 显示橙色 `#FF8800`；改成 `$[红]{…}` 显示原灰色、不出现字面 `$[`。
4. **气泡作用域**：给悬停提示「拼音」段模板写 `${char}：$[error]{${readings}}`：气泡里读音为浅红
   `#F28B82`（取 `tooltip_error`，不是候选窗的深红）；删掉 `$[error]{` `}` 后读音恢复白色。
5. **选中态**：注释模板 `$[accent,selected=error]{…}`：未选中的候选注释为蓝色，高亮候选的注释为红色。
6. **主题角色色**：把 `span-roles` 测试主题（`wind-theme/testdata/themes/span-roles/theme.toml`）复制到
   用户主题目录并切换过去：拼音方案下注释反查编码为 `#C00000`，选中候选的该编码变 `#FFE08A`；
   悬停提示段名 `[编码(五笔)]` 整体为强调色、读音 `#9AD0FF`。
7. **复制不带颜色**：在第 4 条的气泡上右键「复制全部」，粘贴出的纯文本与改模板前完全相同。
8. **宿主渲染**：在带 TSF 宿主渲染的应用（如 Windows 搜索框）里重复第 3 条，颜色与普通窗口一致。

### 13.3 性能

- 协调器：每次按键为整页（≤10）候选各渲染一次注释、一份气泡。**出厂配置下也会产出片段**——协调器不知道
  主题配没配角色，每个变量值、每段字面文字都照样带角色区间（出厂注释一条通常 1 个区间，气泡每行 2～4 个），
  外加角色名的 `Arc<str>` 克隆与构建器的合并判断。模板解析多一个 `$[` 分支。
- UI：出厂下每个片段仍要走一次 §6.2 求色（查角色表、比正文色）才能判定「等于正文色、丢弃」；丢弃之后
  绘制走旧路径，**绘制**零新增开销，但**求色**不是零。有颜色时每叶子一次 `SetDrawingEffect` × 区间数，
  alpha 不同才多遍。
- 以上开销量级都很小，但不作「零开销」的论断，以实测为准。验收：出厂配置下满页 `notify_ui_update`
  耗时增幅 < 5%、候选窗一次 `render_frame` 耗时增幅 < 5%（各 ≥3 次取中位）。
- **P2 实测（2026-09-27）**：「开销很小」不成立。按本节原方案（`Arc<str>` 角色、`Vec` 区间表、逐候选
  解析注释模板）实现后，满页 `notify_ui_update` 从 52.3 µs 涨到 62.0 µs（+19%）；主因是出厂下每条注释 /
  每条气泡行都多出区间表分配、每个变量值一次角色查找与引用计数，以及气泡文档每次按键的深拷贝变重。
  压回预算的做法（提交 `perf(candidate)`）：角色改为契约清单里的常驻 `&'static str`、在模板解析时算好；
  区间表内联前 3 段（smallvec）；内联色 `Arc` 共享；trim / 截断 / 拆行 / 归一 / 折行就地搬移；
  `CandidateItem::tooltip` 改为与右键菜单缓存共享的 `Arc<TooltipDoc>`（去掉一份既有深拷贝）；注释模板
  每页解析一次。之后 53.0～53.5 µs 对基线 52.9～53.5 µs，增幅在噪声内。其中后两条抵掉的是既有开销——
  单看片段本身仍有约 +10% 的成本，是靠这两条让总账回到预算内的。`render_frame`（Linux mock 文字后端）
  前后都在 340 µs 左右，无可见差异；Windows 真 DirectWrite 下出厂路径与改动前走同一个 `draw`，未另测。

### 13.4 前向 / 后向

- 旧引擎读新主题：`roles` 是未知字段，serde 忽略；新 token 无人引用。
- 新引擎读旧主题：无 `roles`，角色全回落正文色。
- 旧 `.app` / 新服务、新 `.app` / 旧服务：§7.6，均退化为单色，不出错。
- 模板里的 `$[…]{}` 在旧引擎上是字面文字（旧 `parse` 只认 `${`）——跨版本回退时用户会看到字面，
  这是新语法的固有代价，文档注明版本号。

## 14 不做的

- 字重 / 字号 / 下划线分段（改度量，§2 非目标）。
- 候选正文（§8.5）、编码栏、状态提示、Toast、菜单。
- 用户级角色色（§9）。
- `[comment.disabled]`、气泡状态态：引擎现在就没有这两个状态，不为本需求新增。
- 内联的 `hover=` 变体：悬停按规则 3 回落已够用；语法里预留了 `key=` 位置。

## 15 分期

| 期 | 内容 | 验收 |
|---|---|---|
| P1 | **前置：在改动前的提交上给 mock 加绘制调用记录并录 golden**（§13.2）；再做 View `color_runs`（含直立态按格切分、`upright_text` 回调扩参）；DirectWrite `draw_runs`（先验 wine 能否传 effect；drawing effect 按指针反查 + alpha 分组 + 直通值草稿区）；CoreText（`FromContext` + 按 CTRun 子区间 `CTRunDraw`）；`parse_hex` 补 `#RGB`（必须带 `#`）；`encode_tooltip_show` 尾段 + Swift 解码与 `TooltipPanel` 着色 | §13.2 P1 各条；Swift 解码新旧两形单测 |
| P2 | `StyledText` / `Span` / 别名表 / `TEXT_ROLES` 契约清单；macOS `Forwarder.last_tip` 改存 `TooltipDoc`、推送时现算 runs（§7.6）；`InlineColor` 解析；模板引擎 `Color` 节点与构建器；注释与气泡全链路带片段（截断、折行、段名、`plain_lines`）；UI 求色（§6.2，含内联色的状态回落与气泡 `tooltip_` 作用域查找）；标准色新 token 入 `_base`、`tooltip_accent` 入 `_qingfeng` 系四个主题（§5.4，内联写名字才有东西可查）；`wind_theme::span_color` 纯函数；软键盘激活键文字的第二级取色 `accent_text` → `on_accent`（`soft_keyboard.rs`，§5.4）；`TooltipDoc::fingerprint` 只 hash 文字与 `raw` | §13.2 P2；`TEXT_ROLES` 清单可求值测试（§3.1）；walker 下钻用例；吞空白跨颜色边界用例（§8.1）；只改颜色时菜单指纹不变的用例；`span_color` 全分支单测（Linux）；性能 < 5%（§13.3）；求色用例：规则 3 按值判（选中态正文色写成与常态同值 ⇒ 不回落；亮暗两档各判）、气泡里 `$[error]` 取 `tooltip_error`、`$[text]` 取 `tooltip_text`、主题未定义 `tooltip_x` 时回落 `x`；`_base` 全部契约名及其 `tooltip_*` 在亮暗两档都能解析（覆盖测试，契约表即断言表）；软键盘在 `_base` / `msime` / `default` 下激活键文字色的断言 |
| P3 | 主题 `roles`（schema / resolve / 状态门控，`transparent` 拒收）与角色色求值；出厂零变化对拍；主题编辑器同步（§10） | §13.2 P3；编辑器 `engineParity` 绿、`pnpm bake:theme` 后 `check:base` 绿；靶机人工验判据随 P3 给出 |
| P4 | wind-webdata 预览 RPC 与 `WebDataHost` 入口；设置端预览行、插入颜色、旧 core 静默降级（§11）；文档站（§12） | 设置仓五道闸门绿；新 label 过拼音检索表；`problems` 区间经转义映射标注正确的用例（含 `\t`） |

P1～P3 与设置仓 P4 同版发布（同 `candidate-tooltip-sections.md` 的做法），P1 不单独出包：
单有渲染能力、没有产出片段的来源，用户看不到任何东西。

**执行记录（2026-09-28）：出厂配色放进 `_base` 的角色表，不放进默认模板**。用户要「开箱就看到分段着色」。
先做过「出厂注释 / 气泡模板加 `$[…]{}`」一版，审查后改选主题角色，理由：

- **内联色优先于主题角色色**（§6.2 ①）：出厂模板一旦带 `$[info]{…}`，所有第三方主题给这些变量配的角色色
  都被压掉，除非用户自己去改模板——等于出厂配置替主题作者做了决定，角色表这条通道形同虚设。放进 `_base`，
  主题在自己的 `[comment.roles]` / `[tooltip.roles]` 里覆盖或写 `""` 撤销即可。
- **颜色本就是主题的事**：可读性由主题的底色决定；配在主题里，底色与字色由同一个作者负责。模板着色则让一份
  用户配置对所有主题同时生效，底色换了没人兜。
- **配置零改动**：默认模板、迁移、设置端产物、用户配置都不变，没有「改过模板的用户拿不到 / 老默认被当自定义」
  这类兼容问题。
- **已接受的取舍**：没继承 `_base` 的第三方主题（如「Switch风格」）不显色。契约兜底（§5.4 末）只补调色板名字、
  不补角色表——补角色表等于替第三方主题改外观，与兜底「不动布局与其它颜色」的原则相悖。

**再修订（同日，用户验收后）**：候选注释不上色，撤掉 `_base` 的 `[comment.roles]`，出厂注释回到 `text_hint` 的原外观；
气泡的 `[tooltip.roles]` 保留。原因：用户验收时要求保持原注释外观。想要注释分色的主题自己配 `[comment.roles]`，
用户也可在模板里写 `$[…]{}`。

落地见 §18。那一版模板着色里与方向无关的收获留下了：气泡旧开关全是旧出厂值时迁移不再写段列表
（守门 `default_legacy_switches_leave_no_sections_custom_ones_kept`），全局注释模板的 L1↔L2 同源守门
（`comment_template_l1_matches_l2`）。

## 16 已确认（2026-09-27）

1. **变量即角色，主题按角色配色**：变量渲染出的文字自动带同名角色，另有结构角色 `title`、`literal`；
   主题落点为视图节点下的 `roles` 子表，沿用状态 patch（`[comment.roles]`、`[comment.selected.roles]`、
   `[tooltip.roles]`）；出厂主题不配任何角色，外观逐像素不变。（2026-09-28 改为 `_base` 配一组气泡角色色，§18；注释不配。）
2. **状态回落**：某状态改了节点正文色、而未给某角色在该状态下的颜色 → 该角色与内联色回落到该状态的
   正文色；状态没改正文色 → 沿用常态角色色 / 内联色（§6.3）。
3. **内联语法 `$[颜色]{内容}`**：只管上色、不改可选段语义，可与 `{…}` 互相嵌套；颜色取 `#RGB` /
   `#RRGGBB` / `#RRGGBBAA`、标准色名、亮暗对、可选 `selected=` 变体；非法 / 未知颜色按正文色显示、
   不回显字面，设置页预览提示；`$[` 不构成完整语法时仍为字面文字。
4. **气泡里的标准色名先查 `tooltip_<名>`**，查不到再用同名色；`_base` 为气泡补一套适合深色底的同名色
   （§5.4 气泡表，对比度 ≥ 4.62）。
5. **规则 3 的「该状态改了正文色」按有效色值比较**：该状态正文色的最终值 ≠ 常态正文色的最终值才回落，
   不按主题写没写来判；亮暗两档各自比较（§6.3、§6.2 伪代码）。
6. **标准色契约新增清单**：`accent_text`（提升到 `_base`）、`success`、`warning`、`error`、`info`，
   每个都配气泡色 `tooltip_*`；不加 `link`。默认值与取值依据见 §5.4。
7. **契约里的每个名字在气泡里都有 `tooltip_<名>`**（由设计审查引出，按第 4 条的原则延伸）：补
   `tooltip_text_dim`、`tooltip_text_hint`、`tooltip_accent`（`_qingfeng` 系各自覆盖为本主题色），
   `tooltip_on_accent`、`tooltip_selection_text` 等于 `tooltip_text`；契约名在气泡里都有可读担保（§5.4）。

## 17 待确认

无。

## 18 出厂角色色（2026-09-28）

用户拍板：分段着色开箱可见，配色放进 `_base` 主题的角色表（选型理由见 §15 执行记录）。出厂默认模板保持无色。
验收后进一步收窄：**只给气泡配**，候选注释不上色、保持 `text_hint` 原外观（§15 执行记录「再修订」）。

### 18.1 角色表

```toml
# data/themes/_base/theme.toml
# [comment] 刻意不写 roles：出厂注释保持原外观。

[tooltip.roles]
full_text       = "${tooltip_accent_text}"
readings        = "${tooltip_accent_text}"
word_code       = "${tooltip_success}"
code_source     = "${tooltip_info}"
chaizi          = "${tooltip_warning}"
chaizi_all      = "${tooltip_warning}"
chaizi_code     = "${tooltip_info}"
chaizi_code_all = "${tooltip_info}"
unicode         = "${tooltip_error}"
unicode_all     = "${tooltip_error}"
```

- **都是 `TEXT_ROLES` 里的角色**，且在气泡的对应求值入口产出：`full_text` / `word_code` / `code_source` /
  `unicode_all` 是气泡候选上下文，`readings` / `unicode` 是逐字上下文，`chaizi*` 两种上下文都有。
- **单值与 `_all` 版同色**：`unicode` 与 `unicode_all`、`chaizi(_code)` 与 `chaizi(_code)_all` 是同一类信息的两种写法。
- **必须显式写 `tooltip_*`**：主题 roles 里的 `${token}` 在 resolve 期按字面解析，`tooltip_` 作用域查找只作用于
  模板内联色的名字（§6.2）。写成 `${info}` 在亮档会拿到为白底调的 `#1A73E8`，压在深色气泡上看不清。
- **段名里的变量**：§6.2 顺序里 ③（常态角色色）先于 ④（段名回落 `title`），所以「编码(五笔)」里的 `五笔`
  取 `code_source` 的 `tooltip_info`；`编码`、括号等字面文字是 `title` 角色，出厂未配，保持气泡文字色。
- **不配色的**：`${char}`、字面文字（`literal` / `title`）、`debug`、`dict`、`pinyin`、`emoji`、`code_hint` / `code_rev` /
  `shuangpin` 等——前缀与标签保持正文色，颜色只用来区分信息种类；调试与词库注释是长文本，整段染色反而难读。
- **派生**：`_qingfeng` 系、`msime` 都继承这张表且不改；`tooltip_accent_text` 取各主题自己的 `tooltip_accent`
  （清风系各自覆盖，§5.4），所以完整原文 / 读音随主题强调色变（蓝 / 橙 / 绿 / 紫）。气泡没有选中 / 悬停态。
- **测试夹具** `span-roles` 继承 `_base`，这张表随之进入它的期望表（与主题编辑器共用）；它的注释部分与分段着色之前相同。

### 18.2 可读性核对

气泡（§5.4 气泡表）：`tooltip_success` / `_warning` / `_error` / `_info` 在 `_base`、`_qingfeng` 两种气泡底亮暗四格上
最低 4.62（`error`，`_base` 亮档）；`tooltip_accent_text` 最低 4.90（`violet` 亮档）。这组数按**不透明**气泡底算；
出厂气泡底带 alpha（`F0` / `F5`），叠在纯白桌面上是最坏情况：`error` 3.89（Unicode 段出厂关着），其余最低 4.36。

守门：wind-theme `factory_role_colors_are_readable` 读 `data/themes` 全部出厂主题（点名必须有的七个），按与渲染同一
口径的 `span_color` 逐格求色——气泡各角色 × 段内 / 段名里真的着了色，不透明底 ≥ 4.5、叠白底 ≥ 3.5；注释的全部变量
角色在常态 / 选中 / 悬停都等于注释正文色（出厂不上色）。另有 `factory_theme_roles_are_the_base_table`（角色表逐项
等于上表、注释与状态 patch 与其余节点一律不配）、wind-ui golden（候选窗部分与分段着色之前逐字节相同，气泡只多出颜色
区间）、主题编辑器 `engineParity`（默认预览主题带这张表、注释预览是正文色）。

（先前给注释配 `info` 时逐格核过对比度：各出厂主题 × 亮暗 × 三态最低 3.85、均高于 `text_hint`。撤掉不是因为可读性。）

### 18.3 影响与取舍

- **第三方主题**：继承 `_base` 的自动拿到这张表，想换色在自己的 `[tooltip.roles]` 里覆盖、想去掉写 `""`；想给注释
  分色的自己写 `[comment.roles]`。没继承 `_base` 的主题气泡不显色（§15 执行记录）。
- **用户**：模板里写 `$[…]{}` 的照旧优先于角色色；注释想上色可写 `$[info]{${code_hint|code_rev|shuangpin}}`。
- **主题编辑器**（WindInputThemeEditor）重新烘焙 `_base`，预览的气泡按角色求色，与实机一致。
- **Android** 拉取调色板（`theme_palette`）不含角色表，移动端气泡的着色另行跟进。
- 发版说明写一句：「悬停提示默认按主题分色显示」。
