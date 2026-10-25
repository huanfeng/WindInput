# Linux 移植设计

> 状态：已合入 main（2026-10-02，WindInput 与 wind-setting 均已合入），后续在 main 上迭代。目标：在主流 Linux 桌面上「能正常输入」，再逐步补齐体验。

## 1. 架构：复用 macOS 的「薄壳 + 服务进程」

```
 宿主应用 ──(IME 框架)──► Fcitx5 addon (wind_linux/, C++17)
                              │  UDS 请求/响应 + push 推送
                              ▼
                        wind_input 服务 (Rust, --features linux-host)
                              │  tiny-skia 光栅化 → POSIX SHM 帧
                              ▼
                        addon 把帧画成候选窗（X11 先行，Wayland 后补）
```

与 `wind_tsf/`（Windows）、`wind_macos/`（macOS）同构：**引擎、词库、候选逻辑全在 Rust 服务里**，
addon 只做按键转发、上屏/预编辑写回、候选窗呈现。宿主选 **Fcitx5**（中文社区主流）；IBus 之后再议。

不走 Fcitx5 原生候选窗：自定义主题、tooltip、注释上方条等特色都靠自绘，原生窗全丢。

## 2. 精简范围（与 macOS 大体一致，工具栏 / 软键盘除外）

**发布状态（2026-10-09 定）**：Linux 版以当前状态作为测试版随下个版本发布，之后按用户反馈再定增减。

| 项 | 状态 | 理由 |
|---|---|---|
| 工具栏 | 做（原「确定不做」已撤回） | 复查后多数中文输入法都带工具栏；改为与 Windows 一致的悬浮工具栏，见 [linux-toolbar-softkeyboard.md](linux-toolbar-softkeyboard.md) |
| 全局热键 | **确定不做** | Wayland 无标准方案；热键只在输入法激活、有焦点时经按键通路生效 |
| 软键盘 | 做（原「暂缓」已撤回） | 与工具栏同一套可交互浮层，见 [linux-toolbar-softkeyboard.md](linux-toolbar-softkeyboard.md) |
| 输入诊断 HUD | 不做 | 依赖 Windows 专有诊断数据 |
| 按应用独立配置（compat） | **提供**（最初列为不做，后改为保留） | 服务按 addon 报的程序名匹配规则；内置兼容规则全是 Windows 宿主修正，Linux 包带零规则版。2026-10-09 真机验过：两个应用分别配默认中文 / 默认英文，切换焦点时正确切换 |

## 3. 平台分层：`ext_presenter` 与 `mock_text`

此前协调器/桥接/UI 里 30 余处 `not(target_os = "macos")` 的真实语义是「Windows 桌面形态」，
Linux 会被误归进去（拼 `.exe` 设置路径、弹进程内菜单并吞键——没有窗口时菜单永不关闭，输入被永久卡死）。

`wind-ui` / `wind-bridge` / `wind-coordinator` 各有一份 `build.rs` 产出两个 cfg 别名
（改动须三处同步）：

| 别名 | 为真的条件 | 语义 |
|---|---|---|
| `ext_presenter` | macOS；或 Linux 且开 `linux-host` feature | 呈现层由**外部宿主进程**承担：服务只光栅化，窗口、菜单、设置入口、按键合成归宿主 |
| `mock_text` | 非 Windows、非 macOS，且未开 `linux-host` | 文本后端是 mock（等宽近似）；含文本数值断言的测试据此门控 |

**`linux-host` 默认关。** 关闭时 Linux 与原先完全一致——开发机 `cargo test` 仍走 mock 文本，
布局/golden 测试的数值不变。发行的 Linux 服务用 `cargo build -p wind_service --features linux-host`。

判据：真正的 macOS API（CoreText、Carbon 热键、AppKit 软键盘面板、`activate_ime`）仍留在
`target_os = "macos"`；其余「宿主承担」的语义一律用 `ext_presenter`。

## 4. 端点与目录

- socket：`$WIND_INPUT_RUNTIME_DIR` → `$XDG_RUNTIME_DIR/WindInput[Dev]/{bridge,bridge_push}.sock`
  → `/tmp/wind_input{_dev}-<uid>`。socket 是运行时状态，不落盘。
- **/tmp 兜底要私有**（无 `XDG_RUNTIME_DIR` 时）：`/tmp` 人人可写，别的用户抢先建好这个目录、在里面
  监听 socket，addon 就把全部按键（含密码框）发过去，对方再经推送通道发 `COMMIT_TEXT` / `KEY_*`
  注入文本与按键、或带任意参数拉起设置程序。服务把它建成 0700，并校验「是目录、不是符号链接、属主是
  本用户、组与其他人无权限」，不合格拒绝启动（`endpoint::ensure_runtime_dir`）；addon 连这个目录下
  的 socket 前做同一校验（`privateDirProblem`）。`$XDG_RUNTIME_DIR`（规范要求本用户 0700）与显式覆盖
  不校验目录。**两条通道两端都校验对端 uid**（`SO_PEERCRED`）：addon 只连本用户的服务，服务只服务
  本用户的进程。
- 配置：`~/.config/WindInput[Dev]`（`dirs::config_dir`）；缓存/日志：`~/.local/share/WindInput[Dev]`。
- data：服务可执行文件同目录的 `data/`（`variant::install_root`）；FHS 打包时再议。
- SHM 名（Linux）：`/WindInput[Dev].<uid>` + 层后缀（`_TIP` / `_STS` / `_TST` / `_MN<k>`），带 uid
  免得多用户同机撞名（`/dev/shm` 全系统共用）；最长 28 字节，守 macOS 的 31 字节上限。macOS 仍是
  `/WindInput_SHM[Dev]`。addon 映射前 `fstat` 校验属主与大小（见 `wind_linux/AGENTS.md`「候选窗」）。

## 5. 系统能力（`wind-ui/src/linux_host.rs`）

服务不链接任何桌面库，与 macOS 的 `pbcopy`/`open` 同一路数走外部命令：

| 能力 | Wayland | X11 |
|---|---|---|
| 剪贴板读写 | `wl-copy` / `wl-paste` | `xclip`，其次 `xsel` |
| 图片剪贴板 | `wl-copy --type image/png` | `xclip -t image/png`（`xsel` 不支持） |
| 打开路径/URL | `xdg-open` | `xdg-open` |

缺工具时如实报错，不返回假成功。⚠️ 由 systemd 用户单元拉起的服务可能没有 `WAYLAND_DISPLAY`/`DISPLAY`，
需要 `import-environment` 或由 addon 拉起服务以继承会话环境。

## 5b. 提示类浮层（状态气泡 / Toast / tooltip）

addon 不排字——服务端光栅化（与候选窗同构），像素走各层独立的 SHM 段，`CMD_OVERLAY_FRAME` 只带落位
规则与自动隐藏时长；落位（需要屏幕几何）与计时都在宿主。macOS `.app` 自 2026-10 起也是这条路（早先
是原生 NSPanel 排字），两平台共用 `wind-ui/src/overlay_host.rs` / `menu_host.rs`，见
`docs/design/mac-overlay-raster.md`。细节见 `wind_linux/AGENTS.md`「光栅浮层」。

**鼠标交互对齐 Windows**（`wind_linux/include/OverlayInput.h`）。按「谁知道判据」分两半：

