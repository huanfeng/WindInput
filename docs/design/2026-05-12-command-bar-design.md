<!-- Created: 2026-05-12 -->

# 命令直通车 (Command Bar) 设计方案

> 给快捷短语扩展一个轻量级表达式语言, 把输入法变成"打字 + 快捷命令启动器", 一致地支持: 时间日期、打开 URL / 程序、按键模拟、剪贴板操作、词典加词、文本处理与组合。

## 1. 背景与目标

### 1.1 现状

`wind_input` 已有"快捷短语"机制 (`app_dict_phrase.go`), 允许将一个触发码映射到一段静态文本, 同时支持有限模板符号 (`$Y` `$M` `$D` 等日期占位)。能力仅止于"静态文本 + 日期模板", 无法表达"打开浏览器""按键模拟""把剪贴板内容加入用户词"这类**带副作用的动作**, 也无法引用动态上下文 (上一次输入、当前选区等)。

### 1.2 调研对比

| 产品 | 命令能力 | 缺点 |
|---|---|---|
| 多多 / 极点五笔 | `$X[显示名]载荷` 内嵌指令, 能起进程 | 显示名静态, 无变量, 无函数组合 |
| Rime (librime-lua) | 完整 Lua 脚本 | 门槛高, 与短语系统脱节 |
| 搜狗 / QQ / 微软拼音 | 仅固定 `$year` 类占位 | 无任意动作 |
| Espanso | `date / clipboard / echo / random / choice / form / shell / script` | 优秀蓝本 |
| TextExpander | snippet + AppleScript/Shell | macOS 特化 |

我们要做的是 **Espanso 风格的可组合动作 + 输入法原生的上下文取值 (`last` / `clip` / `sel` / `app` / `title`)**, 同时保持语法极简。

### 1.3 设计目标

1. **单一语法**: `$CC` 内部只有"表达式", 表达式只有字面量 / 标识符 / 函数调用三种形态
2. **正交可组合**: 取值、文本处理、动作是独立函数, 通过嵌套自由拼装
3. **跨码表通用**: 所有码表统一走全局快捷短语层, 不在码表里各自实现
4. **零图灵完备**: 不引入控制流, 短语保持声明式

## 2. 语法规范

### 2.1 EBNF 文法

```
phrase      = literal | l1_template | l2_command
literal     = <任意不含 "{" "}" "$CC(" 的文本>
l1_template = <可含 "{" expr "}", 不含顶层 $CC>
l2_command  = "$CC" "(" expr_list ")"

expr        = string | number | call | ident
string      = "\"" (char | interp)* "\""
interp      = "{" expr "}"
number      = ["-"] digit+ ("." digit+)?
ident       = letter (letter | digit | "_")*
call        = func_name "(" [expr_list] ")"
func_name   = ident ("." ident)?              # 仅命名空间, 不允许 a.b.c
expr_list   = expr ("," expr)*
```

转义: `\"` `\\` `\{` `\}` `\(` `\)` 在字符串中按字面值处理。

### 2.2 `$CC` 调用约定

```
$CC(display, action1 [, action2, ...])
```

- `display`: 候选框显示文本, 必须是**纯取值表达式** (禁用 `type / open / run / key / clip.copy / dict.addword / ime.*` 等带副作用函数)
- `action_i`: 副作用动作, 用户选中候选时按顺序执行
- 如果**没有** `action`, 退化为"展开为 display 文本上屏" (覆盖普通短语)

### 2.3 字符串与插值

字符串中用 `{expr}` 内插:

```
"百度搜索: {tail(code, 2)}"
"汉典 · {last(1)}"
"https://baidu.com/s?wd={url(tail(code, 2))}"
```

`{expr}` 内部仍是表达式 (可继续嵌套调用)。**不**引入 `+` 拼接运算符, 一律用插值。

### 2.4 标识符即零参调用

`last` ≡ `last()`, `code` ≡ `code()`, `now` ≡ `now()` —— 裸标识符自动当作零参函数调用。这与 Ruby/Crystal 一致, 写起来更短。

### 2.5 `.` 的唯一含义

`.` **只用于函数命名空间**, 形如 `clip.copy`, 不允许出现在变量索引、修饰符链或属性访问中。下表是常见误用对照:

