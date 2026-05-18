# 命令直通车 follow-up (R2)

> **状态**: 本文记录的全部修订 (函数命名宪法 / 权重 tier / options bag / `$SS` 字符串数组) **已经实施完成**, 不再有待办。当前实现的最新行为以代码为准, 本文保留作为设计演进史 + 决策上下文 + 验收清单。新增改动 (例如多 group 支持) 在 [command-bar-design.md](./command-bar-design.md) 和 [candidate-actions.md](./candidate-actions.md) 中迭代, 本文不再追加。
>
> **修订对象**: 对 [command-bar-design.md](./command-bar-design.md) 的修订增量。
> 原文档在 P3~P5 落地过程中暴露了三个架构欠考虑点 —— 函数命名缺规范、权重模型让短语过度靠前、marker 配置语法缺乏弹性。本文给出修订后的设计与迁移顺序。

## 0. 背景与适用范围

### 0.1 三个问题

1. **函数命名不一致** —— `clip.copy` / `key.tap` 用 namespace.verb，`dict.addword` 没有 verb 分隔，`type` / `open` / `search` 完全裸名，`ime.setting` 不是动词。元信息只有 `Pure` 一个标志位，区分不出 5 种异质语义。
2. **权重让短语过度靠前** —— `resolvePhraseWeight` 中 `10000 - position` 的 fallback 公式让旧 yaml 的 `position: 1` (常见默认值) 变成 `weight=9999`，几乎置顶。叠加混输引擎给所有 phrase 候选加 `+CodetableWeightBoost (10M)`，短语 tier 内部档位失去意义。
3. **marker 把配置编码进语法** —— `$CC` vs `$CC1` 通过 marker 后缀表达 prefix 维度，扩展性差；且 cmdbar 服务于"短语 + 词库"两个并行载体，配置必须**嵌在 text/value 表达式自身**，不能走 yaml 字段化路线（词库 entry 没有那些字段位）。

### 0.2 三项决策（与本文档其余章节一一对应）

| 决策 | 详见 |
|---|---|
| 1. 函数命名宪法 + FuncSpec 扩展元信息 | §1 |
| 2. 权重双轴模型 (weight 跨编码 / position 同 code) + PhraseWeightBoost 独立 tier | §2 |
| 3. options bag (trailing ObjectLit) + marker 后缀作 syntax sugar | §3 |
| 4. marker 命名规范 + $SS 字符串数组 + $SS 嵌 $CC 的语义 | §4 |

### 0.3 迁移顺序

按依赖关系，分三个 PR：

1. **PR-1: weight tier 修复**（最小爆炸面）→ §2 + §5.1
2. **PR-2: options bag + marker 重写**（AST 扩展）→ §3 + §4 + §5.2
3. **PR-3: 函数命名宪法 + FuncSpec 扩展**（rename + 文档）→ §1 + §5.3

---

## 1. 函数命名宪法 (修订原 §3.4)

### 1.1 命名规则

```
取值函数 (Pure=true):
  无 namespace, 单词命名
  示例: code / last / clip / sel / app / title / now / date / time / env
       len / upper / lower / trim / sub / replace / regex / split / concat /
       reverse / t2s / s2t / pinyin / url / html / json / base64 / default /
       calc / num

副作用函数 (Pure=false):
  强制 namespace.verb 形式, 二者均为小写英文单词
  禁止使用没有 namespace 的副作用函数 (除两个保留例外, 见下文)

保留例外 (裸名):
  type(s)      — 走 TSF InsertText 通路, 非 Services
  open(target) — 已是通用 ShellExecute 语义 (http://走浏览器, 否则起进程)
```

### 1.2 重命名表

| 旧名 | 新名 | 原因 |
|---|---|---|
| `dict.addword` | `dict.add` | namespace 已含 dict 语义，verb 用 `add` 即可 |
| `ime.setting` | `setting.open(page)` | wind_setting 是独立 UI，单独成 namespace；verb 统一 `open` |
| `search` | `web.search(engine, q)` | 副作用函数必须有 namespace |
| `open` | `open` (保留) | 通用 ShellExecute，已是约定俗成的裸名 |
| `type` | `type` (保留) | TSF InsertText 通路，特殊例外 |
| `run` / `shell` | `proc.run` / `proc.shell` | namespace.verb 规范化 |
| `clip.copy` / `clip.paste` | 不变 | 已符合规范 |
| `key.tap` / `key.seq` | 不变 | 已符合规范 |
| `ime.toggle` | 不变（收窄到 IME 自身状态） | 仅 `cn-en/fullshape/layout/candwin` 等 |