| 交互 | Windows | Linux 落点 |
|---|---|---|
| 悬停提示：指针从候选挪进提示时保持；离开后 280ms 宽限 | `hover_move` / `TooltipMouse` 离开跟踪 | addon：候选悬停变化延后上报、指针进提示就撤掉（服务端的悬停一直指着原候选，提示照画） |
| 悬停提示右键菜单（复制 / 上屏「段」「此行」、复制全部、截图） | `RequestTooltipMenu` → `show_tooltip_menu` | addon 报 `menu.open`（target −3 + 位图内坐标）→ 服务端按最近画的那一帧做命中 → 同一个 `show_tooltip_menu` |
| 状态气泡拖动；松手报位置，固定位置模式落盘 | `StatusTipMouse` + `StatusTipMoved` | addon 拖（隐式抓取）→ `pos.status_tip` → 同一个 `save_status_tip_pos` |
| 状态气泡右键菜单（常驻 / 焦点切换时显示 / 固定位置 / 恢复默认 / 截图） | `RequestStatusMenu` → `show_status_menu` | `menu.open`（target −2）→ 同一个 `show_status_menu`；「固定位置」经 `pos.status_tip.query` 取当前位置 |
| 悬停 / 拖动 / 菜单开着时不自动消失，结束后重新计时 | UI 循环按 `interacting()` 顺延 | addon（计时本来就在 addon） |
| Toast 点击关闭、悬停暂停 | 无（Windows Toast 不收鼠标） | addon 本地（Linux 新增） |
| 「截图此窗口」 | 窗口自己截 | 服务端从该层 SHM 读回、就地存盘进剪贴板（macOS 同） |

不做：「悬停行高亮」——Windows 的提示气泡也没有这一项，没有可对齐的行为。

## 5c. 自绘菜单（主菜单 / 候选右键菜单，M3b）

不用 Fcitx5 / 桌面的菜单：与候选窗、浮层同构，服务进程光栅化、addon 贴图，外观与 Windows 同一份。

- **复用 Windows 的 `popup_menu` 整套**（级联状态机、视图树、主题、定位、命中测试、增量重绘）。
  `wind-ui/src/menu_host.rs` 只换两头：出——各级像素写各自的 SHM 段（`_MN0`…），推
  `CMD_OVERLAY_FRAME`（kind = `OVERLAY_KIND_MENU + 级`，落位 `EXACT`）；进——addon 在菜单打开期间
  **抓住指针**（X11 pointer grab，`owner_events=1`），把移动 / 按下原样报上来（`CMD_MENU_POINTER`
  0x021B，16 字节），命中测试在服务端做。键盘仍走 `forward_menu_key`（不抓键盘）。
- **定位在服务端**：翻转、子菜单左右展开、钳制全是 `popup_menu` 的原逻辑（有单测），工作区由
  addon 随「打开菜单」请求（扩展信封 `menu.open`）报上来；addon 按 `EXACT` 原样摆放、不夹回——
  两边各算各的，命中就会错位。工作区取整块根窗口（不分显示器，同候选窗）。
- **入口**：候选窗右键（命中候选 → 候选菜单，空白 / 翻页按钮 → 主菜单，同 Windows）；Fcitx5 状态区
  （托盘菜单与 kimpanel 面板都会列出）的两项：「清风输入法设置」（addon 直接启动设置程序，与 `settings.open`
  同一条 `launchSettings`，不绕服务端）与「清风输入法菜单」（主菜单弹在**指针处**，即刚点托盘的
  地方，对位 Windows 托盘 / 工具栏的右键菜单）。沿革：最初只有「清风输入法菜单」、主菜单弹在文本
  光标下方，离托盘很远，真机试用觉得怪，2026-09-30 改成设置入口；2026-10-12 用户要求加回菜单入口，
  改为按指针落位、与设置并存（Wayland 下落点取指针进入菜单底板时的输出坐标）。同日去掉候选菜单末尾的「更多…」（原是
  组字中通往主菜单的路，有了状态区入口就多余；且点它时屏幕菜单已先关，服务端随后下发的主菜单
  在 Wayland 下落进候选窗那个 popup surface，把候选窗挤走）。状态区的显示由
  Fcitx5 的 UI 模块决定，我们只挂动作。
- **按平台摘掉的主菜单项**（`build_main_menu_items`）：输入诊断 HUD（同 macOS）。沿革：「截图所有
  窗口到文件」曾因依赖宿主回应 `shot.panel` 而摘掉，改为服务进程就地存盘后恢复；工具栏开关与软键盘
  最初也摘掉（当时工具栏不显示、`open_softkeyboard` 在 Linux 直接拒绝），随
  [linux-toolbar-softkeyboard.md](linux-toolbar-softkeyboard.md) 恢复，文案用 Windows 的「显示工具栏」
  （macOS 的「显示状态图标」不出现）。
- **关闭路径**：Esc / 点选 / 右键 / 点菜单外（抓指针看得见）/ 其它键（关并吞掉）走服务端
  `popup_menu`；失焦与切换输入上下文（addon `deactivate` 无条件收并报 `menu.dismiss`，服务端
  FocusLost 另有一条带 250ms 守卫的）；候选被清空 / 组合结束（候选右键菜单随候选收起，
  `notify_ui_hide`；组合被终止等直接复位的路径补发 `HideMenu`）；服务被杀（push 断线，addon 自收）、
  服务重启（`SERVICE_READY`，addon 自收并补报）；addon 断线（UDS 连接断开，服务端复位）；空闲超时
  （addon 60 秒无操作自收并报；服务端另有同时长的兜底：超时后的第一个键照常处理、不被吞。指针移动
  不续这个计时）；抓取上限（从打开算起 120 秒，不续）；服务卡死（进程在、不响应：指针事件报不上去 /
  请求超时熔断，addon 自收、放开指针，恢复后以 `COMPOSITION_TERMINATED` 复位服务端的 menu_open）。
  两端认识错开时自愈：菜单不在屏上却收到菜单键 / 指针事件，UI 回送 `MenuClose`；菜单已关却收到
  指针事件，服务端让 UI 收菜单。e2e 逐条覆盖，每条之后立即打字验证。

## 5d. 与 Fcitx5 自身 UI 的衔接

- **输入法图标**：条目 `Icon=windinput`（`wind_linux/data/icons/hicolor`，16…256 各尺寸，出自
  wind-setting 的 `wind_setting.ico`，`scripts/linux/gen-icons.py` 拆出）。**托盘 / 面板图标随模式
  切换**：引擎实现 `subModeIconImpl` / `subModeLabelImpl`（`HostUi.h` 的 `ModeIndicator`）。Fcitx5 的
  classicui 托盘、notificationitem（StatusNotifierItem，Deepin / KDE / GNOME 的 AppIndicator 扩展）、
  kimpanel 都经 `Instance::inputMethodIcon` 取这个值。模式来源是服务端的四种帧：焦点进入的
  `MODE_PUSH`、按键响应的 `STATUS_UPDATE`、push 通道的 `STATE_PUSH`（菜单等别处切换）、带
  `MODE_CHANGED` 的上屏；变了就 `updateUserInterface(StatusArea)` 让各 UI 模块重取。不用
  `CMD_MODE_STATUS`：那条由工具栏可见性驱动，而 Linux 的工具栏由服务端自己画，addon 用不上它。

