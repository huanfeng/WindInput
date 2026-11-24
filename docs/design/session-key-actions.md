# 会话态按键功能表（`keys.session_actions`）

> 目标：让「有输入会话时某个键干什么」成为一张可配置的表，与
> [schema-key-actions.md](schema-key-actions.md) 的 `key_actions`（无会话态）构成完整的两层。
>
> 直接诉求有三个：**Tab 向下翻页**、**CapsLock 向上翻页**、**自定义清空键**（Esc 太远，
> 想用 Tab）。但设计要能装下这一族后续需求——它们的共同点是「候选窗开着的时候按某个键」。
>
> 状态：**设计，未实施**。

## 1. 为什么不是「再加两个配置项」

现状是三套形状不同的机制并存，用户的三个诉求正好各落一套：

| 机制 | 形状 | 触发态 | 加一个键的成本 |
|---|---|---|---|
| `keys.key_actions` + 方案级 `[key_actions]` | **键 → 动词**（Map，白名单值域） | 无会话 / 全局态 | 白名单一条 + dispatch 一臂 |
| `NavKeys`（`keys.page_keys` / `highlight_keys`） | **组名 → 成对绑定**（StrList，组硬编码） | 有候选 | 改 `from_config` 加组 + 设置页加勾选项 |
| 各 handler 里的裸 `match vk` | 硬编码 | 混杂 | 改 N 处 |

第三类的规模：`VK_ESCAPE` 在协调器里**独立出现七处**（`coordinator.rs` 主路径、`handle_url`、
`handle_temp` ×2、`handle_mode`、`handle_special`、`handle_addword`、`handle_menu`）——与
[enter-behavior-clear-semantics.md](enter-behavior-clear-semantics.md) 记的「回车五条路径」
同族：分发即 `return`，主路径的任何逻辑都不惠及其余。

★ 一个功能「配置项散落」与「实现散落」通常是同一件事的两面：**Esc 有七个实现点，所以它
至今没有任何配置项**——没人愿意接七次线。加配置项之前先收口，否则第八次接线还在等着。

### 已有能力：Tab 翻页其实不用改内核

`NavKeys::from_config`（`wind-keys/src/keymap.rs:112`）的 page 组已有 `shift_tab`：
Tab → 下一页、Shift+Tab → 上一页。出厂默认把 Tab 给了高亮组
（`data/config.toml:473`：`highlight_keys = ["arrows", "tab"]`）。

把 `page_keys` 加 `"shift_tab"`、`highlight_keys` 去掉 `"tab"` 即达成诉求一。C++ 侧也通：
Tab 在 `_IsSessionKey` 表里（`wind_tsf/include/KeyEventSink.h:225`），有会话时恒吃恒转发。
**待确认的只是设置页有没有暴露 `shift_tab` 这个选项**（wind-setting 仓）。

## 2. ★★ 状态归属判据（本文档最该留下来的一节）

协调器里现存十三个会影响按键处理的状态。若按状态数决定表数，机制立刻失控。正确的做法是
先分类，判据是**「用户是不是停留在这个处境里、反复按键、且有肌肉记忆」**：

| 类 | 状态 | 判据来源 | 归属 |
|---|---|---|---|
| **A 闸门** | 密码框抑制 | `password_suppress` | **永不进表** |
| | 英文模式 | `chinese_mode` | |
| | 全角 / CapsLock ON | `IsFullWidth()` / `GetKeyState` | |
| **B 有输入会话** | overlay 模式内 | `state.active: Option<ModeKind>` | **本表** |
| | 有编码 / 有候选 | `input_buffer` / `candidates` | |
| | 分步上屏中 | `committed_segs` 非空 | |
| | 顶码待确认 | `top_commit_mode = pre_confirm` | |
| **C 模态窗口** | 右键菜单 | `menu_open` → `forward_menu_key` | 不进表，**共享两个动词** |
| | 快捷加词 | `add_word_active`（消费全部按键） | |
| **D 瞬时武装** | 配对跳出待定 | `_pairPendingDepth > 0` | **永不进表** |
| | 智能符号已武装 | `arm.armed` + 500ms | |
| | 夺取回退已登记 | `Rewind{snapshot, host_text}` | |
| | 检索范围已放宽 | `scope_relaxed` | |

四条判据，逐条给出理由：

- **A 闸门**回答的是「要不要走这条链」，不是「这个键干什么」。把闸门写成绑定，用户就得先
  理解闸门才能预测键的行为。
- **B 是一个状态，不是四个**。overlay 模式、有无候选、分步上屏、顶码待确认，全是「用户正在
  组合一段输入」的子态。子态差异进**分发端**，不进**表结构**——见 §2.1。
- **C 有自己的焦点与导航模型**，键位是窗口的属性而非输入法的属性。但它们与 B 共享
  `cancel` / `confirm` 两个动词，这正是 Esc 散成七处的由来。
- **D 是上一次按键的余波**，用户不在里面停留并决策。给它配键 = 让用户配一个自己感知不到
  何时生效的键，配了必然报「时灵时不灵」。

