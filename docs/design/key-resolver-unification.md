# 按键解析层统一（`KeyResolver`）

> 目标：把已经各自长出来的四处「按键解析」收敛成一个显式命名的中间层，使**加一个作用域维度**
> （方案级 `session_actions` 是眼前这个）从「改 N 个消费点」变成「给 resolver 加一个输入」。
>
> 直接诉求：方案级 `[session_actions]`（特殊语系需要按方案改会话态键位）。但本设计要能装下
> 这一族后续需求——它们的共同点是「同一个能力有多个作用域维度」。
>
> 前置阅读：[schema-key-actions.md](schema-key-actions.md)（第一张表，无会话态）、
> [session-key-actions.md](session-key-actions.md)（第二张表，有会话态）。本文不重复那两篇的
> 立论，只处理**两张表之上的解析层**。
>
> 状态：**§7 第 1–3 步已实施**（`wind-coordinator/src/key_resolver.rs` +
> `Coordinator::session_action_for` + `Schema::session_actions`）。
> 第 4 步（设置仓只读按键总览）未做；主仓之外的三个仓（工具站 / 设置仓 / 文档站）
> 尚未同步，清单见 §7。

## 1. 现状：同一个模式已经实现了四遍

| 落点 | 做的事 |
|---|---|
| `CodetableGlobal::resolved` | 全局基线 ⊕ 方案逐字段覆盖 |
| `KeysConfig::effective_session_actions` | 键组折算 ⊕ 显式表，合并成运行时单一真相 |
| `comment::template_for` | 按当前 `ModeKind` 从 5 个来源里挑第一个有值的 |
| `schema_bound_modifier_vks` | 取所有方案的**并集**，而非活跃方案那一份 |

四个都是 resolver：把分散、分层的来源解析成消费点能直接用的单一视图。每一个单独看都是对的，
但它们**没有共同的名字、共同的规则，也没有一处写着「第五个来的时候该怎么办」**。

★ 「配置项越加越乱」有两种根因，修法相反：一种是**存储结构没归类**（修法是重排字段）；另一种是
**缺中间层、消费点直连存储**（修法是加一层，字段一个都不用动）。判据是「新增一个配置项要改几处
消费点」。本仓 `trigger_keys` 曾散在五处、`Esc` 散在七处——是第二种。**重排字段对第二种毫无作用**，
这是讨论「设置页怎么归类」时最容易走错的第一步。

## 2. hotkeys 架构全景（现状速查）

这一节是现状记录，不含新设计。写下来的理由：两张表的文档各写各的，没有一处能回答「一个键按下去
到底走哪条路」，每次讨论都要重新读一遍 `Compiler::compile`。

### 2.1 编译产物与两个 hash

全部来源编译进**一张** `CompiledHotkeys { key_down: Vec<HotkeyEntry>, key_up: Vec<HotkeyEntry> }`。
每条 entry 带两个 hash：

- `tsf_hash` = 裸 hash `|` 策略位 —— **给 C++ 看**，决定转发 / 抢占行为
- `match_hash` = 裸 hash（修饰位取通用位）—— **服务端匹配用**，恒不含策略位

★ 两个 hash 必须分开：策略位是给 TSF 的转发指示，服务端拿入站事件规范化后比对的是裸 hash。
混用的表现是「注册了但匹配不上」。

### 2.2 策略位与 `KeyDownPolicy` 四态

| 策略 | 位 | 语义 |
|---|---|---|
| `Always` | （无位） | 中英两模式都吃 |
| `ChineseOnly` | `0x40000000` | 仅中文模式吃（吃掉 `Ctrl+=` 会让宿主放大失效） |
| `Session` | `0x80000000` | 仅中文 + 有会话吃（置顶 / 删词，组合键由 `pin_candidate` / `delete_candidate` 配置，见下方 ⚠️） |
| `ForwardOnly` | `0x10000000` | 仅注册转发；**无会话时放行并继续按常规按键逻辑判定**，不是直接不吃——中文模式下翻页键、选词键要当标点处理 |

另有正交的 `GLOBAL`（`0x20000000`）：TSF 在「中文 + 焦点在文本框」时用 `RegisterHotKey` 让 OS 在
`WM_KEYDOWN` 派发前直接消费，规避 QQNT / Tabby 等 Chromium 类宿主无视 `pfEaten` 契约的加速键双处理。

> ### ⚠️⚠️ `RegisterHotKey` **不止 `GLOBAL` 位那一条**——`Session` 位的候选热键也走它
>
> 这是本文档 2026-08-24 补的最重要一条，**上一版只写了 `GLOBAL` 那条，害我据此断定
> 「加一个候选热键取值不用动 C++」，真机当场证伪**。
>
> 置顶 / 删词的 `RegisterHotKey` 由**候选可见性**驱动，不由 `GLOBAL` 位驱动：
> `CTextService::NotifyCandidatesVisibilityChanged` → `_RegisterCandidateHotkeys` /
> `_UnregisterCandidateHotkeys`，组合键取自 `CHotkeyManager::SessionHotkeys()`。
>
> ★★★ **它才是实际生效的那条通路**，`OnTestKeyDown` 里的 `IsKeyDownSessionHotkey`
> 分支平时根本走不到——`RegisterHotKey` 先于一切拿到键。
> **判据（可复用）**：tsf 日志里数字键的 `compat.key` 行 **`mods` 恒为 `0x0000`**，
> 连正常工作的 `Ctrl+数字` 置顶都没有一条带修饰位的记录 ⇒ 那条链路没在参与。
>
> ⇒ **改候选热键的值域，必须把这条通路一起数进去。** 它曾把组合键写死成
> `MOD_CONTROL` / `MOD_CONTROL|MOD_SHIFT`，还把「哪段 id 对应哪个动作」烧进 id 编码，
> 于是热键表、TSF 转发白名单、协调器判据三处都改对了也没用（已修，`47c1e58c`）。
> ⚠️ **单元测试对这一族无能为力**：Rust 侧测的是热键表内容，而表一直是对的，
> 错的是「谁去读表」。这类跨进程能力**只能真机验**。