| 错误 | 正确 | 原因 |
|---|---|---|
| `last.1` | `last(1)` | 索引是参数 |
| `last.1.url` | `url(last(1))` | 修饰符是包装函数 |
| `code.length` | `len(code)` | 属性是函数 |
| `clip.add` | `dict.addword(clip())` | 加词是 dict 命名空间的动作, 通过取值函数组合 |

## 3. 函数清单

### 3.1 取值函数 (返回字符串, 无副作用)

| 函数 | 签名 | 说明 |
|---|---|---|
| `code` | `code()` / `code(n)` | 触发候选时的输入编码 (快照) / 从第 n 字符 (1 起) 切到末尾。动作在异步执行时仍能拿到当时的编码 |
| `tail` | `tail(s, n)` | `s` 从第 n 字符切到末尾 |
| `last` | `last()` / `last(n)` | 最近一次上屏 / 倒数第 n 次, n≥1 |
| `clip` | `clip()` / `clip(n)` | 当前剪贴板 / 历史第 n 条 |
| `sel` | `sel()` | 当前应用中选中的文本 (无选区返回空字符串) |
| `app` | `app()` | 当前前台进程名 |
| `title` | `title()` | 当前前台窗口标题 |
| `date` | `date(fmt)` / `date(fmt, offset)` | 日期, fmt 用 `YYYY MM DD HH mm ss` 别名; offset 形如 `"+1d"` `"-2w"` `"+3M"` `"-1y"` |
| `time` | `time(fmt)` | 同 `date`, 默认 fmt = `"HH:mm:ss"` |
| `now` | `now()` | 等价 `date("YYYY-MM-DD HH:mm:ss")` |
| `env` | `env(name)` | 环境变量 |

### 3.2 文本处理 (纯函数)

| 函数 | 签名 | 说明 |
|---|---|---|
| `len` | `len(s)` | 字符数 (按 rune) |
| `upper` / `lower` | `upper(s)` / `lower(s)` | 大小写转换 |
| `trim` | `trim(s)` / `trim(s, chars)` | 去首尾空白或指定字符 |
| `sub` | `sub(s, start)` / `sub(s, start, end)` | 切片, 索引 1 起, 支持负数 |
| `replace` | `replace(s, old, new)` | 字面替换 |
| `regex` | `regex(s, pat, rep)` | 正则替换, Go RE2 语法 |
| `split` | `split(s, sep, n)` | 按 sep 拆分, 取第 n 段 (1 起) |
| `concat` | `concat(a, b, ...)` | 字符串拼接 (兜底) |
| `reverse` | `reverse(s)` | 反转 (按 rune) |
| `t2s` / `s2t` | `t2s(s)` / `s2t(s)` | 繁简转换 |
| `pinyin` | `pinyin(s)` | 汉字转拼音 (空格分隔) |
| `url` | `url(s)` | URL 编码 (component) |
| `html` | `html(s)` | HTML 实体编码 |
| `json` | `json(s)` | JSON 字符串字面量化 |
| `base64` | `base64(s)` | Base64 编码 |
| `default` | `default(s, fallback)` | s 为空时返回 fallback |

### 3.3 计算

| 函数 | 签名 | 说明 |
|---|---|---|
| `calc` | `calc(expr)` | 数学表达式求值, 支持 `+ - * / % ( )`, 浮点 |
| `num` | `num(s, base)` | 进制转换, `num("0xff", 10)` → `"255"` |

### 3.4 动作函数 (有副作用)

| 函数 | 签名 | 说明 |
|---|---|---|
| `type` | `type(s)` | 直接上屏文本 (绕过 display) |
| `open` | `open(target)` | `http(s)://` 走默认浏览器, 否则 ShellExecute |
| `run` | `run(cmd, args...)` | 启动进程 |
| `shell` | `shell(cmdline)` | 通过 `cmd /c` (Windows) 执行 |
| `key.tap` | `key.tap(combo)` | 单次按键, 例 `"Enter"` `"Shift+End"` `"Ctrl+C"` |
| `key.seq` | `key.seq(combo, ...)` | 按键序列, 顺序触发 |
| `clip.copy` | `clip.copy(s)` | 写入剪贴板 |
| `clip.paste` | `clip.paste()` | 模拟 Ctrl+V |
| `dict.addword` | `dict.addword(s)` / `dict.addword(s, code)` | 把 s 加入用户词库, code 可选 |
| `ime.toggle` | `ime.toggle(target)` | target ∈ `"candwin" / "mute" / "fullshape" / "cn-en"` |
| `ime.setting` | `ime.setting(page)` | 打开 wind_setting 的指定页 |
| `search` | `search(engine, q)` | engine ∈ `"baidu" / "bing" / "google" / "zdic"`, 内部组装 URL + `open` |