### 1.3 namespace 总览（更新后）

```
clip.{copy, paste}         剪贴板
key.{tap, seq}             按键模拟
proc.{run, shell}          进程
dict.{add}                 词库
ime.{toggle}               IME 自身状态切换
setting.{open}             设置 UI
web.{search}               搜索引擎

裸名例外:
  open / type
```

### 1.4 FuncSpec 扩展

`internal/cmdbar/registry.go` 的 `FuncSpec` 从 5 字段扩展到 9 字段：

```go
type FuncSpec struct {
    Name          string
    Category      FuncCategory   // Value | Text | Calc | Action | Dict | IME | Setting | Web
    MinArgs       int
    MaxArgs       int            // -1 = variadic
    ArgKinds      []ArgKind      // String | Int | Bool | Enum(values...)
    Pure          bool           // 能否进 display (原义保留)
    Deterministic bool           // 同输入同输出？code/last/clip/now/sel/app/title 都是 false
    Description   string         // 一行说明，wind_setting 直接渲染
    ExampleSrc    string         // 一行示例
    Eval          EvalFunc
}
```

引入 `Deterministic` 是为了区分"纯但有外部状态" —— `code`/`last`/`clip` 这些 Pure=true 但 evaluate 两次结果可能不同，未来缓存优化要看 Deterministic 而不是 Pure。

类型化参数：evaluator 在调用 `Eval` 之前根据 `ArgKinds[i]` 把字符串求值结果转换为目标类型，把现在散布在各函数里的 `parseArgInt` 收敛到一处。枚举型 (`ime.toggle("cn-en"/...)`)在解析期就能拦下未知值。

### 1.5 内省 / 帮助

- `cmdbar.ListFuncs() []FuncSpec` 返回完整 spec 列表
- 内建函数 `help(name)` 返回该函数的 `Description`
- wind_setting 的"命令直通车"页直接拉这张表渲染手册

---

## 2. 权重模型修订 (修订原 §3.8)

### 2.1 双轴模型

| 字段 | 作用域 | 默认值 | 含义 |
|---|---|---|---|
| `weight` | 跨编码 / 跨候选 | **1000** | 短语在自己 tier 内的优先级；用户极少需要改 |
| `position` | 同编码组内 | 0 | 用户手动调整后的相对顺序（小数字在前）；MovePhraseUp/Down/ToTop 调这个字段 |

**核心转变**：

- 删除 `resolvePhraseWeight` 中 `10000 - position` 的 fallback 公式
- `position` 不再参与 weight 计算，仅作为同 code 内的 tie-break
- 同 code 内排序: `weight DESC, then position ASC (0 视为未调整，已调整列于已调整之后)`

```go
// 新 resolvePhraseWeight (拍扁版)
func resolvePhraseWeight(weight int) int {
    if weight <= 0 { return 1000 }
    if weight > NormalizedWeightMax { return NormalizedWeightMax }
    return weight
}

// 新 phraseLess (同 code 排序)
func phraseLess(a, b PhraseEntry) bool {
    if a.Weight != b.Weight { return a.Weight > b.Weight }
    if a.Position != 0 && b.Position != 0 { return a.Position < b.Position }
    if a.Position != 0 { return true }   // 已调整 > 未调整
    if b.Position != 0 { return false }
    return false
}
```

### 2.2 三层 tier

```
Tier 划分          range                  谁在这里               boost 常量
──────────────────────────────────────────────────────────────────────────────
Codetable tier    [10,000,000, +10000)    码表词 + 用户加的码表词    CodetableWeightBoost = 10_000_000
Phrase tier       [ 1,000,000, +10000)    短语 (含 cmdbar 命令)     PhraseWeightBoost     =  1_000_000 (新增)
Pinyin tier       [         0, 10000]     拼音候选                   (无 boost)
```

