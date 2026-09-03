# 快捷加词界面设计（--page add-word）

## 背景与目标

清风输入法核心（Rust core）已实现「快捷加词模式」：`Ctrl+=` 从最近上屏字符中取末尾 N 字组词，
在候选窗内以 `↑↓` 调整词长、`Enter` 直接写库、`Esc` 取消。核心里也已有 `open_add_word_dialog`
（加词模式内按 `Ctrl+Enter`）会构造 `add-word --text=… --code=… --schema=…` 并调用 `open_settings`
拉起设置端——**但设置端（wind-setting）没有对应的 `add-word` 界面，也不解析这些参数**，属于断头路。

本设计补齐两端，对齐原 Go 项目（`WindInput-Go`）：

1. **wind-setting**：新增 `--page add-word` 参数方式，拉起一个**独立的加词小窗**，复用词库页现有的
   加词对话框与提交逻辑，支持 `--text/--code/--schema` 预填。
2. **core**：
   - `Ctrl+=` 加词模式内 `Ctrl+Enter` → 打开加词界面（**已实现**，等设置端接住）。
   - 新增独立热键 `Ctrl+Shift+=` → **不进加词模式**，直接取最近输入预填、拉起加词界面
     （对齐 Go 的 `openAddWordDialogFromHistory`）。

## 参考实现（原 Go 项目）

- `wind_input/internal/coordinator/handle_addword.go`
  - `openAddWordDialog()` — 加词模式内 `Ctrl+Enter`，用当前预览的词/编码打开对话框。
  - `openAddWordDialogFromHistory()` — 独立快捷键直接打开，不进加词模式，仅取最近输入预填。
  - `openAddWordDialogWith(word, code, schemaID)` — 二者共用，构造 `add-word --text=… --code=… --schema=…`。
- `wind_input/pkg/config/config.go` — 热键 `add_word = "ctrl+equal"`、`open_add_word_dialog = "none"`（默认）。
- `wind_setting/frontend/src/pages/AddWordPage.vue` — 加词对话框：词/编码（自动出码，可手改）/权重（默认 1200）/
  连续添加；独立窗口模式关闭即 `Quit()`。

> 与 Go 的差异（按本次需求）：`open_add_word_dialog` **默认绑定 `ctrl+shift+equal`**（Go 默认 none）；
> 加词界面**复用 wind-setting 词库页现有对话框**而非新写；**暂不做**连续添加；编码**不自动重算**（仅"出码"按钮手动触发）。

## 架构总览

两条入口最终汇到同一个 `open_settings("add-word …")`：

```
Ctrl+=  → enter_add_word_mode（候选窗）→ Ctrl+Enter → open_add_word_dialog ┐
                                                                          ├→ open_settings("add-word --text=词 --code=码 --schema=id")
Ctrl+Shift+= → open_add_word_from_history（不进加词模式，取最近输入预填）  ┘
                              ↓
   wind-setting 起「独立加词小窗」（独立单例 app_id）
        → 复用词库加词对话框（预填 / 出码 / 确定）
        → dict.add → toast → 关窗即退出进程
```

## Part A — core（wind-config + wind-coordinator）

### A-1 配置字段（wind-config `config.rs`）
- `Hotkeys` 结构体新增 `open_add_word_dialog: String`，`#[serde(default = "default_open_add_word_dialog")]`。
- `default_open_add_word_dialog()` 返回 `"ctrl+shift+equal"`。
- `Default for Hotkeys` 补 `open_add_word_dialog: default_open_add_word_dialog()`。
- `data/config.toml` 的 `[hotkeys]` 段补 `open_add_word_dialog = "ctrl+shift+equal"`（紧邻 `add_word`）。
- 热键冲突清单补标签「打开加词界面」（对齐 Go `config_hotkey.go` 的 `add(c.Hotkeys.OpenAddWordDialog, "打开加词界面")`）。

### A-2 热键编译（wind-config `hotkey.rs`）
- 在 CHINESE_ONLY 的 key_down 组注册 `open_add_word_dialog`（与 `add_word` 并列，同 `HOTKEY_POLICY_CHINESE_ONLY`），
  action 串 = `"open_add_word_dialog"`。