⚠️ `ForwardOnly` **绝不能**加在真动作热键上。TSF 侧「无 Ctrl/Alt 且无会话就不吃」的闸门只认这个
标记；早先该闸门无差别地套在所有无 Ctrl/Alt 的 keydown 热键上，把 `shift+space`
（`toggle_full_width`）一并放行了——严格 TSF 宿主（EverEdit）不再回调 `OnKeyDown`，全半角切换
彻底失效；宽松宿主（记事本 / Chromium）照调才碰巧还能用。

### 2.3 八个来源

1. 散字段·两模式都吃：`switch_engine` / `toggle_full_width` / `toggle_toolbar` / `open_settings` / `take_screenshot`
2. 散字段·仅中文：`toggle_punct` / `add_word` / `open_add_word_dialog` / `toggle_s2t`（加词类额外带 `GLOBAL`）
3. 临拼直达热键：`input.temp_pinyin.hotkey`（`CHINESE_ONLY | GLOBAL`）
4. **`keys.key_actions` 的组合键部分** → key_down，动词过 `hotkey_action_entry` 白名单
5. **`keys.key_actions` 的修饰键部分** → key_up，action 记 `schema_bound`
6. 数字模板：`pin_candidate` / `delete_candidate` → `SESSION` 位
7. `effective_session_actions()` → keyup-only 键进 key_up + `SESSION`；其余进 key_down + `FORWARD_ONLY`
8. `toggle_mode_keys` → key_up，action 记 `toggle_mode`

⛔ **方案直达热键没有独立编译段**。`keys.schema_hotkeys` 已整体退役：先并入 `key_actions`
（`ad1cb846`，补动词 `switch_schema:<id>` 承载单向语义），随后连兼容折算也删掉
（`044a2a1a`，净删 133 行）——**现在残留旧键只在加载期告警一次然后清空，不折算、不生效**。
字段降级为 `legacy_schema_hotkeys`（serde rename 保住 TOML 键名）且已移出 `config_schema` 登记表。

别「顺手」把独立编译段加回来：它曾造成两个洞——排在 `key_actions` 前面使同键条目静默失效，
以及**不问键形态**地把解析结果塞进 key_down 表（单字符键进表会被 TSF 当热键吞掉，那个符号
从此打不出来）。`key_actions` 走 `route_of_key_action` 三分通路，本没有这两个洞。

### 2.4 三条通路：由**键的形态**决定，不由动词决定

```
keys.key_actions 的一条条目
    ↓ route_of_key_action(键名)
    ├─ Hotkey（带 Ctrl/Alt/Shift/Win）  → key_down 热键表 → dispatch_hotkey
    ├─ ModifierKeyUp（rshift 这类）     → key_up 转发集 → 服务端按 BoundAction 裁决
    └─ LeadingKey（单个有字符的键）      → 不进任何热键表 → 引导键链 bound_action_for
```

分水岭是**「英文模式下这个键要不要能出字」**：顶层链位置 8（英文模式 → PassThrough）之后的一切，
英文态下都跑不到，所以有字符的键只能排在位置 12。

### 2.5 ★ 现存缺陷：同一张表，值域按键形态分裂

组合键那条路要过 `hotkey_action_entry` 白名单，它**只认 3 个动词**：`toggle_schema:` /
`switch_schema:` / `special:`。而单键那条路的 `BoundAction` 值域含 `temp_pinyin` / `temp_english` /
`mix:` / `special:` / `none`。

⇒ `ctrl+alt+e = "temp_english"` 被 warn 掉后**静默失效**，而 `z = "temp_english"` 正常工作。
表面上是一张表，实际上不是。用户无从分辨自己拼错了、还是该组合根本不支持。

这是本设计要顺手收掉的缺陷之一（见 §8 注意点 11）。修法有两个方向，实施时择一：
(a) 补齐组合键白名单到 `BoundAction` 全值域；(b) 保持裁剪但**把 warn 变成用户可见的诊断**。
方向 (a) 需要先回答「进 overlay 的动词绑组合键时策略位怎么取」——`enter_special` 那段注释已经指出
同一个位在两类机制下后果相反，不能沿用 `key_actions` 现在「一律不带 `CHINESE_ONLY`」的做法。

## 3. 逻辑状态清单（补 [session-key-actions.md](session-key-actions.md) §2）

原表的四类判据仍然成立（判据是「用户是不是停留在这个处境里、反复按键、且有肌肉记忆」），
但清单缺两个成员，此处补齐：

| 类 | 状态 | 归属 |
|---|---|---|
| **A 闸门** | 密码框抑制 / 英文模式 / 全角 / CapsLock ON | **永不进表** |
| **B 有会话** | overlay 模式内 / 有编码或候选 / 分步上屏 / 顶码待确认 / **辅助码（补）** / **联想态（补）** | 进 `session_actions` 表 |
| **C 模态窗口** | 右键菜单 / 快捷加词 | 不进表，与 B 共享 `cancel` / `confirm` |
| **D 瞬时武装** | 配对跳出待定 / 智能符号已武装 / 夺取回退已登记 / 检索范围放宽 | **永不进表** |

### ★ 联想态是 B 类里唯一「判据需要被主动维持」的成员

联想态（`assoc_active()` = 候选首项来源为 `Assoc`）下文本**已提交、组合已结束**，
`has_composition` 为假，B 类判据只剩 `has_candidates` 一条路——而它由服务端应答**异步**回填，
赢不了下一次 `OnTestKeyDown` 的竞速（真机日志有铁证）。故联想态专门挂了一个**占位组合**，
才让 C++ 肯转发。

