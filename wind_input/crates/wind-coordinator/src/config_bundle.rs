//! [`ConfigBundle`]：配置 + 其轻量派生缓存的不可变快照，运行时整体原子替换以支持热重载。
//!
//! `build` 是所有配置生效的必经之路（启动、热重载、RPC 改配置、测试直接构造）。

use tracing::warn;
use wind_config::Config;
use wind_config::hotkey::{self, CompiledHotkeys};
use wind_engine::EngineManager;
use wind_keys::keymap;

/// 解析配对表（每项 2 字符 "（）"）为 (左,右) 字符对，忽略非法项。
pub(crate) fn parse_pairs(list: &[String]) -> Vec<(char, char)> {
    list.iter()
        .filter_map(|s| {
            let mut it = s.chars();
            match (it.next(), it.next(), it.next()) {
                (Some(l), Some(r), None) => Some((l, r)),
                _ => None,
            }
        })
        .collect()
}

/// 解析配对跳出键名 → VK 码集合。支持 tab / enter(return) / space / escape(esc)；
/// 大小写与首尾空白不敏感，未知名忽略。这些非可打印键不在 keymap 的 KEY_TABLE
/// （引导/触发用的 OEM 符号键）内，故在此单独映射。
pub(crate) fn parse_jump_out_keys(list: &[String]) -> std::collections::HashSet<u32> {
    list.iter()
        .filter_map(|s| match s.trim().to_lowercase().as_str() {
            "tab" => Some(keymap::VK_TAB),
            "enter" | "return" => Some(keymap::VK_RETURN),
            "space" => Some(keymap::VK_SPACE),
            "escape" | "esc" => Some(keymap::VK_ESCAPE),
            // `right_symbol` 不是键名（右符号是哪个键取决于配对表），由
            // `parse_jump_out_on_right_symbol` 单独解析成开关。
            _ => None,
        })
        .collect()
}

/// 解析英文大小写档位循环的触发键名 → VK。空串 / 未知名 → `None`（＝功能关闭）。
///
/// 值域与 [`parse_jump_out_keys`] 同族（非可打印功能键），另加 `capslock`（Windows 推荐值，
/// 走全局钩子的独立路径）。未知键名的告警在 [`ConfigBundle::build`] 的调用点，静默回落成
/// 「功能关闭」会让用户以为开了。
pub(crate) fn parse_case_cycle_key(name: &str) -> Option<u32> {
    match name.trim().to_lowercase().as_str() {
        "" => None,
        "capslock" | "caps_lock" => Some(keymap::VK_CAPITAL),
        "tab" => Some(keymap::VK_TAB),
        "enter" | "return" => Some(keymap::VK_RETURN),
        "space" => Some(keymap::VK_SPACE),
        "escape" | "esc" => Some(keymap::VK_ESCAPE),
        _ => None,
    }
}

/// `jump_out_keys` 是否含「右符号键本身」这一特殊值 → 打 `）` 跳出已插入的 `（）`。
/// 与 VK 集合分开表示：右符号不是固定按键，取决于当前生效的配对表。
pub(crate) fn parse_jump_out_on_right_symbol(list: &[String]) -> bool {
    list.iter()
        .any(|s| s.trim().to_lowercase() == wind_config::config::JUMP_OUT_RIGHT_SYMBOL)
}