### 3.5 交互函数 (P5 阶段, MVP 不做)

| 函数 | 签名 | 说明 |
|---|---|---|
| `ask` | `ask(prompt)` | 弹小输入框, 阻塞等用户输入, 返回文本 |
| `pick` | `pick(opt1, opt2, ...)` | 弹下拉, 返回选中项 |

### 3.6 示例对照表

```
ocbd = $CC("打开百度", open("https://baidu.com"))
bd   = $CC("百度搜索 {tail(code,2)}", open("https://www.baidu.com/s?wd={url(tail(code,2))}"))
z    = $CC(last(), type(last()))
《   = $CC("《》", type("《》"), key.tap("Left"))
dl   = $CC("[删行]", key.seq("Home", "Shift+End", "Backspace"))
zd   = $CC("汉典 · {last(1)}", open("https://www.zdic.net/hans/{url(last(1))}"))
addc = $CC("加词 · {clip()}", dict.addword(clip()))
addl = $CC("收藏 · {last()}", dict.addword(last()))
calc = $CC("= {calc(tail(code,2))}", type(calc(tail(code,2))))
ip   = $CC("IP", shell("curl -s https://api.ipify.org > %TEMP%\\ip.txt"), clip.paste())
now  = "{date('YYYY-MM-DD')} {time('HH:mm:ss')}"
```

## 3.7 `$AA` 字符组 marker

字符组短语 (一个触发码展开为 N 个独立单字符候选) 与 `$CC` 命令直通车并列,
用 **`$AA`** marker 表达:

```
text: '$AA("groupName", "charList")'
```

- 两个参数均为双引号字符串字面量, 用 Go `strconv.Unquote` 处理转义,
  **不支持单引号**
- yaml 外层可以用单引号包整体 (内部双引号字面量), 上面是推荐写法
- 与 `$CC` 不同: `$AA` 不进 cmdbar parser, 因为它没有 display + actions
  结构, 而是声明性的"候选生成器", 由 dict 层独立的小解析器
  (`internal/dict/aa_marker.go::ParseAAMarker`) 处理

### 展开语义 (沿用 phraseGroups 现行行为)

| 输入 | 行为 |
|---|---|
| 前缀 `zz` (`< code` 的子串) | 显示 1 个导航候选 "标点 (bd)", 不展开字符 |
| 精确 `zzbd` | 展开为 N 个独立字符候选 (每个 rune 一个) |

### 与 `$CC` 的对比

| Marker | 用途 | 解析路径 | 是否前缀展开 |
|---|---|---|---|
| `$CC(display, actions...)` | 命令, 仅精确匹配 | cmdbar parser | 否 |
| `$CC1(display, actions...)` | 命令, 精确 + 前缀都可见 | cmdbar parser | 是 |
| `$AA("name", "chars")` | 字符组, 一对多展开 | dict aa_marker | 前缀→导航, 精确→展开 |

**无 `$AA1` 变体**, 不引入对称, 避免无效配置面。

### 历史: yaml 字段统一

旧版字符组用 `texts: + name:` 两个独立 yaml 字段。现已废弃,
统一用 `text: '$AA(...)'` 单字段。bbolt 内的旧 PhraseRecord
由 `store.MigratePhraseRecordsToAA` 一次性迁移, 幂等。

## 3.8 短语权重 (`weight` yaml 字段)

短语 / 用户词库 / 范化后的码表+拼音权重统一在 `[0, 10000]` 区间比较 (与
`internal/dict/weight_norm.go::NormalizedWeightMax` 一致), 中位默认 1000。
yaml 新增 `weight` 字段, 优先级高于旧的 `position`:

```yaml
phrases:
  - code: sig
    text: "张三 quatebase@100.name"
    weight: 9000          # 必置顶, 永远第一候选
  - code: addr
    text: "深圳市南山区..."
    weight: 5000          # 高频常用
  - code: zzz
    text: "罕用短语"
    weight: 200           # 仅在低频区出
  # 缺省 weight: 走 position fallback (旧行为, 兼容现有 yaml)
  - code: rq
    text: "$Y-$MM-$DD"
    position: 1           # weight = 10000 - 1 = 9999
```

### 权重解析优先级