- 编译测试断言该 action 进入 chinese-only 组。

### A-3 加词处理（wind-coordinator `handle_addword.rs`）
- 抽出公共构造 `open_add_word_dialog_with(&self, word: &str, code: &str, schema: &str) -> KeyAction`
  （对齐 Go `openAddWordDialogWith`）：拼 `add-word` + 非空的 `--text/--code/--schema` → `open_settings` → `ClearComposition`。
- 现有 `open_add_word_dialog`（`Ctrl+Enter`）改为取当前预览词/编码/方案后调 `open_add_word_dialog_with`。
- 新增 `open_add_word_from_history(&self, state: &mut State) -> KeyAction`（对齐 Go `openAddWordDialogFromHistory`）：
  1. 有未上屏输入先清理（`reset_*` + `notify_ui_hide`），避免残留 composition。
  2. 取最近字符（`add_word_recent_chars(ADD_WORD_MAX_LEN)`），默认词长 `ADD_WORD_DEFAULT_LEN`（夹到可用长度）。
  3. `calc_add_word_code` 算码。
  4. 取 `data_schema_id(active)`。
  5. 调 `open_add_word_dialog_with(word, code, schema)`。
  - **不**进加词模式、**不**改候选窗布局、**不**发占位 composition。

### A-4 热键分派（wind-coordinator `coordinator.rs`，key_down 匹配处，约 3239 行）
- 与既有 `add_word` 特判并列，新增：
  ```
  else if action == "open_add_word_dialog" {
      let mut state = self.state.lock()…;
      if state.chinese_mode {
          return self.open_add_word_from_history(&mut state);
      }
  }
  ```
- 仅中文模式响应（与 `add_word` 一致）。

## Part B — wind-setting（独立加词小窗 + 复用词库逻辑）

### B-1 参数解析（`cli.rs`）
- 新增 `AddWordParams { text: String, code: String, schema: String }`。
- 新增 `parse_add_word(argv) -> Option<AddWordParams>`：当 argv 含 `--page add-word` / `--page=add-word` / `--add-word`
  时，解析 `--text/-text=`、`--code/-code=`、`--schema/-schema=` 两种形式（对齐 Go `parseAddWordParams`）。
- `parse_target` 对 `add-word` 返回 `None`（它不是 Tab；`PAGES` 不加 `add-word`）。
- 单元测试覆盖 `--text=` 与 `--text ` 两形式、schema 缺省、仅 `--page add-word` 无预填。

### B-2 独立小窗分支（`main.rs`）
- `main()` 早分支：`if let Some(params) = cli::parse_add_word(&args) { run_add_word_window(params); return; }`
  （在常规设置 App 构建之前，**不落入** `LoadedState::fetch()` / `shell::build` 分支）。
- `run_add_word_window(params)`：
  - 构建独立 `App`：较小尺寸（约 460×300）、`frameless()`、`centered()`、独立 `app_id = format!("wind_setting_addword{}", mode::pipe_suffix())`。
  - content = 复用的词库加词对话框（见 B-3），初始 `edit_visible = true`。
  - `single_instance(addword_app_id, closure)`：二次启动 argv 若为 add-word → 重新 `parse_add_word` → 重新预填并置顶
    （不新起进程 → 加词窗恒定封顶 1 个）。
  - 关窗即退出进程（单窗应用默认行为）；`edit_visible` 翻回 false（确定成功或取消）时请求关闭窗口。

### B-3 复用词库加词逻辑（`pages/dict/state.rs` + `dialogs.rs`）
- 新增 `DictManagerState::open_add_word_prefilled(&self, text: &str, code: &str, schema: &str)`：
  1. 按 `schema` 在 `folded_domains()` 里做别名匹配（双拼族 rep_schema_id 归一为 `pinyin`），设置 `domain`（≥1）+ `sub_tab = 0`（用户词库）。
  2. 预填 `edit_code`、`edit_text`、`edit_weight = "1200"`，`editing_orig = None`（新增模式）。
  3. 设置 `edit_title`/`edit_l_code`/`edit_l_text`（用户词库文案），`edit_visible = true`。
