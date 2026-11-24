# OPENCLOSE compartment 语义：说真话，不要钉死

> 记录 `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE` 该怎么用，以及本仓曾把它钉死为 1 所衍生的
> **四个独立缺陷与九道补丁判据**。2026-08-04 全天排查 gvim / DBX(WebView) 中英切换异常的产物。
> 涉及 `wind_tsf/src/TextService.cpp`（`OnChange` / `_SetOpenCloseCompartment`）与 `KeyEventSink.cpp`。
>
> ⚠ **本文档推翻了此前代码注释里长期存在的多条结论**，被推翻的部分在下文显式标注为「✗ 旧说法」。
> 读到旧注释与本文冲突时，以本文为准。

## 1. 正确语义

compartment 的值**就是**中英状态，没有第二层含义：

```
0 = 英文（IME 关闭）      1 = 中文（IME 开启）
```

`OnChange` 的 OPENCLOSE 分支据此只需一行判定：

```
!_hasThreadFocus 早退 → activate settle 窗 → CapsLock 抑制窗
→ newChineseMode = bOpen        ← 判定本身
→ 同值早退（不回写，值本来就对）
```

**不需要知道是谁写的**。系统热键（Ctrl+Space）取反它、宿主 `ImmSetOpenStatus` 写它，
一律按值采纳。这条性质很关键——它使得「宿主是否把 Ctrl+Space 的 Space 递给 keystroke sink」
变成无关变量，而那正是 WebView 类宿主与普通宿主的分歧点。

> ★ **不变量：任何改变 `_bChineseMode` 的路径都必须 `_SetOpenCloseCompartment(_bChineseMode)`。**
> 全仓 9 个写入点。compartment 现在是我们对宿主说的唯一真话，脱节的后果比钉死更难查——
> 宿主会据此保存并恢复错误的状态。仅当服务端仲裁出与请求不同的模式（如密码框强制英文）时，
> 才需要在 `OnChange` 内回写。

## 2. ✗ 旧设计：钉死为 1

代码曾在 9 处无条件 `_SetOpenCloseCompartment(TRUE)`，公开理由是：

> ✗ **旧说法**：「必须钉死 1，否则英文态收不到 `OnTestKeyDown`，英文统计 / 自动配对失效」

**该理由已被受控实验证伪**：抑制全部写入后 compartment 长期停在 0，英文统计照常计数
（实测 23 次 `openclose=0` 且 `total` 正常递增）、字母正常上屏、中文候选正常。
TSF 在 compartment=0 时依然回调 `OnTestKeyDown`。

## 3. ★ 四个缺陷，同一个根

钉死之后这个位被要求同时承担三件互相冲突的事：既要表达状态、又要承载切换事件、
还要对宿主的查询给出答案。由此衍生：

| # | 缺陷 | 现象 | 为何隐蔽 |
|---|------|------|---------|
| ① | 值失去区分能力 ⇒ 只能把「任何变化」当 toggle | gvim 每次 ESC 退出编辑模式随机翻转中英，方向取决于当时模式 | 同值输入产生相反输出，看起来像"随机" |
| ② | 幂等写入变成事件（我们改回去，宿主再写又算一次"变化"） | 宿主重复下发同一状态被反复触发整轮 IPC + UI 刷新 | 功能"正常"，只是白跑 |
| ③ | **宿主查询到假状态** | gvim 用 `ImmGetOpenStatus` 永远查到「开」，进插入模式据此恢复中文，覆盖用户在英文态的选择 | **出错的是宿主的记忆，不是我们的代码** |
| ④ | **谎言需要持续维护** | `no-op` 早退不发 IPC ⇒ 补写缺失 ⇒ 系统 toggle 方向反转 ⇒「按三次才切一次」 | 副作用要过两层才显形 |

### 3.1 ③ 是被 ① 的修复"激活"的

旧逻辑每次把 compartment 拉回 1，宿主写 1 时值没变、TSF 不触发 `OnChange`，
**这条路径从未执行过**。修好 ① 之后它才第一次浮现。