> ⚠️ D 类的危险在于它**看起来像**可配置项。`scope_relaxed`（末页再按翻页键 → 放宽检索范围）
> 就是典型：它是翻页键的**第二语义**，属于 `page_next` 这个动词的实现细节，不是一个新绑定。
> 做成配置项，用户就得先理解「什么时候算末页」才能预测这个键。

### 2.1 ★ 状态维度进分发端，不进表结构

`page_next` / `highlight_up` 只在有候选时有意义，但**不需要为此再开一张表**——在动词的消费点
守一行 `if candidates.is_empty() { return None }` 即可（`apply_nav_key` 现在就是这么写的）。

理由是成本的量纲：状态进表结构是**乘法**（3 状态 × 2 层 = 6 张表），进分发端是**加法**。
这条判据决定了本设计**到此为止**——不会有第三张表。

## 3. ★★★ 两表边界的真正立论：可达性是物理约束

「插入点不同」只是逻辑理由。更硬的一条是：**C++ 的转发规则本来就把键分成了三个可达性区间**，
两张表的边界与它重合，这不是巧合。

| 通路 | 哪些键 | C++ 机制 | 转发条件 |
|---|---|---|---|
| ① 会话键 | Enter / Space / Backspace / Delete / **Esc / Tab** / 方向 / Home / End / PgUp / PgDn / 数字 | `_IsSessionKey`（`KeyEventSink.h:224-228`） | **有会话时免费转发**，无需注册 |
| ② 可打印符号键 | `-` `=` `[` `]` `;` `'` `,` `.` | key_down 热键表 + `HOTKEY_POLICY_FORWARD_ONLY` | 须显式注册；无会话时放行给宿主 |
| ③ 修饰键 / CapsLock | `lshift` `rshift` `lctrl` `rctrl` `capslock` | keyup 白名单（`IsKeyUpHotkey`） | 须显式注册，且只有 keyup |

★ **`FORWARD_ONLY` 的闸门判据是 `hasComp || _hasCandidates`**（`KeyEventSink.cpp:341-343`）——
C++ 侧**早已在用「有会话」这个判据**决定要不要把这批键交给引擎。第二张表沿用同一判据，
两侧天然同构。

这也解释了为什么 CapsLock 是三个诉求里唯一麻烦的：它**三个区间都不在**——不在 `_IsSessionKey`
表里，keydown 在 `isToggleModeKey` 分支被吃掉且**从不发服务端**，只有 keyup 恒发。

⇒ ★ 可复用判据：**以后再有某个键需要动 C++，一定是因为它落在这三个集合的缝隙里。**
先查这三个集合，再决定要不要改 C++——而不是反过来先改再试。

## 4. 第二张表的判据是「有会话」，不是「有候选」

初版拟名 `candidate_actions`、判据取 `!candidates.is_empty()`。**否决**，三条理由：

1. **诉求本身就要求放宽**。「Tab 清空」在打了码**没出候选**时更需要——而 `apply_nav_key`
   第一行就是 `if state.candidates.is_empty() { return None; }`。
2. **Esc 现在的实际判据就是「有会话」**，七处无一例外。表若用更窄的判据，收编 Esc 时会
   悄悄改变它的行为。
3. **跨进程判据必须同构**（§3）。那条「C++ 吃键集必须 ⊆ Rust 出字集」的不变量，只有两边
   用同一判据才守得住；判据错位正是全角吃键翻转那次的病灶（`KeyEventSink.cpp` 里
   `_HasInputSession` 的注释仍留着那次的教训：判据不一致会形成「吃了再吐」，严格 TSF 宿主
   直接丢键）。

故定名 **`keys.session_actions`**，判据 = `has_composition || has_candidates`，
与 C++ 的 `_HasInputSession()` 一一对应。

## 5. 配置形态与动词值域

```toml
[keys.session_actions]
tab      = "page_next"
capslock = "page_prev"
# 或者：tab = "clear"
```

键名解析、白名单校验、`none` 哨兵三态，全部复用 `key_actions` 的既有实现
（`is_supported_key_action` 那套）。**不做方案级**——翻页在哪个方案都是翻页，加一层合并
与冲突检测是纯成本。同 schema-key-actions.md §2 判 A 类「方案级价值低」。

动词按「它动的是什么」分三组：

| 组 | 动词 | 来源 |
|---|---|---|
| 导航 | `page_prev` `page_next` `highlight_up` `highlight_down` | 收编 `NavAction` |
| 选择 | `select_candidate:2` `select_candidate:3` `select_char:1` `select_char:2` | 收编 `select_key_groups` / `select_char_keys` |
| 处置 | `cancel`（别名 `clear`） | 收编六处 Esc |
| 筛选 | `aux_code` `aux_code:page_next` | 辅助码（拼音候选的字形二次筛选） |
| 上屏 | `commit_highlighted` | 空格「有候选」那一支（上屏当前高亮候选） |
| 命令 | `command:<cmdbar 表达式>` | 与 `keys.key_actions` 的同名动词逐字一致（见 schema-key-actions.md §7 七期） |
| — | `none` | 禁用该键在本态的绑定 |

