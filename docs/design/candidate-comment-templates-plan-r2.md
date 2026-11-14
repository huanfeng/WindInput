# 候选注释 真机反馈修订 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** 修掉 Windows 上 Alt 切换失效，切换键可选 Ctrl/Alt，总表横排为主，设置端拆成独立对话框并重做分配表与模板库。

**Spec:** `docs/design/candidate-comment-templates.md` §13（本计划的依据；与 §4、§9 冲突时以 §13 为准）。

**Architecture:**
- Ctrl 单击：三个平台已经只送「确认是单击」的 Ctrl 松开（TSF 经 keyup 白名单，mac/Linux 无条件送），
  服务端在 keyup 分支里按切换键判定即可。
- Alt 单击 / 按住：mac、Linux 平台层现有转发不动，服务端按 `switch_key` 过滤左右。Windows 改由服务进程的
  全局低级键盘钩子判定并压菜单栏，TSF 里本分支加的 Alt 转发全部删除。
- 横排为主：`comment_lib` 的同行回退方向反过来，出厂分配写横排格。

## Global Constraints

- 三仓都在 `wt-comment/` 下的 `feat/comment-templates` worktree 分支上做；`wind-setting/wind-ui-rust` 是指向主仓的
  符号链接，**不得在里面切分支或改文件**。
- 提交只用 `git commit -F - --only -- <路径…>`；禁用 `git add -A` / `git add .` / `git commit -a` / `git stash` / `--no-verify`；
  `reset` 只用 `--mixed`。
- 格式化只对自己改的文件跑 `rustfmt --edition 2024`，不跑 `cargo fmt --all`。
- 测试隔离：`TMPDIR=/home/dufeng/.cache/wt-comment-tmp`。core 全量用 `cargo test --workspace --no-fail-fast`。
  已知预存红：`config_load_sites_are_all_classified`、`every_line_based_parser_normalizes`、
  `capabilities_snapshot_matches_core`（core）；`snapshot_matches_core_generated_capabilities`、
  `mock_config_matches_core_system_preset`（setting，若本任务改了 core 配置键则必须让它们按新键更新而不是更红）。
- 改 wind-config 的任务要过门禁：`registry_covers_every_config_key`、`data_config_toml_covers_registry`、
  `data_config_toml_has_no_orphan_keys`、`registry_types_match_default_values`、`data_config_toml_values_pass_validation`、
  `tests/write_back_gates.rs`；core 改了配置键后跑 `cargo test -p wind-rpc --test wind_setting_assets` 并按其提示更新 setting 产物。
- 未发布功能不做兼容：`alt_switch` 直接改名，不迁移。
- 值域原文：`switch_key`：`ctrl`（出厂）/ `lctrl` / `rctrl` / `alt` / `lalt` / `ralt`；
  `switch_mode`：`off` / `session`（出厂）/ `persist` / `hold`。
- **按住（hold）只配 Alt**（裁定：Ctrl 的按下三个平台都不送）；`hold` + Ctrl 系按 `session` 处理并启动 `warn` 一次，设置端选 Ctrl 时「按住」置灰并说明。（**已由 Task 12 取代**：Ctrl 系也支持按住）
- 提交信息与注释简短，一句结论，不写日期 / GH 号 / 实测过程。
- 设置端 UI 改动必须用离屏截图自验：`WIND_RPC_MOCK=1 target/debug/wind_setting --page ui --size W H --screenshot out.png`，
  可加 `--click X Y`（可多次）/ `--hover X Y` / `--rclick X Y` / `--key …`；mock 首启会弹设置向导，先找出关掉它的办法
  （mockdata 或参数）。截图存 `/home/dufeng/.cache/wt-comment-tmp/shots/`，报告里列出文件路径与看到了什么。

## Review Focus

1. Windows 选 Alt：组字中 Alt+Tab、Alt+Shift、Alt+数字不切换；单击切换后菜单栏不激活；没在组字时单击 Alt 菜单栏照常激活；
   Alt 状态不会卡住（钩子吃掉松开后必须补发松开）。由 Task 3 的纯函数用例钉住。
