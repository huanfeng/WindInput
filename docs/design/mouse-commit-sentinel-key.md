# 鼠标选词上屏：通过哨兵键复用 KeyEventSink 同步路径

> **状态**：设计草案，待后续优化时实施
> **作者**：huanfeng
> **日期**：2026-05-19
> **优先级**：中（QQ 残留问题非 100% 复现，先用临时方案 A 兜底）

## 一、问题背景

鼠标点选候选词上屏时，部分应用（已知 QQ NTQQ webview 输入框）会出现**嵌入预编辑（preedit）残留**：上屏文本已经进入文档，但 webview 自渲染层的下划线 preedit 文字没有被擦掉，需要用户再次点击输入框才会清除。

空格键路径不会出现该问题。

## 二、根因

### 空格路径（正常）

```
host OnKeyDown
  → CUAS / TSF KeyEventSink::OnKeyDown
    → IPC 同步请求 Go service
    → Go 返回 ResponseType::CommitText
    → 同一 call stack 内 _pTextService->CommitText(text)
      → RequestEditSession(TF_ES_SYNC | TF_ES_READWRITE)
      → host 立即满足（因为它正在等 IME 处理 KeyDown）
      → CCommitTextEditSession::DoEditSession
        → _pComposition->EndComposition + Range.SetText("") + InsertTextAtSelection
      → 一次原子 EditSession 完成 commit
```

关键点：host 此刻处于 "TSF 允许我立即同步编辑" 的上下文，`TF_ES_SYNC` 请求**必然**被立即满足，`_pComposition` 也仍在，`EndComposition` 与 `InsertTextAtSelection` 在同一个 EditSession 内完成，host（包括 QQ webview）会按事件序列正常重绘。

### 鼠标路径（有问题）

```
鼠标点候选窗 → goroutine handleCandidateSelect
  → doSelectCandidate → 返回 InsertText 结果
  → PushCommitTextToActiveClient（写命名管道）
  → TSF 端 async reader 线程接收
  → _commitTextCallback
  → PostCommitText → PostMessage(WM_COMMIT_TEXT) 到自己消息窗口
  → IME UI 线程 pump → MsgWndProc
  → _pTextService->CommitText(text)
    → ⚠️ host 此刻没有在调我们,TSF 不一定满足 TF_ES_SYNC
    → ⚠️ QQ 还可能在 mouse 期间触发 OnCompositionTerminated 把 _pComposition 提前清掉
    → CommitText 走 "无 composition" 分支,只 InsertTextAtSelection
    → QQ webview 的自渲染 preedit 层没被踩到,残留
```

`OnCompositionTerminated` → `Sending composition_terminated (async)` 在日志中已能观察到 QQ 主动终止 composition 的事件（`wind_tsf_debug.log` PID 46284 段）。

差别**不是 Session 对象能不能共享**（实际上空格路径也是每次新建 EditSession），**而是 host 此刻处于什么上下文**。

## 三、临时方案 A（已采用 / 待采用）

`pushKeyEventResult` 在 InsertText 路径之后追加一次 `PushClearCompositionToActiveClient()`：

```go
case bridge.ResponseTypeInsertText:
    srv.PushCommitTextToActiveClient(result.Text)
    srv.PushClearCompositionToActiveClient()   // 兜底,自渲染 webview
```

- 原生 TSF 应用：`CommitText` 内已清 composition，第二次 ClearComposition 是 nop（日志中 `EndComposition: No active composition` 早已大量存在）
- QQ：第二次 EndComposition 经过 EditSession 路径再踩一次 webview 渲染层，残留消失
- 改动量：1 行
- 风险：极低
- **局限**：治标。QQ 这条路上 commit 走的仍然是 `_pComposition == nullptr` 的退化路径，没真正复用空格的原子序列。

## 四、根治方案 E：哨兵键复用 KeyEventSink

### 4.1 核心思路

把"鼠标 commit"包装成一次合成按键，让 host 通过它**自己的键盘消息分发**重新进入我们的 `KeyEventSink::OnKeyDown`。在 KeyEventSink 里识别为哨兵键后，从待办队列取出 commit 内容，**直接走空格那条已验证的同步原子路径**。