### ★ 与翻页共键做成 `aux_code` 的**参数**，不是新动词、更不是通用降级链

需求是「Tab 既翻页又进辅助码」（社区 issue #68 / PR #74）。三种表达形态：

1. **组合动词** `page_next_aux_code`：语义是「两个都做」——先翻页再进入，于是首按就越过了
   第一屏候选，还得在进入路径上另存 / 另恢复 `current_page` 去补救。且组合动词的名字随
   组合数相乘。
2. **通用降级链** `aux_code|page_next`：已在 key-resolver-unification.md §5「不做通用降级链」
   否决。
   具体到这里：它要求给每个动词定义「不适用」，而那些条件多是实现细节——`page_next` 的
   「失败」是已在末页，于是语法允许的 `page_next|cancel` 会在**末页取消整段输入**。
   可表达的组合远多于有意义的组合。
3. **动词参数** `aux_code:page_next`（采用）：值域封闭在 `aux_code` 上，配不出无意义的组合，
   与 `select_candidate:N` 同一套 `verb:arg` 写法。

判据就是本节 §5 那条的同构版本：**一组取值只对一个动词有意义 ⇒ 它是那个动词的参数**。
「与翻页共键」只对 `aux_code` 成立（`page_prev` 共键没有对应心智，别的动词没有这个需求）。

**语义：顺序即优先级**——先试辅助码，进不去才翻页。「进不去」不必新定义，`enter_aux_code`
本来就是「门卫没过返回 `None` 不吞键」的契约，四道门卫直接复用。于是这几件事全部天然成立，
无需各写一个特判：

| 情形 | 结果 | 靠哪道门卫 |
|---|---|---|
| 主输入路 + 辅助码可用 | 进辅助码，**不翻页** | — |
| 已在辅助码态内 | 翻页（模式内继续翻） | `active.is_some()` |
| 功能未开 / 方案无码表 | 退化成纯翻页键 | `enabled` / `files` |
| 无候选（空闲） | 放行，Tab 还给宿主 | `requires_candidates` |

⚠️ **共键与专用触发键在「门卫拒绝」时的归宿不同**：专用键**放行**（键落回原语义），共键
**降级并吞键**（它的第二身份就是翻页键）。实际差异看得见的一处：全拼下反引号是音节分隔符，
`backtick = "aux_code"` 被门卫拒绝后仍能当分隔符用，`backtick = "aux_code:page_next"` 则会
把这个键夺成翻页键（会话表的裁决排在分隔符臂之前）。

⚠️ 辅助码态里「再按触发键 = 退出」那条**只认专用触发键**（`aux_code`）。共键若也被认成退出键，
用户就永远翻不到第二页。模式内的键角色因此由 `aux_code_key_role` 三态裁决
（`Exit` / `PageNext` / `Other`）——用 bool 表达只能表达其中一件事，另一件会漏到兜底臂。

#### ★★★ 那个裁决取 `include_printable = true`，与同模式下的 `handle_candidate_nav` 相反

实施时才暴露的一条。符号键（`backtick` / `slash` / `-` / `=` …）在 `session_key_name_to_vk`
里**一律 `printable = true`**，而辅助码态的 `handle_candidate_nav` 取 `include_printable = false`
（那个取值是为了保护码元）。于是配在符号键上的 `aux_code` 绑定在模式内**查不到**，键一路落到
兜底臂「有候选则上屏高亮候选并退出」——用户只是想退出，却**把首选打了出去**。

取 true 之所以安全：**辅助码的码元只有字母**（`VK_A..=VK_Z` 累积臂），而字母在
`aux_code_key_role` 开头就被单独排除了；符号键在辅助码态不产出任何东西。

⚠️ 这是**既有缺陷**，不是共键引入的：把 `backtick = "aux_code"` 只写进 `session_actions`
即可复现，此前一直被双拼出厂那份 `[key_actions] backtick = "aux_code"` 兜住
（`key_actions` 没有 `printable` 维度）。★ 又一次印证「两张表维度不同 ⇒ 两张表都问不能对称写」。

📌 **同源缺口仍在**：符号键上的 `cancel` 等**其它**会话动词在辅助码态同样不可达。要一并解决，
得让整个辅助码态改用 `include_printable = true`，那就必须先把字母累积臂提到导航裁决之前
——是另一件事，不在本次范围。

### ★ 处置类只做 `cancel`，`clear` 收作别名（二期实施时定案）

初稿把 `clear`（清空编码）与 `cancel`（等同 Esc）列为**两个**动词，实施时合并成一个：

- 用户诉求原话是「ESC 在日常输入时有些太远」——要的是 **Esc 的替代键**，不是新语义。
- 「清空但留在模式里」在普通输入下与 `cancel` **完全无区别**（没有模式可退），只有
  overlay 模式下才分得出来。为一个没人要的差异，要额外定义五个模式各自的边界。
