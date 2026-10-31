# UIAccess 上屏进程（band presenter）设计

> 状态：**暂缓，不实现**。结论：继续使用 HostRender。
> 原因：新方案仍是跨进程渲染（服务出帧、另一进程贴图），右键菜单等服务窗口两种方案都要另外迁；
> HostRender 已支持候选窗悬停/点击/滚轮/翻页；新方案要求签名 + 装在受保护目录，便携版仍须保留
> HostRender 兜底。收益主要是稳定性（不依赖宿主内 `CreateWindowInBand`、免 band 切换/竞态处理），
> 在这些问题成为主要痛点前不值得多维护一个签名进程。以下为设计存档，需要时可直接接着做。
> 依据：`docs/design/uiaccess-band-spike.md` §6.1/§6.2 的实测结论。
> 目标：在开始菜单 / 任务栏搜索里，用「签名 + UIAccess」的独立小进程代为上屏候选窗，
> 取代宿主进程内 DLL 的 `CreateWindowInBand`（HostRender）作为**首选路径**；HostRender 保留作兜底。

## 1. 约束（来自 spike）

| 事实 | 推论 |
|---|---|
| UIAccess 窗口在 band 2，盖得住开始菜单（6）与任务栏搜索（13），收得到悬停/点击/滚轮，不抢焦点、不收起菜单 | 方案成立 |
| UIAccess 令牌是 High 完整性，UIPI 拦截中完整性进程发来的窗口消息 | 不能让整个服务带 UIAccess，也不能靠 `SendMessage`/`PostMessage` 驱动它 |
| `CreateProcess` 直接拉起 `uiAccess=true` 的 exe 返回 740，`ShellExecuteEx` 成功，子进程拿到 UIAccess | 服务拉起它必须走 `ShellExecuteEx` |
| 非受信任目录（便携版）里同一个签名 exe 能起来，但 `uiaccess=0`、窗口在 band 1 | 要运行时自检，不能假设拉起即可用 |
| 未签名的 `uiAccess=true` exe 起不来（预期，T0 未实测） | 未签名构建的 manifest 不能写 `uiAccess=true` |

## 2. 总体结构

```
 宿主(SearchHost) 内 wind_tsf.dll ──管道──▶ wind_input.exe（服务，中完整性，不变）
                                              │ coordinator / wind-ui 渲染 BGRA 帧
                                              │ HostRenderManager 路由
                      ┌───────────────────────┴──────────────────────┐
                      ▼ 首选                                          ▼ 兜底
        BAND 共享内存 + 事件（按 kind）                    现有 HostRender：DLL 内 Band 窗口
                      ▼
        wind_band.exe（UIAccess，High，仅「贴图 + 命中」）
          ├─ 普通 CreateWindowEx 即落在 band 2
          └─ 鼠标 → 管道 → 服务（CMD_CANDIDATE_SELECT/HOVER/SCROLL，现有命令）
```

要点：

1. **渲染仍全在服务里**（`render_frame()`、主题、DirectWrite 都不动）。`wind_band.exe` 只做
   两件事：把共享内存里的 BGRA 帧用 `UpdateLayeredWindow` 贴到窗口上；对鼠标做命中并回报。
2. **不做成整个服务 UIAccess**，也不把 `wind-ui` 搬进新进程：前者违反 UIPI 约束并改变服务被
   DLL 拉起的方式（740），后者体量大且收益为零。
3. 进程小、依赖少（不依赖 `wind-ui`/`wind-coordinator`/词库），原因同时是安全：High 完整性进程
   只应包含最少代码（见 §7）。

## 3. 新增组件

### 3.1 `wind_band.exe`（新 crate `wind_input/apps/band`，仅 Windows）

- 命令行：`--suffix <变体后缀> --service-pid <pid>`。
- 启动：读自身令牌 `TokenUIAccess` → 经管道向服务发 `CMD_BAND_HELLO`（见 §4.3）上报
  `{uiaccess, version}`；`uiaccess=0` 则上报后立即退出（便携版/非受信任目录的降级信号）。
- 打开服务创建的 BAND 共享内存与事件（§4.1），每个 kind（候选/Tooltip/状态）一个
  `WS_EX_LAYERED|TOPMOST|TOOLWINDOW|NOACTIVATE` 的 popup（与 spike 探针同样式，
  `WM_MOUSEACTIVATE→MA_NOACTIVATE`）。
- 事件线程等帧事件 → 校验帧头 → `UpdateLayeredWindow`，按帧头的坐标/尺寸/阴影标志落位，
  VISIBLE 位清零则隐藏。帧语义与 `HostWindow.cpp` 完全一致（同一 `SharedRenderHeader`）。
- 鼠标：只有候选窗可交互（沿用 HostRender 现状）。命中表取自帧头的 hit-rect；移植
  `HostWindow.cpp::_HitTest/_OnMouse*` 与 `HostClickPolicy.h` 的判定到 Rust 纯函数
  （Linux 可单测）。发出现有上行命令，值域沿用 HostRender（见 `host-render-windows-port.md` §11.1）。
