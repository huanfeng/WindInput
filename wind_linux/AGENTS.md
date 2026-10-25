# wind_linux

Linux 上的 **Fcitx5 addon**（C++17），与 Windows 的 `wind_tsf/`（TSF DLL）、macOS 的
`wind_macos/`（IMKit `.app`）对位。它是薄壳：按键转发给 Rust 服务、把服务的上屏/预编辑写回
Fcitx5 的 `InputContext`。引擎/词库/候选逻辑全在服务里，这里**不做任何中英判定**。

服务以 `cargo build -p wind_service --features linux-host` 构建（在 `wind_input/` 下），
`linux-host` 让协调器走「外部宿主 forwarder」（`wind-ui/src/manager_macos.rs`，macOS 与 Linux
共用，文件名沿用）。

## 结构

| 路径 | 角色 |
|---|---|
| `include/Protocol.h` | **直接 include** `wind_tsf/include/BinaryProtocol.h`（不另抄一份）；用宏垫掉它唯一的 Win32 依赖 `GetCurrentModifiers()`。那个函数在 Linux 上**禁止调用** |
| `include/ExtProtocol.h` | 外部宿主专用 cmd（候选帧 / tooltip / 按键合成…），Windows 不用故不在 BinaryProtocol.h 里。每个值由 `tests/protocol_sync_test.cpp` 逐个对照 `protocol.rs` 源码 |
| `include/Codec.h` + `src/core/Codec.cpp` | 帧编解码。每个函数注明对位的 Swift `BinaryCodec` 函数 |
| `include/Bridge.h` + `src/core/Bridge.cpp` | UDS 请求/响应客户端（connect / 读 / 写都 2s 超时，超时后熔断 3s；`MSG_NOSIGNAL`；失败分类决定能否重发）、push 监听线程（断线每秒重连）、端点路径 |
| `include/KeyMap.h` + `src/core/KeyMap.cpp` | X11 keysym / 修饰状态 → Windows VK / 协议修饰位；修饰键单击检测 |
| `include/KeySynth.h` + `src/core/KeySynth.cpp` | 命令直通车按键合成的纯逻辑：键名 → keysym、组合 → 按下 / 抬起事件列、key.hold 的按住记账（卡键保护）与限流。见下「命令直通车按键合成」 |
| `include/ResponseRouter.h` + `src/core/ResponseRouter.cpp` | 响应帧 → 宿主操作。Swift `BridgeResponseRouter` 的逐条移植（待定标点 / 定格前缀 / hold 计时器 / 数字后智能标点记账） |
| `include/Utf.h` + `src/core/Utf.cpp` | UTF-16 码元 ↔ UTF-8 字节换算（服务端光标以 UTF-16 计，Fcitx5 要字节偏移） |
| `include/ServiceLauncher.h` + `src/core/ServiceLauncher.cpp` | 连不上服务时拉起它（节流 20 秒）。`spawnDetached`（设置程序共用）：双 fork（fcitx5 不替我们回收子进程，实测 posix_spawn 出来的服务退出后留 defunct）、新会话、清信号掩码并复位处置、stdio 接 /dev/null、关掉 3 起的 fd |
| `include/SettingsLauncher.h` + `src/core/SettingsLauncher.cpp` | 下行扩展信封 `settings.open` 的 body 解析（JSON argv）与设置程序路径（`WIND_INPUT_SETTING` / `/usr/lib/windinput/wind_setting`）。只启动自己的设置程序，信封内容只进参数位 |
| `include/Menu.h` + `src/core/Menu.cpp` | 自绘菜单的纯逻辑：kind ↔ 级、候选窗右键的目标、空闲超时（`WIND_MENU_IDLE_TIMEOUT_MS`） |
| `include/HostUi.h` + `src/core/HostUi.cpp` | 交给 Fcitx5 呈现的部分：模式镜像（托盘图标：服务端按主字运行时渲染的 `windinput-lbl-[dev-]<状态>-<主字十六进制，超长为 h+散列>`，没写出时退回种子 `windinput-zh/en/caps`；中英、大写锁定位与标签从服务端四种状态帧学）、addon 加载时预建用户图标目录、大写锁定的本端判定（`CapsLockTracker`）、应用内预编辑过滤掉单空格占位组合 |
| `include/OverlayInput.h` + `src/core/OverlayInput.cpp` | 光栅浮层与候选悬停的鼠标交互纯逻辑：按键 → 动作（拖动 / 菜单 / 关闭）、菜单 target、悬停门控、提示的悬停延后与离开重定、拖动落位 |
| `include/ShmFrame.h` + `src/core/ShmFrame.cpp` | 候选帧 SHM 读端（对位 `SharedMemoryReader.swift`）、落位几何 `placePanel`（对位 `CandidatePanel.show` 的翻转/钳制）与浮层落位 `placeOverlay`、命中测试 |
| `src/fcitx/WindEngine.{h,cpp}` | Fcitx5 引擎（`InputMethodEngineV2`）；与 `X11Panel` 是仅有的两个依赖 Fcitx5 头文件的地方 |
| `src/fcitx/X11Panel.{h,cpp}` | X11 候选窗 + 三层光栅浮层 + 自绘菜单：自建 xcb 连接、override-redirect 窗口贴帧、候选窗的鼠标点击/悬停/滚轮/右键回传、浮层的悬停保持 / 拖动 / 右键 / 点击关闭与自动隐藏计时、菜单打开期间抓指针并回报 |
| `data/*.conf.in` | addon / 输入法描述文件模板（构建时生成到 `build/…/share/fcitx5/`） |
| `data/icons/hicolor` | 图标成品，CMake 装到 `share/icons`。`windinput`（应用图标）由 `scripts/linux/gen-icons.py` 拆自 wind-setting 的 ico；托盘图标的种子 `windinput-zh` / `-en` / `-caps`（运行时图标还没写出时的降级）由 `wind_input/crates/wind-ui/examples/gen_tray_icons.rs` 生成，与运行时图标同一个 `tray_icon::render`（`host_ui_test` 核对文件齐全）。运行时图标不在这里，在用户的 `$XDG_DATA_HOME/icons/hicolor` |
| `tests/*_test.cpp` | 纯 C++17 单测（不需要 Fcitx5），与 `wind_tsf/tests` 同风格 |
| `include/GnomeCaretGeom.h` + `src/core/GnomeCaretGeom.cpp` | GNOME 光标折算的纯函数（桌面名 / 前端名 / scale）。D-Bus 客户端在 `src/fcitx/GnomeCaret.cpp`，只在 Wayland 且有 gio 时编进 addon |
| `gnome-shell-extension/` | Shell 扩展 `windinput-caret@windinput`（Shell 47–50）。安装说明见该目录 README |