- 但 `clear` 这个词符合「清空」的心智，所以收作**别名**：用户怎么写都能用，内核只有
  一种行为。回写只用规范名 `cancel`，避免同一份配置在两次保存后出现两种写法。

★ 判据：**两个名字对应同一个行为是可接受的；两个名字对应微妙不同的行为不可接受**——
后者是最难查的一类配置陷阱。

`commit_raw` / `commit_first` 未实施：没有对应的用户诉求，且它们与 `input.enter_behavior`
的取值域正面重叠（§6.1 已论证那属于 Enter 的参数）。真要做，得先想清楚两者的关系。

### ★ `commit_highlighted` 是空格的**动作**，不是空格的**参数**

它与 `commit_first` 的区别正是 §6.1 那条判据：「上屏当前高亮候选」在所有模式都是**同一个
动作**（各模式空格臂有候选那一支），不是某个键的取值域。实现上各模式空格臂与本动词调的是
**同一个函数** `Coordinator::commit_highlighted`——另写一份派发的话，上屏的字对得上、词频 /
上屏历史 / 统计来源却可能悄悄分叉，没人会去对比。

- **两个空格变体，取舍相反**：
  - 临英 `space_as_input`（空格作输入字符）**不跟**：那是空格这个键被拿去干别的、上屏
    职责移交给了别的键；用户绑本动词要的恰恰是「上屏高亮」这个动作本身。
  - 联想态 `space_commits = false`（空格不选联想）**跟**：它表达的是「联想的高亮是输入法
    猜的，别替我选」，与用哪个键无关。此时本动词返回 `None`、不吞键，键回落原语义
    （判据写在 `commit_highlighted` 里；空格臂在调它之前自判，因为空格那一格还有「收窗后
    照常出空格」的下文，那是空格键的原语义）。
- **要候选**（`requires_candidates`）：无候选时按键回落原语义。空码空格的那套处置
  （`space_on_empty_behavior`）不属于「上屏高亮」。
- 网址 / 邮箱 / Unicode 三个模式在这里**有**出口（空格本就上屏它们的高亮补全项），
  与 `select_candidate:N` 在那里没有出口（数字是它们的合法字符）不矛盾——本动词不按序号选。

### `command:<表达式>`：执行命令，当前组合原样不动

会话态下按键执行一段 cmdbar 表达式，**不清空、不上屏**正在打的内容，吞键。判据与 `cancel`
同侧：**有会话即可**（`requires_candidates = false`，打了码没出候选时也能按），空闲时放行——
本表收的 Tab / 翻页键宿主另有原义。想空闲时也能按，绑到 `keys.key_actions`。

`SessionAction` 因此从 `Copy` 降为 `Clone`（载荷是 `String`）；`KeyBinds<A>` 的约束随之
从 `A: Copy` 放宽到 `A: Clone`。

### ★★ 判据挂在动作上，不挂在消费点上

`requires_candidates()` 是 `SessionAction` 的方法：导航类为真、`cancel` 为假。消费点
（`apply_session_action`）只问动作要不要候选，自己不写条件。

理由是消费点有**多个**（主输入 / mix / 候选导航），条件写在那里就是多份要保持一致的守卫
——本仓在「一个能力多条通路」上已经栽过四次。

## 6. 收编范围：哪些折算，哪些不折算

| 现有配置 | 形状 | 折算 |
|---|---|---|
| `keys.page_keys` | 组名 StrList | ✅ 一期 |
| `keys.highlight_keys` | 组名 StrList | ✅ 一期 |
| `keys.select_key_groups` | 组名 StrList | ✅ 三期 |
| `keys.select_char_keys` | 组名 StrList | ✅ 三期 |
| `keys.overflow.*` | Enum ×3 | ❌ 见 §6.2 |
| `input.enter_behavior` | Enum | ❌ 见 §6.1 |

### ★★★ 折算发生在**消费层**，不改写存储层（2026-08-11 修正）

这一条是踩了之后才立的。最初的实现把折算放在 `Config::normalize()` 里，折完还 `clear()`
掉四个原字段——**存储层被视图吃掉了**。用户报「感觉有些乱」时查实的后果：

- 设置页读 `config.get` → `Config::load` → `normalize`，四项恒为空 ⇒ 出厂默认
  （`page_keys` / `highlight_keys` / `select_key_groups` 三项非空）在界面上**全显示为未勾选**。
  这不是边缘情况，**每个用户都会遇到**。
- 用户勾选后保存，重开设置页又变空，像是没保存。
- 在高级表里删掉一条折算来的绑定，下次启动又被折算回来，**删不掉**。
- 高级表里凭空出现约 10 条用户从没配过的绑定。

⇒ **判据：折算属于「怎么解释配置」，不属于「配置是什么」。** 把视图写回存储就丢掉了用户的
原始意图，而设置页读的正是存储。