⇒ 别把「B 类判据 = `has_composition || has_candidates`」当成天然成立的。往 B 类加成员时，
先确认新成员在 C++ 侧**真的**满足这个判据，而不是只在 Rust 侧看起来满足。

## 4. 四条判据

### 4.1 ★★★ 静态维度 vs 动态维度 —— 决定折算合不合法

一个作用域维度，问一句：**它的取值，在写配置文件的时候能确定吗？**

- **能** → 静态维度（出厂默认 / `data/config.toml` / 用户 config）。`normalize` 时合并掉，存储层留一份。
- **不能** → 动态维度（活跃方案、当前 `ModeKind`、当前横竖排）。**必须留到 resolver，绝不能折算进存储层。**

`effective_session_actions` 的文档已经写下这条的一半，但立论用的是「`page_keys = []`（用户清空）
与从没配过同形」——那是**症状**。根因是：**在存储层做折算，等于把这个维度的取值烧死成一个**。
只有确定该维度永远只有一个取值时才合法。

⇒ 直接推论：**方案级 `session_actions` 不能靠给 `KeysConfig` 加字段实现**，只能给 resolver 加输入。

### 4.2 ★★★ 可达性并集 vs 语义表 —— 决定进程级资源的边界

|  | 内容 | 性质 | 下发 |
|---|---|---|---|
| **可达性集合** | 哪些键要送过来 | 所有维度所有取值的**并集**，静态 | 配置变更时算一次，推给 C++ |
| **语义表** | 这个键现在干什么 | resolver 的结果，动态 | **永不下发**，纯 Rust 侧 |

`schema_bound_modifier_vks` 已经在这么做，理由写在它的注释里：按活跃方案裁剪就得每次切方案重推，
漏一次的表现是「刚切完方案这个键不灵、点下别的窗口又灵了」。本设计把它**提升为通则**并让
可达性与语义表**同源**（同一个 `KeyResolver` 的两个方法），避免两者漂移。

⇒ 好消息：这样切之后，方案级 `session_actions` 对 C++ 是**零成本**——并集里多几个 hash。

⚠️ **但并集的代价不对称**，见 §8 注意点 5。

### ★ 并集判据不止用于 C++ 边界

上表容易被读成「跨进程边界才取并集」，那是把**例子当成了判据**。真正的判据是：

> **消费方持有的是进程级资源、且切换代价不可逆或不幂等 ⇒ 取并集，不取当前值。**

C++ 转发表只是最显眼的一例。Rust 侧同样有：`capslock_bound()`（§6 消费点表第 5 行）驱动
`sync_capslock_hook` 装卸 `SetWindowsHookExW` 全局钩子，那里写着**「重复 `SetWindowsHookExW`
会留下卸不掉的旧钩子」**。⇒ 它**必须**读 `reachability()`。若按活跃方案取值，方案 A 配了
CapsLock、方案 B 没配，每次切方案就装卸一次钩子——症状「切完方案 CapsLock 时灵时不灵」与
§4.2 开头那个 C++ 侧的已知案例同形，但成因在 Rust 侧，光有「C++ 收并集」这条口径抓不到。

⇒ 第 3 步给消费点加方案参数时，**逐个问「这个消费方拿到答案后要做什么」**，而不是统一加参数。

### 4.3 编辑分散 vs 审计集中 —— 决定 UI 归属

设置页的困难是结构性的：一个配置项有 N 个坐标（能力 × 层 × 态），而树形导航只能表达一个。
无论怎么挪都有一半用户觉得「不在该在的地方」——**这个问题无解，不要在它上面反复**。

真正缺的是：**没有任何一个地方能回答「这个键现在到底干什么」**。

解法不是挪配置项，是加**只读的总览视图**（键 / 当前动作 / 来源层 / 跳转到编辑处）。

★ 可编辑与否的判据：**集中视图写回的，是不是「同一批字段」？**

- 是 → 可以可编辑（如候选注释集中页，写的就是那 12 个字段本身）
- 否，会引入另一种表达同一件事的方式 → **必须只读 + 跳转**

**按键总览属于后者**：一个键的当前动作可能来自折算或方案层，反写就要决定写哪一层——那就是第二个
真相源，会重蹈 `trigger_keys` 五处并存的覆辙。

### 4.4 ★★★ 动词的方案归属 —— 决定 overlay 里查哪个方案的表

> 2026-08-30 补。起因是三个连续的现场（临拼下音节分隔符失效 / 双拼符号韵母键打不出 /
> 辅助码进不去），以及一个被问成「表该随谁」的设计问题。

诉求形态：临时拼音这类 overlay 里，按键功能表**该随主方案，还是随目标方案，还是独立配置**？

**结论：三个答案都错，因为问题问错了变量。** 同一张表里不同动词的作用域本就不同，
「整张表随 X」在另一半动词上必然出错。

按**这个动词的效果作用于什么**分三类：

| 类别 | 动词举例 | 作用域 | 取值出口 |
|---|---|---|---|
| **编码类** | 码元字符集、音节分隔符、辅助码触发键 | **产出眼前这批候选的方案** | `overlay_engine_schema(state)`，无 overlay 时即主方案 |
| **模式与方案切换类** | `special:*` / `temp_pinyin` / `mix:*` / `switch_schema` / `toggle_schema` | **恒主方案** | `active_schema_id()` |
| **会话导航类** | 翻页 / 高亮 / 选词（`session_actions`） | 主方案 + 全局，**跨模式一致优先** | 现状即正确 |

★ **判据一句话**：**这个动作是在解释「用户敲的码」，还是在决定「从这个输入环境去哪」？**
前者随产出候选的方案，后者随主方案。