2. Ctrl 撞键：`lctrl` 同时是中英切换键 / `commit_code` 绑定 / `lrctrl` 选词键组时，那些功能照旧，切换键让位。由 Task 2 钉住。
3. 竖排没写的格：同本行横排；`inherit` 跳到下一层同排布。旧的「横排同竖排」用例全部翻过来。由 Task 4 钉住。
4. 分配表添加覆盖：同一范围不能加两次；用户加的行可删；出厂行只能恢复出厂。由 Task 7 钉住。
5. 模板库左栏点击切换右栏，编辑后预览即时更新；切到别的模板时未保存的编辑不串到新模板。由 Task 8 钉住。

---

## core（WindInput）

### Task 1: 切换键配置 `switch_key` / `switch_mode`

**Files:** `wind-config/src/comment_lib.rs`（`AltSwitch` → `SwitchMode`，新增 `SwitchKey`）、`wind-config/src/config_schema.rs`、
`data/config.toml`（`[ui.comment]` 段两键与注释）、所有 `alt_switch` 消费点（见调研：`comment.rs:1209-1279`、`comment_alt.rs`、测试）。

- [ ] `SwitchKey { Ctrl, LCtrl, RCtrl, Alt, LAlt, RAlt }`，带 `matches_vk(vk) -> bool`（0xA2/0xA3/0xA4/0xA5）、`is_alt()`；
      `SwitchMode { Off, Session, Persist, Hold }`；`UiCommentConfig` 字段 `switch_key`（默认 Ctrl）、`switch_mode`（默认 Session）。
- [ ] 有效模式：`effective_mode(key, mode)`，hold + Ctrl 系 → Session，启动 `warn` 一次。（**已由 Task 12 取代**：Ctrl 系也支持按住）
- [ ] `data/config.toml` 两键注释写清：选 Alt 的 Windows 会装全局键盘钩子；按住只配 Alt；功能键请用按键绑定 `comment_state`。（「按住只配 Alt」**已由 Task 12 取代**）
- [ ] 用例：解析、默认值、非法值走校验、hold+ctrl 降级。门禁全过。提交。

### Task 2: 服务端按切换键判定（Ctrl 单击 + 按 Alt 左右过滤）

**Files:** `wind-coordinator/src/comment_alt.rs`（改名 `comment_switch.rs`）、`comment.rs` 分发、
`coordinator/message_handler.rs` keyup 分支、`wind-config/src/hotkey.rs`（keyup 白名单登记）、`tests/comment_state*.rs`。

- [ ] Alt 事件（mac/Linux 平台转发来的 0xA4/0xA5）：`switch_key` 不是 Alt 系、或左右不匹配 → PassThrough 且不建 track。
- [ ] Ctrl：`switch_mode != off` 且 `switch_key` 是 Ctrl 系时，在 TSF keyup 白名单里登记对应 lctrl/rctrl（`compile_modifier_key_up_hash`，
      带 SESSION 位，action 独立命名如 `comment_switch`，**不得**用 `toggle_mode`）。keyup 分支里排在 select / session_actions /
      capslock / bound_modifier / toggle_mode **之后**：有候选且匹配切换键 → session 翻转或 persist 写回，回 Consumed；否则落回原语义。
- [ ] `carries_other_modifier` 按键区分：Ctrl 切换键的 UP 自带 Ctrl 位，不能当组合键；Alt 仍按原规则。
- [ ] 启动告警 `warn_comment_switch_conflict`（仿 `warn_english_case_cycle_conflict`）：切换键与中英切换键、选词键组、会话绑定撞键时提示「那些功能优先」。
- [ ] 用例：Ctrl 单击切换（session / persist）、无候选时 Ctrl 落回原语义、`lctrl` 作中英切换键 / `commit_code` / `lrctrl` 时让位、
      `ralt` 配置下左 Alt 不切、Ctrl 系配置下 Alt 不切。提交。

### Task 3: Windows Alt 全局钩子 + 删除 TSF 里的 Alt 转发

**Files:** `wind-keys/src/capslock_hook.rs`（扩展为同一个钩子兼管 Alt，或抽公共部分；**一个进程只装一个 LL 钩子**）、
`wind-keys/src/key_inject.rs`、`wind-coordinator/src/coordinator.rs`（安装门控与闸门同步，仿 `sync_capslock_hook` / `sync_capslock_gate`）、
`wind_tsf/include/AltKey.h`、`wind_tsf/src/KeyEventSink.cpp`、`wind_tsf/include/KeyEventSink.h`、`wind_tsf/tests/alt_key_test.cpp`。