/// 配置 + 其轻量派生缓存的不可变快照；运行时整体原子替换以支持热重载。
/// 重型组件（引擎/方案/词典）不在内，仍需重启才能完全切换。
pub(crate) struct ConfigBundle {
    pub(crate) config: Config,
    pub(crate) compiled_hotkeys: CompiledHotkeys,
    /// 会话态按键绑定（`keys.session_actions` 编译一次），不只是导航（还有 `cancel` 等）。
    /// 动作值域在 `wind-config`、表在 `wind-keys`，本 crate 是唯一同时看得见两边的地方。
    pub(crate) session_keys: keymap::KeyBinds<wind_config::SessionAction>,
    pub(crate) cn_pairs: Vec<(char, char)>,
    pub(crate) en_pairs: Vec<(char, char)>,
    /// 配对跳出键的 VK 码集合（预解析自 `auto_pair.jump_out_keys`，空=不启用）。
    pub(crate) jump_out_keys: std::collections::HashSet<u32>,
    /// 英文大小写档位循环的触发键 VK（预解析自 `input.english_case_cycle_key`）。
    /// `None` = 功能关闭：键即开关，没有第二个闸门。
    pub(crate) english_case_cycle_vk: Option<u32>,
    /// 整句切换键（预解析自 `schema.pinyin.sentence_cycle_key`）。`None` = 功能关闭，键即开关。
    /// 带 `shift` 位：`shift+tab` 与 `tab` 是两个键。
    pub(crate) sentence_cycle_key: Option<keymap::SessionKey>,
    /// 输入右符号本身是否跳出（`jump_out_keys` 含 `right_symbol`）。对称配对不受此项影响。
    pub(crate) jump_out_on_right_symbol: bool,
    /// 英文模式下需 DLL 吃键转发的标点源字符（见 `build` 的合成）。DLL 吃键与本侧出字的同源
    /// 判据，热路径上每键都查，故预计算。有序集合使推送字节可复现。
    pub(crate) custom_en_punct_chars: std::collections::BTreeSet<char>,
    /// 中文模式下产物就是原样半角 ASCII、该让 DLL 透传不吃的标点集合
    /// （见 `wind_punct::chinese_passthrough_punct_chars`）。吃了再吐在非 TSF-aware 宿主上会被
    /// 当成虚拟键码。DLL 吃键与本侧 `should_handle_key` 的同源判据，故预计算。
    pub(crate) cn_passthrough_punct_chars: std::collections::BTreeSet<char>,
    /// 同上，用于英文标点态（中文输入模式 + 标点切英文），是上一份的超集。两份都推给 DLL，
    /// 由它按当下标点态选用（标点态是运行时状态，不进配置快照）。
    pub(crate) en_passthrough_punct_chars: std::collections::BTreeSet<char>,
    /// 空格行配了自定义映射的列（位掩码，全局表 ∪ 各方案表）。中文模式空缓冲时 DLL 据此
    /// 吃下空格（`CONFIG_KEY_SPACE_CUSTOM_COLS`），本侧对吃下的空格必须出字、不能透传。
    pub(crate) space_custom_cols: u8,
    /// 分层按键配置的解析器（当前只收全局 `keys.key_actions` 的预编译引导键表）。
    ///
    /// 放在本 crate 的理由同 `session_keys`。设计见 `docs/design/key-resolver-unification.md`。
    pub(crate) key_resolver: crate::key_resolver::KeyResolver,
    /// 所有方案 `[session_actions]` 里绑过的键 VK（并集，已滤显式 `none`）。
    ///
    /// 当前消费者是 `Coordinator::capslock_bound`（决定装不装 CapsLock 全局钩子），必须用并集：
    /// 钩子是进程级资源，`SetWindowsHookExW` 重复装会留下卸不掉的旧钩子，按活跃方案取值就成了
    /// 切方案反复装卸。
    pub(crate) schema_session_vks: std::collections::BTreeSet<u32>,
    /// 悬停提示（`ui.tooltip.*`）的预解析形态：段模板在这里解析一次，候选循环里只渲染。
    pub(crate) tooltip: crate::tooltip::CompiledTooltip,
}

/// 所有方案 `[key_actions]` 里绑过的纯修饰键 VK（并集）。
///
/// 取并集而非活跃方案那一份：`CompiledHotkeys` 随 activation 推给 C++，按活跃方案裁剪就得
/// 每次切方案后重推。代价只是别的方案里多转发一个不动作的 keyup。详见
/// [`EngineManager::all_key_action_keys`]。
///
/// 枚举源是 `available`，overlay 方案（`hidden = true`）不在内，这是自洽的：进特殊模式不动
/// 活跃方案，overlay 的 `[key_actions]` 没有消费路径。将来要支持它，枚举源与消费路径两处都得改。
/// 见 `docs/design/key-resolver-unification.md` §8。
pub(crate) fn schema_bound_modifier_vks(mgr: &EngineManager) -> std::collections::BTreeSet<u32> {
    mgr.all_key_action_keys()
        .iter()
        .filter_map(|name| keymap::modifier_name_to_vk(name))
        .collect()
}