- DPI：manifest `PerMonitorV2`，帧坐标即物理像素，免去 DLL 在宿主 DPI 上下文里的换算问题。
- 退出：`OpenProcess(SYNCHRONIZE)` 监视 `--service-pid`，服务退出即退出；收到 `CMD_BAND_QUIT`
  （或共享内存里的退出标志）也退出。
- manifest：`uiAccess` 由构建时环境变量决定（§6），带 `asInvoker`。

### 3.2 服务侧 `BandPresenter`（`wind-bridge`，新模块，Windows）

- 生命周期：状态机 `Unknown → Starting → Ready | Unavailable(原因, 冷却到期时间)`。
- 拉起：用 `ShellExecuteExW`（`SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI`，无 `runas` 动词），
  路径取自服务 exe 同目录。**不用** `Command::spawn`/`CreateProcess`。
- 触发时机：第一次有白名单宿主获得焦点时（即现在置 `host_render_avail` 的时机）异步拉起，
  不阻塞按键线程；spike 实测拉起到窗口就绪约 0.2s，被首个按键之前的焦点切换吸收。
  从不进开始菜单搜索的用户不会多出这个进程。
- 就绪判据：收到 `CMD_BAND_HELLO{uiaccess=1}` 才算 Ready；拉起失败、`uiaccess=0`、
  进程提前退出都转 `Unavailable`，冷却后可重试（避免死循环；便携版只试一次并记日志）。
- 进程句柄上注册等待：崩溃/被杀即转 `Unavailable` 并把可见帧收回（走 §5 的路由重评估）。

## 4. 通道

### 4.1 帧（服务 → presenter）

复用现有 `SharedRenderHeader`/`FrameParams` 与 `write_frame_for_kind` 的写帧实现，
新增一组**固定命名**的段：

- 共享内存：`Local\WindInput_BAND_SHM{suffix}{_TIP|_STS}`；事件：`Local\WindInput_BAND_EVT{suffix}{_TIP|_STS}`。
- 服务创建、presenter 只打开（High 进程访问中完整性创建的对象无障碍）。DACL 仅当前用户 +
  SYSTEM，不给 Everyone。
- 单实例消费者，帧头 `target_instance_id` 恒按 presenter 接受任意值处理，hide 帧照旧写
  VISIBLE 清零。

### 4.2 输入（presenter → 服务）

presenter 作为普通客户端连现有 bridge 管道，只发三条已存在的异步命令：
`CMD_CANDIDATE_SELECT` / `CMD_CANDIDATE_HOVER` / `CMD_CANDIDATE_SCROLL`。服务端不需要新的
处理逻辑。不引入 `PostMessage` 路径（UIPI 会拦）。

> 已核对：三条 handler（`handler.rs` 的 `handle_candidate_select/hover/scroll`）签名里没有连接参数，
> 不依赖发送者的 conn_id，presenter 无需登记 conn。

### 4.3 控制消息（新增）

| 命令 | 方向 | 载荷 | 说明 |
|---|---|---|---|
| `CMD_BAND_HELLO` | presenter→服务 | `{version u16, uiaccess u8, pid u32}` | 就绪/降级信号 |
| `CMD_BAND_QUIT`（可选） | 服务→presenter | — | 经 BAND 控制位，不另开通道 |

按「未发布功能不做兼容」原则，协议号直接分配，无兼容层。

## 5. 路由（`HostRenderManager` 的变化）

现有判定（白名单 + `active_target`）不动。变化只在「写到哪」：

```
write_frame_for_kind(kind, target, frame):
    if band.ready():   写 BAND 段 + SetEvent(BAND_EVT)     # 首选
    else:              写 DLL 全局段 + SetEvent(宿主事件)    # 兜底（现状）
    失败 → 返回 Err，调用方照旧回退本地窗口

hide_kind(kind): 两边都发 hide 帧（幂等），保证路由切换瞬间不留残影
```

- DLL 侧 `host_render_avail`/`setup` 握手保持原样：presenter 不可用时它仍是现成的兜底，
  不必在 presenter Ready 时关掉它（空闲不占资源，切换无需再握手）。
- 同一时刻只有一路持有可见帧：路由从 DLL 切到 presenter（或相反）时，先 `hide_kind` 旧路再写新路。
- `write_frame_for_kind` 的调用点（`manager.rs` 的候选/Tooltip/状态三处）不需要改。

## 6. 构建、签名与安装

- **manifest 二选一**：`wind_band.exe` 的 `build.rs` 读环境变量 `WIND_BAND_UIACCESS`
  （`true`/`false`，缺省 `false`）。未签名的 d1 构建缺省 `false`，起得来但降级；
  **签名发布流程**设为 `true`。这样不会出现「未签名构建起不来」。
  签名发布脚本在调用构建时必须带该变量，并在签名前核对产物 manifest（防「签了但
  manifest 是 false」或相反）。
