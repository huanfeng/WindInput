//! [`ConfigBundle`]：配置 + 其轻量派生缓存的不可变快照，运行时整体原子替换以支持热重载。
//!
//! `build` 是**所有**配置生效的必经之路（启动、热重载、RPC 改配置、测试直接构造）。
//! （自 coordinator.rs 平移，纯搬运。）

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
    /// 会话态按键绑定（`keys.session_actions` 编译一次）。**不只是导航**——二期起还装
    /// `cancel`，故不叫 `nav_keys`。动作值域在 `wind-config`，表在 `wind-keys`，两者由
    /// 本结构体所在的 crate 拼起来（唯一同时看得见两边的地方）。
    pub(crate) session_keys: keymap::KeyBinds<wind_config::SessionAction>,
    pub(crate) cn_pairs: Vec<(char, char)>,
    pub(crate) en_pairs: Vec<(char, char)>,
    /// 配对跳出键的 VK 码集合（预解析自 `auto_pair.jump_out_keys`，空=不启用）。
    pub(crate) jump_out_keys: std::collections::HashSet<u32>,
    /// 输入右符号本身是否跳出（`jump_out_keys` 含 `right_symbol`）。对称配对不受此项影响。
    pub(crate) jump_out_on_right_symbol: bool,
    /// 「英半列有自定义标点映射」的源字符集合（预解析自 `punct.custom_mappings`，空=英文模式
    /// 行为与历史一致）。这是 DLL 吃键与本侧出字的**同源判据**，且在英文标点键的热路径上每键
    /// 都要查——故预计算，别在按键时重新遍历 `custom_mappings`。有序集合使推送字节可复现。
    pub(crate) custom_en_punct_chars: std::collections::BTreeSet<char>,
    /// 分层按键配置的解析器（当前只收全局 `keys.key_actions` 的预编译引导键表）。
    ///
    /// 与 `session_keys` 同源的理由：动作值域在 `wind-config`、键名解析在 `wind-keys`，
    /// 本 crate 是唯一同时看得见两者的地方。设计见
    /// `docs/design/key-resolver-unification.md`。
    pub(crate) key_resolver: crate::key_resolver::KeyResolver,
    /// **所有**方案 `[session_actions]` 里绑过的键 VK（并集，已滤显式 `none`）。
    ///
    /// 供需要「任一方案绑过没有」的消费者用，当前是 `Coordinator::capslock_bound`
    /// （决定装不装 CapsLock 全局钩子）。★ 那里**必须**用并集而不是活跃方案那一份：
    /// 钩子是进程级资源且 `SetWindowsHookExW` 重复装会留下卸不掉的旧钩子，按活跃方案
    /// 取值就成了「切方案反复装卸」。同 `schema_bound_modifier_vks` 那条理由，只是它
    /// 落在 Rust 侧而非 C++ 边界。
    pub(crate) schema_session_vks: std::collections::BTreeSet<u32>,
    /// 生僻字模式额外纳入的区块（预解析自 `input.rare_char.include_blocks`）。
    ///
    /// 与 `jump_out_keys` / `custom_en_punct_chars` 同族：**在这里解析而不是在消费点**，
    /// 因为消费点（`retain_rare_admitted`）在候选刷新路径上，而解析要按名字线性查块表、
    /// 还要把拼错的名字 warn 出来——那种 warn 放在热路径上会刷屏。
    pub(crate) rare_char_blocks: wind_candidate::BlockMask,
}

/// 所有方案 `[key_actions]` 里绑过的纯修饰键 VK（并集）。
///
/// 取并集而非活跃方案那一份：`CompiledHotkeys` 随 activation 推给 C++，按活跃方案裁剪
/// 就得在每次切方案后重推，漏一次的表现是「刚切完方案这个键不灵、点下别的窗口又灵了」。
/// 并集是静态的，代价只是别的方案里多转发一个不动作的 keyup（keydown 侧纯修饰键一律
/// 放行，宿主无感）。理由详见 [`EngineManager::all_key_action_keys`]。
///
/// ⚠️ **枚举源是 `available`，overlay 方案（`hidden = true`）不在内——这是自洽，不是漏。**
/// overlay 方案的 `[key_actions]` 没有消费路径：`active_key_actions()` 按 `EngineManager`
/// 的**活跃方案**取表，而进特殊模式只改 `State.active`（`ModeKind::Special`），不动活跃方案
/// ⇒ overlay 模式下查的仍是主方案那张表。把枚举源换成 `installed_schemas` 会让一批查不到
/// 消费点的键进转发集，是纯粹的多余转发。
///
/// ⇒ 将来真要支持 overlay 自己的 `[key_actions]`，**枚举源与消费路径两处都得改**；
/// 届时若把本函数收编成 `reachability()`，那个名字承诺「全集」而实现只覆盖 `available`，
/// 需要一并处置（叫 `reachability_of_available()`，或在注释里写明）。
/// 见 `docs/design/key-resolver-unification.md` §8。
pub(crate) fn schema_bound_modifier_vks(mgr: &EngineManager) -> std::collections::BTreeSet<u32> {
    mgr.all_key_action_keys()
        .iter()
        .filter_map(|name| keymap::modifier_name_to_vk(name))
        .collect()
}