- 临拼里的 `;`：在微软双拼下它是韵母 `ing`，属于**目标方案的编码规则** ⇒ 编码类。
  拿五笔（活跃方案）的码元集去问，答案恒为否，且**静默**——`ying` 一族音节在临拼里完全不可达。
- 临拼里的 `\`（主方案绑了 `special:fuhao`）：快符是**主方案的功能**，拼音方案不该知道它存在
  ⇒ 切换类，随主方案。「随目标方案」在这里立刻出错。
- 英文半角态：**没有目标方案**，「随目标方案」这个答案在这里无定义——这本身就说明它不是通解。

★★ **这个分类的好处是不引入新概念**：`overlay_engine_schema(state)` 早就存在，它回答的正是
「谁产出了眼前这批候选」。不需要新配置键，用户不必多学一层。

⇒ 推论（与 §4.2 末尾那条同源）：**给 `bound_action_for` 加方案参数时，不是统一加，是只给编码类
动词的查询点加。** 逐个问「它拿到答案要做什么」。

#### ⛔ 为什么不做「独立配置」

它同时撞两条既有禁令：§9 的 ⛔ 第三张表；§4.1 的动态维度不可折算——`ModeKind` 是动态维度，
独立配置**不能**靠给 `KeysConfig` 加字段实现，只能给 resolver 加输入，那本质上就是上表的方案，
只是额外付了一份配置面。而配置量会乘以上下文数（主 × 临拼 × 特殊 × 英文态…），
用户真正想区分的只有少数几个键。

#### ⚠️ 现状不是「随主方案」，是「overlay 里整张表没有裁决点」

容易被误记成前者。实际：`bound_action_for` 的调用点只有两类——**进入模式前**的触发键判定
（`is_temp_pinyin_trigger` / `match_special_trigger` / `try_z_fallback`），和**模式内**
「这个键是不是本模式自己的触发键」（`handle_temp.rs` 的复按判定、`aux_code_key_role`）。
没有任何 overlay 处理器拿它做通用功能分派 ⇒ 临拼里按 `\` 不会进快符，它落到兜底臂被当成
「其它键」**把首选打了出去**。

★ 另一半事实（`config_bundle.rs` 注释已记）：若在 overlay 里调 `active_key_actions()`，
拿到的是**主方案**的表——因为 `EngineManager::active`（引擎活跃方案）与 `State.active`
（overlay 模式）是两个不同的 "active"，进 overlay 只改后者。**这两句话都要记住，只记一句会
把「没有裁决点」误判成「裁决错了方案」，从而修错地方。**

## 5. 「一个键多个功能」怎么规划

诉求形态：「配多个功能，前一个条件不符合时自动落下一个」（举例：二三候选键在只有 1 个候选时的策略）。

**结论：不做通用降级链。** 按**条件由谁定义**分三层承载：

| 条件的性质 | 承载形态 | 现有实例 | 用户可配 |
|---|---|---|---|
| **态** —— 用户停留其中、有肌肉记忆 | **表的维度**（查表输入） | 有无会话 → 两张表；活跃方案 → 方案层 | ✅ 配各态下的绑定 |
| **动词自身的边界** —— 这个动作在极端输入下怎么办 | **动词的参数** | `keys.overflow.select_key`（候选不足时 `ignore` / `commit` / `commit_and_input`） | ✅ 配参数 |
| **实现细节** —— 活码前缀 / 引擎类型 / 无会话 / 英文模式 | **硬编码让位，不可配置** | `bound_action_yield_reason` 的五个成因、`FORWARD_ONLY` 无会话放行 | ❌ |

举例中的诉求精确落在第二层，**且已经实现了**（`keys.overflow`）。

三条理由：

1. **降级条件多数不是用户能表达的**。「只有 1 个候选」能表达，但「活码前缀」「非码表引擎」
   「宿主不支持删改」是实现细节。做成可配置链，用户得先理解全部条件才能预测行为——与 D 类
   瞬时武装被排除同源（配了必报「时灵时不灵」）。
2. **链的语义是「条件 → 动作」，那是规则引擎不是绑定表**。一旦允许 `tab = ["page_next", "commit"]`，
   下一个需求必然是「条件写在哪」「顺序能不能变」「能不能加条件」。终点是 DSL，而每一步看起来
   都只是小扩展。
3. **`overflow` 的形态比链更好读**：链把触发条件藏在「第二个绑定」里让读者自己推断；参数把条件
   写进字段名、取值只有三个、设置页就是一个下拉。

★ 判据（**这已是同一条的第三次应用**——前两次见 session-key-actions.md §6.1 拒绝把
`enter_behavior` 折算成 Enter 绑定、§2.1 状态维度进分发端）：

> **一组取值只对一个动词有意义 ⇒ 它是那个动词的参数，不是一条新绑定。**

### 现有的正确形态：同键多身份，靠 action 区分而非链

二三候选键**可能同时是 `toggle_mode` 键**，两条登记同 hash 进 key_up 表。TSF 侧白名单是集合、
重复无害；服务端按 action 区分。

⚠️ 故消费端**不能**用「key_up 里有这个 key_code」当判据（`is_toggle_mode_keycode` 按 action 过滤）。
这个坑已经踩过三次：`select_key_groups` 进 keyup 表是第一次，`schema_bound` 是第二次，
`SESSION_ACTION` 是第三次。**每次往 key_up 加东西都要重查这条。**

### 如果将来真的需要链

预留但不实现：**只允许两级，且第二级恒为「让位」而非另一个动词**——即「绑定 + 一个兜底策略」，
正是 `overflow` 现在的形状。允许任意动词的第二级，等于开了规则引擎的门。

### ★ 应用：中/英文态下同一个键两个身份（2026-08-30 用户诉求）

诉求原话：五笔方案下左 Shift 切到**英文态**（不是英文方案），**进入英文态后左 Shift 是另一个功能**。

先按 §5 那张表定性：中英文态是**「态」**（用户停留其中、有肌肉记忆）⇒ 表面上该是「表的维度」。
但 §9 的升级判据把它挡回来了：

> 同一个态下需要配置的键 > 2 个，**或**同一个键要在 > 2 个态里各自配置——到那时才把该态
> 提升为表的一个维度。

这里是 **1 个键 × 2 个态**，两个条件都不到 ⇒ **落「动词的参数」，不开表维度、不回退全局、
不做独立配置。** 形态是给 `toggle_mode` 这个动词加一个「在英文态下按它做什么」的参数。

⛔ **不把 `keys.toggle_mode_keys` 并进 `key_actions`**——§9 第一条（一功能一键 vs 一键一功能）。

#### ★★ 落点已经存在，而且那条链已经在做同一件事

左 Shift 是**修饰键**，走 **key_up 链**（`message_handler.rs` 的 `EVENT_KEY_UP` 分支），
远早于英文半角态那句 `return PassThrough`。⇒ **不需要动 TSF 转发集**
（该键本就在转发集里），这与"普通单键想在英文态生效"的成本完全不同——那个要动进程级
可达性并集，还受「C++ 吃键集 ⊆ Rust 出字集」铁律约束（见 project_fullwidth_eat_flip）。

更重要的是：那条链**已经在按状态给同一个键选身份**——

1. `handle_select_key_up`：修饰键作二三候选键，**有候选选词**
2. `handle_session_action_key_up`：keyup-only 会话绑定，**有会话归绑定**
3. CapsLock 状态同步
4. 方案级绑定
5. `is_toggle_mode_keycode`：**无会话归切换**

「有候选选词、无候选切换」与本诉求「中文态切换、英文态另一功能」是**同一个形状**，
只是判据从「有无候选」换成「中/英文态」。⇒ 新增一步插在第 5 步的分派里即可，不新建机制。

#### ⚠️ 必须处理的逃生口

英文态下若左 Shift 不再切回中文，用户**可能锁死在英文态**。出厂
`toggle_mode_keys = ["lshift", "rshift"]` 时右 Shift 还能回，但用户只配了左 Shift 就回不去了。

⇒ 该参数生效的前提是**至少还有一条回中文的路**（其余 `toggle_mode_keys` 非空，或配了
`toggle_mode` 热键）。不满足时拒绝该配置并 warn（同 `warn_aux_code_key_taken` 的做法：
「配了没反应」必须留下一句能直接定位的日志）。

★ 这条约束本身也是判据的一部分，不是实现细节：**任何「把唯一出口改成别的功能」的配置都要
先证明还有别的出口**。同类形状将来会在「唯一的取消键改绑」「唯一的翻页键改绑」上重现。

## 6. 目标形态：`KeyResolver`

落在 `ConfigBundle`——**唯一同时看得见 `wind-config` 与 `wind-keys` 的地方**（`SessionAction` 在前者、
`KeyBinds` 在后者，而 `wind-config` 不能反向依赖 `wind-keys`，经 `wind-cmdbar` 成环）。

```
存储层（一律不动）
  keys.toggle_full_width 等散字段     一功能一键 → 设置页各功能页
  keys.key_actions                    一键一功能，全局，无会话态
  keys.session_actions                一键一功能，全局，有会话态
  keys.page_keys / highlight_keys     折算来源，默认值的家
  keys.toggle_mode_keys               散字段，但进 key_up 表 → 必须计入 reachability
  <schema>.[key_actions]              方案级，无会话态
  <schema>.[session_actions]          方案级，有会话态          ← 新增
  <schema>.z_key_action               存量，按「方案级」来源计
                    ↓