```
鼠标点候选 (UI 线程)
  → goroutine handleCandidateSelect → doSelectCandidate
  → 通过 IPC 把 commit payload 推给 TSF Client
  → TSF Client:
      _pendingMouseCommit = { text, ts, hostHwnd }
      PostMessage(hostHwnd, WM_KEYDOWN, VK_F24, lParam)
      PostMessage(hostHwnd, WM_KEYUP,   VK_F24, lParam)
      （lParam 内编码 magic 标记;dwExtraInfo 也写 magic 备份识别）
  → host 消息循环 dispatch → CUAS → TSF → 我们的 KeyEventSink::OnKeyDown
  → 检测 vk == VK_F24 + lParam/extraInfo magic → 命中
  → 锁内 take _pendingMouseCommit
  → 调用与空格相同的 commit 逻辑（_pTextService->CommitText(...)）
  → 返回 eaten = TRUE
```

这条路径下，commit 在 host 的 KeyEventSink 调用栈内执行，TSF 允许立即同步编辑，`_pComposition` 仍在（因为 host 还没机会终止它），与空格行为完全一致。

### 4.2 关键设计要点（合并下方踩坑指南）

#### a. 哨兵键选型

| 选项 | 评估 |
|---|---|
| **`VK_PROCESSKEY` (0xE5)** | ❌ **不要用**。TSF 的 ITfKeystrokeMgr 对它有复杂内部预处理，可能在到达 KeyEventSink 前就被系统层截获。 |
| **`VK_F24` (0x87)** | ✅ 推荐。物理键盘不存在，极少有应用绑定。 |
| 其他保留 VK | 可备选，但 F24 是最干净的。 |

**识别方式**：`VK_F24` + 在 `lParam` 的可用 bit 内塞 magic（注意 lParam 大部分 bit 已被 Windows 占用：repeat count / scancode / extended / context / previous state / transition），主要靠 **`GetMessageExtraInfo()` 取 dwExtraInfo 对比 magic number** 做最终识别。

为什么必须有 magic：万一某个外接设备真的按了 F24，或某个调试工具广播了 F24，绝不能误把真键当成 commit 触发吃掉用户输入。**没匹配上 magic 的 F24 必须放行**。

#### b. 投递方式：`PostMessage(hwnd, ...)` 而非 `SendInput`

| 维度 | `SendInput` | `PostMessage(hwnd, WM_KEYDOWN, ...)` |
|---|---|---|
| 目标定位 | 当前物理前台窗口（focus 漂移会跑偏） | 指定 hwnd（已知 host）—— 精准 |
| 安全拦截 | 可能触发 UIPI 跨完整性级别拦截 | 同进程 In-Proc 注入，**几乎不触发 UIPI** |
| 时序 | 走系统 RIT，全局排队 | 直接进目标线程消息队列 |
| 推荐度 | ❌ 不用 | ✅ 用这个 |

由于 TSF Client DLL 是 In-Proc 注入到 host 进程内部运行，`PostMessage` 从 IME 线程发给同进程 host 窗口**不跨进程边界**，UIPI 不会介入。host hwnd 在 `compat.focus.foreground_host` 日志里已经记录（`KeyEventSink.cpp`、`TextService.cpp` 的 focus 跟踪代码持有这个值），方案 E 实施时把它存到 `_pendingMouseCommit` 一起带过去即可。

#### c. 并发与状态机

`_pendingMouseCommit` 的生命周期需要细致管理：

1. **写入侧**：IPC reader 线程（接到鼠标 commit push 时）。需获取 mutex 写入 + PostMessage。
2. **读取侧**：UI 线程（KeyEventSink::OnKeyDown 接到 F24 时）。需获取 mutex 取出并清空。
3. **过期清理**：
   - 哨兵键投递失败 / host 没消费（极端情况：host 窗口正好销毁）→ 配定时器或下次 KeyEventSink 入口检查 `now - ts > 300ms` 即丢弃，回退到旧的 `WM_COMMIT_TEXT` 路径
   - **关键**：如果在哨兵到来之前，用户敲了真实按键，KeyEventSink 入口必须先检查 pending 是否过期 / 是否要丢弃，**避免把真实按键当成哨兵处理**。规则：只有 `vk == VK_F24 && extraInfo == MAGIC` 才取 pending；其他真键到来时只检查"超时清理"，不消费 pending。