- [ ] 安装门控：`switch_mode != off` 且 `switch_key` 是 Alt 系时才需要 Alt 功能；钩子 = CapsLock 需要 ∪ Alt 需要。
      都不需要时卸载。日志说明为什么装 / 不装。
- [ ] 闸门：一个原子量「Alt 侧掩码」（0 = 不接管），由协调器在候选窗显隐处同步（与 CapsLock 闸门同处调用）。
- [ ] 判定写成纯函数（可在 Linux 单测）：输入 = 事件（Alt 左/右 down/up、其它键 down、是否注入、时间戳）+ 闸门 + 内部状态；
      输出 = 是否吃掉 + 要投递的事件（`Tap` / `HoldDown` / `HoldUp`）+ 是否需要补发屏蔽键。规则：
  - 闸门开时的首个 Alt down 认领这次按下（自动连发不重置时刻）；按住期间任何其它键 down 记为被打断；
  - 认领的这次按下，松开时不论闸门（同 CapsLock 的配对规则）：未被打断 → 吃掉这次松开，然后注入 `VK 0xE8 down/up` + 同侧 Alt up
    （带专用 `dwExtraInfo` 标记，钩子放行带标记的注入事件）；被打断 → 放行，不补发；
  - tap：按住 ≤ 500ms 且未被打断才投递 `Tap`；hold：首个 down 投递 `HoldDown`，松开投递 `HoldUp`（被打断也要投递）。
- [ ] 消费线程：`Tap` → session / persist 翻转并重绘候选窗；`HoldDown` / `HoldUp` → 按住翻转。重绘走现有无按键上下文的推送路径
      （参照 CapsLock 钩子消费线程如何把结果推给前端）。
- [ ] TSF：删掉本分支加的 Alt 转发（`AltKey.h`、`_TryForwardAlt` 及其调用点、`_altHold`、`_altReply`、相应测试），恢复到与 main 同形；
      另加：`VK 0xE8` 一律放行、不转发（屏蔽键）。`g++ -std=c++17` 跑 `wind_tsf/tests` 全过。
- [ ] `cargo check --target x86_64-pc-windows-msvc`（`cargo xwin` 或 `./scripts/dev.sh sk` 同类）编得过钩子代码。
- [ ] 纯函数用例：单击、长按、Alt+Tab（被打断）、Alt+Shift、闸门关时按下（不认领）、按下后闸门关再松开（仍补发）、注入事件放行、
      左右侧过滤、hold 的 down/up 配对。提交。

### Task 4: 横排为主

**Files:** `wind-config/src/comment_lib.rs`（`pick`、`effective_table`、用例）、`data/system.comments.toml`、
`wind-config/src/config.rs`（迁移的横竖规则）、`wind-webdata` 里 `comment.library` 若有列语义、文档 `docs/design` §5 措辞。

- [ ] 同一层：竖排格不写 → 用本层横排格（横排格有值时）；横排格不写 → 跟随下一层横排。`inherit` 规则不变。
- [ ] 出厂分配全部改写横排格（`default_horizontal` / `detail_horizontal`），效果与现在一致。
- [ ] 迁移：横竖相同只写横排；只有竖排有值时竖排写值、横排写 `inherit`；只有横排有值时横排写值、竖排写 `inherit`。
- [ ] 现有「横排同竖排」类用例翻转为「竖排同横排」，新增 Review Focus 3 的用例。提交。

### Task 5: 迁移命名

**Files:** `wind-config/src/config.rs`（`migrate_comment_templates_value`）及其用例。

- [ ] 模板名：`旧版·全局`、`旧版·临时拼音`、`旧版·临时英文`、`旧版·快捷输入`（`mix:<id>` 用 `旧版·<id>`）、`旧版·网址`、
      `旧版·Email`、`旧版·Unicode`；同一来源横竖两串不同时第二个加 `（竖排）`。
- [ ] 旧模板串与内置模板文本相同 → 直接写内置 id，不生成 `migrated_N`。
- [ ] 用例覆盖上述两条。提交。

## 设置端（wind-setting）

### Task 6: 卡片重排与独立对话框入口