- 短语 weight=1000 → 排序值 1,001,000：永远 > 拼音任何词，永远 < 码表任何词
- 用户写 weight=9000 的"必置顶"短语 → 1,009,000，仍 < 码表词；如果真要压制码表，需要显式 schema 配置（不通过 weight 数字达成）
- 这个 tier 划分**不再依赖 weight 数字大小做分层**，而是用 boost 常量做架构分层

### 2.3 mixed engine 的合并改造

`internal/engine/mixed/mixed.go::convertMixed` / `convertMixedOverflow` 当前对 codetableCandidates 整体 `+10M`。改造为：

```go
// 现状: 整体 +10M
for i := range codetableCandidates {
    codetableCandidates[i].Source = candidate.SourceCodetable
    if codetableCandidates[i].Code == input {
        codetableCandidates[i].Weight += e.config.CodetableWeightBoost // +10M
    } else {
        codetableCandidates[i].Weight += codetablePrefixBoost          // +6M
    }
}

// 新策: 分离 phrase 与 codetable 词, 分别 boost
var phraseCandidates, codeWordCandidates []candidate.Candidate
for _, c := range codetableCandidates {
    if c.IsPhrase {
        phraseCandidates = append(phraseCandidates, c)
    } else {
        codeWordCandidates = append(codeWordCandidates, c)
    }
}
for i := range phraseCandidates {
    phraseCandidates[i].Source = candidate.SourcePhrase     // 新增 SourcePhrase
    phraseCandidates[i].Weight += PhraseWeightBoost          // +1M
}
for i := range codeWordCandidates {
    codeWordCandidates[i].Source = candidate.SourceCodetable
    if codeWordCandidates[i].Code == input {
        codeWordCandidates[i].Weight += e.config.CodetableWeightBoost // +10M
    } else {
        codeWordCandidates[i].Weight += codetablePrefixBoost           // +6M
    }
}
```

- `Source` 新增 `SourcePhrase` 枚举值（之前短语借 SourceCodetable）
- `codetablePrefixBoost` 仅作用于真码表词，phrase 在前缀场景也用 PhraseWeightBoost 平直 +1M
- 短码场景（输入 1~2 字符）短语**天然落在码表词之下**，问题自动消解，无需特殊压制

### 2.4 候选位置调整 UI 扩展到短语

`MovePhraseUp/Down/ToTop` 当前已存在（操作 PhraseEntry.Position），但 UI 右键菜单上的"上移/下移"暂未路由到这些方法。本次改造一并接通：用户在候选框右键短语候选 → 调 `PhraseLayer.MovePhraseUp` 等，仅修改 position。

### 2.5 旧 yaml 兼容

- 旧 yaml 中 `position: N` 的字段保留 PhraseEntry.Position，**不再**被 `10000-position` 转换为 weight
- 含义改为字面"同 code 内排第 N"
- 未显式指定 weight 的短语：weight=1000，位置由 position 决定
- 系统短语包 yaml 不需要重写（语义改变但兼容）

---

## 3. options bag (新增)

### 3.1 语法

```
expr        = string | number | call | ident | object        (object 为新增)
object      = "{" [ pair ("," pair)* [","] ] "}"
pair        = ident ":" value
value       = string | number | "true" | "false" | ident
```

**约束**：

1. `object` 只能出现在调用的最后一个参数位置 (trailing options)，中间位置出现 → parse error
2. 每个调用最多一个 trailing options
3. value 只允许字面量，不允许嵌套 call / object
4. 字符串字面量内的 `{expr}` interpolation 维持原有语义；lexer 通过上下文区分（字符串内 = interp，表达式位置 = object）

### 3.2 modifier 词表（首批）

| 名称 | 类型 | 默认 | 适用 marker | 含义 |
|---|---|---|---|---|
| `prefix` | bool | false | `$CC` | 是否允许前缀匹配（旧 `$CC1` 的 default） |
| `expand` | enum: `exact` / `always` / `never` | `exact` | `$AA` / `$SS` | 字符/字符串数组的展开策略 |
| `nav` | bool | true | `$AA` / `$SS` | 字符/字符串数组前缀时是否出导航候选 |
| `async` | bool | true | `$CC` | 动作是否异步执行（false 时阻塞 commit） |
| `scope` | string | (空) | 所有 | 应用作用域（保留位，未来用） |