`resolvePhraseWeight(weight, position)` 顺序判断:
1. `weight > 0`  → 直接使用 (clamp 到 `NormalizedWeightMax` 10000)
2. `weight == 0 && position > 0` → fallback `10000 - position`
3. 都缺 → 默认 1000 (中位)

`PhraseFileEntry.Weight` 用 `*int` 以区分 "未在 yaml 写" vs "显式 weight: 0";
后者表示用户主动让短语完全不参与排序权重。

### 档位指南

| 档位 | weight | 用途 |
|---|---|---|
| 必置顶 | 8000~10000 | signature / 公司名 / 个人 ID |
| 高频备选 | 4000~7000 | 常用短语, 排在主码表词条前 |
| 中位 (默认) | 1000 | 普通短语, 未指定 weight 时自动取值 |
| 罕用 | 200~500 | 仅在低频区域出现 |
| 禁用排序 | 0 | 完全不参与排序权重 |

### 引擎层适用

- **纯码表 / 纯拼音引擎**: phrase 候选 weight 直接与该 layer 候选比较, 无额外 boost。
- **混输引擎 (engine/mixed)**: phrase 候选自动通过 `compositeDict.Search/SearchPrefix/LookupCommand` 流入 `codetableCandidates` 切片, 与码表候选共享 `+CodetableWeightBoost (10M)`, 整体压拼音 tier。无需 mixed.go 单独配置。
- **字符组 (`$AA`)**: 内部所有字符共享 group weight, `Candidate.NaturalOrder` 取字符在 chars 字符串中的下标, `candidate.Better` 在同权重下用 NaturalOrder asc tie-break, 保证按数组顺序排列。

### 旧 db 兼容

`PhraseRecord.Weight` 是新加字段, JSON tag `w,omitempty`。老记录反序列化时
`Weight=0`, 自动走 Position fallback, 行为与旧版完全一致。用户在 UI 修改
weight 时写回 `PhraseRecord.Weight`, Position 自然弃用。下一版本可删 Position。

## 4. 上下文模型

### 4.1 历史缓冲

- 维护一个环形缓冲, 容量 16, 每次成功上屏文本 push 一条
- `last()` → 第 1 条 (最近), `last(2)` → 第 2 条, 最多到 `last(16)`
- 超出范围返回空字符串
- 跨进程切换保留, 输入法重启清空
- 上屏文本由 `coordinator` commit 阶段写入

### 4.2 剪贴板缓冲

- `clip()` 直接读系统剪贴板
- `clip(n)` 需要监听 `WM_CLIPBOARDUPDATE` 维护本地栈, 容量 9 (对齐多多/搜狗剪贴板九格), 超出范围返回空

### 4.3 选区与窗口

- `sel()`: Win32 没有官方"读其它应用选中文本"API。实现策略: 先 `SendMessage WM_COPY` 备份当前剪贴板 → 取 → 还原。失败返回空
- `app()`: `GetForegroundWindow` → `GetWindowThreadProcessId` → `QueryFullProcessImageName` → basename
- `title()`: `GetForegroundWindow` → `GetWindowText`
- 三者均在动作执行时求值, **不在显示名里求值** (避免高频抖动)

## 5. 显示名渲染策略

- 候选构造时, 解析显示名表达式并求值, 把结果作为候选文本
- 输入未补全 (如 `bd` 还没补全 `bdxxx`) 时, `tail(code, 3)` 返回空字符串, 候选显示 "百度搜索 " (空尾巴正常显示)
- 显示名表达式**禁止**调用动作类函数。解析器在构建 AST 时按"动作白名单"反向检查, 命中即报错并退化为字面短语
- 显示名求值若抛错, 退化为短语 key 本身, 不阻断输入流程

## 6. 安全模型

按调研结论, 大部分是用户自配置, 不做执行白名单。仅保留两条低成本防御:

1. **导入审计**: 词库 / 短语包导入时, 扫描含 `run / shell / open` 的条目, 在 wind_setting 中列出, 用户勾选确认才生效; 用户手写的不打扰
2. **显示名禁副作用**: 见 §5

不弹运行时确认对话框。

## 7. 模块设计

### 7.1 包结构