/// 加载期告警：`keys.session_actions` 里认不出的键名 / 动词。
///
/// 静默忽略会让用户分不清是拼错了还是功能没实现。键名错与动词错分两条报，修法不同。
fn warn_unknown_session_actions(config: &Config) {
    for (name, verb) in &config.keys.session_actions {
        if wind_config::SessionAction::parse_checked(verb).is_none() {
            warn!(
                "keys.session_actions[\"{name}\"] = \"{verb}\"：动词无法识别，该绑定被忽略。\
                 可选 page_prev / page_next / page_screen_prev / page_screen_next / highlight_up / highlight_down / cancel / \
                 select_candidate:N / select_char:N / aux_code / aux_code:page_next / \
                 single_char / single_char:on / single_char:off / single_char:toggle / \
                 single_char:follow / commit_highlighted / commit_code / command:<表达式> / none",
            );
            continue;
        }
        if keymap::session_key_name_to_vk(name).is_none() {
            warn!(
                "keys.session_actions[\"{name}\"]：键名无法识别，该绑定被忽略。\
                 可选 tab / shift+tab / capslock / pageup / pagedown / up / down / left / \
                 right / home / end，以及符号键 minus / equal / lbracket / rbracket / \
                 comma / period / semicolon / quote / slash / backtick / backslash",
            );
        }
    }
}

/// 跨方案的按键并集：可达性的数据源。
///
/// 与「当前方案下这个键干什么」（`Coordinator::bound_action_with_source` / `session_action_for`，
/// 按活跃方案查）是两回事。本结构回答「这个键要不要送到服务端 / 装进程级资源」，必须覆盖所有
/// 方案：这些推送只在握手与热重载发生、切方案不推，按活跃方案取值则切方案后手里是旧表。
///
/// 在配置生效期算一次；它要扫全部方案文件，别在按键路径上调。
#[derive(Default)]
pub(crate) struct SchemaKeyUnion {
    /// 所有方案 `[key_actions]` 里绑过的纯修饰键 VK。
    pub(crate) modifier_vks: std::collections::BTreeSet<u32>,
    /// 所有方案 `[session_actions]` 里绑过的键名（已滤显式 `none`）。存键名而非 VK：编译 TSF
    /// 转发条目要区分 `shift+tab` 与 `tab`。VK 形式在 `ConfigBundle` 里另存一份。
    pub(crate) session_key_names: std::collections::BTreeSet<String>,
    /// 所有方案 `[punct.custom_mappings]` 里配了英半列的源字符（`CONFIG_KEY_CUSTOM_EN_PUNCT`）。
    ///
    /// 并集是安全的：没配英半自定义的键会原样出 ASCII，与透传等价（见
    /// `push_custom_en_punct_config`）。代价只是英文模式下多转发几个标点键。
    pub(crate) punct_en_chars: std::collections::BTreeSet<char>,
    /// 所有已安装方案的码元集里可作首码的非字母字符（并集）。
    ///
    /// 方向与上面几项相反：它要从透传集合里减去。任一方案把它当首码，空缓冲按下就要开始组码，
    /// 透传掉就是切到那个方案后打不出字。取并集同样是保守侧：宁可少透传，不可多透传。
    pub(crate) leading_code_chars: std::collections::BTreeSet<char>,
    /// 所有已安装方案（含 overlay）`[key_actions]` 里绑过的键 VK（并集），同样用于从透传集合
    /// 里减去：引导键（`special:*` / `mix` / 临拼 / 临英）空缓冲时就生效，透传掉那个模式就进不去。
    ///
    /// 枚举源是 `installed_schemas()` 而非 `available`：减数漏了有害，而 `modifier_vks` 是加数，
    /// 用 `available` 无害。按整表排除偏保守（表里也可能有只在会话中生效的动词），只是少透传。
    pub(crate) key_action_vks: std::collections::BTreeSet<u32>,
    /// 任一**已安装**方案的 `[punct] custom_mappings` 覆盖过的标点源字符（并集）。
    ///
    /// 第三个减数。`effective_punct` 在方案声明了该表时整表替换（连 `custom_enabled` 一起换），
    /// 只看全局表会让方案配的键被透传掉、静默失效。
    pub(crate) custom_covered_chars: std::collections::BTreeSet<char>,
    /// 任一**已安装**方案的自定义标点表里，空格行配了值的列（位并集，见
    /// `wind_punct::custom_space_cols`）。理由同上一项：方案表整表替换，只看全局表会漏。
    pub(crate) space_custom_cols: u8,
}

