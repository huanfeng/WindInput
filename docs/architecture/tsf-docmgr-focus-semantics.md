# TSF 焦点层级与判据选择

> 记录「焦点变了」这件事在 TSF 里的三个层级，以及每一层各自该用什么判据。
> 本仓已有四个缺陷源于**把某一层的判据用到了另一层**，形状高度一致，故集中于此。
> 涉及 `wind_tsf/src/TextService.cpp`（`OnSetFocus`）与 `wind-coordinator`。

## 1. 三个层级，三种"变了"

```
宿主进程（client_token，DLL 实例级，每进程一个）
  └─ DocMgr（ITfDocumentMgr，一个宿主可有多个，且会频繁互切）
       └─ Context（ITfContext，GetTextExt / 组合都在这一层）
```

**这三层的"变了"是三件不同的事**，任何一层的判据都不能替另一层回答：

| 层级 | 判据 | 它能回答 | 它**不能**回答 |
|------|------|---------|--------------|
| 宿主 | `client_token` | 用户是不是切换了应用 | 应用内部换没换输入框 |
| DocMgr | `pDocMgrFocus` 指针比对 | 是不是同一个文档在抖动 | 这个文档是不是"真实"文档 |
| Context | `dynFlags` / `GetStatus` | 这个容器可不可输入、是不是 transient | 用户在不在上面打字 |

## 2. 实测：宿主内部的 DocMgr 抖动是常态

| 宿主 | 抖动形态 |
|------|---------|
| Excel | cell-select → cell-edit 时把**同一个** DocMgr 置空再设回（指针不变、间隔 6ms）；输入完焦点还会落到**公式编辑栏**这个另一个 DocMgr |
| 资源管理器地址栏 | 同一个 transient DocMgr（`dynFlags=0x20`）连续多次获焦，实测 `focusSession` 26→27→28 全是 `0x219BC540` |
| VSCode | 一次应用切换伴随 5 次 DocMgr 焦点事件 |
| 跨宿主切换（如 Windows Terminal → 记事本） | 旧宿主的 `focus_lost`（`OnKillThreadFocus`）比新宿主的 `focus_gained` 晚约 100ms 才到。服务端按客户端归属判陈旧、丢弃它对激活态的改动；菜单刚弹出时到达的焦点事件另有守卫期豁免 |

> **DocMgr 级是噪声层**。在失焦那一刻无从区分「抖动」与「真的换了文档」，所以清理必须
> 推迟到「另一个文档拿到焦点」时执行——这样抖动自然被判为同一文档而跳过。
> 同源做法见 Weasel：DocMgr 级失焦完全不碰 composition。

## 3. ★ 四个缺陷，同一种形状：判据跨层复用

### 3.1 地址栏首字母上屏（2026-08-02 修复）

`_pLastActiveDocMgr` **刻意排除** transient DocMgr——这是**正确**的决定，因为换文档收口时
`hint` 必须指向真实文档，不能是 transient 容器。

错在 `isSameDocMgr` 复用了这个缓存：transient DocMgr 永远进不了缓存，也就永远等不到自己，
判据恒为假 ⇒ 每次抖动都判成「换了文档」⇒ 收口时 `EndComposition` 终止正在进行的组合 ⇒
**已写入宿主的 preedit 留在了地址栏**。

- **指纹**：第二键的 `prevChar` 是上一个字母的 ASCII（`0x73`='s'）；`_pComposition` 每次都是
  `0x0`（组合在重建而非累积）；同一 doc 指针却次次 `sameDoc=0`。
- **修法**：拆成两个缓存——`_pLastFocusedDocMgr`（含 transient，只答"是不是同一个 doc 在抖"）
  与 `_pLastActiveDocMgr`（排除 transient，只答"上一个真实文档是谁"）。
- 修复后：`sameDoc=1` 486 次 : `sameDoc=0` 84 次（修复前几乎恒 0），组合对象保持同一个累积
  `textLen 1→2→3`。

### 3.2 焦点气泡「输入一次闪两下」（2026-08-02 修复）

气泡的语义是**「切到了新的输入宿主」**，原先按 DocMgr 计数。于是 Excel 里起输入闪一次、
输入完焦点落到公式栏又闪一次，而**同一单元格内连续输入反倒不闪**——闪的时机与用户的操作
节奏完全对不上，这才是它扰人的真正原因。

- **修法**：按 `client_token` 去重（`last_focus_tip_token`），同宿主只在首次进入时弹。
- ⚠ **只在 `FocusLostReason::Thread` 清记录**：`CtxLost`/`DocChanged` 是宿主内换 DocMgr 的
  噪声，若也清就等于退回按 DocMgr 计数。
- **验收判据不是"少弹了多少"，而是"该留的有没有留下"**：实测 Excel 51:16、记事本 0:10——
  同一套判据在两个宿主上比例分化，恰证明它抓的是真实差异而非无差别压制。

### 3.3 候选窗钉死在旧 DocMgr（2026-08-02 修复）