伪代码：

```cpp
struct PendingMouseCommit {
    std::wstring text;
    ULONGLONG postedAtTick;
    HWND       targetHwnd;
};

std::mutex                       g_pendingMu;
std::optional<PendingMouseCommit> g_pendingMouseCommit;

constexpr ULONG_PTR SENTINEL_MAGIC = 0x57494E44'4D4F5553ull; // "WINDMOUS"
constexpr DWORD     PENDING_TIMEOUT_MS = 300;

// IPC reader 线程
void EnqueueMouseCommit(const std::wstring& text, HWND hwnd) {
    std::lock_guard<std::mutex> lk(g_pendingMu);
    g_pendingMouseCommit = { text, GetTickCount64(), hwnd };
    PostSentinel(hwnd);  // 内部 PostMessage F24 down/up,extraInfo=MAGIC
}

// KeyEventSink::OnKeyDown 入口
HRESULT OnKeyDown(...) {
    // 1. 过期清理(任何按键到来都查一下)
    {
        std::lock_guard<std::mutex> lk(g_pendingMu);
        if (g_pendingMouseCommit &&
            GetTickCount64() - g_pendingMouseCommit->postedAtTick > PENDING_TIMEOUT_MS) {
            // 过期: 回退老路径 PushCommitText
            FallbackPushCommit(g_pendingMouseCommit->text);
            g_pendingMouseCommit.reset();
        }
    }

    // 2. 哨兵识别
    if (vk == VK_F24 && GetMessageExtraInfo() == (LPARAM)SENTINEL_MAGIC) {
        std::optional<PendingMouseCommit> taken;
        {
            std::lock_guard<std::mutex> lk(g_pendingMu);
            taken = std::move(g_pendingMouseCommit);
            g_pendingMouseCommit.reset();
        }
        if (taken) {
            _pTextService->CommitText(taken->text);  // 走空格那条路
        }
        *pfEaten = TRUE;
        return S_OK;
    }

    // 3. 真键正常分发(已经过期清理过 pending,不会误吃)
    ...
}
```

#### d. UIPI / AppContainer

- TSF Client 是 In-Proc 注入：`PostMessage` 同进程内不触发 UIPI
- 极端场景（部分系统级 host 不允许 PostMessage）兜底：仍可回退老的 `WM_COMMIT_TEXT` 路径
- AppContainer 类应用（如 UWP 商店应用）通常不是问题（QQ 桌面端不在此列），但落地时需在白/黑名单层面留出 escape hatch

#### e. 焦点二次确认（防漂移加固）

PostMessage 时：

```cpp
HWND currentFg = GetForegroundWindow();
if (currentFg != taken.targetHwnd) {
    // 焦点已漂移,直接走老路径
    FallbackPushCommit(taken.text);
    return;
}
PostMessage(taken.targetHwnd, WM_KEYDOWN, VK_F24, ...);
```

这一步把"几毫秒 IPC 延迟内焦点漂走"的最坏情况也兜住。

### 4.3 优势 vs 方案 A

| 维度 | 方案 A（追加 ClearComposition） | 方案 E（哨兵键） |
|---|---|---|
| 改动量 | 1 行 | 中等（双侧协议 + Pending 状态机 + 超时 + 回退） |
| 是否治本 | 治标 | 治本 |
| QQ webview | 多收一次 EndComposition 兜底 | 完全走原子路径，与空格无差别 |
| SendInput 逐字 fallback | 仍可能触发 | **不会触发**（commit 在 host KeyEventSink 上下文内，TF_ES_SYNC 必给） |
| 撤销栈语义 | 与现状一致 | 与空格一致（更好） |
| 跨应用通用性 | 自渲染 webview 类一般够用 | 所有 TSF host 通吃 |
| 风险 | 极低 | 中（哨兵识别 / 焦点漂移需谨慎） |

## 五、落地步骤（建议未来 PR 拆分）