KeyResolver
  · 预编译成 VK 键表，按 (态 × 层) 四象限
  · lead(schema, vk)    -> Option<(BoundAction,   KeySource)>
  · session(schema, vk) -> Option<(SessionAction, KeySource)>
  · reachability()      -> 并集（静态）：C++ 转发表 + Rust 侧进程级资源
                    ↓
消费点：见下表，按「拿到答案要做什么」分流
```

### ★ 消费点分流表（第 3 步的实际改动面）

`session_keys.classify()` 现有**五个**调用点。它们不是统一加一个方案参数就完事——
**两处根本不在按键路径上**：

| 位置 | 用途 | 走哪个方法 | 实施 |
|---|---|---|---|
| `coordinator.rs` 按键分类 | 这个键在会话态干什么 | `session_action_for`（当前方案） | ✅ |
| `handle_candidate.rs` `select_key_offset` | 选词键 → 第几个候选 | `session_action_for` | ✅ |
| `handle_candidate.rs` `select_char_index` | 以词定字 → 第几个字 | `session_action_for` | ✅ |
| `coordinator.rs` 冲突归属（`owners.push`） | **启动期日志**：码元 × 功能键冲突 | `session_action_for`（当前方案） | ✅ |
| `coordinator.rs` `capslock_bound()` | 装不装 CapsLock 全局钩子 | ★ **并集** `schema_session_vks` | ✅ |

### ★ 冲突归属：本文初稿把两个功能混成了一个未定项

初稿把冲突归属标成「语义待定：当前方案 vs 任一方案」，并倾向后者。查证后这个未定项**不成立**：

`code_char_conflicts` 的消费者是 `warn_code_char_conflicts`——**启动期写日志**，不是设置页。
而它比较的另一方是 `active_input_chars()`，即**活跃方案的码元集**。两边必须同方案才谈得上
冲突：拿并集去比，会报出「别的方案里占了」这种当前根本不存在的冲突。⇒ 走当前方案，无悬念。

「任一方案占用即提示、并点明方案名」（已拍板）适用的是**设置页配某个键时的占用提示**——
那属于 §7 第 4 步的按键总览，是另一个功能，届时它需要的是「键 → (方案, 动作)」的归属映射，
而不是本表这个 `Vec<&'static str>`。