`src/core` 与 `include` 不依赖 Fcitx5，能单独编、单独测——这条边界别破。

## 构建 / 测试

本机无 root 时先搭用户前缀 SDK（幂等，只写 `~/.local/opt/fcitx5-sdk`，不碰 `/usr`）：

```bash
scripts/linux/bootstrap-sdk.sh                  # apt-get download + dpkg -x fcitx5 全家、Xvfb、cmake(pip)、dbus-next(pip)
eval "$(scripts/linux/bootstrap-sdk.sh env)"    # PATH / PKG_CONFIG_PATH / CMAKE_PREFIX_PATH / LD_LIBRARY_PATH
```

```bash
make -C wind_linux test                         # 单测：不需要 cmake / Fcitx5，g++ 直接编
cmake -S wind_linux -B wind_linux/build/cmake -G Ninja && cmake --build wind_linux/build/cmake
ctest --test-dir wind_linux/build/cmake         # 同一批单测经 CMake 跑
scripts/linux/e2e.sh                            # 端到端：真服务 + 真 fcitx5 + DBus 客户端打字
```

`e2e.sh` 的做法（改它之前先读脚本头注释）：`dbus-run-session` 起私有会话总线；服务二进制拷进
临时目录、`data` 下逐项软链到 `build_dev/data`（`system.phrases.toml` 例外：取仓库 `data/` 那份再追加
e2e 专用的按键合成短语——`build_dev/data` 是构建时的拷贝，可能落后于短语改动），配置/数据/缓存全用 `XDG_*_HOME` 隔离，socket 走
`WIND_INPUT_RUNTIME_DIR`；fcitx5 以 `--disable=all --enable=keyboard,dbus,dbusfrontend,windinput`
起，`FCITX_ADDON_DIRS` / `FCITX_DATA_DIRS` 指到构建目录；`e2e_client.py`（dbus-next）经
`org.fcitx.Fcitx.InputContext1.ProcessKeyEvent` 送键，收 `CommitString` / `UpdateFormattedPreedit`
信号断言。临时目录必须短（socket 路径 108 字节上限），默认 `/tmp/wi-linux/e2e.<pid>`。

候选窗与浮层（X11）用例在私有 Xvfb 里跑（`WIND_E2E_X11=0` 关掉）：打字后按 `WM_CLASS` 实例名
`wind-candidate` 找窗（`xdotool search --classname`，**不是** `--class`——后者匹配第二段
`WindInput`），断言已映射、落在光标下方、截图里有内容、上屏/Esc/鼠标选词后隐藏、屏幕底边时
翻到光标上方；鼠标选词因命中矩形只有服务端知道，沿窗口中线逐点点击直到有上屏。浮层用例：
Shift 切中英 / Ctrl+Shift+E 切方案后等 `wind-status` 出现再等它自己消失；Toast 用
`wind_input ui toast`（控制 RPC 在 `$XDG_RUNTIME_DIR` 下，e2e 已把它指进临时目录）；tooltip 沿
候选窗中线 `xdotool mousemove` 直到出现。截图存在 `$W/shots/`（`KEEP=1` 保留）。`WIND_E2E_COMPOSITOR=1` 另起 xcompmgr 走 ARGB 路径。
浮层交互用例：`xdotool mousedown / mousemove / mouseup` 拖气泡，数 fcitx5 日志里「状态气泡拖动松手」
验 `pos.status_tip` 上报、读隔离目录的用户 `config.toml` 验落不落盘；剪贴板是假的
`$W/fakebin/xclip`（写入记进 `$W/clipboard.log`）——为此 Xvfb 先于服务起，服务要继承 `DISPLAY`
才会选 xclip 后端。e2e 把 `ui.status.duration` 调到 2 秒（出厂 800ms，来不及把指针挪上去）。

设置程序用例：e2e 经包装脚本（`$W/svc/wind_setting`，记下 argv 再 exec 真程序，真程序默认取
`~/.cache/wi-tgt-setting-linux/debug/wind_setting`，`WIND_E2E_SETTING` 可覆盖，缺它 e2e 直接失败）
设 `WIND_INPUT_SETTING`，按 `Ctrl+Shift+]` 验「热键 → 信封 → 拉起 → 连上服务 → 再按一次转交首实例
→ 退出」。设置窗口按 WM_CLASS 实例名 `wind_setting` 找；映射早于首帧，截图要轮询到有内容。

e2e 的坑，都踩过：

- **服务 socket 起得比引擎早**。词库加载（首次还要建 `.wdat` 缓存，十几秒）完成前按键一律
  PassThrough。客户端先用真实通路探活（按 `a` 直到被吃再 `Esc`），不要用 sleep 猜。
- **`dbus` 模块依赖 `keyboard`**：`--enable` 里少了 keyboard，dbus 模块根本不加载，
  `org.fcitx.Fcitx5` 这个名字永远没人持有。
- **SDK 里的 Xvfb 在写死的 `/usr/bin` 找 xkbcomp**，没 root 装不进去，键盘初始化失败即退出。
  bootstrap 复制一份 `Xvfb-wind`、把那个字符串原地改成同长的 `/tmp/wlx` 并软链到 SDK 的
  `usr/bin`；xkb 数据走 `-xkbdir /usr/share/X11/xkb`。
- **xcompmgr + Xvfb 下 `xwd -root` 截到的合成结果不可信**（候选窗整体错位、文字出彩边），
  而 `xwd -id <候选窗>` 取到的窗口自身像素是对的。ARGB 路径因此只验到「窗口像素正确」，
  屏幕上的合成效果要在真桌面上看。
- **Fcitx5 出厂 `AltTriggerKeys=Shift_L`** 会在引擎之前截走 Shift 单击。addon 在 `PreInputMethod`
  阶段先接管（`WindEngine::claimShiftTap`，条件与取舍见设计文档 §8），**用户不必改任何 Fcitx5 配置**；
  e2e 的 fcitx 配置保留出厂值并带「键盘 + 清风」两个输入法，Shift 用例就是回归。

## 协议用法（与 macOS 的异同）

- 按键：只发 **keydown**（`CMD_KEY_EVENT`），**同步**等响应再决定 `filterAndAccept`。Fcitx5 与
  IMKit 一样没有 TSF 那种「先决定吃不吃、再问服务」的前置闸门，所以「C++ 吃键集 ⊆ Rust
  出字集」天然成立——吃不吃完全由响应决定。按下被吃的键，其松开也吃掉（按硬件键码记）。
