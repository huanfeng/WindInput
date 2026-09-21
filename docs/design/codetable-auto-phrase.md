# 码表自动造词

码表方案通过「连续单字上屏 + 终止信号」自动组词，按方案 `[[encoder.rules]]` 计算词组编码，
写入临时词库；累计使用达阈值后晋升进用户词库。

## 1. 为什么此前完全不工作

两处**各自独立、都足以致命**的断裂：

| # | 断裂 | 位置 |
|---|---|---|
| 1 | **触发源建错** | 造词挂在 `State::committed_segs` 上，而这是**拼音专属**的「组合区逐步转换」态。分段判据 `partial = consumed_length < total` 依赖拼音引擎才标注的字段，码表候选恒为 0 → `partial` 永远 false → 每次选词后 `reset_pinyin_composition` 立即清空。入口守卫 `committed_segs.len() < 2` 因此**对码表恒真**，函数体一行都执行不到。 |
| 2 | **编码算法错** | 即使触发，编码是各段码**拼接**（`code.push_str`）。五笔「你好」的词组码是 `wqvb`（各取全码前两位），拼接全码得到 `wqiyvbg` 之类 —— 词库里查不到，等于没造。 |

连带查实的三处死配置 / 断链：

- `EncoderSpec` / `EncoderRule` / `formula`：定义齐全、`data/schemas/wubi86.schema.toml` 数据齐全，
  **全仓零消费点**。手动造词也没用它（走的是硬编码五笔 86 分支）。
- `Store::evict_temp_words`：**零生产调用点**，临时词库只进不出。
- `Coordinator::handle_selection_changed`：**空实现**，而 C++ 侧 `TextService.cpp:4622` 一直在发。

设置端另有两处失真：开关 hint 写「加入用户词库」（实际进临时词库）、`capabilities.rs` 注释称
「自动晋升逻辑未接线」（早已接线，`maybe_promote_temp` 一直在跑）。

## 2. 架构

```
单字上屏   → 追加进 charBuffer
多字词上屏 → 终止（选了词组说明这不是散字序列）
终止信号   → flush 并清空
两字间隔 > idle_timeout → 先 flush 旧序列再起新序列
```

- 状态机：`wind-coordinator/src/auto_phrase.rs`（**纯逻辑、零 IO**，打断语义全部单测覆盖）
- 编码器：`wind-engine/src/encoder.rs`（**纯函数**，公式求值 + 规则匹配）
- 单字全码表：`wind-dict/src/cached.rs::build_single_char_full_codes`，
  落盘格式 `wind-dict/src/charcodes.rs`（`.wscc`）

  这张表与反查索引同源（同一批词库、同一次全表扫描），**造词的就绪闸是两者的与**
  （`EngineManager::single_char_codes_ready` ∧ `reverse_index_if_ready`）。它此前是唯一
  没有磁盘缓存的那份派生产物，于是 `.wridx` 命中缓存省下的时间被它原样吃回去——
  开机后头几次造词照样整次跳过。补上 `.wscc` 后（沿用 `.wridx` 的指纹/原子替换那一套，
  但**不做 mmap**：只收单字条目、不随词条数增长，wubi86 实测 185 KB）该窗口大幅缩短。
  ⚠️ 它的指纹比 `.wridx` 多一项 `max_code_length`——这张表拿它当闸筛超长码，
  而反查索引与之无关；漏了这一项，用户改了码长而词库未动时会永久复用旧表。
- 接线与 IO：`wind-coordinator/src/handle_addword.rs`

### flush 流程

```
① 长度 < min_phrase_len(2) → 丢弃
② 长度 > max_phrase_len(5) → 丢弃（不切末尾 N 字）
③ 逐字取全码 → 任一字无码 → 整词作废 + DEBUG 日志记下卡在哪个字
④ 按 [[encoder.rules]] 的 formula 算词组编码
⑤ 系统词库已有「码+词」→ 跳过；用户词库已有「码+词」→ 跳过
⑥ 写入临时词库（立即可作为候选 —— StoreTempLayer 是可查询层）
⑦ count 达 promote_count → 晋升进用户词库（默认 0 = 永不晋升）
```

**超长整体放弃**而非截末尾 N 字：在连续多字中间切一刀，切出来的多半不是词，是杂词主要来源。
宁可放过，不可错造。（Go 版取末尾 N 字，此处刻意不对齐。）

