# UIAccess + Band 方案 spike 验证方案

> 状态：**已完成，结论见 §6.1/§6.2；后续立项暂缓**（决定继续使用 HostRender，见 `uiaccess-band-presenter.md` 顶部）。探针代码留在分支 `spike/uiaccess-band` 的 `tools/uia-spike/`。
> 本文只回答「这条路对我们走不走得通」，不是正式设计。

## 1. 背景

开始菜单 / 任务栏搜索（`SearchHost.exe`）的窗口在 Band 6 / 13，普通进程的候选窗在 Band 1，
永远盖不过。早期没有代码签名，所以做了 HostRender（宿主进程里的 `wind_tsf.dll` 用
`CreateWindowInBand` 代为上屏，见 `docs/redesign/host-render-windows-port.md`）。

它的缺口：开始菜单里候选框不能鼠标操作；要处理 band 6/13 切换、owner、开机竞态、共享内存
清理一整套；依赖未文档化 API。

Go 版当年的分析（Go 仓 `docs/archive/startmenu-zorder-solution.md` §2.3 / §5）记下了
微信输入法的做法：候选窗在独立进程 `wetype_renderer`，Band=2，靠 **UIAccess + 数字签名**。
当时列为「方案 B（备选）」，三个前提：

1. manifest `uiAccess="true"`
2. 受信任 CA 的代码签名
3. 安装在受保护目录（`%ProgramFiles%` / `%SystemRoot%`）

现在三条里前两条已具备（Certum 签名、安装器默认装 `%ProgramFiles%`），值得验证。

## 2. 要回答的问题

| # | 问题 | 为什么要验 |
|---|---|---|
| Q1 | 签名 + `uiAccess="true"` + Program Files 的 exe，令牌里 UIAccess 是否为 1？ | 一切的前提 |
| Q2 | 它的窗口 `GetWindowBand` 是否为 2，且真能盖在开始菜单（6）、任务栏搜索（13）之上？ | 旧文档里「2 高于 6」是对微信的观察，没在我们自己的 exe 上验过 |
| Q3 | 点击 / 悬停 / 滚轮能否收到？点击时开始菜单是否保持打开、搜索框是否保持焦点？ | 这是换方案的主要收益；点一下就把开始菜单关了则收益归零 |
| Q4 | 拉起路径：普通进程拉起、**TSF DLL 在 SearchHost 内 `CreateProcessW` 拉起**（标志同 `IPCClient.cpp`）都能拿到 UIAccess 吗？ | 服务目前就是这样被宿主拉起的，宿主可能是低完整性 / AppContainer，这点无法靠推理确定 |
| Q5 | 失败边界：未签名、签名但不在受信任目录（便携版），行为是「启动失败」还是「静默降级」？ | 决定 manifest 策略（日常未签名 `d1` 构建、便携版怎么办） |
| Q6 | 顺带观察：DPI 125%/150% 下坐标、全屏应用、UAC 安全桌面 | 不阻塞结论，记录即可 |

## 3. 测试件

全部放 `tools/uia-spike/`（独立 crate，自带空 `[workspace]`，不进 `wind_input` workspace，
不影响主构建）。

### 3.1 `uia_probe.exe`

最小窗口程序，参数 `--pos x,y --secs N --tag 名字`。

- 启动先写一行 JSON 自报到 `%TEMP%\uia_probe.log`：
  `TokenUIAccess`、完整性级别 RID、`TokenIsAppContainer`、父进程 PID 与进程名、
  自身路径、`GetWindowBand`（`GetProcAddress` 取 user32 未导出函数）。
- 建 `WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` 的 `WS_POPUP`
  窗口（与 `wind-ui/src/window.rs:247` 同样式），`UpdateLayeredWindow` 画一块色块 + 文字
  （显示 Band、UIAccess、IL），`WM_MOUSEACTIVATE` 返回 `MA_NOACTIVATE`。
- 记录 `WM_MOUSEMOVE` / `WM_LBUTTONDOWN` / `WM_MOUSEWHEEL` / `WM_MOUSELEAVE`，写日志，
  并让色块变色，便于肉眼确认。
