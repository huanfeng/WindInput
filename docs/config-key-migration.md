# 配置键域重命名迁移表（旧 → 现行）

> **不做向后兼容**：运行时只认现行键，旧键加载即被 serde 丢弃（软件开发期、仅作者自用，旧配置遗弃可接受）。
> 本表是键名的**单一映射真相**：驱动 `config.rs` 结构体、`config_schema.rs` 注册表、`data/config.toml`、
> `manifest.toml`、前端 `config-keys.json` 的同步改动；亦作人工查阅。共 126 键。
>
> 注：第二轮微调（`general`→`input.default` 去前缀 · `ui.status_indicator`→`ui.status` ·
> `dict.phrase`→`input.phrase` · 移除 `debug.perf_sampling`）已合入下表的"现行"列。

## 顶级域（6 个，"正交大类"准则）

`schema`(方案/拼音/模式) · `input`(输入行为，含 `default` 启动默认 / `phrase` 短语) ·
`keys`(全部按键) · `ui`(外观) · `stats`(统计) · `debug`(调试)

移除的旧顶级：`hotkeys`(→keys) · `pinyin`(→schema.pinyin) · `features`(拆解：stats 升顶级 / s2t·cmdbar→input / 模式三件套→schema) ·
`general`(→input.default) · `dict`(→input.phrase) · `compat`(唯一字段 `host_render_processes` 已迁入 `compat.toml` 的
`AppCompatRule::host_render`，与按进程名匹配的其余兼容规则同库，详见 `docs/redesign/host-render-windows-port.md` §11.7)。

---

## schema（25）

| 旧 | 现行 |
|---|---|
| `schema.active` / `available` / `primary_codetable` / `primary_pinyin` | 不变 |
| `pinyin.show_code_hint` | `schema.pinyin.code_hint_source`（0.122 起由布尔改为四档枚举，值迁移见 `Config::migrate_show_code_hint_value`） |
| `pinyin.use_smart_compose` | `schema.pinyin.use_smart_compose` |
| `pinyin.candidate_order` | `schema.pinyin.candidate_order` |
| `input.pinyin_separator` | `schema.pinyin.separator` |
| `pinyin.fuzzy.enabled` | `schema.pinyin.fuzzy.enabled` |
| `pinyin.fuzzy.{zh_z,ch_c,sh_s,n_l,f_h,r_l,an_ang,en_eng,in_ing,ian_iang,uan_uang}` | `schema.pinyin.fuzzy.*`（11，名不变） |
| `features.quick_input.enabled` | `schema.quick_input.enabled` |
| `features.quick_input.decimal_places` | `schema.quick_input.decimal_places` |
| `features.quick_input.force_vertical` | `schema.quick_input.force_vertical` |
| `features.special_modes` | `schema.special_modes` |
| `features.mix_modes` | `schema.mix_modes` |

## input（41；含 default 启动默认 4 项 + phrase 短语 2 项）

| 旧 | 现行 |
|---|---|
| `input.filter_mode` / `enter_behavior` / `space_on_empty_behavior` / `numpad_behavior` | 不变 |
| `general.remember_last_state` | `input.default.remember_last_state` |
| `general.default_chinese_mode` | `input.default.chinese_mode` |
| `general.default_full_width` | `input.default.full_width` |
| `general.default_chinese_punct` | `input.default.chinese_punct` |
| `input.punct_follow_mode` | `input.punct.follow_mode` |
| `input.smart_punct_after_digit` | `input.punct.smart_after_digit` |
| `input.smart_punct_list` | `input.punct.smart_list` |
| `input.punct_custom.enabled` | `input.punct.custom_enabled` |
| `input.punct_custom.mappings` | `input.punct.custom_mappings` |
| `input.smart_symbol_mode` | `input.symbol.smart_mode` |
| `input.smart_symbol_timeout_ms` | `input.symbol.smart_timeout_ms` |
| `input.smart_symbol_chars` | `input.symbol.smart_chars` |
| `input.auto_pair.{chinese,english,chinese_pairs,english_pairs}` | 不变（4） |
| `input.shift_temp_english.enabled` | `input.temp_english.enabled` |
| `input.shift_temp_english.show_english_candidates` | `input.temp_english.show_candidates` |
| `input.shift_temp_english.shift_behavior` | `input.temp_english.shift_behavior` |
| `input.shift_temp_english.trigger_keys` | `input.temp_english.trigger_keys` |
| `input.shift_temp_english.allow_symbols` | `input.temp_english.allow_symbols` |
| `input.shift_temp_english.space_as_input` | `input.temp_english.space_as_input` |
| `input.capslock.cancel_on_mode_switch` | 不变 |
| `input.temp_pinyin.trigger_keys` | 不变 |
| `input.url_input.enabled` | `input.url.enabled` |
| `input.url_input.prefixes` | `input.url.prefixes` |
| `input.code_commit.{auto_commit_at_full,auto_commit_min_len,clear_on_empty_max,top_code_commit,auto_commit_block_on_pinyin}` | 不变（5） |
| `features.s2t.enabled` | `input.s2t.enabled` |
| `features.s2t.variant` | `input.s2t.variant` |
| `features.cmdbar.enabled` | `input.cmdbar.enabled` |
| `features.cmdbar.candidate_prefix` | `input.cmdbar.candidate_prefix` |
| `input.phrase.min_prefix_length` | `input.phrase.min_prefix` |