/// 加载期告警：`keys.session_actions` 里认不出的键名 / 动词。
///
/// ★ 静默忽略与「这个功能坏了」完全同形——用户无从分辨自己拼错了、还是该功能压根没实现。
/// 这是 `is_supported_key_action` 当初立的口径，本表沿用。
///
/// 分两条报而不是合并成一条：键名错与动词错的修法不同，合并后用户还要自己二选一去试。
fn warn_unknown_session_actions(config: &Config) {
    for (name, verb) in &config.keys.session_actions {
        if wind_config::SessionAction::parse_checked(verb).is_none() {
            warn!(
                "keys.session_actions[\"{name}\"] = \"{verb}\"：动词无法识别，该绑定被忽略。\
                 可选 page_prev / page_next / highlight_up / highlight_down / cancel / \
                 select_candidate:N / select_char:N / aux_code / aux_code:page_next / none",
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

/// 跨方案的按键**并集**：可达性的数据源。
///
/// ★ 与「当前方案下这个键干什么」是两回事，别混用。语义表按活跃方案查
/// （`Coordinator::bound_action_with_source` / `session_action_for`）；本结构回答的是
/// 「这个键要不要送到服务端 / 要不要装进程级资源」，必须覆盖**所有**方案，否则切方案后
/// 手里是旧表，表现为「刚切完方案这个键不灵、点下别的窗口又灵了」。
///
/// 在配置生效期算一次（构造 / 热重载），不是热路径——但它要 `read_schema` 扫全部可用方案，
/// 别在按键路径上调。
#[derive(Default)]
pub(crate) struct SchemaKeyUnion {
    /// 所有方案 `[key_actions]` 里绑过的纯修饰键 VK。
    pub(crate) modifier_vks: std::collections::BTreeSet<u32>,
    /// 所有方案 `[session_actions]` 里绑过的**键名**（`EngineManager` 侧已滤掉显式 `none`）。
    ///
    /// 存键名而非 VK：编译 TSF 转发条目要区分 `shift+tab` 与 `tab`，而 VK 集合把
    /// `shift+` 前缀丢了。VK 形式在 `ConfigBundle` 里另存一份供快速判定。
    pub(crate) session_key_names: std::collections::BTreeSet<String>,
    /// 所有方案 `[punct.custom_mappings]` 里配了**英半列**的源字符。
    ///
    /// 与上面两项同一条理由，只是这次的资源在 C++ 那边：`CONFIG_KEY_CUSTOM_EN_PUNCT` 只在
    /// 握手与配置热重载时推送，**切方案不推**。按活跃方案裁剪就得给五条切方案路径各接一次
    /// 推送，漏一条的表现是「刚切完方案这个键不灵」——正是本结构存在的那个坑。
    ///
    /// 并集在这里是**安全**的，理由现成：集合内没配英半自定义的键会原样出 ASCII，与透传
    /// 等价（见 `push_custom_en_punct_config` 与 `wind_punct::english_smart_source_chars`
    /// 的文档）。代价只是英文模式下多转发几个标点键。
    pub(crate) punct_en_chars: std::collections::BTreeSet<char>,
}

/// 算一次跨方案并集。
pub(crate) fn schema_key_union(mgr: &EngineManager) -> SchemaKeyUnion {
    SchemaKeyUnion {
        modifier_vks: schema_bound_modifier_vks(mgr),
        session_key_names: mgr.all_session_action_keys(),
        punct_en_chars: schema_custom_en_punct_chars(mgr),
    }
}

/// 所有**已安装**方案的方案级标点表里，配了英半列的源字符（并集）。
///
/// ⚠️ 枚举源是 `installed_schemas()` 而非 `available`：overlay 方案（`hidden = true`）不进
/// `available`，而快符这类方案恰恰是最想配自己符号表的一类。这与 `all_key_action_keys` 用
/// `available` 是**两种情况**——那里的键在 overlay 下查不到消费点，本表的键则实打实要出字
/// （`effective_punct` 走 `effective_data_schema`，特殊模式正是它照顾的对象）。
///
/// ★ 判据复用 [`wind_punct::custom_english_punct_chars`] 而不是在此另写一份「哪一列非空」：
/// 吃键判据与出字判据必须同源，两份迟早漂移成「吃了再吐」丢键。
pub(crate) fn schema_custom_en_punct_chars(
    mgr: &EngineManager,
) -> std::collections::BTreeSet<char> {
    let mut out = std::collections::BTreeSet::new();
    for id in mgr.installed_schemas() {
        let Some(table) = mgr.behavior_for(&id).punct_custom_mappings.clone() else {
            continue; // 跟随全局 ⇒ 已由全局那份贡献，不重复算
        };
        // 造一份「只含该方案表」的 PunctConfig 去问同一个判据函数，与 `effective_punct`
        // 合成生效配置的方式保持一致（整表替换连开关一起换）。
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
    /// `schema_keys` 打包两份跨方案并集（见 [`SchemaKeyUnion`]）。
    /// 其中 `modifier_vks` = 所有方案 `[key_actions]` 里出现过的**纯修饰键** VK
    /// （见 [`Coordinator::schema_bound_modifier_vks`]）。它们要追加进 `key_up` 转发集，
    /// 否则 TSF 根本不把这些键的 keyup 送过来——`CompiledHotkeys` 编译自全局 config，
    /// 方案文件不在其中，这是 keyup 类绑定唯一的可达性来源。
    pub(crate) fn build(mut config: Config, schema_keys: &SchemaKeyUnion) -> Self {
        // 归一化 + 存量迁移。放在这里而不是只在 `Config::load()` 里：本函数是**所有**
        // 配置生效的必经之路（启动、热重载、RPC 改配置后的 `refresh_config_in_memory`、
        // 测试直接构造）。挂在 load 上会漏掉后三条——设置页保存一次就绕过了迁移，
        // 而消费点已改成只读新表，表现是「保存后引导键全失效」。`normalize` 幂等。
        config.normalize();
        let mut compiled_hotkeys = hotkey::Compiler::new(config.clone()).compile();
        // action 用专门的 `schema_bound` 而不是 `toggle_mode`：`is_toggle_mode_keycode` 按
        // action 过滤，混用会让「只在某方案里绑了 rshift」的键在所有方案里都切中英文
        // （与 `select_key_groups` 那次踩的是同一个坑，见该函数的 ⚠ 注释）。
        for vk in &schema_keys.modifier_vks {
            // 修饰键的 hash 要带通用位+具体位，与 `compile_toggle_mode_key` 同构：
            // C++ `GetCurrentModifiers()` 对修饰键同时返回两者，只带一边匹配不上。
            if let Some(hash) = hotkey::compile_modifier_key_up_hash(*vk) {
                compiled_hotkeys.key_up.push(hotkey::HotkeyEntry {
                    tsf_hash: hash,
                    match_hash: hash,
                    action: "schema_bound".to_string(),
                });
            }
        }
        // 方案级 `[session_actions]` 的键也要进转发表，否则 TSF 根本不把它们送到服务端，
        // 表现是「方案里配了完全没反应」。与 modifier_vks 同一条理由（取并集而非活跃方案
        // 那一份），但这批键形态更杂——编译规则走 `hotkey::compile_session_key`，与全局
        // 那段**同源**，避免「同一个键名全局能用、写进方案就不转发」。
        //
        // 去重按 `tsf_hash`：全局已登记过的键不再追加。重复条目本身不改变行为（`.find()`
        // 先到先得，而两条一模一样），但会让推给 C++ 的表凭空变大，也让「切方案前后推送
        // 字节不变」这条验证手段失去意义。
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
        // 会话态按键绑定。数据源是 `effective_session_actions()`＝四组键组配置的展开结果
        // ⊕ `session_actions`（后者优先）。
        //
        // ★ 合并只在这里发生，**配置文件里两套各自保持原样**——设置页的四个勾选框读的正是
        // 存储层，折算若写回存储，界面就永远显示为空。判据见该函数的文档。
        //
        // ★ 这里是两个 crate 的接缝：动作值域（`SessionAction`）在 `wind-config`，绑定表
        // （`KeyBinds`）在 `wind-keys`，而 `wind-config` 不能反向依赖 `wind-keys`（后者经
        // `wind-cmdbar` 依赖它，加进去成环）。本函数是唯一同时看得见两者的地方。
        //
        // 表**直接持有 `SessionAction`**，不再翻译成某个中间枚举——一期那层 `NavAction`
        // 映射在加 `cancel` 时立刻成了瓶颈（新动词没有对应的 `NavAction`）。
        // 显式 `none` 与写错的动词都在此过滤掉；后者由上一行的 `warn_unknown_session_actions`
        // 报出来，静默忽略与「功能坏了」完全同形。
        let effective_session = config.keys.effective_session_actions();
        let session_keys =
            keymap::KeyBinds::from_binds(effective_session.iter().filter_map(|(name, verb)| {
                let action = wind_config::SessionAction::parse(verb);
                action.is_enabled().then_some((name.as_str(), action))
            }));
        let cn_pairs = parse_pairs(&config.input.auto_pair.chinese_pairs);
        let en_pairs = parse_pairs(&config.input.auto_pair.english_pairs);
        let jump_out_keys = parse_jump_out_keys(&config.input.auto_pair.jump_out_keys);
        let jump_out_on_right_symbol =
            parse_jump_out_on_right_symbol(&config.input.auto_pair.jump_out_keys);
        // 英文模式下需要 DLL 吃下转发的标点键 = 「全局配了英半列自定义」∪「英文智能符号参与集」
        // ∪「**任一方案**配了英半列自定义」。三个来源都是「英文半角下 DLL 默认透传、core 却
        // 需要收到」的键，合并成一份推送即可（DLL 侧判据是数据驱动的字符集查表，集合变大自动
        // 多吃，无需改 C++）。
        //
        // 第三项取跨方案**并集**而非活跃方案那一份，理由见 [`SchemaKeyUnion::punct_en_chars`]。
        let custom_en_punct_chars: std::collections::BTreeSet<char> =
            wind_punct::custom_english_punct_chars(&config.input.punct)
                .into_iter()
                .chain(wind_punct::english_smart_source_chars(&config.input))
                .chain(schema_keys.punct_en_chars.iter().copied())
                .collect();
        // 预编译放在 `normalize()` 之后：`trigger_keys` 收编等存量迁移会往 `key_actions`
        // 折算，早于迁移编译就会漏掉那批键。
        let key_resolver = crate::key_resolver::KeyResolver::build(&config);
        // 拼错的区块名 warn 后跳过，不让整份列表失效（同 schema.frequency.exclude_blocks）。
        // 「表情符號」这种繁简混写肉眼极难分辨，静默跳过的表现就是「配了没反应」。
        let (rare_char_blocks, unknown_blocks) =
            wind_candidate::BlockMask::from_config(&config.input.rare_char.include_blocks);
        if !unknown_blocks.is_empty() {
            tracing::warn!(
                "input.rare_char.include_blocks 有 {} 个名字不认识、已跳过: {}（应为区块名、\"其它\" 或预设组名 emoji）",
                unknown_blocks.len(),
                unknown_blocks.join("、")
            );
        }
        Self {
            config,
            compiled_hotkeys,
            session_keys,
            cn_pairs,
            en_pairs,
            jump_out_keys,
            jump_out_on_right_symbol,
            custom_en_punct_chars,
            key_resolver,
            schema_session_vks,
            rare_char_blocks,
        }
    }
}

#[cfg(test)]
mod reload_tests {
    //! 热重载基础：验证 ConfigBundle 能从 Config 正确重建轻量派生缓存。
    //! （reload_user_config 走磁盘 IO 不在此测；这里测其核心——从配置重建派生状态。）
    use super::*;

    /// 方案级 `[session_actions]` 的键**必须**进 TSF 转发表。
    ///
    /// 不进表的后果不是「优先级低」而是**服务端根本收不到这个键**——C++ 按转发表决定送不
    /// 送过来。表现是「方案里配了完全没反应」，与配错了同形，用户无从分辨。
    #[test]
    fn schema_session_keys_enter_the_forward_table() {
        let cfg = Config::default();
        let bare = ConfigBundle::build(cfg.clone(), &Default::default());
        // `home` 是功能键（非可打印）⇒ 走 key_down + FORWARD_ONLY 那条，且不在出厂的任何
        // 键组里（page_keys = pageupdown/minus_equal、highlight_keys = arrows/tab、
        // select_key_groups = semicolon_quote）。
        let vk = keymap::session_key_name_to_vk("home").unwrap().vk;
        assert!(
            !bare
                .compiled_hotkeys
                .key_down
                .iter()
                .any(|e| e.match_hash == vk),
            "前置条件：被测键须是出厂默认未登记的，否则测不到「新增登记」这件事"
        );
        let union = SchemaKeyUnion {
            modifier_vks: Default::default(),
            session_key_names: ["home".to_string()].into_iter().collect(),
            punct_en_chars: Default::default(),
        };
        let with_schema = ConfigBundle::build(cfg, &union);
        assert!(
            with_schema
                .compiled_hotkeys
                .key_down
                .iter()
                .any(|e| e.match_hash == vk),
            "方案级会话态键必须进 key_down 转发表，否则服务端根本收不到"
        );
        assert!(
            with_schema.schema_session_vks.contains(&vk),
            "VK 形式也要留一份，供 capslock_bound 那类「任一方案绑过没有」的判定"
        );
    }

    /// ★ 去重：方案绑的键若全局已登记，不再追加第二条。
    ///
    /// 重复条目不改变行为（`.find()` 先到先得且两条一模一样），但会让推给 C++ 的表随方案
    /// 数量膨胀，也让「切方案前后推送字节不变」这条验证手段失去意义——那正是并集策略是否
    /// 生效的唯一凭据。
    #[test]
    fn schema_session_key_already_registered_globally_is_not_duplicated() {
        let mut cfg = Config::default();
        cfg.keys
            .session_actions
            .insert("home".to_string(), "page_prev".to_string());
        let global_only = ConfigBundle::build(cfg.clone(), &Default::default());
        let union = SchemaKeyUnion {
            modifier_vks: Default::default(),
            session_key_names: ["home".to_string()].into_iter().collect(),
            punct_en_chars: Default::default(),
        };
        let both = ConfigBundle::build(cfg, &union);
        assert_eq!(
            both.compiled_hotkeys.key_down.len(),
            global_only.compiled_hotkeys.key_down.len(),
            "全局已登记的键，方案级不该再追加一条"
        );
    }

    #[test]
    fn config_bundle_rebuilds_pairs_from_config() {
        let mut cfg = Config::default();
        cfg.input.auto_pair.chinese_pairs = vec!["（）".to_string(), "【】".to_string()];
        cfg.input.auto_pair.english_pairs = vec!["()".to_string()];
        let b = ConfigBundle::build(cfg, &Default::default());
        assert_eq!(b.cn_pairs, vec![('（', '）'), ('【', '】')]);
        assert_eq!(b.en_pairs, vec![('(', ')')]);
    }

    #[test]
    fn parse_jump_out_keys_maps_names_to_vk() {
        // 支持的键名（大小写/空白不敏感），未知名忽略。
        let set = parse_jump_out_keys(&[
            " Tab ".into(),
            "ENTER".into(),
            "space".into(),
            "esc".into(),
            "unknown".into(),
        ]);
        assert!(set.contains(&keymap::VK_TAB));
        assert!(set.contains(&keymap::VK_RETURN)); // enter → VK_RETURN
        assert!(set.contains(&keymap::VK_SPACE));
        assert!(set.contains(&keymap::VK_ESCAPE)); // esc → VK_ESCAPE
        assert_eq!(set.len(), 4); // "unknown" 被忽略
        // "return" 别名等价 enter
        assert!(parse_jump_out_keys(&["return".into()]).contains(&keymap::VK_RETURN));
        // 空配置 → 空集（不启用）
        assert!(parse_jump_out_keys(&[]).is_empty());
    }

    #[test]
    fn config_bundle_parses_jump_out_keys() {
        let mut cfg = Config::default();
        cfg.input.auto_pair.jump_out_keys = vec!["tab".into(), "enter".into()];
        let b = ConfigBundle::build(cfg, &Default::default());
        assert!(b.jump_out_keys.contains(&keymap::VK_TAB));
        assert!(b.jump_out_keys.contains(&keymap::VK_RETURN));
        assert_eq!(b.jump_out_keys.len(), 2);
    }

    #[test]
    fn config_bundle_carries_config_values() {
        // 改配置 → 重建 bundle → bundle.config 反映新值（热重载替换后读取生效的基础）。
        let mut cfg = Config::default();
        cfg.input.symbol.smart_mode = true;
        cfg.ui.candidate.per_page = 9;
        let b = ConfigBundle::build(cfg, &Default::default());
        assert!(b.config.input.symbol.smart_mode);
        assert_eq!(b.config.ui.candidate.per_page, 9);
    }
}