★ 判据：**在设计文档里给一处代码标「语义待定」之前，先确认它的消费者是谁**。两个功能长得
像（都在回答「这个键被占了吗」），但一个是启动期日志、一个是设置页交互，取值范围正好相反。

### 两条路的起点不一样干净 —— 这是分期顺序的真正理由

措辞要精确：`bound_action_for` 本身有**六个**调用点（`handle_temp` ×3 / `handle_special` /
`handle_mode` / `handle_candidate`），但它们全是薄包装，**查表逻辑只有 `bound_action_with_source`
一处**（`active_key_actions()` 在全仓也只被它调用）。

⇒ **`lead()` 侧已经存在一个事实上的 resolver，只是没有名字**；第 1 步是给它正名 + 预编译。
而 `session()` 侧连这个都没有——五处**各自直连** `KeyBinds::classify`，没有任何一层可以插入方案维度。

这才是第 1 步先做 `lead()` 的理由，也是第 3 步实际工作量大于第 1 步的原因。**别把两步估成同一量级。**

三个设计要点：

1. **预编译成 VK 键表**。现在 `bound_action_with_source` 每次按键 clone 一个 `BTreeMap`
   （`active_key_actions()`）再线性遍历做字符串→VK 解析，且这在热路径上。
2. **输出带 `KeySource`**。不是锦上添花，见 §8 注意点 1。
3. **`reachability()` 与语义查表同源**。两者从同一份预编译数据派生，结构上不可能漂移。

## 7. 分期

| 步 | 改动 | 验证 |
|---|---|---|
| **1** ✅ | 建 `key_resolver.rs`；**全局层** `keys.key_actions` 预编译成 VK 表；`bound_action_with_source` 的全局分支改查表；两层共用同一个键名→VK 解析口 | 纯重构，`schema_key_actions` 27 项 + 新增 5 项单测 + `wind-coordinator` 全量 900+ 全绿 |
| **2** ◐ | 已做：`active_key_actions()` 改返 `Arc`（消除每键 clone 整表）。**剩下的搬家并进第 3 步**，见下 | `wind-engine` / `wind-coordinator` 全绿 |
| **3** ✅ | 方案级 `[session_actions]`：`Schema` 加字段 → `EngineManager` 供表+并集 → `session_action_for` 两层逐键合并 → `none` 哨兵 → 并集进转发表与 `capslock_bound` | 新增 4 项 `key_resolver` 单测、1 项 engine 合并/并集用例、2 项转发表用例；四 crate 全量 1500+ 全绿 |
| **4** | 设置仓：只读按键总览 | 与 1–3 解耦，可后置 |

第 1、2 步不改任何行为，可以合并提交；第 3 步才是新功能。

### ★★ 第 1 步为什么只预编译了全局层（方案层预编译的前置条件）

原计划是「三个来源一并提成 `lead()` 并预编译」。实施时发现**方案层预编译需要一个不存在的
失效信号**，故收窄，把前置条件留在这里：

- 方案层的源数据在 `EngineManager::key_actions_cache`（`schema_id → BTreeMap`），随
  `invalidate_schema` 失效。要在其上再叠一层 VK 预编译，就多出一个必须同步失效的镜像态。
- ⚠️ **现成的 `schema_generation` 不能当失效判据**：它只在**活跃方案真正改变**时 +1
  （`on_active_changed`），而设置页改 `schema_overrides` 走的是 `invalidate_schema`，**不 bump 它**。
  拿它做判据的表现是「设置页改了不生效、重启才生效」——本仓已有同型教训。
  且不能顺手让 `invalidate_schema` 去 bump `schema_generation`：方案往返键
  （`toggle_schema` 的来源记录）依赖它的精确语义「活跃方案变过没有」，多 bump 会让往返键误作废。
- ⇒ 前置条件是**给 `EngineManager` 加一个独立代际**，在 `invalidate_schema` 里 bump，
  与 `schema_generation` 并存而不混用。放在第 2 步或单独一步做。

★ 判断收益边界：全局层是静态的、条目通常也更多，预编译零失效风险；方案层已有一层缓存，
再叠一层的边际收益是每键省几次字符串解析，**而代价是一个新的「改了不生效」风险源**。
先做前者是划算的，后者要等失效信号到位。

✅ **已在第 2 步消除**：`active_key_actions()` 改返 `Arc<BTreeMap>`，命中缓存只加一次引用计数。
测试无需改（`.get()` / `.is_empty()` 经 `Deref` 照常可用），失效链路由
`active_key_actions_merges_schema_file_and_override_per_key` 守住。

### ★ 第 2 步剩下的两项为什么并进第 3 步

原计划第 2 步还要「`session_keys` 并入 `KeyResolver`」与「`reachability()` 收编
`schema_bound_modifier_vks`」。这两项**单独做是纯搬家**——它们的收益（两表同源、加维度只改
一处）要到第 3 步真正引入方案维度时才兑现。

而第 3 步会把 `session_keys.classify(vk, ..)` 改成 `session(schema, vk)`，**那 5 个调用点本来
就要动一遍**。先搬一次、第 3 步再改一次，是把一次改动拆成两次，中间那版还谁都不服务。

⇒ 合并到第 3 步一次完成。第 2 步只保留有独立收益的部分（`Arc` 改造）与把查证结论固化进代码
注释（`schema_bound_modifier_vks` 的枚举源说明）。

★ 判据：**一次重构若其收益完全依赖于下一步，就该和下一步一起做**——「先把结构搬好」听起来
稳妥，实际是让同一批代码承受两次改动、两次回归风险，而中间态没有任何消费者受益。

### 第 3 步的两个未定项 —— 均已定

1. **合并粒度：逐键合并**（用户 2026-08-21 拍板），与 `[key_actions]` 一致。方案只写想改的键，
   其余继承全局；「方案想完全接管翻页键」要把不要的键逐个写 `none`。