- **签名清单**：新增 1 个 PE，纳入 `docs/design/code-signing.md` 的签名顺序表与
  `check-signed.py` 的检查范围（该表目前写的是 5 个 PE，实现时同步改成 6 个）。
  UIAccess 要求**签名发生在 manifest 定稿之后**，次序与现有 PE 一致。
- **安装位置**：与 `wind_input.exe` 同目录（`%ProgramFiles%\WindInput`），自然满足「受信任目录」。
  安装器、卸载、升级的文件清单与 dev/prod 变体命名（`wind_band_dev.exe`）同步增加。
  升级时旧进程占用文件的处理照 `wind_input.exe` 的现有做法（先通知退出）。
- **便携版**：不在受信任目录，presenter 会自报 `uiaccess=0` 并退出，路由自动落回 HostRender。
  文档里如实写明「便携版在开始菜单里走兜底路径」。

## 7. 安全

High 完整性 + UIAccess 意味着该进程有权向更高层窗口发输入、截取窗口。对策是**让它做不了这些事**：

1. 代码只含贴图与命中，不含任何输入注入、截屏、`SendInput`、`SetWindowsHookEx`、文件/网络访问；
   crate 依赖仅 `windows` 的必要 feature，评审时以此为清单。
2. 共享内存内容视为不可信输入：校验魔数/版本、宽高上限（沿用 `MAX_SHARED_RENDER_SIZE`）、
   `rect` 数量与帧长一致，越界即丢帧。DACL 限当前用户。
3. 拒绝被别的进程当「高完整性跳板」：只接受命令行里的 `--service-pid` 进程存活期间工作；
   不监听任何自己的管道/端口；`CMD_*` 只出不入（除 §4.3 的退出位）。
4. 签名 + 受信任目录本身就是 UIAccess 的信任根，不额外放宽目录权限。

## 8. 失败与降级矩阵

| 情形 | 表现 |
|---|---|
| 正常（已签名、Program Files） | presenter Ready，走 BAND 段 |
| 未签名 d1 / 便携版 | `uiaccess=0` 自退出，`Unavailable`，走 DLL HostRender |
| `ShellExecuteEx` 失败（含 740 之外的错误） | `Unavailable` + 冷却，走 HostRender，日志记错误码 |
| presenter 崩溃 | 等待句柄触发，`Unavailable`，可见帧由路由重评估收回；冷却后重拉 |
| 服务重启 | 旧 presenter 随 `--service-pid` 退出，新服务按需重拉 |
| 帧写入 BAND 段失败 | 同现有语义：回退本地窗口，不丢帧 |

## 9. 不做什么（本期）

- 不改 `wind-ui` 渲染、不改 coordinator 白名单逻辑（仍是 compat `host_render` 规则）。
- 不删 HostRender（DLL 侧与服务侧都保留，作便携版/失败兜底）。
- 不把右键菜单、工具栏、软键盘等其它服务窗口迁到 presenter。这些窗口在开始菜单里仍是 band 1，
  被盖住——这是**与旧方案相同的缺口**，不是新回归；presenter 的帧通道是通用的，后续
  可按 kind 增加，另立项。
- 不让 presenter 对所有应用生效，仅白名单宿主。

## 10. 实施阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| S1 | `apps/band` crate：贴图 + 事件 + 单测（帧头校验、命中纯函数）；`CMD_BAND_HELLO` 协议号 | Linux 单测通过；Windows 编过 |
| S2 | 服务侧 `BandPresenter`（`ShellExecuteEx` 拉起、状态机、句柄等待）+ `HostRenderManager` 路由 + BAND 段 | 靶机：开始菜单/任务栏搜索候选走 presenter（日志可见路由） |
| S3 | 鼠标回传（悬停/点击/滚轮/翻页） | 靶机：开始菜单内鼠标点选，菜单不收起、焦点不丢 |
| S4 | 构建/签名/安装/便携版降级；`WIND_BAND_UIACCESS`；签名顺序文档 | 签名构建 `uiaccess=1`，d1 构建与便携版自动走兜底 |
| S5 | 文档（FAQ 的开始菜单条目、`host-render-windows-port.md` 标注新首选路径） | — |

## 11. 实现前还要实测/核对的点

1. **T0**：未签名 + `uiAccess=true` 的 exe 经 `ShellExecuteEx` 拉起的错误码（决定降级日志文案）。
2. **服务真实拉起路径**：服务（含 `guard_process_context`、DLL 以 `CREATE_NEW_CONSOLE` 拉起的环境）
   里调 `ShellExecuteEx` 是否仍拿到 UIAccess；父进程/Job 对象是否影响（spike 是从命令行进程拉起的）。
3. **T7**：125%/150% DPI、多显示器混合 DPI、全屏应用、UAC 安全桌面下的表现。
4. **拖动期间/点击后焦点**：spike 已证明前台窗口不变；需在真实 TSF 流程（选词上屏后继续输入）里复核。
5. presenter 与 DLL 兜底同时存在时，白名单宿主重连、`avail` 位补推的交互有无副作用。