**缺码整词作废**而非跳过该字：跳过会把「你X好」算成「你好」的码，静默产出错词。
旧 `wubi_word_code` 的 `firstn` 正是静默跳过。

### 单字全码判据

```
该字在方案词库中的全部编码
→ ① 滤掉码长 > 方案 max_code_length（0 = 不设闸）
→ ② 取剩下里最长的码长
→ ③ 同码长按权重降序
→ ④ 码字典序升序
```

- **① 上限闸**不可省：扩展词库塞进来的 5/6 码怪码否则就是「最长码」，②③④ 根本没机会参与。
- **② 必须取最长**：公式取「第 2 码」时，简码「工」=`a` 只有一位，直接越界。
  注意 `CachedDict::build_reverse_index` 是**码长升序**（`codes[0]` 是最短码），
  照搬「取首个」的写法会让每个有简码的字都算不出编码。
- **④ 为什么不是「首次出现」**：`CodetableDict::for_each_entry` 遍历 `HashMap`，**顺序不确定**；
  mmap 路径则为码字典序。取「首次出现」会让同权同长的字在两条路径下、甚至两次构建间得到不同的码。
  字典序是确定性代偿。

### 码源统一

手动造词（快捷加词 / 设置端 `dict.encode`）与自动造词**共用** `EngineManager::encode_word`。

原 `wind_reverse::wubi_word_code` 已弃用：码源是**拆字表**、规则是**硬编码的五笔 86 三分支**。
问题有三——拆字表是可选资源（全仓 5 个方案只有 `wubi86` 配了，未配的第三方方案取码恒空、
手动加词直接失败）；硬编码规则对非五笔码表方案静默出错；拆字表与词库解耦，用户换词库或加扩展库后
可能造出**打不出来**的码。

判据：**造词的唯一目的是「造出来的词以后能打出来」，码源必须与实际词库同源。**

## 3. 终止信号

| 信号 | 接入点 |
|---|---|
| 标点 / 英文 / 数字上屏 | `feed_auto_phrase`（非汉字文本即终止） |
| 多字词上屏 | `AutoPhraseBuf::on_commit` |
| 焦点丢失 | `handle_focus_lost` |
| IME 停用 | `handle_ime_deactivated` |
| 中英切换 | `handle_toggle_mode` |
| 光标移动 | `handle_selection_changed` |
| idle 超时（默认 5s） | `AutoPhraseBuf::on_commit` |

### 自提交宽限期（SELF_COMMIT_GRACE）

码表下 composition 绝大部分时间是关闭的（每选一字就上屏并关闭），此时 Space/Enter
**被 TSF 直接透传给宿主，协调器收不到按键**（`KeyEventSink.cpp:398/966/1024` —— Backspace/
Enter/Escape 仅在有 composition 或 input session 时才拦截）。`SelectionChanged` 是唯一能感知
「用户敲空格结束一句」的途径。

但本输入法自己提交文字后，宿主插入文本同样导致光标移动、同样回送该事件，且在协议层
**与用户真实光标移动完全无法区分**（C++ 守卫是 `selChanged && _pComposition == nullptr`，
上屏后 composition 恰已关闭，正好放行）。只能靠时间判别：距上次自提交足够近 → 判为回声、忽略。

不做这个区分，每上屏一个字就被自己的回声判成「用户移动光标」→ flush → 缓冲永远只有 1 个字
→ **表现仍然是「自动造词完全不工作」**。

> **`SELF_COMMIT_GRACE` 目前的 200ms 是待实测校准的初值。** Go 版实测宿主回声 <50ms 并取 200ms
> 留余量，但那是另一套进程/宿主组合下的观测值。`handle_selection_changed` 已埋 DEBUG 日志
> `selection_changed: since_self_commit=...`，真机跑一遍（记事本 / Chrome / EverEdit）看实际
> 分布后再定值。
> 取值过小 → 回声被误判为用户操作，序列被切碎、造词失效；
> 取值过大 → 用户上屏后短时间内的真实光标移动漏掉一次终止（由 idle 超时兜底）。

**打点必须收口在一处。** `last_self_commit` 打在 `handle_key_event_policed`，与 `record_input_stats`
同一收口理由：上屏有 40+ 个返回点，且约 10 处**绕过** `commit_action` 直接构造 `InsertText`
（顶码 / 智能符号 / 临拼等）。散点打点必漏，漏一条那条路径的上屏就会切碎序列。