- 修饰键本身的按下不上报；**干净单击**（按下→无别的键→500ms 内松开，同 Windows
  `TOGGLE_TAP_THRESHOLD_MS`）时发一帧 `eventType=UP` 的 KeyEvent（`VK_LSHIFT` 等）。协调器
  只在 keyup 分支处理切换键。这一帧的 `toggles` 必须是真实锁定态：服务端拿**每一帧**的 toggles
  校准大写锁定镜像，发 0 就是告诉它「大写锁定关了」。
- CapsLock：按下不上报，松开发 `VK_CAPITAL` keyup、toggles 带**新**锁定态（同 Windows 的状态通知）。
  X11 事件的 state 是事件之前的，关大写那次的松开 state **仍带 Lock**，新状态只能按「按下时没锁 ⇒
  松开后锁上」推（`CapsLockTracker`，实测与取舍见设计文档 §5d「大写锁定的判定」）。
- 焦点：Fcitx5 的 `activate` 同时承载「切到本输入法」与「焦点进入文本框」，按事件类型分开——
  `InputContextSwitchInputMethod` 才发 `CMD_IME_ACTIVATED`（服务端据此套用激活初始状态），
  普通焦点进入只发 `CMD_FOCUS_GAINED`。每次聚焦都报 IME_ACTIVATED 会让「不记忆中英状态」的
  用户每切一次窗口就被重置。`deactivate` 对称地发 `FOCUS_LOST`（+ 切走时 `IME_DEACTIVATED`）。
- `CMD_IME_ACTIVATED/DEACTIVATED` 服务端**从不回响应**，必须带 AsyncFlag 发、不读——否则
  主线程白等 2s 超时。
- `FOCUS_GAINED` 载荷发满 39 字节 + `bundleId`（= `InputContext::program()`）+ 空 windowClass。
  clientToken 高 32 位 = 程序名的 FNV-1a 散列（同 macOS：Wayland/DBus 客户端拿不到可靠 pid），
  低 32 位 = IC uuid 的散列。
- 焦点重叠：没有焦点组的前端允许 B 先 FocusIn、A 才 FocusOut。A 迟到的 `FOCUS_LOST` 会被服务端
  当「陈旧失焦」丢弃（active token 已是 B），服务端缓冲里的旧编码就接着拼进 B（实测上屏
  「你好你好」）。`activate` 发现上一个 IC 还挂着时先替它收尾。e2e 7b 钉这条。
- 预编辑带 `TextFormatFlag::DontCommit`：客户端没声明 `ClientUnfocusCommit` 时，Fcitx5 会在失焦
  时把 client preedit 当正文上屏（实测收到 `"ni'hao"`）。组合串是编码不是正文。
- 密码框：`CapabilityFlag::PasswordOrSensitive` → `INPUT_SCOPE_PASSWORD_BIT`。同一 IC 内能力位
  翻转不触发 activate，按键前比对、翻转即补报 FocusGained（同 macOS 的安全输入跟随）。
- 光标：`InputContext::cursorRect()`（X11 客户端是根窗口坐标，正是 wire 坐标系）→
  `CMD_CARET_UPDATE` 12 字节版，y 取行顶、height 取行高。宿主没报过位置（全 0）时不发。
  GNOME 无 IM-v2、且 `XDG_CURRENT_DESKTOP` 分段里有 `GNOME`、且 IC 的 `display()` 为 `wayland:`
  时，`dbus`/`ibus`/`fcitx4` 才按 `RelativeRect` / `scaleFactor` / frontend 折到 X11（扩展
  `windinput-caret` 补窗口原点；`x11:` 组含 XWayland 应用，已是根物理像素，直通）。扩展成功结果
  在同一次光标更新内复用，约 1s 后失效以免拖窗后粘旧坐标。非 GNOME 保持根坐标直通。换算纯函数在
  `GnomeCaretGeom`，`gnome_caret_geom_test` 覆盖。
  扩展声明 Shell 47–50，42.9 那次观察装不上它。细则 §7.10 与
  `gnome-shell-extension/README.md`）。
- 回删（`CMD_REPLACE_BACKWARD`，智能符号 / 撤销上屏）的计数是 **UTF-16 码元**（Windows TSF 的 ACP、
  macOS 的 NSRange 同量纲），Fcitx5 的 `deleteSurroundingText` 按码点删：按光标前文本换算
  （`codepointsForUtf16Back`，落在代理对中间宁少删），读不到光标前文本才按码元数直删。配对插入后的
  光标回退（`cursorOffset`）是「按几下方向键」，三平台都合成方向键，不换算。两者转 int 取负前钳到
  1024 / 64（服务端给 u32，> INT_MAX 取负是未定义行为）。帧宽高超过 16384 拒收（X11 尺寸 16 位，
  截断后的请求会让 xcb 关连接）。
- prevChar（数字后智能标点）：「数字后」只认本端记账（`DigitTracker`，三平台一致）；surrounding text
  读到的光标前字符只在**非数字**时照报，供智能符号 press2 守卫比对（`DigitTracker::resolve`）。
- 断线自愈：请求连接 2s 超时（connect 之前就设好，服务停住时 connect 本身也会阻塞）。只有「对端
  确定没处理」的失败才换新连接**重发当前帧一次**：写失败、或一个响应字节都没读到就 EOF /
  ECONNRESET（服务重启 / 读帧前后崩溃）。**读超时不重发**：服务可能已经处理、只是回得慢，协调器
  不按 `event_seq` 去重，重发就是重复上屏。push 通道断开后每秒重连，收到第二次起的
  `SERVICE_READY` 即视为服务重启：换新请求连接并对当前焦点重报 FocusGained。
- 只连本用户的服务：两条通道连上即查对端 uid（`SO_PEERCRED`），不是本用户就断开、不重试也不拉起
  服务（报一次警告，按键透传）；无 `XDG_RUNTIME_DIR` 时的 `/tmp/wind_input{_dev}-<uid>` 兜底目录先校验
  私有（是目录、非符号链接、本用户、无组 / 其他人权限）再连。取舍见设计文档 §4。