- **完全复用**：
  - 界面 = `dialogs::build_edit_dialog`（编码 + 「出码」按钮 + 词条 + 权重 + 确定/取消）。
  - 「出码」= `encode_current`（`dict.encode`，**手动**触发，符合"点击再次生成、不自动重算"）。
  - 「确定」= `submit_word` → `save_calls` 的 `UserDict / None` 分支 → `dict.add`（code/text/weight）+ `reload` + toast。
- 加词小窗启动只做**最小 RPC**：拉一次启用方案列表（解析 schema 别名 + 引擎类型给出码用）；出码/提交按需调用。
  **不**调用整页的 `LoadedState::fetch()`。

### B-4 单例隔离（满足"不与完整设置窗单例冲突"）
- 主设置窗 app_id：`wind_setting{suffix}`（现状不变）。
- 加词小窗 app_id：`wind_setting_addword{suffix}`（独立）。
- 效果：
  - 设置窗已开 + `Ctrl+Shift+=` → 加词参数走 addword 单例 → 独立小窗，**不劫持**设置窗 Tab。
  - 加词窗已开 + 再按 `Ctrl+Shift+=` → addword 单例转发 → 复用同一小窗重新预填，**不新起进程**。
  - 加词窗已开 + 从菜单开完整设置 → 不同 app_id → 各自独立。

### B-5 IPC 连接约束（满足"控制 IPC 连接数量"）
- wind-setting RPC 为**按调用短连接**（连接→发一帧→读一帧→关闭，`rpc.rs`），无持久连接、无 push 订阅。
- 加词窗自身单例 → 至多 1 个加词进程；叠加主设置窗至多 1 个 → 对 core 的**瞬时并发连接封顶 2 个进程**。
- 加词分支跳过重量级全量 fetch，仅最小 RPC → 启动连接风暴最小化。

## 数据流（端到端）

**Ctrl+= → Ctrl+Enter 路径：**
```
Ctrl+= → enter_add_word_mode（候选窗：↑↓/Enter/Ctrl+Enter/Esc）
  → Ctrl+Enter → open_add_word_dialog → open_add_word_dialog_with(词,码,方案)
  → open_settings("add-word --text=… --code=… --schema=…")
  → wind-setting 起加词小窗（预填）→ 出码/编辑 → 确定 → dict.add → toast → 关窗退出
```

**Ctrl+Shift+= 路径：**
```
Ctrl+Shift+= → open_add_word_from_history（不进加词模式，取最近输入预填）
  → open_add_word_dialog_with(词,码,方案) → open_settings("add-word …")
  → 同一加词小窗
```

## 测试策略

### core
- `open_add_word_from_history`：headless coord + 手动注入 code，断言构造的 page 串含正确 word/code/schema、且未进入加词模式（`add_word_active == false`、候选窗布局未改）。
- `open_add_word_dialog_with`：空 word/code/schema 时对应 `--text/--code/--schema` 不出现。
- `hotkey.rs`：编译测试断言 `open_add_word_dialog` 进 chinese-only key_down 组。

### wind-setting
- `parse_add_word`：`--text=`/`--text ` 两形式、`--schema` 缺省、仅 `--page add-word` 无预填、非 add-word argv 返回 None。
- `open_add_word_prefilled`：按 schema 别名选中正确 domain（含双拼族归一），edit_* 预填正确、weight = "1200"、editing_orig = None。
- `save_calls`（已有）：UserDict/None → `dict.add`（复用，无需新测）。

## 风险与开放点

- **紧凑独立窗可行性**：需确认 windui `App` 支持"非 shell 的替代 content + 关窗即退进程"。`main.rs` 现已按 args 分支
  尺寸/主题，`run_add_word_window` 走独立 App 构建路径可行；关窗退出是单窗应用默认行为。