- 倒计时 `--secs` 后自行退出；窗口创建前后各打一次 band，避免「建出来没显示」查不出原因。

同一份源码编出的变体（只在 manifest 与签名上不同）：

| 变体 | manifest | 签名 | 放哪 |
|---|---|---|---|
| A | `uiAccess=true` | 否 | `%ProgramFiles%\WindInputDev\spike\` |
| B | `uiAccess=true` | 是 | `%ProgramFiles%\WindInputDev\spike\` |
| C | `uiAccess=true` | 是（同 B 的文件） | 用户目录（非受信任目录） |
| D | `uiAccess=false` | 是 | `%ProgramFiles%\WindInputDev\spike\`（Band=1 基线） |

### 3.2 TSF 拉起钩子（仅 T3 用）

`wind_tsf` 里加一段**只在 spike 分支、只在 Dev 变体、受标记文件控制**的代码：宿主进程为
`SearchHost.exe` 且 DLL 同目录存在 `uia_spike.flag` 时，用与
`wind_tsf/src/IPCClient.cpp`（拉起服务那段）**完全相同的标志**
（`CREATE_NEW_CONSOLE | CREATE_DEFAULT_ERROR_MODE`，先带后不带 `CREATE_BREAKAWAY_FROM_JOB`）
`CreateProcessW` 拉起 `spike\uia_probe.exe --tag from-host`。

这样 Q4 测的是真实路径，不是模拟。T1/T2 不通过就不必做这一步。

## 4. 签名与部署

编译机的签名会话当前可用（`sign.ps1 -Status` 报「会话: 可用」，证书与时间戳正常）。

1. 编译机原生 MSVC 编 `uia_probe`（槽位按 worktree 父目录名派生，与其它分支互不覆盖）。
2. 编译机用 `scripts/sign.ps1 <exe 绝对路径>` 签 B（槽位里没有 `sign.local.ps1`，用主槽位那份
   脚本、传绝对路径）。`Get-AuthenticodeSignature` 验到 `Valid` 才算数。
3. 回传 → 推靶机 `spike\` 目录。靶机是常有并发会话的日常机，**只放探针，不碰服务、管道、
   COM 注册**（同「靶机验证用探针 exe」的做法）。T3 需要推 Dev 版 `wind_tsf_dev.dll`
   （`pdm1`），做之前先确认没有别的会话在用靶机。
4. 签名次数：B 一次，改代码重编则重签，量很小。

## 5. 测试矩阵

靶机 SSH 在 session 0，没法脚本化驱动 GUI，所以每项是「脚本起探针 + 人工 ≤1 分钟 + 日志判据」。
交互会话里启动 exe 可用计划任务（对启动 exe 有效，对跑脚本无效），也可由人在 RDP 里直接双击。

| ID | 变体 | 拉起者 | 人工动作 | 看什么 |
|---|---|---|---|---|
| T0 | A | 交互会话 | 双击 | 是否启动失败（弹窗文案 / 错误码）。预期失败 |
| T1 | B | 交互会话 | 按 Win 键打开开始菜单，探针 `--pos` 对准搜索框下方 | Q1：日志 `UIAccess=1`；Q2：`band=2`、色块在开始菜单之上 |
| T1b | B | 计划任务（交互） | 同 T1 | 拉起者不同时结果是否一致 |
| T2 | C | 交互会话 | 双击 | Q5：失败，还是起来了但 `UIAccess=0` |
| T3 | B | SearchHost 内 DLL（§3.2） | 按 Win 键，打字触发 | Q4：探针是否被拉起、`UIAccess` 与 `IL/AppContainer` 值 |
| T4 | B | T1 或 T3 | 开始菜单打开时：点色块、悬停、滚轮 | Q3：日志收到三类事件；开始菜单**是否仍开着**；继续打字，搜索框**是否仍收字** |
| T5 | B | 同上 | 任务栏搜索框同样测一遍 | band 13 下同样结论 |
| T6 | D | 交互会话 | 开始菜单打开 | 基线：`band=1`，被遮挡 |
| T7 | B | 同上 | 125% / 150% 缩放、全屏应用、UAC 弹窗 | Q6，只记录 |

T4 的两条是整个方案成败的核心，单独强调：色块能收到点击不等于方案成立，要确认点击不会让
开始菜单收起，也不会把键盘焦点从搜索框抢走（否则选词后无法继续输入）。

## 6. 判定与分支

```
T1 通过（UIAccess=1 且 band=2 且盖得住）？
├─ 否 → UIAccess 路线对我们不可行，保持 HostRender，spike 结案，写一页结论入库。
└─ 是 → T3（宿主内拉起）拿到 UIAccess？
        ├─ 否 → 退到「由服务拉起渲染进程」：补测 T3b（服务进程拉起探针）；
        │        服务本身也是被宿主拉起的，要连同 guard_process_context 一起评估。
        └─ 是 → T4/T5 点击不关开始菜单、不抢焦点？
                ├─ 否 → 只解决「显示」不解决「鼠标」，收益大减，重新评估是否值得换。
                └─ 是 → 进入正式设计：渲染进程独立 vs 整个服务 UIAccess、
                         manifest 按签名与否二选一、便携版降级、HostRender 保留作兜底。