- 服务卡死（进程在、不响应）：任何一次超时（连接 / 写 / 读）后**熔断 3 秒**，期间 connect 直接
  失败、按键透传、不再发同步请求——每次卡死至多付一次 2s 超时，而不是每键 4~6 秒（caret +
  按键 + 重试，`activate` 还有三次请求）。熔断时本端收起组字、候选窗、浮层、菜单（放开指针）并补发
  key.hold 的抬起；到期后下一键再试，连上即发 `COMPOSITION_TERMINATED` + 重报焦点——服务恢复时会
  把积在旧连接里的帧处理掉（超时不等于它没收），不复位的话旧码会拼进新输入。e2e 用 SIGSTOP /
  SIGCONT 验（`stall_cases`）。
- `reset`（宿主要求重置，如鼠标点击挪了光标）→ 收组合 + `CMD_COMPOSITION_TERMINATED`。

## 候选窗（X11）

与 macOS 同构：服务进程光栅化（BGRA 预乘 alpha）→ 写 POSIX SHM `/WindInput[Dev].<uid>` →
push `CMD_HOST_RENDER_FRAME`（坐标 + flags）与 `CMD_CANDIDATE_RECTS`（命中矩形，晚于帧）。
addon 在主线程（经 EventDispatcher）按名只读打开 SHM、拷出一帧，交 `X11CandidatePanel` 呈现。

- **自建 xcb 连接**，不借 Fcitx5 的 xcb 模块：候选窗与 Fcitx5 自己的 UI 无关，xcb 模块没加载时
  照常工作。连接的 fd 挂在 Fcitx5 事件循环上，鼠标事件与按键同在主线程、不会交错，所以
  选词/悬停/滚轮直接复用按键那条请求连接。连接断了只停 IO 事件，下一帧重连。
- **override-redirect 顶层窗口**：不归窗口管理器管、不抢焦点（点候选不会让宿主失焦）。
  `WM_CLASS = wind-candidate / WindInput`，`_NET_WM_WINDOW_TYPE_POPUP_MENU`。
- **透明两条路**：有合成器（`_NET_WM_CM_S<n>` 有人持有）→ 32 位 ARGB visual，圆角与软件阴影
  真透明；没有 → 根 visual + XShape 按 alpha≥128 抠形（圆角保住，半透明阴影退化）。每帧都查
  一次合成器，合成器中途起停会自动换路（重建窗口）。
- **位图挂成窗口背景 pixmap**：被遮挡后露出由 X 服务器自己重绘，不处理 Expose。PutImage 按
  最大请求长度分块（无 BIG-REQUESTS 时 256KB）。
- **落位**：服务端给左上角建议点；addon 按 macOS 同规则水平钳制、下方放不下翻到光标上方
  （光标高估 18px）、`FLAG_ABSOLUTE_POS` 只钳制不翻转。工作区取根窗口尺寸。
- **SHM 与重启**：收到 `SERVICE_READY` 关掉旧映射（服务重启会 unlink + 重建段，旧映射成孤儿、
  候选窗卡在旧帧），下一帧按名重开。
- **SHM 名带 uid、打开先 `fstat`**：`/dev/shm` 全系统共用，名字不带 uid 时同机第二个用户的服务
  建不了段（`shm_unlink` EPERM、`O_EXCL` EEXIST）。macOS 名字不变（`/WindInput_SHM[Dev]`，Swift
  写死）。addon 打开后先 `fstat`：属主不是本用户就拒绝（别人抢先建的同名段）；比一个帧头还小也
  拒绝、下一帧再开——服务端 `shm_open(O_CREAT)` 与 `ftruncate` 之间段大小为 0，按 4MB 映射一读就
  SIGBUS、带走整个 fcitx5；映射取 `min(实际大小, 4MB)`，帧头里的尺寸再按映射大小校验。剩余风险：
  别的用户抢先建了我们的名字，服务端建不了段（候选窗不显示），但拿不到也伪造不了内容。
- 悬停只报候选下标（≥0）：命中表里 -1/-2 是翻页按钮，而悬停协议里 -1 表示「无」。点击翻页
  按钮直接发负下标的 `CMD_CANDIDATE_SELECT`（服务端按 -1 上页 / -2 下页处理）。滚轮 ±120。

## 光栅浮层：状态气泡 / Toast / 悬停提示（X11）

**与 macOS 同构**（2026-10 起 macOS 也改成这样，见 `docs/design/mac-overlay-raster.md`）：
本 addon 不排字，这三者与候选窗同构——服务进程按主题光栅化（`wind-ui/src/overlay_host.rs`，
真实字形走 `text/linux`），像素写进**各层自己的** SHM 段（`/WindInput[Dev].<uid>` + `_TIP` /
`_STS` / `_TST`，`overlayShmName`），再推 `CMD_OVERLAY_FRAME`（0x0513，68 字节基础布局，macOS 版
末尾多一个 f32 scale，本 addon 不读；布局见 `Codec.h` 的 `OverlayFramePayload`）。服务端代码在
`cfg(ext_presenter)` 下，macOS `.app` 与本 addon 共用。

- **一层一窗**：`X11CandidatePanel` 里候选窗与三层浮层各一个 `Surface`，共用一条 xcb 连接；
  实例名 `wind-tooltip` / `wind-status` / `wind-toast`（e2e 按它找窗）。窗口类型
  `_NET_WM_WINDOW_TYPE_TOOLTIP`；透明两条路同候选窗，输入区只留不透明处（无合成器时随外形裁，
  ARGB 时另设 XShape 输入区），点阴影 / 圆角外落到下面的应用。