- **schema 别名匹配**：复用词库页 `folded_domains` 的 `rep_schema_id` + 别名归一逻辑（双拼→pinyin），
  与 Go `AddWordPage.vue` 的 `aliasIds` 匹配一致。
- **真机验证清单**（实现后手测）：
  - `Ctrl+=` → `Ctrl+Enter` 拉起加词小窗且预填正确。
  - `Ctrl+Shift+=` 直接拉起、不改候选窗、预填最近输入。
  - 设置窗已开时 `Ctrl+Shift+=` 不劫持设置窗、开独立小窗。
  - 连按 `Ctrl+Shift+=` 只复用同一小窗。
  - 出码按钮、确定写库、toast、关窗退出。
  - 非中文模式下 `Ctrl+Shift+=` 不触发。

## 后续变更：剪贴板作为加词来源（2026-09-02）

上文「取最近上屏字符」的描述自本次改动起**只是两个来源之一**。字符池抽成
`add_word_pool()`（`handle_addword.rs`），由 `State::add_word_from_clip` 选源：

| 入口 | 默认来源 | 切换 |
| --- | --- | --- |
| `Ctrl+=` 加词面板 | 最近上屏；**最近上屏为空且剪贴板可用时自动落在剪贴板** | `Tab` 恒可切，两侧对称 |
| `Ctrl+Shift+=` 直开加词界面 | **剪贴板优先**，不可用才回退最近上屏 | —— |

两个入口的默认来源**刻意相反**：`Ctrl+=` 是带面板的连续加词，接着刚打的字最顺；
`Ctrl+Shift+=` 一按就把词交给设置端、不给调长度的机会，按它多半是「刚复制了一个词」。

### 两个来源必须对称

⛔ 曾给剪贴板侧加过「空则不许切、面板也不提示 `Tab`」的守卫，**已推翻**：最近上屏为空时
面板照常显示、照常能停在那儿，剪贴板凭什么不能。用户看到的是「`Tab` 有时在有时不在」，
比一个诚实的空态更费解。落地后：

- 标题的来源后缀（`· 最近输入` / `· 剪贴板`）与提示里的 `Tab切换来源` **恒显示**。
- 空的一侧照样切得过去，正文换成该侧空态：`无最近输入` / `剪贴板无可用内容`
  （后者的注释交代准入条件「需单行、不超过 10 字」——不合规与真的没内容在界面上是同一个
  可见状态，不说清用户会以为复制没生效）。
- 「切不切得动」不再是一个需要判断的问题，`toggle_add_word_source` 无守卫。

### 其余约定

- **剪贴板不可用**＝trim 后为空 / 含换行 / 超过 `ADD_WORD_MAX_LEN`（10 字）。三条守卫与
  命令栏 `dict.add` 的 `sanitize_dict_add_text` 同源，但出口是**静默降级**而非报错。
- **裁剪方向随来源反向**：最近上屏取池子末尾 N 字，剪贴板取开头 N 字（默认全选，`↓` 砍尾）。
- 剪贴板在**进入模式时读一次即定格**，`Tab` 复用该结果——读取必须走阻塞版
  `clipboard_get_text`（cached 版会给出陈旧内容，加词是执行路径），代价最坏 40ms。
- 面板由两行改为**三行**（标题含来源 / 词与编码 / 操作提示）：`Tab` 那一项加进来后，
  「标题 + 五个动作」挤一行会把面板撑到半个屏幕宽。提示放 `comment` 字段而非 `text`
  ——前者走注释色与更小字号，放 `text` 会用候选正文色，最不重要的一行反而最显眼。
  这条判据只体现在颜色上，由 `panel_hint_is_a_dim_third_row` 守门。

真机验证清单增补：`Ctrl+=` → `Tab` 两侧对称切换（含空态）、`↑↓` 裁剪方向、无最近输入时的
自动落点；`Ctrl+Shift+=` 在剪贴板有词/为空两种情形下的预填来源。macOS 侧 `Tab` 转发未验证。

## 后续变更：设置端加词界面也有两个来源（2026-09-03）