## keys（20，扁平；overflow 保留一层因是真·分组配置）

| 旧 | 现行 |
|---|---|
| `hotkeys.toggle_mode_keys` | `keys.toggle_mode_keys` |
| `hotkeys.commit_on_switch` | `keys.commit_on_switch` |
| `hotkeys.switch_engine` | `keys.switch_engine` |
| `hotkeys.toggle_full_width` | `keys.toggle_full_width` |
| `hotkeys.toggle_punct` | `keys.toggle_punct` |
| `hotkeys.toggle_toolbar` | `keys.toggle_toolbar` |
| `hotkeys.open_settings` | `keys.open_settings` |
| `hotkeys.add_word` | `keys.add_word` |
| `hotkeys.toggle_s2t` | `keys.toggle_s2t` |
| `hotkeys.activate_ime` | `keys.activate_ime` |
| `hotkeys.pin_candidate` | `keys.pin_candidate` |
| `hotkeys.delete_candidate` | `keys.delete_candidate` |
| `hotkeys.global_hotkeys` | `keys.global_hotkeys` |
| `input.select_key_groups` | `keys.select_key_groups` |
| `input.page_keys` | `keys.page_keys` |
| `input.highlight_keys` | `keys.highlight_keys` |
| `input.select_char_keys` | `keys.select_char_keys` |
| `input.overflow.number_key` | `keys.overflow.number_key` |
| `input.overflow.select_key` | `keys.overflow.select_key` |
| `input.overflow.select_char_key` | `keys.overflow.select_char_key` |

## ui（36；仅 tooltip 拍平，顶层名保留 ui）

| 旧 | 现行 |
|---|---|
| `ui.candidate.*`（12） | 不变 |
| `ui.font.*`（3） | 不变 |
| `ui.theme.{name,style}` | 不变 |
| `ui.mode_indicator.style` | 不变 |
| `ui.status_indicator.*`（9） | `ui.status.*`（9，子键名不变） |
| `ui.toolbar.{visible,hide_in_fullscreen}` | 不变 |
| `ui.tooltip.delay` | 不变 |
| `ui.tooltip.code.enabled` | `ui.tooltip.code_enabled` |
| `ui.tooltip.pinyin.enabled` | `ui.tooltip.pinyin_enabled` |
| `ui.tooltip.pinyin.heteronyms` | `ui.tooltip.pinyin_heteronyms` |
| `ui.tooltip.pinyin.max_readings` | `ui.tooltip.pinyin_max_readings` |
| `ui.tooltip.chaizi.enabled` | `ui.tooltip.chaizi_enabled` |
| `ui.tooltip.debug.enabled` | `ui.tooltip.debug_enabled` |

## stats（2，从 features 升顶级）

| 旧 | 现行 |
|---|---|
| `features.stats.enabled` | `stats.enabled` |
| `features.stats.track_english` | `stats.track_english` |

## debug（1）

| 旧 | 现行 |
|---|---|
| `debug.log_level` | 不变 |
| `debug.perf_sampling` | **已移除**（无业务读取点） |
| `compat.host_render_processes` | **已移除**（并入 `compat.toml` 的 `AppCompatRule::host_render`） |