- **鼠标交互**（对位 Windows 各窗口，纯逻辑在 `OverlayInput.h`）：
  - 状态气泡：左键拖动（靠按下时的**隐式抓取**收移动 / 松开，松手即结束、不会卡住；内容盒夹进
    工作区，同 `placeOverlay` 的末步），松手报 `pos.status_tip`（内容左上；落不落盘由服务端
    `save_status_tip_pos` 按定位方式定）；拖动中来的新帧只换像素、不重新落位。右键 → `menu.open`
    target `MENU_TARGET_STATUS` → 服务端弹 Windows 同款气泡菜单。应 `pos.status_tip.query`
    报当前位置（「固定位置」以当前位置落盘）。
  - 悬停提示：指针离开候选行时，若该候选的提示正显示，悬停变化延后（去往「无」280ms、去往另一
    候选 150ms，同 Windows `hover_move`）；指针进了提示就撤掉，悬停留在原候选、提示不动。离开
    提示后同样 280ms 宽限，到期按指针真实位置重定（`tipRecheckHover`）。右键 → target
    `MENU_TARGET_TOOLTIP`，另带位图内坐标 `lx/ly`：命中（段 / 原始行）在服务端按它最近画的那一帧
    做（`UiCommand::TooltipMenuAt` → `RequestTooltipMenu`，之后与 Windows 同一个
    `show_tooltip_menu`）。提示菜单随候选收起（`candidate_menu_open`）。左键无动作（同 Windows）。
  - Toast：任意键点一下立即关（Windows 没有 Toast 交互，这是 Linux 加的）。
  - 自动隐藏的暂停：气泡在悬停 / 拖动 / 自己的菜单开着（请求发出到菜单收起）时不计时，Toast 在
    悬停时不计时；交互结束重新给满一份时长（同 Windows `interacting()` + 边沿重新计时）。「悬停」
    只认窗口出现之后指针真的动过（`HoverGate`）：气泡弹在静止的指针下不算，否则永不消失。
  - 菜单收起（任何路径）后按指针真实位置重定一次：指针在提示上就留下，否则按指针处重报候选悬停
    （提示随之收起，同 Windows 菜单关闭时「光标不在气泡上就隐藏」）；气泡 / Toast 的悬停同理。
  - 复位：浮层被摘（隐藏帧 / `SERVICE_READY` / 连接断开）即归位拖动与悬停，被摘时正在拖的那次
    **作废、不上报**——X 在窗口不可见时自动放掉隐式抓取，不会有松开事件。典型是拖动中失焦：服务端
    失焦即收状态提示（同 Windows），这次拖动随之作废（Windows 的气泡窗口隐藏后仍持有鼠标捕获，
    松手照报落点；Linux 不跟：用户没松手，谈不上摆到了哪）。气泡菜单请求 2 秒内菜单没来（服务
    没了）就放开保持。截图（菜单里的「截图此窗口」）在服务端就地从该层 SHM 读回。
- **落位归 addon**：服务拿不到屏幕几何，帧里只给规则（坐标都指**内容盒**，窗口 = 内容 − 阴影扩边）：
  `ABSOLUTE`（状态气泡固定位置）、`FLIP`（首选点 + 右溢/下溢时的备选点：状态气泡跟随光标）、
  `FOLLOW_CANDIDATE`（tooltip：坐标按候选窗**建议**落点算，addon 先平移「候选窗实际落点 −
  建议落点」再同 FLIP——候选窗被翻到光标上方时 tooltip 跟着走）、`ANCHOR`（Toast 七个位置、
  状态气泡的屏幕锚点，离边 `margin`）。纯逻辑在 `placeOverlay`，公式逐条对位 Windows 各窗口的本地定位。
  状态气泡的**窗口锚点**降级为同位置的屏幕锚点（服务端编码前就降级；Linux 拿不到前台窗口边框）。
- **计时归 addon**（同 macOS `.app`）：帧带 `durationMs`，>0 时本端计时器到点摘窗；0 = 常驻到
  下一帧 / 隐藏帧（常驻型状态气泡、tooltip）。服务端 forwarder 线程阻塞在命令通道上，没有到期唤醒。
- **隐藏**：服务端对某层推 `FLAG_VISIBLE` 缺席的帧；候选窗隐藏时 tooltip 两端都会藏（服务推隐藏帧，
  `hide()` 也兜底藏）。`SERVICE_READY` 时关掉三层 SHM 映射并藏掉全部浮层。

## 自绘菜单（X11）

设计与关闭路径全表见 `docs/design/linux-port.md` §5c。本端要点：

- **每级一窗**：`wind-menu-0`…`wind-menu-5`（WM_CLASS 实例名，e2e 按它找窗），`OVERLAY_PLACE_EXACT`
  原样摆放、不夹回。只有**新映射**的一级才提到最上；已显示的级只换像素 / 位置、不重排 z 序——
  否则父菜单一次高亮重绘就会盖住翻到左侧的子菜单（Windows `plan_render` 的同一教训）。
- **抓指针**：第 0 级出现时 `xcb_grab_pointer(owner_events=1)`，全部收起时放开。抓取期间本连接
  上的全部指针事件（含候选窗上的）只进菜单：移动按批合并、按下立即报（先把积着的移动报掉保序），
  松开 / 滚轮 / 离开不报。别的客户端还抓着（刚点完托盘菜单）时每 50ms 重试、最多 1 秒；抓不住菜单
  照用，只是点菜单外看不见。**不抓键盘**：键要照常经宿主到服务端的 `forward_menu_key`。
- **本端收菜单并报 `menu.dismiss`**：`deactivate`（失焦 / 换 IC / 切走输入法）、空闲超时、抓取上限、
  `SERVICE_READY`（补报）。push 断线（服务没了）、菜单指针事件报不上去 / 服务卡死（熔断）只收不报。
  服务端推来的隐藏帧不报（是它关的）。
- **抓着指针就必须放得掉**（抓取期间整个桌面的鼠标都归我们）：
  - 指针事件上报失败即本端收菜单、放开指针。服务卡死（SIGSTOP）时 push 仍在线，「push 断线收菜单」
    那条路不触发；e2e `menu_stall_cases` 验动一下鼠标约 2 秒（一次超时）放开。
  - **移动不续空闲计时**：只有按下、按键、新菜单帧才续（移动带来的高亮重绘是新帧，服务活着才有）。
    曾经每批移动都续，服务卡死时鼠标一动菜单就永远收不掉。
  - **绝对上限**：从第一级出现算起 120 秒（`WIND_MENU_MAX_GRAB_MS`，e2e 调成 12 秒），不续，到点收并报
    `menu.dismiss`（`max_grab`）——任何续计时的漏洞都不该让抓取变成永久。
  - 合成器起停时第 0 级菜单换 visual 重建，旧窗口连同抓取一起没了（X 在抓取窗口不可见时自动放掉）：
    重建后重新抓（否则点菜单外再也看不见）。e2e 起停 xcompmgr 验。
- **回调里不销毁正在执行的计时器 / IO 源**：查指针（`pointerPosition`）与工作区只读现有连接、不重连；
  重连（`ensureConnection` → `dropConnection`）只在贴帧路径上发生。曾经菜单空闲计时器到点 → 收菜单 →
  按指针重定悬停 → 查指针时发现 X 连接已坏就重连 → `dropConnection` 销毁了正在执行的那个计时器
  （xkill / Xwayland 重启后 60 秒触发）。计时器在 `dropMenu` / `dropConnection` 里一律只停不毁。
  e2e `x_conn_lost_cases`（xkill 菜单窗口的连接、等空闲超时）验 fcitx5 存活并重连。本机 SDK 的
  Fcitx5 5.1.7 上，计时器回调里销毁自身经 ASan 实测不报错（最小复现与 ASan 版 addon 跑 e2e 均无报告），
  别的 Fcitx5 版本未验——修法不依赖哪个版本恰好容忍。