上一节让**输入法侧**的两个入口都认剪贴板；这一节把同一件事补到**设置端**那个窗口里，
并顺带修好 `wind-setting --add-word` 这条裸入口。

### `--add-word` 裸启动此前是个不可工作的窗

那条入口不经输入法热键，`--schema` 与 `--text` 都没有。方案为空 ⇒ `dict.encode` 与
`dict.add` 都会失败——窗口看着完全正常（标题、输入框、按钮都在），点「生成编码」没反应、
点「确定」报错，用户无从知道差的是什么。

现按「**带了参数就原样用，没带才补**」补齐（`add_word_window::resolve_fields`）：

| 缺什么 | 补什么 |
| --- | --- |
| `--schema` | core 的**加词目标方案**（混输已解析到主码表方案，与 Ctrl+= 同一个函数） |
| `--text` | 剪贴板；剪贴板无可用内容则退到最近上屏（与 Ctrl+Shift+= 的优先级一致） |

### 上限为什么是 10 字

`ADD_WORD_MAX_LEN` 原为 20（对齐 Go），2026-09-03 真机实测改为 **10**：码表方案的词组码
由方案 `[[encoder.rules]]` 组装，出厂 wubi86 的最后一条规则是 `length_in_range = [4, 10]`
⇒ 超过 10 字没有任何规则匹配，`calc_word_code` 直接出不了码。原值有一半是空转——11–20 字
选得出来、却注定加不进去，用户要按着 ↓ 一路减到 10 才看见编码出现。

收到真实能力边界上之后，「选得出来的都加得进去」才成立。上限是单一真相源，面板字符池、
`↑↓` 可选范围、剪贴板准入、`check_derivable_word`、`dict.addWordContext` 的取值一并跟随。

深链带了参数就不补——那时用户的意图已经明确，不该被默认值盖掉。

### 新增 RPC `dict.addWordContext`

返回 `{ schemaId, recentText }`。设置端**只在自己缺参数时**调它，以及窗内「最近」按钮
每次点击时调一次（要的是当下最新的上屏内容，不能用开窗那一刻的快照）。

走 `WebDataHost` 窄面而不是让 webdata 自己拼：目标方案要经混输解析，最近上屏更是纯输入态
——webdata 按设计不碰 `State`。

取不到一律回空串（core 没起来、或旧版本没有这个方法），窗口照常打开、只是没有默认值，
与改动前的行为相同。**不弹错**：用户可能就是想手填。

### 窗内「最近」按钮 ＝ Ctrl+Shift+= 的另一半

Ctrl+Shift+= 进来时词条预填的是剪贴板，这个按钮把它切到最近输入。它与既有的「粘贴」
并列成一组，正好是加词的两个来源，对应输入法侧加词面板的 `Tab`。

★ **取字符池上限（`ADD_WORD_MAX_LEN` = 10 字）而不是默认词长两字**。判据来自「加词是
为了打不出来的词」：一个词若能整段一次上屏，说明词库里已经有它、根本不需要加；真正要加
的词恰恰是逐字/分段上屏的，散落在多条上屏记录里，取末尾两字往往只截到半个词。设置端的
词条框是多行的，多给的部分删起来很便宜，少给却要用户回输入法重打一遍。

⚠️ 由此与 core 侧 Ctrl+Shift+= 的**回退**取值（末尾两字，`ADD_WORD_DEFAULT_LEN`）不同口径：
那条是「没有更好的线索时自动猜一个」，这条是用户主动点的、意图明确。

### 布局：三个按钮改竖排

词条框有 88 高，容得下三个按钮竖排，输入框宽度也不必再让出一截。宽度显式取
`widgets::ENCODE_BTN_W` 与上一行的「生成编码」对齐——两行右边缘要齐平。原先两按钮横排时
这件事由 `ENCODE_BTN_W == TEXT_BTN_W * 2 + 8` 那条等式承担，竖排后等式失效，改由加词窗
里的显式宽度接手（那两个按钮是共用组件，不能为这一个窗口改它们的 `min_width`）。