> 同 [TSF 焦点层级 §3.3](tsf-docmgr-focus-semantics.md) 的教训：
> **「机制终于生效了」和「机制是对的」是两个独立命题。**

### 3.2 ④ 的成因是性能优化与状态维护耦合

`no-op` 早退本身是对的（省掉一轮无谓的 `EndComposition` + 同步 IPC + UI 刷新，实测一轮 30ms），
但那轮 IPC 恰好是 compartment 唯一可靠的补写路径——

> ★ **`OnChange` 上下文内写 compartment 必然失败**：`_SetOpenCloseCompartment` 内部有
> 「值相同就不写」的守卫，而此处 `GetValue` 读到的是**尚未落定的旧值**，于是跳过写入。
> 实测回读 8/8 为 0。真正生效的是 IPC 状态推送回来后 `_SyncStateFromResponse` 里那次
> （不在 `OnChange` 上下文），窗口实测 400~670ms。

于是 `no-op` 分支让 compartment 停在 0，下一次系统 toggle 得到 1 而非 0，
周期从 2 变成 3。

## 4. 九道判据的由来与消亡

为 ①②③④ 各自打的补丁曾在 `OnChange` 里堆成这样，彼此优先级只存在于代码顺序里：

```
not foreground 早退 → activate settle 窗 → CapsLock 抑制窗 → Ctrl+Space 时间戳标记
→ Ctrl 物理按下兜底 → CONVERSION 联动抑制窗 → 忽略 bOpen=1 → 值语义 → 同值早退
```

其中 4 道是时间窗或物理按键嗅探——都属于「没有可靠信号，只能猜」。改为值语义后，
中间 4 道（标记、兜底、联动窗、忽略 bOpen=1）**全部删除**，净减代码。

几条被实测否掉的设计，记录以免重蹈：

| 设计 | 为何失败 |
|------|---------|
| 在 `OnTestKeyDown` 吃掉 Ctrl+Space，阻止系统翻 compartment，自己在 `OnKeyDown` 切换 | **`pfEaten=TRUE` 拦不住系统 IME 热键**。msctf 在 keystroke sink 之下就消费了它，compartment 照样被翻，且**不再回调 `OnKeyDown`**。日志佐证：`ctrl_space_intercept` 命中 4 次，`OnKeyDown` 侧 0 次——该实现从未执行过，却被错误的兜底路径掩盖成"功能正常" |
| 在 `OnTestKeyDown` 打时间戳，供 `OnChange` 区分「切换请求」与「宿主状态请求」 | 隐含前提是「Space 会经过 keystroke sink」。WebView 类宿主（实测 DBX / msedgewebview2）**根本不递 Space**，标记永远打不上 |
| 用 `GetAsyncKeyState(VK_CONTROL)` 作兜底 | 与 gvim 的 `Ctrl+[`（代替 ESC 退出插入模式）无法区分 |
| CONVERSION 联动抑制窗 | 基于错误假设——实测那些 `changed: 0` 间隔 2233ms，是用户按键而非联动（联动实测 119~490ms） |

> 值语义之后，最后一项也自然消解：`_SetConversionMode` 的联动写 0 只在「值与模式已经一致」
> 时到达，天然是 no-op。实测无自发回退。

## 5. 排查设施

TSF DLL 常驻宿主进程，`pdm1` 停不掉已加载它的宿主，多进程日志混写到同一文件。
两条诊断是这类环境的**地基**，不要删：

| 锚点 | 位置 | 回答 |
|------|------|------|
| `build=<__DATE__>_<__TIME__>` | `dllmain.cpp` 的 `PROCESS_ATTACH` | 该 PID 跑的是**哪次构建**。曾因缺它空转整整一轮——部署的 DLL 只含新串、进程却打旧串，答案是那些进程挂着旧 DLL |
| `tid=` / `inst=0x…` | `openclose.onchange` + `OnSetThreadFocus/OnKillThreadFocus` | 区分「同一实例被清零」与「回调落在另一实例」 |