- **入口**：候选窗右键 → `menu.open`（带工作区）；主菜单另走两个
  状态区动作（不论组字与否）（`activate` 时挂进 `StatusGroup::InputMethod`）：`windinput-settings`（「清风输入法设置」，
  直接启动设置程序）与 `windinput-menu`（「清风输入法菜单」，`openMainMenuAtPointer`：主菜单弹在
  指针处——X11 按 `pointerPosition()`，Wayland 走屏幕菜单、落点等指针进入底板时取），取舍见设计文档 §5c。
- e2e 的菜单用例都经状态区「清风输入法菜单」开主菜单（`open_main_menu`，指针先挪到固定点）；菜单用例把空闲超时
  调到 5 秒以覆盖超时那条路。客户端扮演 kimpanel（持有 `org.kde.impanel`、发 `TriggerProperty`
  点状态区动作，听 `org.kde.kimpanel.inputmethod` 的 `UpdateProperty` 取当前输入法图标），故
  fcitx5 要 `--enable` 上 `kimpanel`。

## 命令直通车按键合成

服务端（`ext_presenter`）不自己合成按键：`key.tap / seq / hold / release` 经
`handle_cmdbar_macos.rs::CoordKeys` 编成 `CMD_KEY_TAP / SEQ / HOLD / RELEASE` 推给宿主，载荷是
`split_combo` 归一过的（小写键名 + `mods ⊆ {ctrl, shift, alt, win}`）。addon 在 `onPushFrame` 接住，
经 **Fcitx5 `InputContext::forwardKey`** 交给焦点 IC 的应用（`key.type` 另走上屏通道，不在此列）。

**为什么是 forwardKey，不是 XTest**（XTest 未做，理由与限制都在这里）：

| | forwardKey（采用） | XTest `xcb_test_fake_input`（未做） |
|---|---|---|
| 前端 | DBus / XIM / GTK·Qt 模块 / Wayland 都有 | 只对 X11 / XWayland 窗口有效 |
| 回环 | 转发的键**不进**输入法引擎，天然不回流 | 合成键经 X 服务器回到 Fcitx5 再进我们的 `keyEvent`，得记「预期回流」防组字 |
| 可测 | DBus 客户端收 `ForwardKey` 信号逐个断言 | 要在 Xvfb 里另写 X 客户端收事件 |
| 作用范围 | **只到当前输入上下文所在的应用** | 系统级：窗口管理器 / 全局快捷键也看得见 |

实测：DBus 前端的 `ForwardKey(sym, state, isRelease)` 序列见 e2e；XIM（Xvfb 里的 Xlib 小客户端，
`XMODIFIERS=@im=fcitx`、fcitx5 开 `xim`）收到的是带真实键码的 KeyPress / KeyRelease——键码由
Fcitx5 按 keysym 在当前布局里反查（Ctrl_L=37、c=54、Home=110、End=115、BackSpace=22），state 位与
下表一致，合成的 `a` 作为文字到达应用，addon 日志里没有它的按键记录（不回流）。XTest 只在
forwardKey 对某类应用确实无效时才值得做，目前没有这样的证据。

- **键名**（`keyNameToKeysym`）：Windows `key_inject::parse_key` 的全部名字与别名（`return` / `esc` /
  `bksp` / `del` / `ins` / `pgup` / `pgdn`、KEY_TABLE 符号键及其字符写法）∪ macOS `keyCodeMap`
  （`capslock`、修饰键本身作主键）∪ `f1`…`f24` ∪ `vk:NN`（按 **Windows VK** 反查，十六进制、`0x`
  可省，同 Windows）。`keysynth_test` 直接读 `KeySynthesizer.swift` / `key_inject.rs` / `keymap.rs`
  源码逐个核对，两侧加键名这里当场红。字母 / 符号取无 Shift 的基础层 keysym，Shift 放进 state
  （同 `xdotool key ctrl+shift+c`）；键码填 0 由各前端反查。
- **顺序与 state**（X 语义：state 是事件之前的修饰态）：`tap Ctrl+C` = Ctrl↓(0)、c↓(Ctrl)、c↑(Ctrl)、
  Ctrl↑(Ctrl)；多个修饰键按给出顺序按下、逆序抬起（同 Windows / macOS）。`seq` 逐个 tap。
- **拒绝**：未知主键或未知修饰名整条丢弃并 WARN（同 Windows `parse_combo`，`Hyper+C` 不退化成裸
  `c`；macOS 是跳过该键）；`seq` 里有一个不认识就整条不做（Windows 会做完前面的再报错）。
- **限流**（`KeySynth.h`）：单条 `seq` 至多 **64** 个组合，超了整条丢弃（不截断）；tap / seq / hold
  合成的事件每 **1 秒至多 1024** 个，超额的帧整条丢弃；补发的抬起不受限。
- **key.hold 的卡键保护**：按住的组合记在 `KeyHoldTracker`（同时至多 8 个，同一组合不重复按），
  抬起发回**当初收到按下的 IC**（不是此刻的焦点）。以下任一发生即补发全部抬起（逆序）：
  `deactivate`（失焦 / 切走输入法）、`activate` 发现上一个 IC 还挂着（焦点重叠）、帧到达时焦点已
  不是按下的那个 IC、宿主 `reset`、`SERVICE_READY`、push 断线、addon 析构；另有**最长保持时间**
  兜底（默认 10 秒，`WIND_KEY_HOLD_TIMEOUT_MS` 可覆盖，给 e2e 用），到点自动抬起。按住修饰键时
  tap / seq 的 state 叠上它（`hold Shift` 再 `tap End` = Shift+End），已按住的修饰键不再重按。
  `release` 没按住的组合什么也不发（不凭空造抬起）。
- **与用户真实按键的交错**：一帧（一条 seq）在主线程一次回调里全部 forwardKey 完，中间插不进
  用户的键；但 `$CC` 里多个 key.* 是多帧，帧与帧之间用户的键可能先到。按住只是「应用收到了
  按下」：X 服务器的真实修饰态没变，用户此时实打的键不带那个修饰位。