```
wind_input/internal/cmdbar/
├── parser/         # 词法 + 语法, 输出 AST
├── ast/            # 表达式节点定义
├── eval/           # 求值器, 持有 EvalContext (input, history, clipboard, ...)
├── funcs/
│   ├── value.go    # §3.1 取值函数
│   ├── text.go     # §3.2 文本处理
│   ├── calc.go     # §3.3 计算
│   ├── action.go   # §3.4 动作 (run/open/shell/...)
│   ├── key.go      # §3.4 按键模拟, 走 wind_tsf 注入通路
│   ├── clip.go     # §3.4 剪贴板
│   ├── dict.go     # §3.4 dict.addword, 复用现有加词链路
│   └── ime.go      # §3.4 IME 控制, 走 schema/manager
├── registry.go     # 函数名 → 元信息 (元数, 是否副作用) 注册表
└── context.go      # EvalContext 接口
```

`funcs/registry.go` 集中维护 `FuncSpec{ Name, Pure, Eval func(ctx, args) (string, error) }`, 显示名校验、解析期 arity 检查都走这张表。

### 7.2 与短语系统衔接

- `app_dict_phrase.go` 的短语 value 改为统一走 `cmdbar.Parse(value)`:
  - 解析失败或 AST 为纯字面量 → 老路径 (字面 / `$Y` 模板)
  - AST 含 `$CC` → 新路径, 候选生成时求值 display, 上屏阶段触发 action 链
- `$Y$M$D` 旧模板符号兼容: 在解析器前置一个 shim, 把旧符号翻译成 `{date('YYYY')}` 等

### 7.3 历史缓冲挂载

- 在 `internal/coordinator` 增加 `History` 结构 (环形缓冲, 容量 16, 互斥访问)
- coordinator commit 文本时 push 一条
- `EvalContext` 持有 `History` 引用

### 7.4 按键注入

- `key.tap` / `key.seq` 解析 combo 字符串 → 复用 `wind_tsf` 现有的 `INPUT` 注入路径
- 修饰符: `Ctrl` / `Shift` / `Alt` / `Win`, 按 `+` 拼
- 特殊键名复用 `pkg/keys/keys.go` 的 token 表 (与 §enum-constraint 一致)

## 8. 实施路线 (5 阶段, 5 PR)

| PR | 阶段 | 范围 | 验收 |
|---|---|---|---|
| **P1** | 解析器 + AST | `cmdbar/parser` + `cmdbar/ast`, 表驱动单测覆盖 §2 全部例子, 含错误恢复 | `go test ./internal/cmdbar/parser/...` 全绿 |
| **P2** | 取值 + 文本处理 + 计算 | `cmdbar/eval` + `funcs/{value,text,calc}.go` + registry; `History` 接到 coordinator | `go test ./internal/cmdbar/...` 全绿, 单测覆盖 §3.1-§3.3 |
| **P3** | 动作 MVP | `funcs/{action,key,clip,search}.go`, `type / open / run / shell / key.tap / key.seq / clip.copy / clip.paste / search`; 接通 wind_tsf 注入 | 手测打开 URL / 起进程 / 按键序列均成功 |
| **P4** | 短语接入 + dict/ime | 改 `app_dict_phrase.go` 走 `cmdbar`; `funcs/{dict,ime}.go`; 旧 `$Y` 模板 shim; 候选生成 + 触发链 | 在真实输入下完成 §3.6 全部示例 |
| **P5** | 计算交互 + 设置 UI + 导入审计 | `ask / pick`, `app / title / sel`, wind_setting 增加"命令直通车"页 (文档 + 测试沙箱 + 导入审计列表) | 端到端验收 |

P1+P2 合并为单 PR 提交 (纯库, 全单测)。P3 / P4 / P5 各一个 PR。

## 9. 验收用例

进入 P4 完成时, 以下 10 条短语必须工作:

1. `ocbd` → 打开 baidu.com
2. `bdxxx` → 百度搜索 xxx
3. `z` → 重复上次上屏
4. `《` → 输出《》并光标停在中间
5. `dl` → 删除当前行
6. `zd` → 汉典查上次上屏的词
7. `addc` → 把剪贴板内容加入用户词库
8. `now` → 输出当前日期时间
9. `cal1+2*3` → 输出 `7`
10. `t1` → 输出明天日期 (`date("YYYY-MM-DD", "+1d")`)

## 10. 不在本次范围内

- HTTP 请求函数 (`http.get` 等) —— 用 `shell` + `curl` 兜底
- 文件 IO (`read / write`) —— 同上
- 任意 JS / Lua 脚本执行 —— 不引入解释器依赖
- 条件 / 循环 —— 短语场景不需要图灵完备
- 跨设备同步 —— 短语本身的能力, 不在本特性