2. **冲突归属**：查证后未定项不成立，见 §6。

### ⚠️ 第 3 步的跨仓落点（`Schema` 加字段会牵动三个外仓）

给 `wind-config/src/schema.rs` 的 `Schema` 加 `session_actions` 字段，**没有任何编译期约束**
提醒你还要改别处。清单：

| 仓 | 落点 | 有无守门测试 |
|---|---|---|
| `../WindInputTools` | `src/lib/schema/schemas.ts`（zod） | ❌ |
| `../WindInputTools` | `src/tools/schema-editor/field-spec.ts`（表单 label/desc） | ❌ |
| `../WindInputTools` | `src/lib/schema/__tests__/schema-fields.test.ts` | ⚠️ 自陈「内核加字段时这里会漏报」 |
| `../wind-setting` | 已有 `keys.session_actions` 的 manifest 项 + `dialogs/session_actions.rs` 专用对话框，方案级需与之对齐 | 五道闸门（见该仓记录） |
| `../WindInputDocs` | config 参考页 + 用法页两处 | ❌ |

★ **不适用的一条**（写下来省一轮查证）：`Schema.key_actions` 是 `BTreeMap<String, String>` +
`#[serde(default)]`，新增 `session_actions` 同形，属**纯新增字段**，⇒ **不触发**「改已发布配置
字段的类型必须加 Value 层迁移」那条铁律。改的若是既有字段的类型，则另当别论。

### 第 4 步的前置：总览放哪个仓、与既有对话框什么关系

`../wind-setting` 已经有 `dialogs/session_actions.rs`（`keys.session_actions` 的行编辑器，
两列下拉）。只读总览**不是**它的替代品，两者关系要先定：总览按 §4.3 恒只读、按键排序、跨表跨层；
该对话框按 §4.3 属「写回同一批字段」故可编辑。⇒ 总览里点某一行应**跳转**到对应编辑处，
其中一个跳转目标就是这个对话框。

## 8. 注意点

### 架构不变量（破了就是回归）

1. **★★ resolver 的输出必须带来源层，不能只返回值。** `code_char_takes_lead` 靠它区分冲突性质：
   全局引导键 × 方案 `leading_chars` 是**跨层**冲突 ⇒ 让位给码表（全局配置无从知道某方案把这个
   符号当码元用了）；方案级绑定 × 同方案 `leading_chars` 是**同层**冲突 ⇒ 绑定优先（两条声明都
   出自这个方案，显式绑定比从字符集隐式推导更具体）。**合并时丢掉这个区分，全局引导键会抢走
   方案的码元。** 这是「简化」时最容易丢的一条。
2. **★ 折算只在消费层，绝不写回存储。** 设置页那四个键组勾选框读的正是存储层，折算若写回，
   界面永远显示为空。
3. **★ 进程级资源收并集，不收当前值。** 理由见 §4.2。**不止 C++ 转发表**——Rust 侧
   `capslock_bound()` → `sync_capslock_hook` 装卸的全局钩子同理（重复装会留下卸不掉的旧钩子）。
   第 3 步给消费点加方案参数时逐个问「拿到答案要做什么」，别统一加。
4. **★ 默认值留在被折算的那一侧。** 写进 `session_actions` 的话，`page_keys = []`（用户清空）
   就与「从没配过」同形，用户的清空意图静默丢失。

   > ⚠️ **这条只在「设置页写被折算的那一侧」时成立**，照抄到 `key_actions` 上会翻车——五处
   > `trigger_keys` 的默认值留在被折算侧，而设置页在五c 收编后改写**下游**的 `key_actions`，
   > 于是用户层根本没有能压制出厂值的键：删掉的绑定每次加载都被折算灌回来，备份/还原也带不走
   > 「我删过它」。现已改为一次性物化，见
   > [key-actions-materialization.md](key-actions-materialization.md)。
   >
   > 落笔前先答那份文档 §3 的通用判据：**用户在 UI 上做的删除落到哪个键？那个键能否压制
   > 出厂值所在的那个键？**

### 第 3 步会扩大并集 —— 代价边界不对称

5. **⚠️⚠️ keyup 侧并集安全，keydown 可打印键侧不安全。** 前者多转发一个不动作的键、宿主无感；
   后者带 `FORWARD_ONLY`，**无会话时必须放行给下游按标点处理**。而 `session_key_to_vk` 支持
   减号、等号、方括号、分号、引号、逗号、句点、斜杠、反引号、反斜杠等可打印符号键——一旦某方案
   把它们绑进 `[session_actions]`，并集会让**所有**方案都注册这些键。

   ⇒ **第 3 步的真机验证必须覆盖：在没绑这个键的方案里、无会话时，打这个符号能正常出字。**
   不验的话症状是丢键，且只在部分方案下复现。

### ★★ `reachability()` 这个名字会承诺它做不到的事

`reachability()` 要收编的 `schema_bound_modifier_vks` → `EngineManager::all_key_action_keys()`，
而后者**只遍历 `self.available`**。overlay 方案 `hidden = true`、**不进 `available`**
⇒ overlay 方案自己 `[key_actions]` 里的键不在这个并集里。

⚠️ **但这不是「配了不生效」的缺口，别照这个定性去修**（本节初稿正是这么定性的，查证后推翻）：

> overlay 方案的 `[key_actions]` **根本没有消费路径**。`bound_action_with_source` 查的是
> `EngineManager::active_key_actions()`，它按 `EngineManager::active`（活跃方案）取表；
> 而进特殊模式只改 **`State.active`**（`ModeKind::Special(idx)`），**不动活跃方案**。
> ⇒ overlay 模式下查的仍是主方案那张表。既然不消费，不转发就是**自洽**的，不是漏。