**Files:** `src/assets/settings_manifest.toml`、`src/manifest.rs`、`src/dialogs/comment_settings.rs`（拆分）、
`src/dialogs/comment_dicts_dialog.rs`、capabilities 产物与 mockdata。

- [ ] 「候选标注」分区行：
  - 「候选注释」：Switch（`ui.comment.enabled`）+「分配…」，样式同「悬停提示」那种 Switch + 按钮行；
  - 「注释模板」：「管理…」→ 模板库对话框（Task 8）；
  - 「注释切换与显示」：「设置…」→ 对话框：当前状态、切换键（`switch_key`）、切换方式（`switch_mode`，选 Ctrl 系时「按住」置灰并说明——**已由 Task 12 取代**）、
    上方注释条、横 / 竖最大字数；
  - 「自动使用词库自带的注释」照旧；「注释词库」恢复为独立一行 + 原 `comment_dicts_dialog`（不再挂在注释对话框里）。
- [ ] 删除 Tab 外壳与对话框里重复的「显示候选注释」开关。
- [ ] 截图自验卡片与「注释切换与显示」对话框。提交。

### Task 7: 分配对话框

**Files:** `src/dialogs/comment_settings.rs`（或拆出 `comment_assign.rs`）。

- [ ] 顶部一行：「添加方案覆盖」「添加模式覆盖」按钮 + (i)（`widgets::info_badge`），提示写设计稿 §13.4 的优先级与「跟随 / 同横排」说明；顶部不再有大段说明文字。
- [ ] 只有表格内部滚动（参照 `tooltip_sections_dialog.rs` 自拼 frame 的写法），对话框本身不滚。
- [ ] 列顺序：作用范围 / 默认·横 / 默认·竖 / 详细·横 / 详细·竖；格子显示「跟随 · 模板名」「同横排 · 模板名」（灰色斜体）。
- [ ] 每行范围名右侧一个预览图标按钮，点开 `.popup` 浮层：这一行四格在样例候选上的渲染效果（复用 `appearance.previewTemplate`）。
- [ ] 添加覆盖：按钮弹选择对话框，列出尚无覆盖的方案（`schema.list`）/ 模式（`BASE_MODES` + mix + special），单选后点确认才加行；已存在的不列出。
- [ ] 用户加的行有删除按钮；出厂行只有「恢复出厂」（清掉用户格）。
- [ ] 纯逻辑用例（可选项过滤、删除 / 恢复出厂、列顺序、格子显示文案）+ 截图自验（表格、(i) 悬停、预览浮层、添加对话框）。提交。

### Task 8: 模板库对话框

**Files:** 新 `src/dialogs/comment_templates.rs`（从 `comment_settings.rs` 拆出模板库部分）。

- [ ] 左栏：windui 现成单选列表（`Element::list` 或 `list_pill`），内置 / 自定义两组；点击即切换右栏。底部或顶部：新建、复制、删除。
- [ ] 右栏：名称、模板串（沿用变量插入辅助）、**实时预览**（横排与竖排两种排布、至少三个样例候选）；内置只读，可「复制为自定义」；
      覆盖内置 id 的用户版标「已修改」并可「恢复内置」。
- [ ] 切换选中时未保存编辑的处理：自动写回当前模板（与现有编辑写回机制一致），不得串到新模板。
- [ ] 用例 + 截图自验（点左栏第二项后右栏确实切换、编辑后预览变化）。提交。

### Task 9: 注释词库条目右键「打开文件所在位置」

**Files:** `src/dialogs/comment_dicts_dialog.rs`。

- [ ] 列表条目 `.on_context_menu`：「打开文件所在位置」（相对路径经 `data.locate` 解析，再 `log_tools::reveal`）；文件不存在时菜单项置灰。
- [ ] 截图自验（`--rclick`）。提交。

## 文档站（WindInputDocs）

### Task 10: 文档同步

- [ ] `content/docs/settings/appearance/candidate-comment.mdx`：切换键两键与值域、按住只配 Alt（**已由 Task 12 取代**）、Windows 选 Alt 会装全局钩子、
      功能键用按键绑定、横排为主的跟随规则、设置端新布局（入口名称）、迁移命名。提交。

## 追加（执行中发现）

### Task 11: 预览样例扩成一组（core）