> ★ **姊妹表 `key_actions` 后来在同一个坑上翻了车**（现象一字不差：「删掉一条折算来的绑定，
> 下次启动又被折算回来」），因为五处 `trigger_keys` 的折算**留在了 `normalize()` 里**、而设置页
> 的写入口被收编到了折算的**下游**。它没有照搬本节的解法（那等于撤销五c 收编），改走
> 「把存储层搬到下游、折算一次性落盘后停止」——见
> [key-actions-materialization.md](key-actions-materialization.md)，其 §3 给出了比本节更好用的
> 通用判据：**用户在 UI 上做的删除落到哪个键？那个键能否压制出厂值所在的那个键？**

现落点为 `KeysConfig::effective_session_actions()`（纯函数视图），两个消费点各自调用：
`ConfigBundle::build`（运行时绑定表）与 `hotkey::Compiler`（TSF 转发白名单）。
⚠️ 后者**不能**直接读 `config.keys.session_actions`——那只是显式配的那部分，漏掉四组展开的键，
表现是翻页/选词键全失效。守门测试 `key_group_config_reaches_forward_set_without_explicit_table`。

配置文件里两套**各自保持原样**，设置页因此可以：四个勾选框如实显示、高级表只显示用户
显式配的（用户要求的「隐藏内部细节，只开放高级自定义」）。

### ★★ 折算顺序 = 消费点的判定顺序

合并成同一张表后，**一个键只能有一个动词**，而 `comma_period` 同时是选词键组和以词
定字键组的合法值。撞键时谁赢，唯一正确的依据是**收编前的消费顺序**：主输入路径上是
以词定字（`select_char_index`）→ 翻页/高亮（`apply_session_action`）→ 二三候选
（`select_key_offset`），故合并按同序进行、先折的占位；显式表最后覆盖。

搞反了的表现是「一直用的 `,` 突然从取字变成选次选」，而用户什么都没改——这类由迁移引起
的行为漂移最难联想到原因。

> 收编的**副作用是好的**：撞键从「两个函数各自命中、靠消费点顺序隐式裁决」变成「表里
> 只有一个动词」，隐性冲突变显性。设置页可以据此报冲突，而此前它根本看不见。

折算手法照抄 schema-key-actions.md 五c 的「四处 `trigger_keys` → `keys.key_actions`」，
连同那条已经用血换来的教训：

> ★★ **默认值必须留在被折算的那一侧。** 曾试图把默认值直接写进新表，被推翻：合并后
> `page_keys = []` 与「从没配过」同形，折算跳过、默认绑定仍在 ⇒ **用户清空的意图丢失**。
> 保持默认值在旧字段一侧则三种情况全对：没配过→折算出默认、改成别的键→折算出新值、
> 清空→折算出空。

### 6.1 ★★★ `input.enter_behavior` 为什么不折算成 Enter 的绑定

表面上它就是「Enter 键做什么」，而表里恰好有 `clear` 动词，看起来是同一件事。**不是。**
四条独立理由，每条单独成立：

#### ① 判据：一组取值只对一个键有意义 ⇒ 它是那个键的参数，不是动词