候选窗锚的是**组合起点**而非当前光标（防止随输入右移），而锚定「同一组合只锁一次」的隐含
前提是**起点不会移动**。Excel 换 DocMgr 时组合整体迁移（实测 `(593,572)` → `(1457,959)`），
锚点却还指着旧 DocMgr。

- **指纹**：协调器判出 `reshow: dx=1297` 说要重定位，**下发的 UI 位置却纹丝不动**——因为
  reshow 用 `state.caret_*` 判、下发用组合起点。
- **修法**：`handle_focus_gained` 时作废组合起点锚定，由下一帧 `caret_update` 就地重锁。

> ⚠ **这个缺陷是被另一个修复"激活"的**：此前 Excel 的 `compStart` 取不到或被距离校验丢弃，
> 锚定**从未真正生效**，候选窗一直跟着 caret 走，只表现为"跳一下"。修好 selection 退化降级
> 与越界判据后 `compStart` 变可靠，反而让这段从未被执行过的逻辑第一次跑起来。
> **"机制终于生效了"和"机制是对的"是两个独立命题**——修好上游时，要把下游那些因上游沉默
> 而空转的分支当作**新代码**看待。

### 3.4 坐标来源跨域冒充

同属一类：`GetGUIThreadInfo` 的**跨窗口** Win32 光标冒充 TSF 插入点。详见
[候选窗坐标时序与定位设计 §3.1](../redesign/candidate-window-positioning.md)。

### 3.5 `_hasThreadFocus` 一个变量两个职责（2026-08-04 修复）

它同时被用作「热键注册门卫」与「TSF 线程焦点信号」。这两个职责在**多进程宿主**
（WebView 类：前台窗口在渲染进程、TSF 加载在另一进程）下期望**正好相反**：

| 职责 | 正确判据 | 多进程宿主下应为 |
|------|---------|----------------|
| 热键注册门卫（防多实例争抢同一组热键，`ERROR_HOTKEY_ALREADY_REGISTERED` 1409） | 本**进程**是否前台窗口所属进程（`GetForegroundWindow` 的 pid） | FALSE（不该抢） |
| TSF 线程焦点（过滤 compartment 变化噪声） | `ITfThreadFocusSink` 回调 | TRUE（应用确实在前台） |

500ms 自检定时器按前者把它清零，且**永不恢复**——恢复分支的条件是 `nowForeground`，
在这类宿主里恒假。于是 `OnChange` 的 `!_hasThreadFocus` 早退恒成立，
**DBX 里中英切换整个失效**（实测 `hasFocus=1 hasThreadFocus=0` 连续 30 次）。

- **指纹**：`OnSetThreadFocus called` 之后 2ms 就出现
  `FocusCheck timer: not foreground (fgPid=165104 ownPid=154820), releasing`，
  两个 pid 不同即多进程宿主。
- **反讽**：该分支的注释早就写着「它纠正的是热键状态而非 `_hasThreadFocus` 的正确性」，
  但代码**确实在写** `_hasThreadFocus`。声明的意图与实际职责不一致，正是它能悄悄
  搞坏多进程宿主的原因。
- **修法**：拆成 `_hasThreadFocus`（`ITfThreadFocusSink` + 初始种子独占）与
  `_isProcessForeground`（`GetForegroundWindow` 判据）。热键三处门卫要求两者同时成立，
  `OnChange` 只看前者。单进程宿主下两者恒相等，**热键行为不变**。

## 4. 可推广的判据

1. **先问"这个判据属于哪一层"**，再问"它取什么值"。跨层复用时，原判据的注释和实测支撑都
   留在原地，复用点看不到——**一个正确的决定被复用到它没考虑过的问题上就成了错误**。
2. **一个变量只承担一个语义**。上面 3.1、3.3、3.5 都是"一个值被两个问题借用，而它们对同一个
   边缘输入的期望恰好相反"。合用一个缓存 = 必有一方错。
   3.5 还多一层教训：**注释声明的职责不等于代码实际承担的职责**——那段注释写着"不纠正焦点
   信号"，代码却在写它。审查时要以赋值语句为准，不要以注释为准。同源案例见
   [OPENCLOSE compartment 语义 §2](tsf-openclose-compartment-semantics.md)：钉死为 1 的
   公开理由被实验证伪，它真正影响的是完全另一件事。
3. **基于「某事件不会再发生」的优化，要把该前提写成断言或日志**。3.1 那段注释里作者已实测
   "用户停在 transient DocMgr 上打字"，但结论是"它不会再发 focus_gained"；这个前提后来失效
   （同一 DocMgr 反复获焦）却没有任何痕迹，最终靠 `prevChar` 这个无关字段才反推出来。
4. **改动 `OnSetFocus` 里任一守卫的命中条件时，必须同步检查其它守卫的预判**。收口分支会
   预判 XamlIsland 守卫是否将命中来决定发不发 `focus_lost`——两个决策各自都对，组合起来
   却可能让服务端只收到半边失焦。