**Files:** `wind-coordinator/src/comment/preview.rs`、`wind-webdata/src/lib.rs`（`appearance.previewTemplate`）、
`wind-webdata/tests/appearance_preview_template.rs`；设置端 mock `src/rpc.rs` 的 `mock_preview_template` 同步（只改 mock 与调用方，不动别的 UI）。

现状：只有一个固定样例「你好」，设置端预览三个候选注释完全相同，看不出模板效果。保留「固定样例、不现查词库」的取舍，扩成一组：

- [ ] 中文样例至少 3 个（如 你好 / 中文 / 输入法，各有自己的拼音、双拼、反查码、拆字、`${dict:zh_en}` 英文释义、编码提示），
      英文样例至少 3 个（如 apple / hello / world，`${dict:en_zh}` 给「词性. 中文义」，`${pinyin}` 等中文变量为空）。
      `${dict:<id>}` 按 id 取对应语种的样例值；裸 `${dict}` 取该样例的首个释义。
- [ ] 入参加可选 `sample`（样例 id，缺省 = 原「你好」，保持现有回包与用例不变）；回包加 `candidate`（样例词本身）。
      另加 `samples` 查询（或在回包里列出可用样例 id 与语种），设置端据此取「中文三条 / 英文三条」。
- [ ] 用例：每个样例的关键变量值、英文样例下中文变量为空且可选段消失、缺省入参回包不变。
- [ ] 设置端：mock 同步；模板库预览与分配表预览按作用范围选样例语种（英文类型行、临时英文模式行用英文样例，其余中文）。

### Task 12: Ctrl 也支持按住；候选显示时 Ctrl 按下 / 松开如实上报（维护者 10-23 定）

**动机**：「按住只配 Alt」是平台层只报「已判定的 Ctrl 单击」造成的，不是系统限制。另有一个既有漏洞：Ctrl+数字（置顶）/
Ctrl+Shift+数字（删除）走 `RegisterHotKey → WM_HOTKEY → DispatchHotkey`，不经 TSF 按键回调，TSF 的待判单击作废不了，
500ms 内松开会被当成 Ctrl 单击（lctrl 作中英切换键时会顺带切英文；出厂 Ctrl 切换键时会顺带切注释）。

**规则（维护者已定）**：
- 只要**候选窗显示着**，三端平台层就把 Ctrl（左右分开，0xA2 / 0xA3）的按下与松开如实上报服务端，与切换键、切换方式无关；
  候选窗不显示时维持原「平台层本地判单击、只报确认过的松开」。平台层仍不吃 Ctrl。
- 服务端统一判定：按下建 track；之间任何别的键（含 `DispatchHotkey` 送来的 Ctrl+数字、Ctrl+C 等）弄脏 track；松开时干净且 ≤500ms 才算单击。
  原有消费者（中英切换键、`session_actions` 的 lctrl/rctrl、`lrctrl` 选词）照旧优先，且必须照旧只在「单击」时触发——
  也就是说服务端要替这些消费者做原来平台层做的单击判定，行为与原来一致。
- 按住：Ctrl 系也支持 `hold`，**按下立即翻转**、松开恢复（组合键时注释闪一下，维护者接受）；去掉 `effective_mode` 的 hold+ctrl→session 降级。
- TSF `WM_HOTKEY` 分发处（`DispatchHotkey`）作废待判单击（候选窗不显示那条路也修好）。

**Files**：TSF `wind_tsf/src/KeyEventSink.cpp`（待判逻辑与转发）、`TextService.cpp`/`DispatchHotkey`；Linux Fcitx5 addon（`WindEngine.cpp`、
`KeyMap.*`）；macOS `InputController.swift`（`handleFlagsChanged` / `sendModifierTap`）；core `comment_switch.rs`、`message_handler.rs` keyup/keydown
分支、`hotkey.rs`（白名单是否仍需要）、`comment_lib.rs`（`effective_mode`）；设置端 `comment_switch.rs`（去掉「仅 Alt」置灰与提示）；文档站。

**用例**：候选显示时 Ctrl 单击 / 长按 / Ctrl+C / Ctrl+数字（经 DispatchHotkey）各一；中英切换键 lctrl 在候选显示时单击照切、Ctrl+1 不切；
hold 按下翻转松开恢复、按住期间候选关闭不残留；候选不显示时行为与原来逐项一致；TSF 的 g++ 单测补待判作废。