/// 算一次跨方案并集。
pub(crate) fn schema_key_union(mgr: &EngineManager) -> SchemaKeyUnion {
    SchemaKeyUnion {
        modifier_vks: schema_bound_modifier_vks(mgr),
        session_key_names: mgr.all_session_action_keys(),
        punct_en_chars: schema_custom_en_punct_chars(mgr),
        leading_code_chars: schema_leading_code_chars(mgr),
        key_action_vks: mgr
            .installed_key_action_keys()
            .iter()
            .filter_map(|name| crate::key_resolver::key_action_name_to_vk(name))
            .collect(),
        custom_covered_chars: schema_custom_covered_punct_chars(mgr),
        space_custom_cols: schema_custom_space_cols(mgr),
    }
}

/// 任一已安装方案的自定义标点表里空格行配了值的列（位并集）。见
/// [`SchemaKeyUnion::space_custom_cols`]，写法同 [`schema_custom_covered_punct_chars`]。
pub(crate) fn schema_custom_space_cols(mgr: &EngineManager) -> u8 {
    let conv = wind_transform::punctuation::PunctuationConverter::new();
    let mut mask = 0;
    for id in mgr.installed_schemas() {
        let Some(table) = mgr.behavior_for(&id).punct_custom_mappings.clone() else {
            continue;
        };
        let punct = wind_config::config::PunctConfig {
            custom_enabled: !table.is_empty(),
            custom_mappings: table,
            ..Default::default()
        };
        mask |= wind_punct::custom_space_cols(&conv, &punct);
    }
    mask
}

/// 任一已安装方案的自定义标点表覆盖过的源字符（并集）。见
/// [`SchemaKeyUnion::custom_covered_chars`]。
///
/// 写法与 [`schema_custom_en_punct_chars`] 对齐：给每个方案造一份只含该方案表的 `PunctConfig`，
/// 再问同一个判据函数 `wind_punct::custom_covered_punct_chars`，不另写一份以免与出字侧漂移。
pub(crate) fn schema_custom_covered_punct_chars(
    mgr: &EngineManager,
) -> std::collections::BTreeSet<char> {
    let conv = wind_transform::punctuation::PunctuationConverter::new();
    let mut out = std::collections::BTreeSet::new();
    for id in mgr.installed_schemas() {
        let Some(table) = mgr.behavior_for(&id).punct_custom_mappings.clone() else {
            continue; // 跟随全局 ⇒ 已由全局那份贡献，不重复算
        };
        let punct = wind_config::config::PunctConfig {
            custom_enabled: !table.is_empty(),
            custom_mappings: table,
            ..Default::default()
        };
        out.extend(wind_punct::custom_covered_punct_chars(&conv, &punct));
    }
    out
}