- **系统输入法配置里的「配置」**：输入法与 addon 都是 `Configurable=True`，addon 的配置只有一项
  `fcitx::ExternalOption`（指向设置程序，`WIND_INPUT_SETTING` 可覆盖）。依据：fcitx5-mozc /
  fcitx5-anthy 的包里**没有**静态 `configdesc/*.desc`，Fcitx5 5 的配置描述是 addon 运行时
  `getConfig()` 给的，外部工具就是 `Type=External` + `External=<命令>`。fcitx5-configtool 5.1.6 起
  「整页只有一个 External 项」时直接启动它；更早的版本（Ubuntu 22.04 的 5.0.x）显示一页、页上一个
  启动按钮。Deepin 自带的输入法配置界面是否认这套，未验证。
- **非嵌入模式的占位组合**：服务端在编码显示于候选窗（`preedit_display` ≠ `app_inline`）以及联想态、
  加词等模式下，给宿主一段单个空格的占位组合——Windows 的 TSF 要有 composition 才取得到光标坐标。
  Fcitx5 直接给 `cursorRect`，不需要它，而它在应用里是真实的一格空白、把光标推走。**在 addon 过滤**
  （`clientPreeditText`：组合串恰为该常量时应用内预编辑写空），不在服务端门控：① addon 仍需把它记成
  「有组合」——失焦、宿主重置要据此收尾并报 `COMPOSITION_TERMINATED`，服务端若改发空组合，addon 会
  当成组合结束；② 占位的来源不止 `with_composition_placeholder` 一处（联想态 `ASSOC_COMPOSITION`、
  加词、临时模式各自直接发），服务端要逐处加 cfg，addon 一处全收；③ Windows / macOS 零改动。
  常量与服务端同值由 `host_ui_test` 读 `handler.rs` 对账。

### Windows 语言栏图标：状态 → 主字 / 角标（对照基准）

出处：主字 `Coordinator::mode_icon_label`（唯一产地）、图标规格 `coordinator/langbar_icon.rs`
`publish_langbar_icon`、渲染 `wind-ui/src/langbar_icon.rs`、配置 `docs/design/mode-icon-label-config.md`。

| 状态 | 主字 | 主字色格（`TextColors`） | 角标（`[ui.langbar] badge`，出厂 `none` = 都不画） |
|---|---|---|---|
| 有效中文（`chinese_mode && !caps_lock`） | 方案 `[schema] icon_label`：全拼「拼」、五笔「五」、笔画「笔」、双拼「双」、五笔拼音「中」、英文方案「英」；未配 →「中」 | 中文格 | 右下：中文标点 / 英文标点；右上：全角 |
| 英文（`!chinese_mode`，大写锁定关） | `[ui.labels] english`，出厂「英」 | 英文格 | 右上：全角（英文态不画标点角标） |
| 大写锁定（**无论中英**） | `[ui.labels] caps_lock`，出厂「A」 | 英文格 | 同英文 |
| 不可输入（密码框 / 无编辑上下文，`InputBlock::shows_english`） | 英文标签（只覆盖图标，不动 `icon_label`） | 英文格 | 不画 |
| 线程级 `KEYBOARD_DISABLED` | 同上 | 同上，整体变淡 | 不画 |

主字色出厂中英同色（浅色任务栏黑、深色白），区分中英靠字；标签宽度上限 2（汉字记 2，双汉字截首字）。

### Linux 托盘图标：按主字运行时渲染（同 Windows）

**主路径**：服务端按（状态档, 主字）渲染 PNG，写进用户图标目录，addon 把图标名交给 Fcitx5。
主字就是上表那一份（`mode_icon_label`），所以切方案、第三方方案标签、改 `[ui.labels]` 都会上托盘。

| 状态档 | 底色 | 主字 | 图标名 |
|---|---|---|---|
| 有效中文 | 蓝 `#2F86E6` | 方案标签（未配为「中」；英文方案在中文模式下是**蓝底**「英」，同 Windows 取中文色格） | `windinput-lbl-zh-<主字 UTF-8 十六进制>` |
| 英文 | 灰 `#5E6B78` | `[ui.labels] english` | `windinput-lbl-en-…` |
| 大写锁定（无论中英） | 橙 `#E08A1E` | `[ui.labels] caps_lock` | `windinput-lbl-caps-…` |

- **落点**：`$XDG_DATA_HOME/icons/hicolor/<N>x<N>/apps/`（缺省 `~/.local/share`；N = 16/22/24/32/48/64）。
  XDG 图标主题规范的用户基目录，用户目录下不必有 `index.theme`（Fcitx5 / GTK / Qt 都合并各基目录的
  hicolor，子目录表取系统那份）。名字用十六进制编码主字：图标名要进 kimpanel 以冒号分段的属性串、
  进文件名，只用 ASCII 最稳。编码规则两侧各一份（`wind_ui::tray_icon::icon_name` /
  `ModeIndicator::dynamicIconName`），两边单测钉同一组样例（addon 侧读 `tray_icon.rs` 源码核对）。
  dev 版前缀 `windinput-lbl-dev-`（正式版保持 `windinput-lbl-`，已装机的文件照认）：两个变体共用这个
  目录，名字不分时启动清理会互删对方的图标。主字超过 32 字节（只有手配的超长 `[ui.labels]`）改为
  `h` + FNV-1a 64 位散列，文件名不超过 255 字节。
- **时序由数据依赖保证**：`Coordinator::build_status` 返回之前确保本状态用得到的图标已在盘上
  （`coordinator/tray_icon.rs`，`not(test)`），任何带新标签的状态帧到 addon 时文件必然已写完。
  每次一并备好中文 / 英文 / 大写三组：addon 本端判定大写锁定翻转时不等服务端的帧。
  写入原子（同目录临时文件 + rename），按尺寸从小到大、64 最后；addon 以 64 那张在不在判断「这组写完」，
  不在就用随包**种子** `windinput-zh` / `-en` / `-caps`（服务没起来、目录不可写、缺字体）。
- **协议不变**：标签本来就在 `STATUS_UPDATE` / `STATE_PUSH` / `ACTIVATION_STATUS_PUSH` 的尾部
  （Windows 的 `_inputTypeLabel` 用的就是它），addon 以前没读。没有新增 cmd、没有扩展信封，
  Windows / macOS 帧格式一字不变。
- **文字标签**：`subModeLabelImpl` 返回同一个主字。Fcitx5 的 classicui / notificationitem 有
  `PreferTextIcon`（用户在 Fcitx5 配置里开，我们控制不了），开了的用户看到的是 Fcitx5 按这个字画的
  文字图标，同样正确。
- **缓存与清理**：同一（状态, 主字）只画一次（进程内集合 + 磁盘上已齐全就跳过；进程内命中时仍补查
  64 那张还在不在，被别人删了就重画）。服务第一次构建状态时同步备好本状态的三组，**其余方案在后台
  线程画**（每组拿一次锁），画完删掉本变体名下不在其中、也不是本进程备好过的旧图标（别的文件、另一
  个变体的一概不碰）；运行中新出现的标签只增不删、下次启动收。上限是「可用方案数 + 2」组，不会无限
  增长。首轮预渲染的耗时记在服务日志「托盘图标已就绪」一行（`elapsed_ms`；e2e 两个方案 + 英文 / 大写
  四组，debug 构建实测 204ms）。挪到后台的理由：方案多、字体缓存冷时整轮同步做可能逼近 addon 每个
  响应 2 秒的超时（超时即熔断 3 秒）。`build_status` 在有效中文态下直接复用算好的主字，不再为托盘
  图标多读一遍方案文件。