```

T2 / T0 的结果不影响主线判定，只决定正式设计里的降级策略。

## 6.1 实测结论

| ID | 结果 |
|---|---|
| T1 开始菜单（B，签名+Program Files，中完整性 cmd 启动） | `uiaccess=1`、`il=0x3000`、`band=2`，色块盖在开始菜单（前台窗口 band=6）之上；悬停、点击、滚轮都收到；点击期间前台窗口始终是开始菜单的 CoreWindow，没有被抢焦点，菜单未收起；左键拖动正常 |
| T2 非受信任目录（C） | 进程**正常启动**但 `uiaccess=0`、`band=1`，被开始菜单盖住：静默降级，不是启动失败 |
| T3b 服务式拉起（D 为中完整性 spawner，拉起 B） | 直接 `CreateProcess` 返回 740（需要提升）；改走 `ShellExecuteEx` 成功，子进程 `uiaccess=1`、`il=0x3000`、`band=2`，父进程为 spawner；开始菜单内滚轮、点击正常 |
| T5 任务栏搜索 | `band=2` 盖在 band 13 之上，悬停/点击/滚轮正常，前台不变 |
| T6 基线（D） | `band=1`，被遮挡 |
| T0 未签名 + uiAccess=true | 未测（预期起不来，正式设计里靠「未签名构建 manifest 不带 uiAccess」规避） |
| T7 DPI/全屏/UAC | 未测 |

## 6.2 判定

主线全过：UIAccess + 签名 + Program Files 可行，且鼠标事件不关菜单、不抢焦点，收益成立。

对正式设计的约束：

1. UIAccess 令牌是 High 完整性，会被 UIPI 拦住中完整性进程发来的窗口消息，所以只拆一个
   小渲染进程，不让整个服务带 UIAccess。
2. 服务拉起渲染进程必须走 `ShellExecuteEx`，不能沿用 `Command::spawn` / `CreateProcess`。
3. 便携版 / 非受信任目录会静默降级成 band 1，必须保留 HostRender 兜底，并在运行时用
   令牌 UIAccess 与窗口 band 自检来决定走哪条。
4. 未签名的 d1 构建 manifest 不能写 `uiAccess="true"`，按是否签名二选一。
5. 仍未验证：在真实服务（含 `guard_process_context`）里经 `ShellExecuteEx` 拉起、与服务之间的
   通信（中完整性服务 → High 渲染进程）、DPI/全屏/UAC。

## 7. 不做什么

- 不改主线 `wind-ui` / `wind-coordinator`，不动 HostRender 现有代码。
- 不改正式服务 exe 的 manifest（未签名构建带 `uiAccess=true` 会直接起不来，这正是 T0 要
  坐实的事，不在主线上冒这个险）。
- 不做安装器改动，不做配置项。

## 8. 清理

结束后：靶机删 `spike\` 目录与 `uia_spike.flag`、还原 Dev 版 DLL；删本分支 worktree 与独立
`CARGO_TARGET_DIR`；结论（含每个 T 的实测值）写成一页，替换本文第 6 节之后的内容。