- `page_next` 能有意义地绑给 Tab / CapsLock / `=` / `.` → **动词**
- `clear` 能有意义地绑给 Tab / Esc / `\` → **动词**
- `commit` / `clear` / `commit_converted` 这一组，只有绑给 Enter 才成立 → **参数**

关键区别不在动词名，在**形状**：

| | 绑定表 | 策略参数 |
|---|---|---|
| 语义 | 「让这个键**也能**做 X」——加法 | 「这个角色**用哪种方式**履职」——在互斥全集里选一 |
| 未声明时 | 落默认链 | 无「未声明」，恒有一个取值 |
| `none` | 合法（禁用该键） | 不成立 |

#### ② Enter 是最后的兜底出口，不能被 `none` 哨兵架空

表的三态含「显式禁用」。用户写 `enter = "none"`，这一段输入就**没有上屏通路**了。这与
enter-behavior-clear-semantics.md §「临时英文是唯一例外」记的死锁**同形**——那次是
`space_as_input` + `clear` 两个开关叠加才归零，这次一步到位，连叠加都不需要。

★ 绑定表的 `none` 语义对「兜底出口」类的键天然有害。识别方法：**问「这个键被禁用后，
用户还有没有别的路完成同一件事」**。Tab 有（Esc 还在），Enter 没有。

#### ③ 待办的第三种取值证明它是枚举

enter-behavior-clear-semantics.md 已定：`commit_converted`（上屏已转换部分、丢弃剩余原码）
**必须作为第三种取值扩展，不要在 `clear` 内部加分支**。三个取值互斥且穷尽「Enter 如何结束
这一段」——这是枚举的形状。做成动词，则这三个动词只能绑给 Enter 一个键，而**一个只对单键
有意义的动词集，本质就是那个键的参数**，绕了一圈回到原点，还多了一层。

#### ④ 它有 per-mode 豁免，而绑定表没有「豁免」这个概念

临英的 `clear` 只管空缓冲，非空缓冲一律照常上屏（`handle_temp.rs` 那条被反转过一次的决策）。
作为策略参数，「临英对该策略有豁免」是可文档化的正常事；作为绑定，就变成「这个键配了在
某模式下不生效」——那正是最难排查的一类（`bound_action_yield_reason` 那五个同形成因就是
为此而生）。

#### ★★ 但实现层必须合并：配置不合并 ≠ 代码不合并

`clear` 动词的消费点与 `enter_behavior = "clear"` **必须走同一个函数**（把
`enter_clears_composition()` 推广成 `clear_composition_action()`）。否则「Tab 清空」与
「Enter 清空」会在**丢不丢 `committed_text`** 上慢慢漂移——而那已经拍过板（clear 一并丢弃，
与主输入路径一致）。

> ★★★ 提炼：**配置层按「这是绑定还是参数」分家；实现层按「最终做同一件事吗」合并。**
> 两个维度独立，不要用其中一个推另一个。参照 schema-key-actions.md §4.2.1 记的那次
> 「两处读同一概念却取值不同」——那里的结论是**先问它们是不是在回答同一个问题**，
> 两个对齐方向各被真机推翻一次。本节要防的是它的镜像：**不该合的配置硬合**。

#### 边界：表里出现 `enter` 键名怎么办

**显式拒绝并 `warn`**，不静默忽略。理由同 `is_supported_key_action` 白名单：静默忽略与
「配了没生效」完全同形，用户无从分辨自己拼错了还是功能坏了。

#### 对照组：Space **可以**进表

Space 也有一个 `input.space_on_empty_behavior`，但它管的是**空缓冲**时的处置，而「空格翻页」
这类诉求是**有会话**时的绑定——两者作用域不重叠，可以共存。

★ 所以判据不是「凡带 `behavior` 后缀的都不折算」，而是**作用域是否正面撞车 + 是否枚举形状**。
`enter_behavior` 的两个取值都在有会话时起作用，与表正面撞车；`space_on_empty_behavior` 不撞。

### 6.2 `keys.overflow.*` 同理不折算

它回答的是「这个动作**失败**时怎么办」（数字键 / 二三候选键 / 以词定字键超界），是动词的
**失败策略**，与绑定正交。硬并进去会让表里混进一个维度不同的东西。

`input.numpad_behavior`（follow_main / direct）、`input.top_commit_mode` 同族，都留在原处。

★ 这四个加上 `enter_behavior` 构成一整族「某键/某类键在某情形下的处置策略」。**只折算其中
一个，表里就混进了异类；全折算，表就退化成配置总汇**，失去「键 → 动词」的单一语义。

## 7. CapsLock：不走 TSF，走全局低级键盘钩子

> **本节结论是三版真机失败后重写的。TSF 侧原有的所有 CapsLock 会话态代码已全部移除，
> 且不应重新加回。**

### 7.1 为什么 TSF 做不到

TSF 里 `*pfEaten = TRUE` 的语义是「这个键事件我处理了」，**不是**「这个键没发生过」。
CapsLock / NumLock / ScrollLock 的锁定态由系统在**输入线程状态机**里维护，位置在
`ITfKeyEventSink` 回调**之前**——2026-08-11 真机实测：吃掉 keydown，大写照样翻转。
旁证：微软 KB127190 明说 `SetKeyboardState()` 改不了这三个键的锁定态。

### 7.2 「让它翻转再回敲复原」也不行（第二版，真机否）

`SendInput` 回敲一次能把状态翻回来，慢速按键下工作正常，但有两个无解问题：

1. **快速连按下有竞态**：物理事件与注入事件在输入队列里的相对顺序无法保证，任何「放行
   自注入事件」的窗口都可能误放行真实按键，表现为**大写卡住**。
2. **那次真实的状态变化是可观测的**：厂商 OSD 工具（联想等）会弹出大小写切换提示框。

★ 可复用判据：**事后修正在竞态下没有正确解**。这类需求只能「在它发生之前阻止它发生」。

### 7.3 现方案：`WH_KEYBOARD_LL`（`wind-keys/src/capslock_hook.rs`）

低级键盘钩子是用户态唯一在锁定态更新**之前**的位置（MS 文档：「the callback function is
called before the asynchronous state of the key is updated」），返回非零即阻止该事件继续传递。

装在**服务进程**，故不需要额外的 Broker 进程，也**不需要 F24 之类的代理键**——服务进程
本身就持有会话状态，钩子事件直接走进程内 channel。

三条硬约束（都来自文档，违反了都是无声故障）：

| 约束 | 后果 |
| --- | --- |
| 回调必须极快返回 | 超时后 Win7+ **静默移除**钩子，且「no way for the application to know」 |
| 安装线程必须有消息泵 | 钩子靠给该线程发消息来调用 |
| ★ 必须专用线程，**不能搭 UI 线程** | UI 线程渲染候选窗可能慢过 `LowLevelHooksTimeout`，钩子永久掉且无信号 |

⇒ 回调只做「读一个 `AtomicBool` + 一次非阻塞 `send`」，动作在消费线程里执行。

### 7.4 两条硬门控

1. **没配 `capslock` 就不装钩子**（用户明确要求）。判据取**编译后的绑定表**——动词/键名
   写错的条目已被 `ConfigBundle::build` 剔除，那些情况装钩子纯属白担风险。
2. **`SHOULD_EAT` 为真的时间窗必须尽量短**。钩子是全局的，标志滞留意味着用户在**别的
   应用**里按 CapsLock 也切不动大小写。

   ★★ 闸门的两个方向**后果不对称**：少吃只是「这一次绑定没生效」，多吃是「系统级按键
   失灵」。凡拿不准一律归零。故 `notify_ui_hide` 与 `handle_focus_lost` 都无条件归零，
   后者尤其重要——它是「用户去了别处」的最宽路径。

### 7.5 与 TSF 路径的关系：互补，不重叠

有会话时钩子吃掉整个 CapsLock，TSF 根本收不到；无会话时钩子放行，TSF 照常收到 keyup 并
走原有的大写状态通知路径。`hotkey.rs` 仍把 capslock 编进 `key_up` 表（带 SESSION 位）——
那个登记现在只用于让 `IsKeyUpSessionOnlyHotkey` 把它**排除在 toggle 语义之外**，
否则服务端会把它的 keyup 当成模式切换请求。

### 7.6 一次按下吃到底：配对状态优先于闸门

闸门「这次按下要不要接管」并不在 down 与 up 之间保持不变：绑定动作自己就会关掉它（选词上屏
⇒ 候选清空 ⇒ 无会话），而用户还没松手。于是钩子另记一个配对状态 `EATEN_DOWN`（「这次物理
按下已归我们」），由 `decide_eat` 裁决：

- 只看闸门放行 up：系统与宿主收到没有 down 的孤儿 up，TSF 把它当成「用户切了大写」转给服务端，
  表现为功能正常却在首次操作时闪一下状态提示泡（`show_status()` 拿空的 `last_status_text`
  比较，首次必弹）。翻页不触发，因为翻完页候选还在、闸门不归零。
- 只看闸门放行重复 down：按住超过自动重复延迟后，每个重复 keydown 都翻转一次真正的大写锁定，
  而最后的 up 仍被吃掉，CapsLock 状态悄悄变了且无日志。

扩大的吃键范围限于已经拥有的那一次按下；唯一越界是钩子被 `LowLevelHooksTimeout` 摘掉而丢了
一个 up，那时下一次按下会被多吃一次，其 up 销账后自愈。配对状态只由 CapsLock 的 down/up
与钩子装卸清零，`notify_ui_hide` / `handle_focus_lost` 的闸门归零不碰它：它们收回的是接管
新按下的权限，不是撤销一次吃了一半的按键。

## 8. 三条实施约束

1. **`printable` 标志不能在收编时丢**。`-` `=` `[` `]` `,` `.` 作导航键时，在临英 / 快捷输入
   里必须回落成输入字符——这就是 `NavKeys::classify(..., include_printable)` 那个参数的由来
   （`wind-keys/src/keymap.rs:135-147`）。换成 Map 表达后要有等价物，否则临英里打不出减号。
2. **每条通路都要接**。`apply_session_action` 的消费点是：主路径、`handle_mix_key`，
   以及 `handle_candidate_nav`（临拼 / 临英 / 特殊模式三个 handler 共用它）。只接主路径
   的表现是「快捷输入里 Tab 不翻页」——本仓「一个能力多条通路、闸门必须每条都接」已反复
   栽过，混输上屏那组通路栽了四次。

   > ★★★ **二期实测：网址模式是唯一漏接的那条**，而且是被测试抓出来的，不是审出来的。
   >
   > 一期没暴露，因为那时动词全是导航类，而网址模式原样累积文本、**从不产候选**——
   > 导航在那条路上本就无事可做，漏接与正确行为完全同形。二期的 `cancel` 一加进来，
   > 缺口立刻变成「Tab 在网址模式里按了没反应」。
   >
   > ⇒ 可复用判据：**新增一类动词时，要重查每条通路是否都接了消费点，不能因为「现有
   > 动词在那条路上没意义」就默认它不需要接。** 「当前无意义」是会随值域扩张而失效的
   > 隐含前提，而它失效时没有任何编译期或运行期信号。
3. **模态窗口只共享两个动词**。菜单 / 快捷加词的 `cancel` / `confirm` 跟随本表绑定（用户
   改了「Tab = 取消」，在加词小窗里也该是 Tab），**导航键不跟随**——那是窗口自己的模型。
   这条边界要在实施前定死，别留给临场判断。

## 9. 分期

| 期 | 内容 | 交付 | 状态 |
|---|---|---|---|
| 一 | 建 `keys.session_actions`；`page_keys` / `highlight_keys` 折算；CapsLock keyup 通路（含 C++ 那处） | 两个翻页诉求可用；Esc 一行不动 | ✅ 已实施，**未真机** |
| 二 | `cancel` 动词 + 六处 Esc 收敛为单点 | 「Tab 清空」；此后同类需求零成本 | ✅ 已实施，**未真机** |
| 三 | `select_key_groups` / `select_char_keys` 折算 | 二三候选键、以词定字键可自由改绑 | ✅ 已实施，**未真机** |
| 四 | 设置页改造（wind-setting 仓） | 不必手改 config.toml | 未开始 |

一期不碰 Esc：它是硬编码多处，风险与收益都在二期，而用户当时等的是翻页。

### 二期落地情况

- **`NavKeys` 泛化成 `KeyBinds<A>`**。一期写死 `NavAction` 的表在加 `cancel` 时立刻不够用
  ——新动词没有对应的 `NavAction`。★ 判据：**一张「键 → 动作」的表，动作类型不该由表来
  规定**。泛型后 `wind-keys` 仍不认识 `SessionAction`（那住在 `wind-config`，反向依赖会成环），
  实例化由协调器做。
- **六处 Esc 收敛为 `cancel_session`**，按 `state.active` 分派回各自的 `exit_*`。收敛前六处
  形态逐字相同、只有退出函数不同。
- **菜单与快捷加词刻意不收**（§2 的 C 类模态窗口）：菜单把键直接转发给 UI 窗口自行解释
  （`UiCommand::MenuKey`），协调器这边不决定语义；加词模式消费全部按键。要让自定义取消键
  在那两处生效，得改 `wind-ui` 的键解释器，是另一层的事。
- C++ 两处 CapsLock 判据同步从 `_hasCandidates` 放宽到 `_HasInputSession()`，与服务端的
  `has_input_session` 保持逐字一致（§7 的 ⚠️ 已预告这一步）。

### 三期落地情况

- **选词 / 以词定字动词带序号载荷**（`select_candidate:2` / `select_char:1`），配置面向人
  用「第几个」，转成 0-based 偏移只在消费点做一次。
- **两个动词刻意不在 `apply_session_action` 里执行**，返回 `None` 让键落到各自的既有消费点。
  ★ 理由是它们带 **overflow 语义**（候选不足 / 词长不够时按 `keys.overflow.*` 分三档处置），
  而那个函数只有「命中就执行」一种结局。收编改的是**配置从哪来**，不是执行路径——后者
  一行未动，overflow 与各模式的选中语义零回归。
- **删掉四个平行解析器**（`compile_select_key_group` / `compile_select_modifier_group` /
  `select_key_vks` / `select_char_vks`）。留着不用就是第二套真相源——此前
  `select_key_vks`（不含 `brackets`）与 `select_char_vks`（含）就被张冠李戴过一次，
  `brackets` 配置静默失效。收编后两者靠**动词**区分而非靠解析器区分。
- ⚠️ **补了一个一期的遗漏**：`lshift` / `rctrl` 这类修饰键名 `hotkey.rs` 认得、`wind-keys`
  的表里没有，而跨 crate 一致性测试的键名列表恰好没覆盖到它们。★ 那条测试的**覆盖面就是
  它的全部价值**——漏一个名字等于那个名字没被守。三期用到这些键名时才暴露。
- 顺带删掉 `coordinator.rs` 里一个重复的 `#[test]` 属性（既有，非本次引入）。它本身无害，
  但会让同类的 `duplicated attribute` 警告淹没在噪音里——而那个警告正是发现「测试函数体
  丢失」的唯一信号（二期就靠它发现过一处）。