- **字体**：fontconfig 按 `sans-serif` + Bold + 简中排序，取第一款覆盖主字全部字符的字体文件，直接读
  轮廓（`text::linux::font_file_covering`）。机器上没有 Bold 字面（本机只有 Noto Sans CJK Regular）时
  用 Regular，22px 起笔画偏细但清楚；Ubuntu / Deepin 的 `fonts-noto-cjk` 带 Bold。

**宿主的图标主题缓存**（查的上游源码 master 分支，均未在真桌面上实测）：

| 宿主 | 查找方式 | 对「刚写出的新名字」 | 我们的对策 |
|---|---|---|---|
| Fcitx5 classicui（XEmbed 托盘 / 面板）`IconTheme` | 每次按名 `is_regular_file` | 可见；但**构造时不存在的基目录被永久丢弃**，按名结果另有缓存 | addon 加载时先建好整棵目录树；名字与内容一一对应，不复用名字 |
| GTK3/4 `GtkIconTheme`、GNOME Shell `StIconTheme`（AppIndicator 扩展走它） | 加载主题时快照子目录文件名；只看搜索根与主题根两层目录 mtime、至多每 5 秒重扫 | 往已有 `48x48/apps/` 加文件**不触发重扫** | 写完一组就在 `hicolor/` 根建删一个空文件推进 mtime；最坏约 5 秒后可见。启动时预渲染全部方案，日常切方案引用的都是早已存在的文件 |
| KDE Plasma SNI（`KIconLoader`）、kimpanel（`Kirigami.Icon`，走哪条未查实） | 构造时为已存在的子目录建索引，文件实时 `exists`；找不到有 5 秒负缓存 | 子目录先在则可见 | 同上：目录先建、先写文件后给名字 |
| Deepin dde-tray-loader（`QIcon::fromTheme`） | Qt `QIconLoader`：主题目录构造时定；「找不到」按名缓存、不自行重查 | 先写后引用可见；先引用后写会一直找不到 | 先写后给名字（数据依赖保证）；名字只在文件齐全时才给出 |

不走绝对路径：Fcitx5 自己的 `IconTheme` 与 GNOME AppIndicator 放行 `/` 开头的名字，但 kimpanel 的
`Kirigami.Icon` 与 Deepin 的 `QIcon::fromTheme` 对绝对路径的行为没查实；主题名是所有宿主都认的形式。
遗留风险：Fcitx5 / 面板**先于** addon 构建图标主题、而用户目录此前从未存在——首次安装后要重启一次
Fcitx5（或重新登录）托盘才看得见运行时图标，之前显示种子。

**清晰度**（`wind-ui/src/tray_icon.rs`）：本仓 Linux 文本后端没有 hinting，12~13px 字身直接栅格会把
「英」「笔」的笔画间隙糊成灰带（8 倍放大看是一团）。

- 16px 的常用单字（中 英 A 拼 五 笔 双）用手绘点阵，每一笔落整像素；
- 其余按字形轮廓栅格，在 ±½ 像素 × 字号 ±4% 内挑半透明像素最少的一版（粗粒度对格）；16px 再做一次
  对比度拉伸（`(a − 0.3) / 0.4`）——自定义标签「虎」、两字母「En」在 16px 上由灰雾变成实笔画，代价是
  斜笔更锯齿；「虎」这类笔画密的字在 16px 仍难辨，这是 16px 的物理上限；
- 多字符标签按墨迹宽度回缩（同 langbar_icon：判据是实测宽度，不是字符数）。
- 没直接调 `langbar_icon::IconRenderer`：那套是透明底 + 任务栏明暗字色 + Light 字重，托盘要的是
  实心色块上的白字；借的是「按墨迹盒居中」「小字号按发虚与否定字重」（这里取 Bold）两条策略。
- 肉眼验收：`gen_tray_icons --preview <目录>` 出种子与样例主字的 8 倍放大图（`zoom-16/22/24/32.png`）、
  浅 / 深面板 1× 并排（`panel-1x.png`，另附 3 倍放大）；`--preview-from <hicolor 根>` 对服务端实际写出的
  运行时图标出同样的图（真机上验「用那台机器的字体画出来什么样」）。种子由同一个 `render` 画，
  Noto Sans CJK SC Bold，SVG 里是路径。

其余取舍：

- **角标（全角 / 标点）不做。** Windows 出厂 `badge = none`，默认观感就是只有主字；开了角标的用户在
  Linux 上看不到它们。
- **不可输入态（密码框）不换「英」。** 帧里的标签是 `mode_icon_label`，不含 `InputBlock` 覆盖（Windows
  那一层只在语言栏图标发布里做）；密码框里键仍全透传，只是托盘仍显示当前模式。

### 大写锁定的判定

**本端判定 + 松开时报服务端**，两者读的是同一份数据（按键的 Lock 位），不是两个真相源：

- X11 事件的修饰状态是事件**之前**的。Xvfb + xev 实测：开 → 按下 `state=0`、松开 `state=Lock`；
  关 → 按下 `state=Lock`、松开**仍是** `Lock`（XKB 的 LockMods 在松开时才解锁）。所以「松开事件的
  state 含新状态」只对「开」成立，对「关」不成立；`CapsLockTracker` 按「按下时没锁 ⇒ 松开后锁上」推。
  其余键的 Lock 位就是当前锁定态，用来校准（在别的输入法 / 应用里切过大写，回来第一个键就对上）。
- Caps_Lock 的按下不转发、松开发一帧 `VK_CAPITAL` keyup（toggles 带新状态），同 Windows TSF：
  服务端据此同步镜像、按「切英文」语义处理正在打的编码、弹状态气泡，并回 `STATUS_UPDATE`
  （`STATUS_CAPS_LOCK` + caps 标签）。此前 Linux 把 Caps_Lock 的按下当普通键发给服务端（toggles
  是按下前的旧值），服务端什么也不做，大写锁定要等下一个键才被发现。
- 为什么不只靠服务端回传：服务端用每一帧的 toggles **静默**校准镜像（不推送），在别处切过大写
  后托盘会一直停在旧图，直到下一次模式变化；本端判定让图标当场换。
- 焦点进入的 `MODE_PUSH` 只有中英 / 全角 / 标点三位，不带大写锁定——镜像沿用已知值，换 IC 不回退。
- 顺带修了一个真坑：修饰键单击（Shift 切中英）那一帧曾发 `toggles = 0`，服务端据此把大写锁定
  镜像校准成「关」——大写锁定时按 Shift，托盘会从「A」掉回「中」。现在带真实 Lock 位。

## 5e. 随包数据里的平台相关内容（审计）

范围：`data/` 下全部随包文件，查 `proc.run` / `proc.shell` / `open(` / `key.*` / `clip.paste` /
`wind.cli` / `setting.*`、`.exe`、`%APPDATA%` 之类环境变量、反斜杠路径。