### 3.3 示例

```yaml
phrases:
  # 旧写法 (marker 后缀)
  - code: bd
    text: '$CC1("百度搜索 {tail(code,2)}", open("https://www.baidu.com/s?wd={url(tail(code,2))}"))'

  # 新写法 (options bag)
  - code: bd
    text: '$CC("百度搜索 {tail(code,2)}", open("https://www.baidu.com/s?wd={url(tail(code,2))}"), {prefix: true})'

  # 阻塞模式的快速插入字符
  - code: 《
    text: '$CC("《》", type("《》"), key.tap("Left"), {async: false})'
```

### 3.4 AST 与求值

- `ast/ast.go` 新增 `ObjectLit{ Pairs []Pair }` 节点；`Pair{ Key string; Value ast.Expr (限字面量) }`
- `CommandPhrase` 节点新增 `Modifiers map[string]any` 字段
- `parser/parser.go::parseExprList` 在末尾允许 ObjectLit；解析时立刻拆开放入 Modifiers
- `eval/eval.go::Evaluate` 把 Modifiers 透传到 `cmdbar.ResolvedAction` 携带的元信息（或单独返回）
- `dict/phrase.go::expandDynamicEntry` 把 Modifiers 写进 `Candidate.Modifiers` (Candidate 加新字段 `Modifiers map[string]any`)
- `dict/cmdbar_filter.go::IsExactOnly` 由"扫 marker 字符串"改为"读 Modifiers["prefix"]"

---

## 4. marker 命名规范 + $SS

### 4.1 marker 命名规范

```
形态:
  1. 双字母大写, 形如 $XX(
  2. 必须以 ( 结尾, 视为 marker 调用（区别于 $YY 模板变量）
  3. 第一字母 = 功能 mnemonic (C=Command, A=Array, S=String, ...)
  4. 第二字母:
     - 集合/数组类: 重复第一字母 ($AA, $SS) → 字母重复视觉上表 plural
     - 单实体类: 重复第一字母 ($CC)
     - 其他子类: 选用相关字母 (暂未启用)
  5. 后缀数字 ($XX1): 仅作 syntax sugar, 与现有 marker 形成二元简写

禁用:
  - 单字母 $X      — 与模板变量冲突 ($Y/$M/$D)
  - 三字母 $XXX    — 留作未来扩展
  - 大小写混杂 $Xx — 视觉不统一
  - 跟 §3.1 取值函数同形 (如 $CODE) — 避免与函数命名混淆

登记机制:
  本文档 §4.2 维护一张 "已用 marker → 含义 + default options" 表;
  新增 marker 前必须先更新该表。
```

### 4.2 已用 marker 登记表

| Marker | 含义 | 元素类型 | default options | 备注 |
|---|---|---|---|---|
| `$CC(d, a...)` | 命令直通车 | (display, actions...) | `{prefix: false, async: true}` | display 必须纯函数 |
| `$CC1(d, a...)` | 命令直通车 (前缀简写) | 同 $CC | `{prefix: true, async: true}` | parser 重写为 $CC + {prefix:true} |
| `$AA("name", "chars")` | 字符数组 | (name, chars-string) | `{prefix: true, expand: "exact", nav: true}` | 每个 rune 一个候选 |
| `$SS("name", elem...)` | 字符串/命令数组 | (name, elements...) | `{prefix: true, expand: "exact", nav: true}` | 每个元素一个候选；元素可为 string lit **或** $CC |

未来若新增 marker，需在此表追加一行 + 更新 §4.1 规范。

### 4.3 `$SS` 字符串数组

#### 4.3.1 语法

```
ss_call      = "$SS" "(" string "," ss_arg ("," ss_arg)* [options_bag] ")"
ss_arg       = string | embedded_command
embedded_command = "$CC" "(" expr "," expr ("," expr)* [embedded_options_bag] ")"

embedded_options_bag = "{" pair ("," pair)* [","] "}"
   注: 嵌套 $CC 的 modifiers 禁用 prefix (由外层 $SS 控制), 其他 modifier 仍可用
```