- **落点**：`focusedIC()`，没有就丢弃（debug 日志），同 `CMD_KEY_TYPE`。
- e2e：`keysynth_cases` / `keysynth_restart_cases`（测试短语由 `e2e.sh` 追加进隔离目录的
  `system.phrases.toml`）。push 断线与 `SERVICE_READY` 两条都会补发，服务重启时先到的是断线那条；
  `SERVICE_READY` 那条与 addon 析构那条没有单独的 e2e（析构时应用那头已看不到信号）。
- 已知风险：GTK 等宿主在光标被程序挪动时也可能 reset IC——`hold` 之后紧跟的 `tap` 若让应用
  挪了光标，其 reset 会提前抬起按住的键（未在真实 GTK 应用里验）。

## 与 Windows / macOS 的差距

| 能力 | 现状 | 备注 |
|---|---|---|
| 候选窗显示（X11） | 已接 | 见上「候选窗（X11）」 |
| 候选窗显示（Wayland 原生） | 已接（`WaylandPanel`），treeland 真机验过 | 跟着光标走的（候选、tooltip、跟随光标的状态气泡）走 `zwp_input_method_v2` 的 popup surface（KWin 只有 input-method-v1，走 `zwp_input_panel_v1` 的 overlay 面板：KWin 全局只认一个面板、角色拆不掉，故每个显示周期换新 surface，旧的经 `RetiredSurfaces` 延迟销毁，见 `docs/design/linux-kde-input-method-v1.md`），**由合成器按文本光标摆位**（客户端报的 `cursorRect()` 是窗口相对坐标，没法自己摆），合成进同一张位图（tooltip 按服务端落位相对候选窗摆）；按屏幕摆的（Toast、屏幕锚点 / 固定位置的状态气泡）在合成器有 wlr-layer-shell 时各开一个 OVERLAY 层 surface，按锚点 + 边距摆，在任务栏之上，且不论焦点在哪种应用都走这条（XWayland 窗口会被 treeland 压在任务栏下）。气泡右键菜单开在一张铺满输出的透明 layer 底板上（各级菜单是子 surface），坐标即输出坐标，菜单外的点击也由底板接住（代替抓指针；请求 2s 无菜单 / 60s 无点击 / 120s 上限三道兜底收起）。layer surface 收起时连角色对象一起销毁、下次重建（只摘 buffer 时 treeland 不再发 configure）；所有我们的 wl_surface 只建不毁并挂全零 user_data 占位（classicui 的指针回调会读它）。鼠标：点选、翻页、滚轮、悬停、右键菜单；layer 上的气泡可拖（relative-pointer 取位移，surface 自己在动，按 surface 坐标算会乱跳）、Toast 可点击关闭；popup 里只有候选与气泡收鼠标。跟随光标的气泡不能拖；菜单靠近屏幕下 / 右缘可能被合成器裁掉；锚在底 / 右边的 layer 气泡开始拖时可能跳一个任务栏厚度（合成器不告诉独占区）；候选窗落在任务栏处会被盖住（popup 层级归合成器）；GNOME 不支持 input-method-v2，应用走 X11 / XWayland 那条路（仅当 `XDG_CURRENT_DESKTOP` 分段含 `GNOME` 时按 §7.10 折光标，Shell 扩展见 `gnome-shell-extension/README.md`；非 GNOME 保持直通）；KDE Plasma（input-method v1）Kubuntu 26.04 / KWin 6.6.6 真机验过；多显示器 / 混合缩放只近似；arm64 上未实机验 |
| 多显示器 | 未做 | 工作区取整个根窗口，不按 RandR 显示器切分：跨屏边缘的翻转/钳制按整块虚拟屏算 |
| HiDPI（帧 `scale>1`） | 按物理像素原样贴 | X11 没有逻辑坐标，服务端在 Linux 上目前恒发 scale=1 |
| 候选窗拖动 / 固定位置回报（`pos.candidate` / `pos.candidate.query`） | 未接 | 服务端问位置时不答 = 保留旧值（macOS 不可见时也不答，语义安全） |
| 候选右键菜单 / 功能主菜单 | 已接（X11） | 见上「自绘菜单」。缺：Wayland；多显示器（工作区取整块根窗口）；点菜单外那一下被菜单吃掉（同 X11 原生菜单，Windows 会透传）；菜单开着时在候选上再右键只关菜单、不接着弹新菜单（Windows 会） |
| tooltip / 状态气泡 / toast | 已接（X11），含鼠标交互 | 见上「光栅浮层」：悬停保持、气泡拖动与右键菜单、提示右键菜单（复制 / 上屏 / 截图）、Toast 点击关闭，e2e 逐项覆盖。缺：Wayland；多显示器（拖动夹回按整块根窗口）；「截图所有窗口到文件」（`TakeScreenshot` 的 `shot.panel`）仍只截候选窗（气泡 / 提示菜单里的「截图此窗口」已可用）；提示菜单开着时点在提示上只关菜单、不接着弹新菜单（Windows 会重新请求） |
| 多显示器下的浮层锚点 | 未做 | 工作区取整个根窗口（同候选窗）：Toast / 锚点气泡落在整块虚拟屏的角上，而不是光标所在显示器 |
| 命令直通车按键合成（`CMD_KEY_TAP/SEQ/HOLD/RELEASE`） | 已接（`forwardKey`） | 见上「命令直通车按键合成」。与 Windows（`SendInput`，系统级）/ macOS（CGEvent 发到会话）不同：**只打进当前输入上下文所在的应用**，窗口管理器级的全局快捷键（如 Super+E）不会触发；`hold` 不改变 X 服务器的真实修饰态；`capslock` 只是把键交给应用，不切换大写锁定。e2e（DBus 前端）逐条覆盖，XIM 手工探针验过；GTK / Qt 模块、Wayland 前端、真实应用里的行为未验 |
| 出厂命令短语（`system.phrases.toml` 的 `$CC`） | 按平台取舍 | `cono` / `coca` 在 Linux 上是 `proc.any` 多候选（装了哪个编辑器 / 计算器就开哪个），`cohm` 走 `xdg-open`；`codl`（删行，`key.seq("Home", "Shift+End", "Backspace")`）经上一行的按键合成生效，e2e 断言了它的 ForwardKey 序列。审计清单与取舍见设计文档 §5e。e2e 仅在 `linux-host` 形态下验证（假程序放进 PATH）；各真实桌面上的程序名没有逐个验过 |
| 模式指示 | 托盘 / 面板图标（`subModeIcon`）按模式主字运行时渲染，同 Windows 语言栏：中文为方案标签、英文「英」、大写锁定「A」，自定义方案标签 / `[ui.labels]` 同样上图 | 见设计文档 §5d（含 Windows 状态对照表、宿主图标缓存）。e2e 经 kimpanel 验了图标名与标签随 Shift / CapsLock / 切方案 / 菜单切换、换焦点不回退，并验文件在用户图标目录、是合法 PNG；notificationitem（SNI）取的是同一个值但未单独验，真机托盘（GNOME AppIndicator、KDE、Deepin dde-dock）的实际显示与图标缓存时序未验。缺：角标（全角 / 标点）；密码框里不换「英」；首次安装时若用户图标目录此前不存在，托盘要重启 Fcitx5 后才看得见运行时图标（之前显示种子） |
| 系统输入法配置里的「配置」按钮 | ExternalOption → 设置程序 | e2e 验了 `Controller1.GetConfig` 的描述与命令可启动；fcitx5-configtool 5.1.6+ 直接启动，22.04（5.0.x）显示一页一个按钮；真机点按钮与 Deepin 配置界面未验 |
| 非嵌入模式的占位组合 | 不写进应用（addon 过滤） | 见设计文档 §5d |
| 工具栏 / 软键盘 | 已接（0.127，X11 与 Wayland） | addon 自绘、服务端出像素，见 [linux-toolbar-softkeyboard.md](../docs/design/linux-toolbar-softkeyboard.md)；已知限制见 linux-port.md |
| 输入诊断 HUD | 不做 | 同 macOS；主菜单里按平台摘掉 |
| 截图所有窗口到文件（主菜单「高级」） | 已接 | 候选窗与各浮层的像素都在服务进程，就地编码存盘（`manager_macos.rs` 的 `TakeScreenshot`，macOS 同一份）；不含菜单。此前因流程按「像素在宿主」写（`shot.panel`）而摘掉，现已不需要宿主回应 |
| 按应用独立配置（compat） | 机制可用，真机验过（2026-10-09：两个应用分别默认中文 / 英文，切换焦点时正确切换）；**打包时不带 Windows 内置规则**（`scripts/lib/gen-compat.sh linux` 生成「字段说明 + 零规则」的 Linux 版系统层，各平台兼容策略不共用） | 服务按 `FOCUS_GAINED` 的 bundleId（= `InputContext::program()`）匹配规则，同 macOS；设置端的应用兼容性窗口保留 |
| 打开设置（`settings.open` 扩展信封） | 已接 | `WindEngine::onExt` → `spawnDetached`（与拉起服务同一个：双 fork 不留僵尸、新会话、继承 fcitx5 的会话环境，但不继承它的 fd 与信号处置；`fcitx::startProcess` 只做前两样）。设置程序已开着时由它自己的单实例转发参数；但**转来的切页要等设置窗口下一次输入事件才显示**（windui Linux 后端，冷启动深链正常） |
| 系统关联（`windinput://` 协议、`.wpkg` / `.wtheme`） | 已接（deb 声明 + 设置程序可按用户注册） | deb 装 `scripts/linux/pkg/` 下两个 `.desktop` 与 `windinput.xml`，类型图标在 `data/icons/hicolor/*/mimetypes/`（`scripts/linux/gen-mime-icons.py`）；便携 / tarball 在设置「高级 → 系统集成」按用户注册（写 `$XDG_DATA_HOME`，语义见 wind-setting README「系统关联」）。`scripts/linux/e2e-assoc.sh` 覆盖类型识别、默认处理者、`xdg-open` / `gio open` 拉起设置程序进导入确认、首实例已开时转参、注册/取消一圈。未验：真桌面文件管理器双击（Nautilus / Dolphin / Deepin）、KDE 的 ktraderclient 查询分支、snap / flatpak 浏览器点链接；用户级注册不装图标 |
| 全局热键 | 不做（确定） | 设置端藏掉热键对话框的「全局」勾选；热键只在输入法激活、有焦点时经按键通路生效 |
| Shift 单击切中英 | 已接（addon 接管 Fcitx5 的 `AltTriggerKeys`） | `claimShiftTap`，设计文档 §8；用户清空 `AltTriggerKeys` 或当前不是清风时不插手 |
| 「Shift+鼠标拖选不算单击」 | 缺 | Windows 靠 ToggleTapPolicy 的四个鼠标信号；Fcitx5 引擎收不到鼠标事件 |
| CapsLock 状态通知（`VK_CAPITAL` keyup） | 已发 | 松开时发，服务端同步镜像、弹状态气泡、托盘换「A」；另由每键 `toggles` 校准 |
| 英文输入统计（`CMD_INPUT_STATS`） | 不发 | 同 macOS |
| 小键盘 / 符号键映射 | keysym 走 US 布局反查 | 非 US 布局的 Shift 符号键（如德语 `§`）没有 VK，透传给宿主 |
| 彩色 emoji | 已接（服务端文字后端） | CBDT（Noto Color Emoji）、COLR v0/v1、OpenType-SVG 实测可画；肤色 / ZWJ / 国旗 / 键帽按字体 GSUB 合成一个字形。依赖系统装彩色 emoji 字体（`.deb` Recommends `fonts-noto-color-emoji`），没有时画单色字形或方框。sbix 未实测、COLR v1 扫掠渐变降级为纯色，见 `wind-ui/src/text/linux/mod.rs` 模块头 |
| 非 X11 坐标（Wayland） | 已绕开 | 不用坐标：popup surface 交给合成器摆位。XWayland 应用仍走 X11 窗口 |
| 界面缩放（DPI） | 已接 | Wayland 取 wl_output / xdg-output（分数缩放经 wp_viewporter），X11 取 `Xft.dpi`（回退 `GDK_SCALE`）；经 `host.display` 扩展信封上报，服务端 `wind_ui::dpi::scale_for_point` 统一按它光栅化 |

## 协议同步铁律

改 cmd id 或帧布局：`protocol.rs` + `codec.rs`（SSOT）→ `wind_tsf/include/BinaryProtocol.h` →
`wind_macos/…/{ProtocolTypes,BinaryCodec}.swift` → **本目录的 `ExtProtocol.h` 与 `Codec.cpp`**。
`protocol_sync_test` 会对 cmd id 当场报红，但**帧布局**它管不到——改布局要同时改
`tests/codec_test.cpp` 里按 Rust 解码器写死的字节偏移。

新增下行 cmd 必须在 `ResponseRouter::apply` 显式接一臂（default = 消费按键但不出字）；判断接不接
只看**谁产出**，不看名字像不像 Windows 专有（见 wind_macos/AGENTS.md 同名条目）。