| 位置 | 内容 | 归类 | 处理 |
|---|---|---|---|
| `system.phrases.toml` `cono` / `coca` / `cohm` | `notepad.exe` / `calc.exe` / `env("USERPROFILE")` | Windows 专属（已有 `darwin` 对应条目） | 补 `platform = 'linux'` 条目：编辑器、计算器用 `proc.any` 多候选，主目录用 `open(env("HOME"))`（`xdg-open`） |
| `system.phrases.toml` `codl` | `key.seq("Home", "Shift+End", "Backspace")` | 需按平台写 | 补 `platform = 'linux'` 条目（与 Windows 同写法）。经 addon 的按键合成生效（§5f） |
| `system.phrases.toml` 其余 `$CC` | `open(URL)`、`type`、`clip.copy` / `clip.paste`、`ime.*`、`dict.add`、`dict.rev`、`setting.open` | 跨平台 | 不动（`open` → `xdg-open`；`clip.paste` 在 `ext_presenter` 下经上屏通道落文本） |
| `system.quick.toml` / `system.softkeyboard.toml` / `config.toml` / `schemas/shuangpin.schema.toml` | 注释里的 `%APPDATA%\WindInput\…` 路径；`config.toml` 注释里的 `proc.run("charmap.exe")` 等示例 | 仅文档 | 不动：只在注释里，不执行 |
| `compat.toml` | 全部内置规则是 Windows 宿主修正 | Windows 专属 | 已由 `scripts/lib/gen-compat.sh` 在打包时换成零规则版 |
| cmdbar 帮助文本（`funcs/action.rs`） | `proc.run` 示例 `notepad.exe`；`clip.paste` 写着「模拟 Ctrl+V」；`verb` / `show` 已注明「仅 Windows」 | 文案偏 Windows | 不改：示例只是写法说明，Windows 的文案保持不变 |
| cmdbar lint（`lint.rs`） | 只查「路径参数里的控制字符」（反斜杠单写） | 平台中立 | 把 `proc.any` 加进被查的函数 |

**根因不在数据，在过滤。** 短语文件早就用 `platform` 字段按平台写了成对条目，但
`wind-phrase` 的两个加载入口（`load` / `parse_system_entries`）都写死了只收 `"windows"`，
所以 Linux 和 macOS 上显示的都是 Windows 那一份，`darwin` 条目从未生效。现在按编译目标
取当前平台名（`windows` / `darwin` / `linux`，`macos` 作 `darwin` 的别名；其它平台只收
全平台条目）。Windows 上的条目集与改动前逐条一致（`builtin_platform_phrases` 用旧规则对拍）。入库同步按条目哈希判断，过滤改了之后首次启动就会重新同步，旧行随之清掉。

**为什么不在打包时生成变体**（`compat.toml` 用的是那种办法）：`compat.toml` 在别的平台上
一条规则都不保留，生成器只需要截掉规则部分。短语文件不同，绝大多数条目是跨平台的，只有少数几条要按平台
替换；打包时替换就得有第二份「哪条换成什么」的表，改 Windows 版时容易跟着漂移。
字段机制已经存在，改成在源文件里按平台并列写，一眼能对照，也不需要改打包脚本。

`proc.any(a, b, …)`（`wind-cmdbar`）：按顺序尝试启动，第一个成功的即停。「未找到」
（`io::ErrorKind::NotFound`）和「找到了但启动失败」（权限等）都会接着试下一个，全部
失败时两类分开列出名字。「成功」只看 spawn，程序起来后自己退出不算失败。Windows 上启动
要转给 TSF 执行，这边拿不到结果，只会启动第一个，所以 Windows 词条仍用 `proc.run`。
候选表按常见桌面排列：GNOME（`gnome-text-editor` / `gedit`、`gnome-calculator`）、KDE
（`kate` / `kwrite`、`kcalc`）、Deepin、MATE、Xfce（`mousepad`、`galculator`）、Cinnamon
（`xed`）、LXQt / LXDE，最后兜底 `xcalc`。同一台机器上装了多个时取排在前面的，不看当前桌面。

## 5f. 命令直通车按键合成

`key.tap / seq / hold / release` 在 `ext_presenter` 下由服务端编成 `CMD_KEY_*` 下行帧（服务端零改动，
与 macOS 同一条路），addon 用 Fcitx5 `InputContext::forwardKey` 交给焦点 IC 的应用。选它而非 XTest：
跨前端（DBus / XIM / GTK·Qt 模块 / Wayland）、转发的键不再进输入法引擎（不回流、无需防回环）、
DBus 客户端能直接断言 `ForwardKey` 序列；代价是只作用于当前应用，全局快捷键不触发。

与 Windows / macOS 的差异：Windows `SendInput`、macOS CGEvent 都是会话级合成，全局快捷键看得见、
`hold` 改变真实修饰态；Linux 两者都不成立。Wayland 下由 Fcitx5 的 Wayland 前端转发，未验。
键名全集、事件顺序与 state、限流数值、`hold` 的卡键保护（失焦 / 换 IC / reset / 服务重启或断线 /
析构补发抬起，最长保持 10 秒）逐条见 `wind_linux/AGENTS.md`「命令直通车按键合成」。

## 6. 阶段

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | `ext_presenter`/`mock_text` 别名、服务端 `linux-host` 形态、端点路径、系统能力 | 完成 |
| M1a | Linux 真实文字后端（`wind-ui/src/text/linux`，ttf-parser + ab_glyph_rasterizer + dlopen fontconfig）；彩色 emoji（CBDT / COLR v0·v1 / OpenType-SVG，emoji 序列经 rustybuzz 整簇整形），限制见该模块头 | 进行中 |
| M1b | Fcitx5 addon：输入通路（按键/上屏/预编辑/焦点/自愈），DBus 集成测试 | 进行中 |
| M1c | addon 的 X11 候选窗呈现、鼠标回传；状态气泡 / Toast / tooltip 光栅浮层 | 进行中 |
| M2 | Wayland：input popup surface（候选 / tooltip / 气泡 / Toast / 自绘菜单合成进同一 popup）+ 鼠标交互 + DPI 缩放 | 已做（treeland 真机验过；遗留见 `wind_linux/AGENTS.md` 差距表） |
| M3a | 设置端适配：`wind-setting` Linux 原生构建、平台门控、addon 处理 `settings.open`、`.desktop` 入口、随 `.deb` 分发 | 完成（见 §6b） |
| M3b | 自绘菜单（主菜单 / 候选右键菜单），见 §5c | 完成（X11） |

## 6b. 设置端（M3a 结论）

- **能用**：`wind_setting` 在 Linux 原生编译、单测全绿；windui 默认走 X11（Wayland 会话经
  XWayland，`WINDUI_BACKEND=wayland` 才试原生 Wayland），文字经 fontconfig，中文渲染正常。
  控制 socket、用户目录、日志目录与服务同一套规则（`$XDG_RUNTIME_DIR/wind_input[_dev]_ctrl.sock`、
  `~/.config/WindInput[Dev]`），改设置写入用户 `config.toml` 并被服务热加载（实测）。