第 1 参 `name` 是 group 显示名（前缀导航候选用），其余每参是一个候选。

#### 4.3.2 展开规则（与 $AA 对称）

| 输入 | $AA 行为 | $SS 行为 |
|---|---|---|
| 前缀 (`url`, len < code 完整长度) | 1 个导航候选 "name (code)" | **同**（导航候选） |
| 精确 (`url1`) | N 个 rune 候选 | N 个独立候选，每参一个 |

候选生成时（精确匹配）：

- **string lit 元素** → `Candidate{Text: 字面量, Code: code, Weight: groupWeight, NaturalOrder: i, IsPhrase: true}`
- **$CC 元素** → `Candidate{Text: $CC.display 求值结果, DisplayText: 同, Actions: $CC.actions, Code: code, Weight: groupWeight, NaturalOrder: i, IsCommand: true, IsPhrase: true}`

group weight 来自 $SS 的 PhraseEntry，所有元素共享；NaturalOrder = 元素在参数列表中的下标，做同权重 tie-break。

#### 4.3.3 示例

```yaml
phrases:
  # 纯字符串数组
  - code: url1
    text: '$SS("常用网址", "https://google.com", "https://github.com", "https://baidu.com")'

  # 纯字符数组 (与 $AA 等价但语法不同)
  - code: zzbd
    text: '$AA("标点符号", ",.()[]{}<>")'

  # 字符串 + $CC 混用 (核心新能力)
  - code: bd
    text: |
      $SS("百度",
        $CC("打开百度", open("https://baidu.com")),
        $CC("百度搜索 {tail(code,2)}", open("https://www.baidu.com/s?wd={url(tail(code,2))}")),
        "https://baidu.com",
        $CC("汉典查 {last()}", open("https://www.zdic.net/hans/{url(last())}"))
      )
```

输入 `bd` 时（精确匹配）：

```
1. 打开百度        ← $CC, 选中触发 open(...)
2. 百度搜索 ab     ← $CC, display 引用 tail(code,2), 选中触发 open(...)
3. https://baidu.com ← string lit, 选中即上屏
4. 汉典查 你好      ← $CC, display 引用 last(), 选中触发 open(...)
```

输入 `b`（前缀，长度<2 不触发导航 per current 代码）或更长前缀 `bdx` → 1 个导航候选 "百度 (bd)"。

### 4.4 $SS 嵌套 $CC: 冲突分析与决策

这是把 $CC 从"phrase 顶层 marker"升级为"也能作为 expression"的扩展。逐项验证与原设计的兼容性。

#### 4.4.1 与 §2 EBNF 的兼容性

原 EBNF（§2.1）将 `$CC` 列为 `l2_command` （phrase 顶层），不出现在 `expr` 产生式中。
**修订**：把 `embedded_command` 列入 expr 候选之一，仅在 `$SS` 参数位置允许。

```
expr  = string | number | call | ident | object | embedded_command  (新增最后一项)
但是: embedded_command 不能出现在 $CC 的 actions 列表内, 也不能嵌套
```

实现上 parser 不修改 `parseExpr`，而是在 `$SS` 的参数解析路径里**特判**：
解析每个参数前，先 peek 是否 `$CC(`，是则走 `parseEmbeddedCommand`；否则按普通 expr (string lit) 处理。

这样 `$CC.actions` 内部的 parseExpr 不会接受 embedded_command —— 嵌套深度被语法层限制为 1。

#### 4.4.2 嵌套深度约束

| 容器 | 允许的元素 | 禁止的元素 |
|---|---|---|
| `$SS` | string lit, `$CC(...)` | `$SS`, `$AA`, 其他 marker |
| `$CC` actions | call (含 namespace 函数), ident, string lit, number | `$CC`, `$SS`, `$AA` |
| `$AA` | string lit (chars 单字符串) | 其他 |
| 顶层 phrase | `$CC` / `$AA` / `$SS` / literal / template | 直接嵌套 marker |

**深度上限：1**。$SS 内可嵌 $CC，但 $CC 内不可再嵌任何 marker。避免组合爆炸。

#### 4.4.3 modifier 作用域