/// 所有已安装方案的码元集里可作首码的非字母字符（并集），含开着的符号通配键。见
/// [`SchemaKeyUnion::leading_code_chars`]。
///
/// 只收非字母：字母本就是默认码元，与标点集合不可能相交，白扫一遍。
///
/// 混输方案取到的是它自身的 `[engine.codetable]`（通常为空），但主方案本身也会被单独扫到，
/// 并集不变。枚举源用 `installed_schemas()`：overlay 方案（快符等）最可能把符号配成码元。
///
/// 必须走 `schema_lead_inputs`（纯 TOML 读），不能用 `input_chars_of`：后者会构建引擎、把码表
/// 词库整个载入，而本函数的调用点（构造、配置刷新、热重载）都不该阻塞，也不该让非活跃方案常驻。
pub(crate) fn schema_leading_code_chars(mgr: &EngineManager) -> std::collections::BTreeSet<char> {
    let mut out = std::collections::BTreeSet::new();
    for id in mgr.installed_schemas() {
        // 读不到方案文件 ⇒ 该方案本就用不了，跳过不影响可达性。
        let Some((cs, wildcard)) = mgr.schema_lead_inputs(&id) else {
            continue;
        };
        // 符号通配键同样可作首码，不在 `input_chars` 里也得算上，否则会进透传集、首位收不到。
        out.extend(wildcard);
        if cs.is_default_alpha() {
            continue; // 默认集只有字母，不可能与标点相撞
        }
        for ch in cs.chars() {
            if !ch.is_ascii_alphabetic() && cs.contains_leading(ch) {
                out.insert(ch);
            }
        }
    }
    out
}

/// 所有已安装方案的方案级标点表里，配了英半列的源字符（并集）。
///
/// 枚举源是 `installed_schemas()`：overlay 方案（快符等）的标点表在特殊模式下实打实要出字
/// （`effective_punct` 走 `effective_data_schema`），与 `all_key_action_keys` 用 `available` 不同。
/// 判据复用 [`wind_punct::custom_english_punct_chars`]，吃键与出字必须同源。
pub(crate) fn schema_custom_en_punct_chars(
    mgr: &EngineManager,
) -> std::collections::BTreeSet<char> {
    let mut out = std::collections::BTreeSet::new();
    for id in mgr.installed_schemas() {
        let Some(table) = mgr.behavior_for(&id).punct_custom_mappings.clone() else {
            continue; // 跟随全局 ⇒ 已由全局那份贡献，不重复算
        };
        // 只含该方案表的 PunctConfig，与 `effective_punct` 一致（整表替换连开关一起换）。
        let punct = wind_config::config::PunctConfig {
            custom_enabled: !table.is_empty(),
            custom_mappings: table,
            ..Default::default()
        };
        out.extend(wind_punct::custom_english_punct_chars(&punct));
    }
    out
}