- **入口**：应用菜单（`windinput-setting.desktop`，兼 `windinput://` 协议）、Fcitx5 状态区「清风输入法设置」
  与系统输入法配置里的「配置」（§5d），以及「打开设置」热键
  （出厂 `Ctrl+Shift+]`）。后者：服务 `open_settings_with` 推扩展信封 `settings.open` → addon
  （`SettingsLauncher` + `spawnDetached`）启动 `/usr/lib/windinput/wind_setting`
  （`WIND_INPUT_SETTING` 可覆盖）；只启动自己的设置程序，信封内容只当参数。e2e 覆盖整条链。
- **系统关联**（`windinput://`、`.wpkg`、`.wtheme`）：deb 装 `.desktop` ×2 + `windinput.xml` + hicolor `mimetypes` 类型图标，开箱即用；高级页「系统集成」按 XDG 现查（`xdg-mime query default` + 读 `.desktop` 的
  `Exec`），系统包提供的注明「在此不能取消」。便携 / tarball 可按用户注册：包里同一份文件以本程序绝对路径写进
  `$XDG_DATA_HOME`（带标记），取消只撤带标记的文件与 `mimeapps.list` 里指向我们的项，别人的关联不动；
  自愈只修指向已消失路径的用户级文件。端到端 `scripts/linux/e2e-assoc.sh`；真桌面文件管理器、KDE 查询分支、
  snap / flatpak 浏览器未验。
- **门控**：清单 `platform` 字段加 `linux`；工具栏 / 软键盘相关项（`keys.toggle_toolbar`、`keys.softkeyboard`、`ui.toolbar.*`，后者 `windows, linux`，仅 `fullscreen_watch` 留给 Windows）已对 Linux 放开，与 macOS 不同；其余在 macOS 精简范围之上，Linux 另藏 `activate_ime`、Dota2 兼容与热键的「全局」勾选。逐项依据见 wind-setting README
  「按平台屏蔽的设置项」。应用兼容性（按应用配置）**保留**：服务按 addon 报的程序名匹配规则，
  与 macOS 同一机制——§2「不做」指的是不为 Linux 维护内置规则，不是机制不可用。
- **托盘 / 常驻（`run_resident`）缺失不影响设置端**：`wind_setting` 不用常驻模式也不用托盘；
  输入法自己的状态指示改由 Fcitx5 托盘图标承担（见 §5d）。
- **已知问题**（windui Linux 后端）：设置程序已开着时，深链转来的切页要等下一次输入事件才显示；
  文件对话框走 xdg-desktop-portal，弹不出时退 zenity、再退 kdialog；都不行时设置端提示安装 zenity 或 kdialog。

## 7. 待验证的风险

1. Wayland input popup 表面是否收得到鼠标事件（决定候选点选/悬停/自绘菜单是否成立），GNOME 与 KDE 可能不同。
2. 自绘菜单在 Wayland 下只能画进候选窗自己的表面；菜单外点击不可观测（X11 靠指针抓取）。
   `menu_open` 期间协调器会吞方向键/回车/Esc——X11 已按 §5c 补齐全部关闭路径并逐条 e2e；
   Wayland 阶段要重新验证每一条（尤其没有抓取时的「点菜单外」只能退回失焦 / 超时）。
   另：抓指针期间点菜单外，那一下被菜单吃掉、不传给下面的应用（同 X11 原生菜单，与 Windows
   「点外面照常响应」不同）。
3. `wind-coordinator` 在 macOS 目标上依赖 C 库，本机无法交叉 check，改动靠 CI 的 macOS job 兜底。
4. **菜单关闭回送没有代际（已知限制，不修）**：UI 收掉一个可见菜单时回送 `MenuClose`，协调器分不出
   是哪个菜单的。候选右键菜单 A 随候选收起（`notify_ui_hide` 发 `HideMenu`、靠回送收口）后、UI 处理
   之前，addon 又报 `menu.open` 弹出 B，则 UI 收 A 的回送会让协调器把 B 也收掉——B 一闪即逝，时间窗是
   一次跨线程往返（毫秒级）。两端认识始终一致（协调器复位时补发 `HideMenu`，不会出现「屏上没菜单、
   键却被吞」），`handle_menu.rs` 的 `stale_menu_close_echo_closes_new_menu_consistently` 钉住这一点。
   不加代际的理由：要改 `wind-ui-types` 里三平台共用的 `HideMenu` / `MenuClose`（Windows 的
   `popup_menu` 也发它），换来的只是一个人手几乎点不进去的时间窗里少闪一次菜单。
5. **连接断开不分是哪条（已知限制）**：`server_unix` 每条请求连接结束都调
   `handle_client_disconnected`，协调器据此把开着的菜单复位，不看断的是哪条连接。addon 目前只有一条
   请求连接，成立；将来若出现第二个请求客户端（探针、并行的 e2e 客户端），它断开会把 addon 那边开着的
   菜单在服务端复位（UI 随之收菜单，两端仍一致，只是菜单被无故收掉）。届时给断线回调带上连接标识。
   同一回调还给工具栏 / 软键盘各补一次指针离开（addon 崩溃时按住的软键盘键当场停连发），同样不分
   连接：请求连接超时熔断时 addon 还在，指针若正按在软键盘上，那次按住会被提前松开；指针若停在工具栏上，
   会被当成已离开，可能开始自动隐藏淡出（下一次移动即恢复）。宿主上报状态的
   完整复位（全屏态等）放在 push 连接重建时（渲染端 `HostReconnected`），不在这里。
6. **字体文件被原地截断会 SIGBUS（已知限制）**：Linux 文本后端把字体文件只读 `mmap` 后常驻
   （`text/linux/store.rs` 的 `map_file`），文件若在服务运行中被**原地截断改写**（而不是包管理器那样
   写新文件再 rename），访问越过新文件尾的页触发 SIGBUS、服务崩溃。包管理器升级字体走 rename，旧映射
   仍指向旧 inode，不受影响。读进内存可免，代价是 CJK 字体常驻数十 MB 私有内存，不做。
7. **服务卡死恢复时的积压帧（已知窗口）**：addon 超时后关掉连接，但已写进 socket 的帧服务恢复后照样
   读到、处理（只是回不了）。熔断到期后 addon 第一次重连即发 `COMPOSITION_TERMINATED` + 重报焦点把
   组字复位；若服务恰好在这次重连**之后**才恢复、才处理积压帧，旧码仍会拼进来——要求服务停住超过
   3 秒且恰在用户按下一个键的那一刻恢复。根治要协调器按 `event_seq` 去重或给连接带代际，不做。
8. **托盘图标首次同步渲染**：本状态的三组图标仍在第一次 `build_status` 里同步画（数据依赖，见 §5d），
   字体缓存冷（装完没跑过 `fc-cache`）时 fontconfig 查字体可能超过 addon 的 2 秒超时——表现为首个键
   透传、熔断 3 秒后自愈，不卡死。
9. **SHM 名被别的用户抢先占用**：名字带 uid 后别人仍可在 `/dev/shm` 建我们的名字（`O_EXCL` 失败），
   服务端建不了段、候选窗不显示；addon 按属主拒收，内容伪造不了。只是拒绝服务，不处理。