| 位置 | 允许的 modifier | 禁止的 modifier | 决策依据 |
|---|---|---|---|
| 顶层 `$CC` / `$AA` / `$SS` | 全部（prefix, expand, nav, async, scope） | 无 | 控制 entry 级行为 |
| `$SS` 内嵌的 `$CC` | async, scope | **prefix** | prefix 是 entry 级属性（"该 code 是否前缀匹配"），由外层 $SS 控制；嵌套 $CC 是单元，不是 entry |

解析期校验：嵌套 $CC 含 `prefix` modifier → parse error，错误信息引导用户把 prefix 提到 `$SS` 的 options bag。

#### 4.4.4 display 纯函数约束（§5 兼容）

原文 §5："显示名表达式禁止调用动作类函数。解析器在构建 AST 时按动作白名单反向检查，命中即报错并退化为字面短语。"

嵌套场景：内层 $CC 的 display 仍按此约束校验；string lit 元素天然纯（除非含 `{expr}` interp，则同样校验 expr 内函数为 Pure）。**一致性保持，无新约束**。

#### 4.4.5 求值时机

- $SS 展开候选时：每个元素求 display，actions 包装成 thunk 延迟执行
  - string lit 元素 → `display = 字面量, actions = []` （选中即上屏字面量）
  - $CC 元素 → `display = 求值 $CC.display, actions = $CC.actions（thunk）`
- 选中候选时：
  - string lit 元素 → 走 InsertText 通路上屏 display
  - $CC 元素 → 按 §3.4 / P5 action 模型执行 ActionEffect + ActionText 链

与单独 $CC 的求值时机**完全一致**，无新行为。

#### 4.4.6 history (`last()`) 语义

- 选中 $SS 的 string lit 元素：push 该字符串到 history（与普通短语一致）
- 选中 $SS 的 $CC 元素：
  - 若该 $CC 含 `ActionText` (`type(...)`) → push text 部分
  - 若全为 `ActionEffect` (open/key.tap/...) → 不 push（与单独 $CC 行为一致）

#### 4.4.7 权重与排序

- $SS 整体享有 PhraseEntry.Weight（如 1000）
- 展开后每个候选 `Candidate.Weight = groupWeight`，全部相同
- 同权重 tie-break: `Candidate.NaturalOrder = 元素在参数列表中的下标`
- 选择候选时按参数顺序展示

#### 4.4.8 yaml 与持久化

- $SS 短语的存储仍走 PhraseRecord 的 Text 字段（marker + 参数全在一个字符串里）
- 词库 entry 也能用相同语法（value 字段）
- **无 schema 变更**，与 §0.1 第 3 点呼应

#### 4.4.9 setting UI 编辑器

- 类型化对话框（commit e9dbdf9 的 cmdbar 子编辑器）需要识别 $SS 元素层级
- UI 设计建议：$SS 编辑器是"元素列表"控件，每项可选 string lit / $CC 命令两种形态
- 显式 options 用 modifier 编辑器（已有 design 雏形）

#### 4.4.10 设计冲突小结

| 维度 | 冲突？ | 备注 |
|---|---|---|
| EBNF / parser | 微小 | 给 $SS 参数解析加 embedded_command 分支，深度限制为 1 |
| AST | 微小 | 复用 `CommandPhrase` 或抽 `CommandExpr` 共享内核 |
| eval | 无 | 求值时机与单独 $CC 完全一致 |
| modifier 作用域 | 需明确 | $SS 内嵌 $CC 禁用 prefix（解析期校验） |
| display 纯函数 | 无 | 原约束完全适用 |
| history | 无 | 与单独 $CC 一致 |
| 权重 / 排序 | 无 | groupWeight + NaturalOrder 已有机制覆盖 |
| 持久化 | 无 | 仍走 Text 字段，无 schema 变更 |
| UI | 中等 | 子编辑器要支持元素列表 + 二态切换 |

**结论：$SS 嵌套 $CC 与原设计兼容，所需修改集中在 parser 一处，eval / dict / 持久化无需调整**。

---

## 5. 实施路线 (取代原 §8)

### 5.1 PR-1: weight tier 修复

**范围**：