日志判据（`level=trace`）：

```
DllMain PROCESS_ATTACH pid=… build=…        ← 先对指纹，对不上后面都不用看
Compartment OPENCLOSE changed: 0|1 (current mode: ?)
Compartment opened|closed: mode X -> Y      ← 正常切换
… matches current mode (?), no-op           ← 值与模式一致（宿主重复下发）
Service arbitrated mode to ? (requested ?)  ← 服务端仲裁
```

验收：gvim 英文态反复 `i`/`ESC` 模式恒定；**插入模式里切过中文后 `o`/`i` 应恢复中文**
（值语义特有，钉死年代做不到）；DBX 按 Ctrl+Space 应 0/1 完美交替、无空转。

## 6. ★★ 方法论：四次误判，形状完全相同

排查过程中有四次归因错误，**全部是从「日志里没有 X」反推结论**——把"没记录"当成了"没发生"：

| 误判 | 实际 | 推翻它的正证据 |
|------|------|--------------|
| 用户把 IME 热键绑到了 Ctrl 单键 | 一直按的是 Ctrl+Space，只是 Space 不走 keystroke sink | 用户否认 |
| `lctrl` 返回 `PassThrough` 是隐患 | **正确行为**：lctrl 被配成选词键（`select_key_groups`），`toggle_mode_keys` 是默认值不含它 | 查实际配置文件 |
| DBX 失效是本次改动的回归 | 既有的 `_hasThreadFocus` 多进程缺陷 | 用户说「以前也不行」 |
| 「钉死 1 另有真实职责，不可拆」 | **错**。那次实验失败是因为代码里还留着「忽略 `bOpen=1`」补丁 | 用户追问「严格按系统状态会有什么问题」 |

最后一条代价最大：**把一个由补丁引起的实验失败，归因给了被测的假设本身**，于是在错误方向上
又加了两轮补丁。

可推广的判据：

1. **在多进程 × 多线程实例 × DLL 常驻的环境里，「没有日志」是弱证据。** 缺席有两种成因
   （事件没发生 / 事件没被记录），必须配一个能区分二者的正证据才能下判断。
2. **拆除一个长期存在的机制前，先确认它实际影响了什么**，而不是只看它声称做什么——
   但反过来，**若实验失败，先检查实验环境里是否还留着为旧设计打的补丁**。
3. **让日志能自证身份**（构建指纹、实例 id）比多推理一轮更快收敛。

## 7. 宿主自己写 compartment：`ignore_host_ime_switch`（per-app）

值语义下，宿主写 OPENCLOSE 也一律采纳。少数宿主把它当成「本控件不接受输入」来用，写的却是全局中英态：

- WinForms 的 `ImeMode.Disable`、WPF 的 `InputMethod.IsInputMethodEnabled=False` 内部都是
  `ImmSetOpenStatus(false)`，经 IMM→TSF 兼容层落到 OPENCLOSE=0。用户点一次按钮就被切成英文，回到文本框
  未必能恢复（宿主的恢复链只在相邻控件都由它托管时才闭合）。实测：X60_Toolbox（WinForms）、beanfun（WPF）。
- 反方向：SSMS 在内部焦点切换时把 OPENCLOSE 写成「开」，英文态下被切成中文、第一键被吃。

`compat.toml` 的 `ignore_host_ime_switch`（全局 `input.ignore_host_ime_switch`）忽略这类切换，开与关两个
方向都拦。全局出厂关：gvim 一类宿主正确依赖值语义保存/恢复状态，全局开启等于回到第 2 节的钉死年代。

系统热键 Ctrl+Space 与宿主写 compartment 走同一条通路、无来源可分；唯一可用的区分是伴随按键——系统热键
触发时 Ctrl 正被按住，宿主自己写则没有按键（判据同 CapsLock 联动抑制窗）。DLL 在发消息时一并交代
（`MODE_SWITCH_CTRL_HELD`），Ctrl 按住时放行。