- [ ] **Step 1**：IPC 协议新增 / 复用一条 push 消息携带 `(text, targetHwnd)`。TSF 端 reader 收到后调 `EnqueueMouseCommit`。
- [ ] **Step 2**：TSF 端新建 `_pendingMouseCommit` mutex + 状态结构。
- [ ] **Step 3**：实现 `PostSentinel`（`PostMessage WM_KEYDOWN/WM_KEYUP VK_F24`，dwExtraInfo via `SetMessageExtraInfo` 或在 lParam 编码 magic + GetMessageExtraInfo 配合）。
  - 注意：`SetMessageExtraInfo` 只影响当前线程的下一条 hardware-generated message，对 PostMessage 不直接生效。可能需要换用 `keybd_event` + 在 host 线程通过 hook 写 extra info；或者直接用 wParam/lParam bit 编码 magic（lParam 的 bit 30 之类）。**这是落地时最需要 prototype 的一步**。
- [ ] **Step 4**：`KeyEventSink::OnKeyDown` 入口加哨兵识别 + 过期清理。
- [ ] **Step 5**：焦点二次确认 + fallback 回退到老 `WM_COMMIT_TEXT` 路径。
- [ ] **Step 6**：日志埋点
  - `MouseCommit: sentinel posted, hwnd=..., textLen=...`
  - `MouseCommit: sentinel consumed, took=Xms`
  - `MouseCommit: sentinel expired, fallback to PushCommitText`
  - `MouseCommit: focus drift detected, fallback`
- [ ] **Step 7**：QQ / 钉钉 / 微信桌面 / VSCode / Word / 浏览器 等关键 host 全量回归
- [ ] **Step 8**：保留方案 A 的兜底 ClearComposition 作为双保险（注释说明：方案 E 走通后 commit 已含 EndComposition；方案 E 回退到老路径时仍依赖 A）

## 六、风险与回滚

- 哨兵键 magic 识别若 prototype 失败（`GetMessageExtraInfo` 在 PostMessage 路径下不可靠），可改用更"重"的标识：定义一个 `dummy=1` 标志位 + 通过 `wParam` / `lParam` 的某个保留 bit + 配合临时全局变量"刚刚 Post 过哨兵"的 short-lived flag。
- 全部失败时直接关闭方案 E，回到方案 A。代码层面用 `compat.mouse_commit_via_sentinel = true/false` 总开关控制，方便 A/B。

## 七、参考代码位置

- `wind_input/internal/coordinator/handle_ui_callbacks.go:227` — `handleCandidateSelect`（鼠标 commit Go 入口）
- `wind_input/internal/coordinator/handle_ui_callbacks.go:968` — `pushKeyEventResult`（方案 A 改这里）
- `wind_input/internal/bridge/server_push.go:245` — `PushCommitTextToActiveClient`
- `wind_tsf/src/TextService.cpp:1810` — `SetCommitTextCallback`（reader 线程触发点）
- `wind_tsf/src/LangBarItemButton.cpp:1051` — `PostCommitText`
- `wind_tsf/src/LangBarItemButton.cpp:692` — `WM_COMMIT_TEXT` 处理器（当前老路径）
- `wind_tsf/src/TextService.cpp:3269` — `CTextService::CommitText`（空格 / 老路径共用的核心）
- `wind_tsf/src/TextService.cpp:2981` — `OnCompositionTerminated`（QQ 触发的不期望终止）
- `wind_tsf/src/KeyEventSink.cpp` — `KeyEventSink::OnKeyDown`（方案 E 哨兵识别加在这里）

## 八、未来观察

- 方案 E 实施后，如果发现部分 host（如 Office 某些上下文）拒绝 `WM_KEYDOWN VK_F24`，可考虑改用 `WM_INPUTLANGCHANGEREQUEST` 之类的非按键消息 + 自定义 wParam，但这条路 host 不会过 TSF KeyEventSink，需要在 `LangBarItemButton::MsgWndProc` 自行 dispatch。属于备选。
- 类似的"通过合成按键复用同步路径"在搜狗、微软拼音、QQ 拼音中都有过先例（公开资料有限），E 方向是 IME 工程的常见 idiom。