impl ConfigBundle {
    /// `schema_keys` 是跨方案并集（见 [`SchemaKeyUnion`]）。其中 `modifier_vks`
    /// （见 [`schema_bound_modifier_vks`]）要追加进 `key_up` 转发集：`CompiledHotkeys` 编译自全局
    /// config，这是方案里 keyup 类绑定唯一的可达性来源。
    pub(crate) fn build(mut config: Config, schema_keys: &SchemaKeyUnion) -> Self {
        // 归一化 + 存量迁移放在这里而非 `Config::load()`：挂在 load 上会漏掉 RPC 改配置等路径，
        // 设置页保存一次就绕过迁移。`normalize` 幂等。
        config.normalize();
        let mut compiled_hotkeys = hotkey::Compiler::new(config.clone()).compile();
        // action 用 `schema_bound` 而非 `toggle_mode`：`is_toggle_mode_keycode` 按 action 过滤，
        // 混用会让只在某方案里绑了 rshift 的键在所有方案里都切中英文。
        for vk in &schema_keys.modifier_vks {
            // 修饰键的 hash 带通用位+具体位（同 `compile_toggle_mode_key`）：C++
            // `GetCurrentModifiers()` 对修饰键同时返回两者。
            if let Some(hash) = hotkey::compile_modifier_key_up_hash(*vk) {
                compiled_hotkeys.key_up.push(hotkey::HotkeyEntry {
                    tsf_hash: hash,
                    match_hash: hash,
                    action: "schema_bound".to_string(),
                });
            }
        }
        // 方案级 `[session_actions]` 的键也要进转发表（取并集），否则 TSF 不把它们送到服务端。
        // 编译走 `hotkey::compile_session_key`，与全局那段同源。按 `tsf_hash` 去重，避免推给 C++
        // 的表凭空变大。
        let mut schema_session_vks = std::collections::BTreeSet::new();
        {
            let mut seen_up: std::collections::HashSet<u32> =
                compiled_hotkeys.key_up.iter().map(|e| e.tsf_hash).collect();
            let mut seen_down: std::collections::HashSet<u32> = compiled_hotkeys
                .key_down
                .iter()
                .map(|e| e.tsf_hash)
                .collect();
            for name in &schema_keys.session_key_names {
                if let Some(k) = keymap::session_key_name_to_vk(name) {
                    schema_session_vks.insert(k.vk);
                }
                let Some((to_key_up, entry)) = hotkey::compile_session_key(name) else {
                    continue;
                };
                if to_key_up {
                    if seen_up.insert(entry.tsf_hash) {
                        compiled_hotkeys.key_up.push(entry);
                    }
                } else if seen_down.insert(entry.tsf_hash) {
                    compiled_hotkeys.key_down.push(entry);
                }
            }
        }
        warn_unknown_session_actions(&config);
        // 会话态按键绑定：`effective_session_actions()` = 四组键组配置的展开 ⊕ `session_actions`
        // （后者优先）。合并只在这里发生，不写回存储：设置页的勾选框读的是存储层。
        // 表直接持有 `SessionAction`；显式 `none` 与写错的动词在此过滤（后者已由上一行告警）。
        let effective_session = config.effective_session_actions();
        let session_keys =
            keymap::KeyBinds::from_binds(effective_session.iter().filter_map(|(name, verb)| {
                let action = wind_config::SessionAction::parse(verb);
                action.is_enabled().then_some((name.as_str(), action))
            }));
        let cn_pairs = parse_pairs(&config.input.auto_pair.chinese_pairs);
        let en_pairs = parse_pairs(&config.input.auto_pair.english_pairs);
        let jump_out_keys = parse_jump_out_keys(&config.input.auto_pair.jump_out_keys);
        let english_case_cycle_vk = parse_case_cycle_key(&config.input.english_case_cycle_key);
        if english_case_cycle_vk.is_none() && !config.input.english_case_cycle_key.trim().is_empty()
        {
            // 写了值却解析不出来：用户以为开了，实际是关的。
            warn!(
                "input.english_case_cycle_key = {:?} 不是可识别的键名，英文大小写档位循环按关闭处理；                 可用值：capslock / tab / enter / space / escape（Windows 建议 capslock，macOS 建议 tab）",
                config.input.english_case_cycle_key
            );
        }
        // 整句切换键与会话动作同一张键名表（`session_key_name_to_vk`），组合期间 C++ 本就转发。
        let sentence_cycle_key = parse_sentence_cycle_key(
            "schema.pinyin.sentence_cycle_key",
            &config.schema.pinyin.sentence_cycle_key,
        );
        // 码表那把键按方案折叠、运行期现取（见 `Coordinator::active_sentence_cycle_key`），
        // 这里只为全局值告警。
        parse_sentence_cycle_key(
            "schema.codetable.sentence_cycle_key",
            &config.schema.codetable.sentence_cycle_key,
        );
        let jump_out_on_right_symbol =
            parse_jump_out_on_right_symbol(&config.input.auto_pair.jump_out_keys);
        // 英文模式下需要 DLL 吃下转发的标点键 = 全局英半列自定义 ∪ 英文智能符号参与集 ∪
        // 任一方案的英半列自定义（并集理由见 [`SchemaKeyUnion::punct_en_chars`]）。
        let custom_en_punct_chars: std::collections::BTreeSet<char> =
            wind_punct::custom_english_punct_chars(&config.input.punct)
                .into_iter()
                .chain(wind_punct::english_smart_source_chars(&config.input))
                .chain(schema_keys.punct_en_chars.iter().copied())
                .collect();
        // 预编译须在 `normalize()` 之后：存量迁移会往 `key_actions` 折算。
        let key_resolver = crate::key_resolver::KeyResolver::build(&config);
        // 中文模式下该透传不吃的标点，两层判据合成，缺一层都是静默失效：
        //   ① 标点转换层（`wind_punct`）：自定义映射 / 中文标点表 / 全半角任一步会改写它则必须吃。
        //      转换器只用来 peek、不推进引号交替态，临时建一个即可。
        //   ② 按键占用层（下面的过滤）：空缓冲也生效的绑定（方案首码、引导键）与方案级自定义表。
        //      会话态绑定不必排除，两侧透传闸门本就带 `!hasInputSession`。两个标点态共用这道过滤。
        let not_occupied = |ch: char| -> bool {
            // 方案级自定义表覆盖了它（全局表由 `wind_punct` 那层判）。
            if schema_keys.custom_covered_chars.contains(&ch) {
                return false;
            }
            // 任一方案把它当码元首码，见 `SchemaKeyUnion::leading_code_chars`。
            if schema_keys.leading_code_chars.contains(&ch) {
                return false;
            }
            let Some(vk) = crate::key_convert::punct_source_vk(ch) else {
                // 反查不到说明它不是主键盘标点键产出的，来路不明就别透传。
                return false;
            };
            // 引导键：方案层取并集，全局层查 `key_resolver`，与 `bound_action_for` 的链一致。
            !schema_keys.key_action_vks.contains(&vk) && key_resolver.global_lead(vk).is_none()
        };
        let cn_passthrough_punct_chars: std::collections::BTreeSet<char> =
            wind_punct::chinese_passthrough_punct_chars(
                &wind_transform::punctuation::PunctuationConverter::new(),
                &config.input,
            )
            .into_iter()
            .filter(|&ch| not_occupied(ch))
            .collect();
        // 英文标点态那份（超集，见 `wind_punct::english_passthrough_punct_chars`）。
        let en_passthrough_punct_chars: std::collections::BTreeSet<char> =
            wind_punct::english_passthrough_punct_chars(&config.input)
                .into_iter()
                .filter(|&ch| not_occupied(ch))
                .collect();
        // 取并集同 `custom_en_punct_chars`；多吃的列由出字侧兜住（吃下的空格照样上屏）。
        let space_custom_cols = wind_punct::custom_space_cols(
            &wind_transform::punctuation::PunctuationConverter::new(),
            &config.input.punct,
        ) | schema_keys.space_custom_cols;
        // `input.rare_char.include_blocks` 不在这里解析：它在 `EngineManager` 装配
        // `CharsetRegistry` 时统一吃进去并告警。全进程只装配一份，重复解析可能得出不一致的类。
        let tooltip = crate::tooltip::CompiledTooltip::compile(&config.ui.tooltip);
        Self {
            config,
            compiled_hotkeys,
            session_keys,
            cn_pairs,
            en_pairs,
            jump_out_keys,
            english_case_cycle_vk,
            sentence_cycle_key,
            jump_out_on_right_symbol,
            custom_en_punct_chars,
            cn_passthrough_punct_chars,
            en_passthrough_punct_chars,
            space_custom_cols,
            key_resolver,
            schema_session_vks,
            tooltip,
        }
    }
}

/// 解析整句切换键。空串 = 关闭；写了值却认不出则告警后按关闭处理。
pub(crate) fn parse_sentence_cycle_key(path: &str, raw: &str) -> Option<keymap::SessionKey> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = keymap::session_key_name_to_vk(raw);
    if parsed.is_none() {
        warn!(
            "{path} = {raw:?} 不是可识别的键名，整句切换按关闭处理；\
             可用值同 keys.session_actions 的键名（如 tab / backtick，可带 shift+）"
        );
    }
    parsed
}

#[cfg(test)]
#[path = "config_bundle_reload_tests.rs"]
mod reload_tests;