## 10. 测试陷阱

- ★★ **每个模式的用例必须先断言「确实进了该模式」**。七处 Esc 的返回值很可能都是
  `ClearComposition`——触发键没生效、按键落回主输入路径，测试照样绿。这与
  enter-behavior-clear-semantics.md 记的假绿**同形**，那次是靠把判据临时改成 `false && …`
  做回退验证才逼出来的（四个 clear 测试须全挂、对照组须仍过）。
- ★ **对照组不可删**：没有 commit 模式的对照，无法区分「配置生效」与「该模式本来就不上屏」。
- ⚠️ **跑测试前先确认 `build_dev/data` 存在**。`input_flow.rs` 全部用例以
  `if !has_schemas() { return; }` 开头，缺数据时全部静默跳过、计数照绿。判据是耗时：
  假绿 0.0x s，真跑约 1.6 s。缺数据时先跑 `scripts/dev.ps1 gd`。
- ⚠️ CapsLock 那条**单测覆盖不到**（跨进程）。真机清单至少含：有候选时按 CapsLock 翻页且
  大小写锁定状态不变、无候选时 CapsLock 仍正常切大小写、配了 `toggle_mode_keys = ["capslock"]`
  的用户升级后行为不变。

## 11. 相关文档

- [schema-key-actions.md](schema-key-actions.md) —— 第一张表（无会话态），本表的形态来源
- [enter-behavior-clear-semantics.md](enter-behavior-clear-semantics.md) —— §6.1 的论据来源
- [key-resolver-unification.md](key-resolver-unification.md) —— 两张表之上的解析层统一（`KeyResolver`），方案级扩展见其 §7
- [../redesign/key-pipeline.md](../redesign/key-pipeline.md) —— S5「按钮自定义」的原始预留