- `internal/dict/phrase.go`: `resolvePhraseWeight` 拍扁；`resolveWeightFromFileEntry` 同步；`sortByPosition` 加 position tie-break
- `internal/dict/weight_norm.go` 或 `internal/engine/mixed/mixed.go`: 新增 `PhraseWeightBoost = 1_000_000` 常量
- `internal/engine/mixed/mixed.go`: `convertMixed` / `convertMixedOverflow` 把 phrase 候选从 codetableCandidates 分离，单独 +1M
- `internal/candidate/candidate.go`: 新增 `SourcePhrase` 枚举值
- 测试: `phrase_test.go`, `phrase_cmdbar_test.go`, `mixed_intent_test.go` 更新断言

**验收**：

- `go test ./internal/dict/... ./internal/engine/mixed/... ./internal/cmdbar/... ./internal/coordinator/...` 全绿
- 短码场景（输入 1-2 字符）短语不再霸占首位；输入 `bd` 时短语 + 码表词的相对位置由 schema tier 顺序决定，与用户预期一致

### 5.2 PR-2: options bag + marker syntax sugar

**范围**：

- `internal/cmdbar/ast/ast.go`: 新增 `ObjectLit` 节点；`CommandPhrase` 加 `Modifiers map[string]any` 字段
- `internal/cmdbar/parser/`: lexer / parser 支持 `{ident: value, ...}` 字面量；marker 后缀 ($CC1) 重写为 $CC + {prefix:true}
- `internal/cmdbar/eval/eval.go`: Modifiers 透传到 `ResolvedAction` 或独立返回
- `internal/candidate/candidate.go`: `Candidate` 加 `Modifiers map[string]any`
- `internal/dict/cmdbar_filter.go::IsExactOnly`: 读 Modifiers["prefix"] 替代 marker 字符串扫描
- `internal/dict/phrase.go::SearchPrefix`: 短语 entry 的 prefix 决策同上
- $SS marker 实现（含嵌套 $CC 支持）
- 测试: 新建 `parser_object_test.go` / `eval_options_test.go` / `phrase_ss_test.go`

**验收**：

- 旧 `$CC1(...)` / `$AA(...)` 写法行为不变（marker syntax sugar 透明）
- 新 `$CC(..., {prefix: true})` 与 `$CC1(...)` 行为等价
- $SS 元素混用 string lit + $CC 端到端通过；输入精确码时候选正确展开

### 5.3 PR-3: 函数命名宪法 + FuncSpec 扩展

**范围**：

- `internal/cmdbar/registry.go`: FuncSpec 扩展字段
- `internal/cmdbar/funcs/`: 各函数补 Description / ExampleSrc / ArgKinds / Deterministic
- `internal/cmdbar/funcs/dict_ime.go`: rename `dict.addword → dict.add`, `ime.setting → setting.open`
- `internal/cmdbar/funcs/action.go`: rename `search → web.search`, `run/shell → proc.run/proc.shell`
- `internal/cmdbar/funcs/register.go`: 注册 `help(name)` 内建
- 系统短语包 (`assets/system_phrases.yaml`): 一次性更新调用名
- 测试: registry / func 调用全部 rename
- 文档: 本设计文档 §1 已含完整名单；同步更新 `internal/cmdbar/AGENTS.md`

**验收**：

- `go test ./internal/cmdbar/...` 全绿
- wind_setting 命令直通车手册页可自动渲染所有函数

---

## 6. 不变项 (确认保留)

以下原设计内容**不修改**：

- §2.1 EBNF 主结构（仅扩展 expr 候选）
- §2.2 `$CC` 调用约定 `(display, action1, action2, ...)`
- §2.4 标识符即零参调用（`last` ≡ `last()`）
- §2.5 `.` 仅用于函数命名空间
- §4 上下文模型（history 容量 16、剪贴板栈、sel/app/title 来源）
- §5 显示名渲染策略（display 必须纯函数）
- §6 安全模型（导入审计 + display 禁副作用）
- §10 不在范围内（不引入条件/循环/HTTP/JS/Lua）

---

## 7. 关联文档

- 原设计：[command-bar-design.md](./command-bar-design.md)
- 枚举约束：[enum-constraint.md](./enum-constraint.md)
- 内部 AGENTS 锚点：[internal/cmdbar/AGENTS.md](../../wind_input/internal/cmdbar/AGENTS.md)