10. **GNOME Wayland 会话下候选窗跟不准插入点（Shell 扩展补窗口原点）**：GNOME 没有
   input-method-v2（`Using Wayland native input method protocol: 0`），Wayland 原生应用经
   `dbus` / `ibus` / `fcitx4` 报的 `cursorRect` 往往不是 XWayland 根坐标。分数缩放下还有
   输出缩放 vs Xft 缓冲缩放两套像素。`GetWindows`/`Eval` 仍拒，做准需要 Shell 内的
   `focus_window`。**做法**（`windinput-caret@windinput`）：
   - **只在 GNOME 会话走这条路**。`XDG_CURRENT_DESKTOP` 按冒号分段，有一段与 `GNOME` 全等
     （大小写不敏感，如 `ubuntu:GNOME`）才折算。KDE、treeland 以及没设这个变量的会话保持
     原来的根坐标直通，不乘 `hostScale`、也不叠窗口原点。
   - 仅 `RelativeRect`：扩展 `TranslateRelativeSpot`（`protocol_to_stage_rect` + compositor
     actor）再折到 X11；无该能力位的绝对屏坐标只缩放，不得叠窗口原点。
   - 扩展 `spotScale`（矩形坐标系）：sf≈缓冲缩放 → 用客户端 sf（已预乘）；sf≈1 且 frontend=ibus
     → 用输出缩放（相对矩形常已预乘却未报 scale）；其余（sf≈1 的其它前端，以及 sf>1 但不贴近
     缓冲缩放的逻辑 DIP）主题 scale≈1。纯函数在 `GnomeCaretGeom`，单测覆盖 frontend × scale。
   - 扩展未启用时退回 toolkit 折中——最大化大致可用，浮动窗会偏。问总线的连接会缓存；失败后
     约 3 秒内不再同步重试；同一矩形不重复调用。`hostOutput` 复用 `sendCaretUpdate` 已经成功
     换算的结果。
   - **扩展能装上的 Shell 是 47–50，不是 42，也不是 45/46。** ESM 扩展格式从 Shell 45 起；
     `gi://Mtk` 也从 45 起就有（gnome-shell 45.0 自己的 `windowManager.js` 在 import）。
     但本扩展调用的 `Meta.Window.protocol_to_stage_rect` 从 mutter 47.0 才进入公开头文件，
     46.0–46.7 没有。`metadata.json` 因此只声明 47–50。安装与这段核对写在
     `wind_linux/gnome-shell-extension/README.md`；CMake 在 Wayland 且找到 `gio-2.0` 时把扩展
     装到 `${datadir}/gnome-shell/extensions/`（没有 gio 不阻塞其余 addon）。
   - **验证环境还没定。** 2026-10-10 在 Ubuntu 22.04（GNOME Shell 42.9）上看到的是扩展还不存在
     时的行为：gedit 报 `rect=(344,76 0x18)`，服务端摆到 `(344,96)`。Shell 42 不能加载 ESM
     扩展，那次不能当作本扩展的实测。Ubuntu 24.04 的 Shell 是 46，同样不在 `shell-version` 里
     （这是发行版包版本，不是在那台机器上启用过）。47–50 只核对了上游标签里相关符号还在，
     **没有**在真实 GNOME 会话里对过光标，分数缩放也没有确认。需要作者补测后改这一条。
   源码在 `wind_linux/gnome-shell-extension/windinput-caret@windinput/`。非 GNOME 的
   X11 会话与 treeland/KWin popup 不受影响。
11. **Wayland 下工具栏 / 软键盘记住的位置可能跳角（已知限制）**：Wayland 面板坐标的单位是「合成器逻辑
   坐标 × 服务端此刻的光栅化缩放」（§5b / `OverlayInput.h`「Wayland 面板坐标」），而光栅化缩放跟着焦点
   走：XWayland 应用取 `Xft.dpi`，原生 Wayland 应用取输出缩放。两者不一致时（例如输出 150%、`Xft.dpi`
   仍是 96），同一个落点在两种焦点下换算出的锚点不同，记住的位置会在记录点和默认角之间跳。只在混合
   缩放配置下出现，未修。
12. **软键盘不跟物理 Shift 换层（已知限制）**：Windows / macOS 上物理按住 Shift（或开着大写锁定）时软键盘
   切到第二层（上档字符）；Linux 服务端读不到全局修饰键状态（`soft_keyboard.rs` 的 `MODIFIER_POLLS`），
   按住 Shift 不换层。点软键盘自己的 Shift 键照常切换。
13. **无合成器的 X11 下工具栏悬停唤回不可用（已知限制）**：没有合成器时窗口外形按像素 alpha 用 XShape
   裁（§5b），自动隐藏淡出的整窗透明度乘进了像素，假隐藏那帧处处 alpha≈1，裁出来是空的——窗口既看不见
   也收不到指针（X 的输入区不能超出外形），移回原位唤不回来，只能等状态变化重新出现。有合成器（ARGB）
   时外形不裁、面板整个矩形都收指针，正常。

## 8. 安装包：只装 deb 就能用（不再有 `windinput-setup`）

- **升级 / 卸载结束服务**：postinst（仅 `configure`）与 postrm（仅 `remove`）用
  `pkill -x -f '/usr/lib/windinput/wind_input( --restarted)?'`，按整条命令行精确匹配包里那个路径
  （addon 拉起时（`spawnDetached`，双 fork 后 execve）argv 只有这个全路径；菜单「重启服务」自拉起的多一个 `--restarted`），
  同名开发构建与 `wind_input restart` 这类 CLI 调用不受影响。卸载放 postrm：文件删掉后再结束，
  已加载的 addon 想重新拉起时程序已不在。
- **已知限制：被结束时丢最后 1 秒的运行时状态**。服务不处理 SIGTERM（默认动作即刻终止），
  `state.toml` 的单点写入器（`state_writer`，1s 防抖）只在 `Drop` 里 flush——被信号结束时不跑；
  菜单「重启服务」走的 `process::exit` 同样不跑。丢的只是防抖窗口内的界面位置类状态（工具栏位置、
  软键盘页等），`config.toml` 的写入不经防抖、不受影响。补优雅退出要在 wind-coordinator 暴露 flush
  接口并在服务里装信号处理线程，收益只是「升级那一刻前 1 秒内拖过的位置」，暂不做。
- **不再有一次性配置脚本**（2026-10-10 删除 `windinput-setup`）。发布的软件不能要求用户先跑一个
  外部脚本才能用；原脚本做的四件事逐项处理如下：

  | 原脚本做的事 | 现在 |
  | --- | --- |
  | 把清风加进 Fcitx5 输入法组 | 用户在 Fcitx5 配置（fcitx5-configtool）的输入法列表里添加——所有 Fcitx5 输入法的标准用法。脚本要先让运行中的 fcitx5 退出再改 `profile`（它退出时整份写回会冲掉改动），这类对用户配置的强改本身就是问题 |
  | 清空 `AltTriggerKeys`（出厂 `Shift_L`） | addon 自己解决：`WindEngine::claimShiftTap` 在 `PreInputMethod` 阶段（早于 Fcitx5 在 `InputMethod` 阶段处理触发键）接过 Shift_L / Shift_R 的按下与松开，自己走 `keyEvent` 后 `filter()`，Fcitx5 的触发键逻辑看不到它（按下没记 `keyReleased_`，松开不会触发 `altTrigger`）。条件：本引擎是当前输入法、且该键确在 `AltTriggerKeys` 里——用户清空了它或当前是别的输入法就不插手。代价：想用 Shift 在清风与键盘布局间切换的人改用 Ctrl+Space（清风自己的 Shift 本来就是切中英）。e2e 的 fcitx5 配置保留出厂值并带「键盘 + 清风」两个输入法，Shift 用例即回归 |
  | 用户配置里写初始方案 | 不做。出厂方案与 Windows 一致（五笔），随包的 `data/config.toml` 预置层原样分发；用户在设置里改方案后写进自己的 `config.toml` |
  | `im-config -n fcitx5` | 不做。系统输入法框架是 Fcitx5 是所有 Fcitx5 输入法的共同前提（Deepin 默认；Ubuntu GNOME 在 `im-config` 里 `DESKTOP_SETUP_IBUS` 命中时恒选 ibus，装 fcitx5 不会自动切），写进安装说明；`im-config` 留在 Recommends |

  依赖随之去掉 `python3`（只有该脚本用）。`procps` 保留（postinst / postrm 的 `pkill`）。