`manager.rs` overlay 注册表那条注释（「⚠️ `all_key_action_keys` 至今仍只遍历 `available`——
**将来若要收** overlay 方案自己的 `[key_actions]`，那里也得换源」）措辞是准确的：它说的是
将来要收时得换源，不是现在漏了。

★ 真正的风险在**命名**：`all_key_action_keys` 是描述性的（读者自然会去看它遍历什么），而
`reachability()` 承诺的是「全集」。将来真要支持 overlay 的 `[key_actions]` 时，**枚举源与消费
路径两处都要改**，名字不该提前替其中一处打包票。

⇒ 第 2 步的处置：收编时**要么叫 `reachability_of_available()`**，要么保留 `reachability()`
但在文档注释里写明枚举源是 `available` 以及为什么这样是自洽的。**不要**顺手「修」成
`installed_schemas`——那会让一批没有消费路径的键进转发集，是纯粹的多余转发。

★ 判据（本节自己踩了一次）：**看到「A 有配置项、B 不收 A」先别急着叫缺陷，先查 A 有没有
消费路径**。没有消费路径的配置项不被收集是一致，不是漏。

### 跨 crate 契约（无编译期约束）

6. **⚠️ 会话态键名表有两份**：`wind-config::hotkey::session_key_to_vk` 与
   `wind-keys::keymap::session_key_name_to_vk`（`wind-config` 不能依赖 `wind-keys`，成环）。
   靠 `session_key_tables_agree_across_crates` 守门。**加键名要同步两处。**
7. **⚠️ 方案级 `session_actions` 的编译只能在 wind-coordinator。** 不要顺手放进
   `EngineManager::key_actions_cache` 旁边——那里看不见 `KeyBinds`。这是个很自然会犯的错，
   因为 `key_actions` 的缓存确实在那儿。

### 语义与存量

8. **⚠️ `none` 哨兵必需。** `merge_toml` 只能新增 / 覆盖，**无法表达「删除 base 的某个键」**，
   所以「本方案禁用某绑定」只能靠显式值，不能靠「把这行从 override 里删掉」。
9. **⚠️ `z_key_action` 按「方案级」来源计**（它与 `leading_chars` 是同层冲突）。收编进 resolver
   时不能改这个归类。

### 热路径

10. **⚠️ 预编译是这次重构的实际收益之一，别只搬结构不做预编译**，否则这活白干（见 §6 要点 1）。

### 顺手能修的既有缺陷

11. **组合键动词白名单与单键值域不一致**（§2.5），以及**同一个键被多处绑定时静默失效**
    （`.find()` 先到先得、遍历顺序即胜者，schema-key-actions.md §1 记录在案）。

    ★ 收敛成单一 resolver 后，重复绑定在**构建期**就能发现——**应该顺手 warn 出来**。判据同
    `warn_unknown_session_actions`：静默忽略与「这个功能坏了」完全同形，用户无从分辨自己拼错了、
    还是该功能压根没实现。

    ★ 这是这类重构最容易被浪费的红利：分散查表时「谁赢」是遍历顺序的副产物，**没有任何一处代码
    有资格报错**；收敛之后构建期天然拿到全集，冲突检测从「要额外实现」变成「顺手就有」。

### 测试

12. **⚠️ 跑测试前先确认 `build_dev/data` 存在。** `input_flow.rs` 全部用例以
    `if !has_schemas() { return; }` 开头，缺数据时全部静默跳过、计数照绿。判据是耗时。
13. **⚠️ 默认值相反的成对开关，测试必须交叉翻**，否则接错线照样绿（联想的回车 / 退格两开关是
    现成教训）。
14. **⚠️ 每个模式的用例必须先断言「确实进了该模式」**，否则触发键没生效、按键落回主输入路径，
    测试照样绿。

## 9. 明确不做

- ⛔ **不**把 `keys.toggle_full_width` 等散字段并进 `key_actions`——方向相反（一功能一键 vs
  一键一功能），并了设置页就变成给普通用户看的裸表。**统一发生在 resolver 层和 effective view 层，
  不发生在存储层。**
- ⛔ **不**开第三张表——状态维度进分发端是加法，进表结构是乘法（session-key-actions.md §2.1）。
  联想态是这条判据实施后的第一个真实案例，**它验证了判据是对的**：加联想只多了两个开关两个函数。
- ⛔ **不**做通用降级链（§5）。
- ⛔ **不**做可编辑的按键总览（§4.3）。
- ⛔ **不**给 `OverlaySpec` 加 `trigger_keys` / `hotkey`——第三个入口，正是上一轮重构消除掉的东西。
- ⛔ **不**让 overlay「整张表随目标方案」，也**不**给 overlay / 中英文态开独立的按键功能表
  （§4.4、§5 末两节）。差异按**动词的作用域**与**动词的参数**表达，表的份数不变。

### ⚠️ 真正的增殖风险不在「第三张表」

「第三张表」有 §2.1 挡着，而**分发端开关的组合爆炸**没有任何东西挡着：现已有
`input.enter_behavior`（全局）、`association.enter_passthrough`、`association.backspace_cancels_only`
三个「某态下某键的参数」。再来「URL 态回车干什么」「联想态 Tab 干什么」就是 N 态 × M 键。

★ **升级判据**：同一个态下需要配置的键 > 2 个，**或**同一个键要在 > 2 个态里各自配置——到那时
才把该态提升为表的一个维度。在此之前开关更便宜也更可解释。

## 10. 相关文档

- [schema-key-actions.md](schema-key-actions.md) —— 第一张表（无会话态），动词值域与三条通路的立论
- [session-key-actions.md](session-key-actions.md) —— 第二张表（有会话态），状态归属判据与可达性三区间
- [special-mode-entry-hotkey.md](special-mode-entry-hotkey.md) —— 特殊模式直达热键收编进 `key_actions` 的经过
