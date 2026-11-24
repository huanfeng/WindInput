# 应用兼容规则：上屏文本与焦点类字段的宿主事实

`compat.toml` 的 `[[apps]]` / `[[commit_newline]]` / `[[initial_mode_scope]]` 里，若干字段只能按应用
配置——不同宿主对同一份数据的正确答案相反。本文记录这些字段背后的宿主行为与证伪过的方向。
候选窗定位类字段（`stale_probe_guard`、`composition_start_pair_guard`、`pin_anchor_when_start_drifts`、
`caret_use_top`）见 [caret-position-host-compat-notes.md](./caret-position-host-compat-notes.md)；
`host_drawn_candidates` 见 [game-compat-tsf-uielement.md](../design/game-compat-tsf-uielement.md)；
`ignore_host_ime_switch` 见 [tsf-openclose-compartment-semantics.md](./tsf-openclose-compartment-semantics.md)；
窗口条件匹配见 [compat-window-match.md](../design/compat-window-match.md)。

## 一、上屏换行（`[[commit_newline]]`，`NewlineStyle`）

「换行」在不同文本模型里不是同一个东西：

- **富文本模型**（Word / WPS / RichEdit / TOM）：段落边界就是 CR。LF 在这里不构成换行——Word 把
  上屏文本里的 LF 渲染成一段类似 Tab 的空白，而记事本、WPS 对同一段文本分段正常。对这类宿主转 CR
  是唯一能成为换行的形式。
- **字节即存储的宿主**（VS Code / 终端 / 浏览器 textarea / Edit 控件）：写进去什么就存什么，任何
  转换都是改写用户数据，且失败是静默的（曾出现「正则直通后行尾从 `\n` 变成 `\r`」）。

没有对所有宿主都对的值，故按应用配置，出厂默认 `keep`。漏配富文本宿主的代价是用户看得见的显示
异常（能反馈、加名单即可）；反方向是静默改写行尾——可见的失败优于静默的失败。

`[[commit_newline]]` 独立成段而不做成 `[[apps]]` 的字段：它与候选窗定位无关，也不参与右键菜单写回。

## 二、组合区占位字符（`composition_placeholder` / `assoc_placeholder`）

有会话却没有可见编码要放进宿主时（非嵌入模式、联想态、直达热键进入的模式等），组合区要挂一个
占位字符：TSF 不接受空组合，WPS 一类宿主对零长度 range 回退化矩形（height=0），候选窗失去坐标来源。

| 档位 | 字符 | 适用 | 不适用 |
|---|---|---|---|
| `space`（默认） | U+0020 | 历史行为 | 浏览器受控输入框：组合中的 value 被 `trim()` 后回写（如 React `onChange: e => setV(e.target.value.trim())`），state ≠ DOM ⇒ React 回写 ⇒ 浏览器终止组合，一个字都上不了屏 |
| `zwsp` | U+200B | 不被 `trim()` / `\s` 当空白，躲开上述回写；出厂只给浏览器开 | 钉钉在线表格：选中单元格直接打字时靠「出现了内容」才进编辑态，零宽字符被当成没有内容，组合停在代理输入框、候选窗错位 |
| `blank` | U+2800 | 非空白且有宽度，两头都要时用 | — |

ZWSP 在非浏览器宿主里的光标度量未经实测，故按应用开。DLL 三种字符都认作占位。

联想态另可写 `none`（不挂占位组合）：转发资格改由上屏应答里的 `COMMIT_FLAG_ASSOC_SESSION` 告知 DLL；
代价是没有 range，候选窗沿用上屏前最后一次的光标坐标。

## 三、多行上屏（`multiline_commit`）

新版微信收到 TSF 上屏的多行文本时，会把组合区的编码显示残留在界面上，换哪种 TSF 写法都一样；
结束组合后写剪贴板、注入 Ctrl+V 即无残留。粘贴会覆盖剪贴板且不恢复，Ctrl+V 不是粘贴的宿主（终端等）
不能开，故按应用配置。

## 四、初始模式作用域（`[[initial_mode_scope]]`）

per-app 规则的身份是进程映像名，而 `explorer.exe` 一个名字同时承载桌面（停留型）与任务栏 / Alt+Tab /
任务视图 / 溢出区 / 资源管理器（路过型）。为桌面配 `initial_mode = "english"` 会一并命中后者，而它们是
每次切换应用的必经之路（实测非桌面焦点与桌面焦点约 14:1）。

判据必须是白名单「规则在哪些窗口类上生效」，不能是黑名单「哪些是过渡窗口」。黑名单版本当天即被推翻：

- explorer 新起一个 TSF 连接时，头一个 `focus_gained` 拿不到窗口类（TSF 此刻还没有 view，caret 也退到
  `last_known`），空类名不在黑名单里 ⇒ 套上英文规则 ⇒ 语言栏闪「英」；
- Windows 每个版本都在新增 XAML 岛窗口类，漏一个就套错一次。

白名单下「拿不到窗口类」与「新出现的类」都落在作用域外，即保持现状——这两种情况下唯一安全的答案。

## 五、其它按宿主开的字段

- **`auto_pair = false`**：Excel / WPS 表格在「输入态」下把方向键解释成「确认单元格并移动」，配对后无法把
  光标退回两符号之间（TSF `SetSelection` 路线实测失败）。关掉配对是目前唯一可行的兼容策略。
- **`pair_via_comp`**：见 [pair-commit-host-compat-notes.md](./pair-commit-host-compat-notes.md)。
- **`smart_method = "hold_composition"`**：`DeleteReplace` 依赖对宿主做删改，在 Tabby 一类终端上出严重错误。
- **`caret_offset_x/y`**：修宿主上报 caret 坐标的系统性偏移（如 Windows Terminal，其它输入法同样偏），与主题里
  候选窗相对光标的布局偏移不是一回事。单位 dp，按目标点所在显示器 DPI 换算在协调器侧完成。
- **`launch_by_service`**：Win11 开始菜单、便笺（`Microsoft.Notes.exe`）等宿主收到「拉起外部程序」请求后
  并不执行，表现为点了没反应。改由服务进程 `ShellExecuteW`，代价是窗口可能停在后台。
- **`first_show_mode`**：三档的取舍与默认档沿革见
  [candidate-window-positioning.md](../redesign/candidate-window-positioning.md) 第 6 层。