> ⚠ 写测试时注意：`tests/input_flow.rs` 多数用例调的是**裸 `handle_key_event`**，
> 那条路不经过打点与投喂。造词相关测试必须走 `handle_key_event_policed`。

## 4. 混输

拼音打出的单字**照样进码表造词缓冲**。因为编码在 flush 时由公式从字**重算**，段自身带什么码
不影响结果 —— **来源不再携带任何信息**。

原 `learn_phrase_on_commit` 有「混源即跳过」守卫，那是为**拼接码**服务的（拼音码 `ni` 与五笔码
`wq` 拼在一起确实无意义）。改成重算后该守卫失去存在理由，故码表路径不再设。

混输的**拼音分步转换**（`committed_segs`）仍走 `learn_phrase_on_commit` 学成拼音词 —— 与单字序列
学成码表词是不同维度，可并存。故该函数排除的是 `is_codetable()`（纯码表），**不是** `!is_pinyin()`。

## 5. 配置

`[schema.codetable.auto_phrase]`

| 键 | 默认 | 设置页 |
|---|---|---|
| `enabled` | `false` | ✅ 「启用自动造词」 |
| `promote_count` | `0`（不晋升） | ✅ 「转入用户词库次数」 |
| `min_phrase_len` | `2` | ❌ 内部 |
| `max_phrase_len` | `5` | ❌ 内部（原为 10，10 字序列几乎必是跨句杂词） |
| `idle_timeout_ms` | `0` → 5000 | ❌ 内部 |
| `temp_max_entries` | `5000` | ❌ 内部 |

`min/max_phrase_len` 不开放：调错直接抬高杂词率，普通用户无判断依据。

## 6. 已知缺口

### 6.1 退格无法感知（架构限制）

用户退格删掉已上屏的字时，`charBuffer` 里那个字**还在**，后续 flush 会造出含幽灵字的词。

**做不到精确处理**：composition 关闭时按退格，C++ 直接透传，协调器收不到该按键
（与 Space/Enter 同一结构性原因）。唯一能感知的是它引发的 `SelectionChanged`，
而那**无法与「敲空格结束一句」区分** —— 两者在协议层完全一样。
（试过用 `prev_char` 判别：用户敲空格结束一句时 `prev_char` 是空格，与缓冲末字同样对不上，
会把主用例一起误杀。此路不通。）

**接受该缺口**，因为错词的实际危害有限：

1. 错词进的是**临时层**，不是用户词库。
2. `promote_count` 默认 0 → 永不晋升；即使用户设了阈值，也不会重复打同一个错，`count` 停在 1。
3. **错词的编码是按错序列算的**，与用户下次打正确词用的码不是同一个 —— 不污染正确词的候选列表。
4. `temp_max_entries` 会自然淘汰。

若真机验证发现频繁困扰，后续方案是让 DLL 在无 composition 时也转发退格
（代价：改 C++，须连带处理构建缓存不清导致 DLL 停在旧版、宿主进程常驻旧 DLL 不重启不生效
这一串陷阱，见 `reference_windows_build_dev_ps1`）。

### 6.2 `CodetableGlobal::resolved()` 未折叠 `auto_phrase` / `frequency`

`wind-config/src/config.rs` 的 `resolved()` 折叠了 9 个行为字段，**独缺 `auto_phrase` 与
`frequency`** —— 方案 `.schema.toml` 的 `[engine.codetable]` 无法覆盖这两项，它们是全局唯一的。

**本轮不补**。两项同时缺席不像遗漏，更像有意：造词与调频属「用户偏好」而非「方案特性」，
方案不该替用户决定学不学词。若后续确认是遗漏，补齐时**两项要一起补**，否则语义更不一致。

## 7. 验证

- 单测：`wind-engine/src/encoder.rs`（12）、`wind-dict/src/cached.rs` 全码判据（4）、
  `wind-coordinator/src/auto_phrase.rs` 打断语义（10）
- 端到端：`wind-coordinator/tests/input_flow.rs::test_codetable_auto_phrase_*`（4），
  用**真实 wubi86 方案与词库**验证取码、终止时机、开关闸门、单字不成词
- **待真机**：`SELF_COMMIT_GRACE` 实测校准（见 §3）；混输下拼音单字入缓冲；晋升与淘汰