## 9. 发布流程接入（CI）

**产物**：`WindInput-<版本>-linux-amd64.deb`、`WindInput-<版本>-linux-arm64.deb`，各带同名 `.sha256`（资产名与
Windows/macOS 同口径）。包内 `Version` 把预发布后缀的 `-` 换成 `~`（`0.123.0-dev.x` → `0.123.0~dev.x`），
否则 dpkg 把它当「打包修订」而排在正式版之后。

**工作流**（`.github/workflows/`）：

| 文件 | 作用 |
| --- | --- |
| `linux-build.yml` | 可调用的构建流水线（仅 `workflow_call`）：门控 → 词库数据（一次）→ 两架构并行构建 → 核对 → 安装冒烟 → 上传 `dist-linux-<arch>` |
| `release-linux.yml` | **独立入口**（手动触发）：不填 tag 只出 artifact；填 tag 则构建该 tag 并把 `.deb` 挂到该 tag 的 Release（不存在建草稿）。`setting_ref` 可指定 wind-setting 分支（默认 main，已含 Linux 适配） |
| `release.yml` | tag 推送时并行调 `build-linux`，`publish` 汇总进同一个草稿 Release。Linux 与 Windows / macOS **同为硬依赖**（2026-10-08 起；此前不阻塞、单独 `attach-linux` 补挂，Linux 一挂草稿就悄悄缺包）：任一平台失败都不建草稿，两个架构都在才算齐。fork 未配 `WIND_REPOS_TOKEN` 时不构建 Linux，草稿里只告警 |
| `release-published.yml` | Release 发布后把 `.deb` + `latest-linux.json`（`debs.<arch>.{url,sha256,size}`）同步到 R2，有则同步、两架构齐全才推 |

**为什么拆成 `linux-build.yml` + `release-linux.yml`**：被调用的工作流不能申请比调用方更高的权限；「挂 Release」
要 `contents: write`，和构建写在一个文件里，`release.yml` 一调用整个文件就因权限越界校验失败。

**为什么原生 runner**：基线必须是最老的目标（`ubuntu-22.04`，glibc 2.35 / fcitx5 5.0.x），更新的构建机会引入
`GLIBC_2.38+` 符号；arm64 用 `ubuntu-22.04-arm` 原生机而非 qemu（Rust LTO 在模拟器上要数小时）。镜像钉死 22.04，不用 `-latest`。

**门禁**：`scripts/linux/verify-deb.sh` 查 control 架构、包内每个 ELF 的机器类型、`GLIBC` 符号上限 ≤ 2.35、关键文件与词库体积、
`compat.toml` 零规则、无 group/other 可写、maintainer 脚本语法；随后在 runner 上真装一次（apt 解析 Depends、`ldd` 无缺失库、能卸载）。
构建脚本 `scripts/linux/build-deb.sh`（本机 docker 的 `package-deb.sh` 与 CI 共用，路径经环境变量传入）。

**设置程序是私有仓**：没配 `WIND_REPOS_TOKEN`（fork）时整个 Linux 构建跳过并告警，不出缺设置入口的残缺包。

**未做**：rpm（Fedora/openSUSE）与 Arch（PKGBUILD）需各自发行版实测；apt 仓库（签名密钥与托管）；客户端在线升级对 Linux 的消费
（`latest-linux.json` 已就位，设置程序侧尚无读取方，升级暂走 apt/手动下载）；Wayland。

**已知风险（审查结论，未处理）**：
- 给**已发布**的 Release 补挂 `.deb` 不会触发 `release: published`，R2 要靠手动 dispatch `release-published.yml`；
  但它会按该 tag 把三个平台都重推一遍并清理「前一版本」——对老 tag 这么做会让 `latest*.json` 回退、删掉新版 exe。
  只对**最新**版本补同步；要支持老版本需给它加 `platforms` 输入或版本单调性守卫。
- 安装冒烟的 `ldd` 看不到运行时 `dlopen` 的库（Vulkan/EGL、xkbcommon），且 runner 镜像预装了它们，缺 Depends 测不出来；
  真正的验证仍是干净 22.04 / Deepin 上装一次。
- 补包用 `release-linux.yml` 只对包含本接入的 tag 有效（prep 会检查脚本是否存在）。

### 打包规范审查（对照通行的 Fcitx5 插件打包建议）

目前只发 Debian 系（Ubuntu 22.04+、Deepin 25），rpm / Arch 暂不做。逐项对照：

- **发布基线**三条同时写明：glibc ≥ 2.34（`dpkg-shlibdeps` 实算，构建机 22.04）、libstdc++ ≥ 11、Fcitx5 ≥ 5.0.14
  （addon 按 5.0.14 头文件编；5.1.x 的 Deepin 25 上实测可加载）。基线只能随构建机变高不能变低。
- **addon 装到 `${CMAKE_INSTALL_LIBDIR}/fcitx5`**（`GNUInstallDirs`，Debian 得 `lib/<multiarch>`），不写死；
  服务 / 设置程序 / 词库放私有目录 `/usr/lib/windinput/`（架构相关二进制，由包的 Architecture 区分）。
- **依赖自动算**：`build-deb.sh` 用 `dpkg-shlibdeps` 取链接期依赖，只手写算不到的（fcitx5 本体与最低版本、
  运行时 `dlopen` 的 `libfontconfig1`、字体、python3、procps）；`verify-deb.sh` 校验关键项在。
  （此前手写表漏了 libstdc++6 / libgcc-s1 / libfcitx5*，靠 fcitx5 传递依赖才碰巧能装。）
- **不写用户 HOME**：包只装 `/usr` 下文件；用户配置 / 词库由程序首次运行时在用户目录创建；
  `remove` / `purge` 都不动用户数据。
- **不杀 fcitx5**：postinst / postrm 只结束包路径下的 `wind_input` 服务（addon 下次连不上会自动拉起新版），
  fcitx5 本身留给用户重启（`fcitx5 -rd` 或重新登录）。
- **包名保持 `windinput`**：不改 `fcitx5-<名>` 形式——这个包不只是 addon，还带服务、设置程序与词库；已部署的测试机以此名升级。
  日后若拆包，再按 `fcitx5-windinput`（addon）+ `windinput`（服务 / 数据）拆。
- **不用 AppImage / Flatpak / Snap**，也不用 `alien` 转格式；将来做 rpm 要在 Fedora 容器里独立构建、独立实测。

