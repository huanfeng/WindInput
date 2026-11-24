/// 覆盖层在构造时就位：不走重载路径（`engines_gen` 仍为 0），生效配置已是 `[mobile.grammar]`。
#[test]
fn overlay_applied_at_construction_without_rebuild() {
    let mut cfg = Config::default();
    cfg.schema.pinyin.grammar.weight = 0.0;
    cfg.mobile.grammar.weight = 0.7;
    let m = EngineManager::with_store_override_overlay(
        &cfg,
        None,
        None,
        None,
        wind_config::ConfigOverlay::Mobile,
    );
    assert_eq!(m.config_overlay(), wind_config::ConfigOverlay::Mobile);
    assert_eq!(m.pinyin_global().grammar.weight, 0.7);
    assert_eq!(
        m.engines_gen.load(std::sync::atomic::Ordering::Acquire),
        0,
        "构造期不得走重载路径"
    );
    let base = EngineManager::new(&cfg, None);
    assert_eq!(base.pinyin_global().grammar.weight, 0.0);
}

use super::*;

#[test]
fn reverse_index_keeps_pinned_schemas() {
    let pins = vec!["pinyin".to_string(), "wbx".to_string()];
    assert!(
        reverse_index_keeps("wbx", "stroke", "wubi86", &pins),
        "在用集合里的不淘汰"
    );
    assert!(
        reverse_index_keeps("wubi86", "stroke", "wubi86", &pins),
        "主码表不淘汰"
    );
    assert!(
        reverse_index_keeps("stroke", "stroke", "wubi86", &pins),
        "本次请求的不淘汰"
    );
    assert!(!reverse_index_keeps("old", "stroke", "wubi86", &pins));
}

/// 只有改纠错规则或「误读」勾选才重建引擎；标记、提示显示与其余来源不重建。
#[test]
fn correction_rebuild_needed_only_for_engine_side_fields() {
    let base = wind_config::CorrectionConfig::default();
    let with = |f: fn(&mut wind_config::CorrectionConfig)| {
        let mut c = base.clone();
        f(&mut c);
        c
    };
    assert!(!correction_rebuild_needed(&base, &base));
    assert!(correction_rebuild_needed(
        &base,
        &with(|c| c.enabled = !c.enabled)
    ));
    assert!(correction_rebuild_needed(
        &base,
        &with(|c| c.rules.gn_ng = !c.rules.gn_ng)
    ));
    assert!(correction_rebuild_needed(
        &base,
        &with(|c| c.custom = vec!["ab=ba".into()])
    ));
    assert!(correction_rebuild_needed(
        &base,
        &with(|c| c.hint.misread = !c.hint.misread)
    ));
    assert!(!correction_rebuild_needed(
        &base,
        &with(|c| c.marker = String::new())
    ));
    assert!(!correction_rebuild_needed(
        &base,
        &with(|c| c.hint.enabled = !c.hint.enabled)
    ));
    assert!(!correction_rebuild_needed(
        &base,
        &with(|c| c.hint.fuzzy = !c.hint.fuzzy)
    ));
    assert!(!correction_rebuild_needed(
        &base,
        &with(|c| c.hint.correction = !c.hint.correction)
    ));
}

/// 只改提示开关：不重建，但共享开关立刻跟上；「误读」勾选变了要重建。
#[test]
fn sync_correction_refreshes_hint_switch_and_flags_rebuild() {
    let mut cfg = Config::default();
    cfg.schema.pinyin.fuzzy.enabled = true;
    let m = EngineManager::new(&cfg, None);
    assert!(!m.pinyin_hint_active(), "出厂不显示提示");
    let mut on = cfg.clone();
    on.input.correction.hint.enabled = true;
    assert!(!m.sync_correction(&on), "只改提示显示不重建");
    assert!(m.pinyin_hint_active(), "开关即时跟上");
    let mut off = on.clone();
    off.input.correction.hint.misread = false;
    assert!(m.sync_correction(&off), "误读勾选变了要重建");
    assert!(!m.sync_correction(&off), "同一份配置再同步不重建");
}

/// 用户数据目录（`<userdata>/data`）下的 `pinyin_misread.yaml` 与内置表合并，同键覆盖内置。
#[test]
fn load_misread_merges_user_table() {
    let dir = std::env::temp_dir().join(format!("wind_misread_user_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (data, user) = (dir.join("install"), dir.join("userdata").join("data"));
    std::fs::create_dir_all(data.join("schemas")).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    let file = crate::pinyin::misread::MISREAD_FILE;
    std::fs::write(
        data.join("schemas").join(file),
        "- {text: 莘莘, wrong: xin xin, right: shen shen}\n",
    )
    .unwrap();
    std::fs::write(
        user.join(file),
        "---\nname: user\n...\n- {text: 炽热, wrong: zhi re, right: chi re, note: 自加}\n",
    )
    .unwrap();
    let t = EngineManager::load_misread(Some(&data), Some(&user));
    assert_eq!(t.len(), 2);
    assert!(t.lookup("炽热", &["zhire"]).is_some(), "用户表生效");
    assert!(t.lookup("莘莘", &["xinxin"]).is_some(), "内置表保留");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 每次同步都重读误读表：表文件改了 ⇒ 要重建（「误读」开着时）。
#[test]
fn sync_correction_rereads_misread_table() {
    let dir = std::env::temp_dir().join(format!("wind_misread_sync_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    let file = dir
        .join("schemas")
        .join(crate::pinyin::misread::MISREAD_FILE);
    std::fs::write(&file, "- {text: 莘莘, wrong: xin xin, right: shen shen}\n").unwrap();
    let cfg = Config::default();
    let m = EngineManager::new(&cfg, Some(&dir));
    assert!(!m.sync_correction(&cfg), "表没变不重建");
    std::fs::write(
        &file,
        "- {text: 莘莘, wrong: xin xin, right: shen shen}\n\
             - {text: 炽热, wrong: zhi re, right: chi re}\n",
    )
    .unwrap();
    assert!(m.sync_correction(&cfg), "表变了要重建");
    assert_eq!(m.misread.lock().unwrap().len(), 2, "已换成新表");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 引擎算提示的条件：`(hint.enabled || 模板引用) && (模糊来源有效 || misread || 纠错来源有效)`；
/// 模糊来源要模糊音开、纠错来源要纠错开。
#[test]
fn pinyin_hint_on_truth_table() {
    for bits in 0u32..128 {
        let b = |i: u32| bits & (1 << i) != 0;
        let (fz_on, corr_on, hint_on, src_f, src_m, src_c, tpl) =
            (b(0), b(1), b(2), b(3), b(4), b(5), b(6));
        let f = wind_config::PinyinFuzzy {
            enabled: fz_on,
            ..Default::default()
        };
        let corr = wind_config::CorrectionConfig {
            enabled: corr_on,
            hint: wind_config::PinyinHintConfig {
                enabled: hint_on,
                fuzzy: src_f,
                misread: src_m,
                correction: src_c,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            pinyin_hint_on(&f, &corr, tpl),
            (hint_on || tpl) && ((fz_on && src_f) || src_m || (corr_on && src_c)),
            "bits={bits:07b}"
        );
    }
}

/// 模板引用翻转只翻共享开关、不清引擎表；`enabled` 关时开关恒 false。
#[test]
fn template_flag_flips_switch_without_rebuild() {
    let mut cfg = Config::default();
    cfg.schema.pinyin.fuzzy.enabled = true;
    let m = EngineManager::new(&cfg, None);
    let built = m.engines_generation();
    assert!(!m.pinyin_hint_active());
    m.set_correct_pinyin_in_templates(true);
    assert!(m.pinyin_hint_active());
    m.set_correct_pinyin_in_templates(false);
    assert!(!m.pinyin_hint_active());
    assert_eq!(m.engines_generation(), built, "翻转不该清表重建");

    cfg.schema.pinyin.fuzzy.enabled = false;
    cfg.input.correction.hint.enabled = true;
    cfg.input.correction.hint.misread = false;
    m.reload_from_config(&cfg);
    m.set_correct_pinyin_in_templates(true);
    assert!(!m.pinyin_hint_active(), "勾选的来源都不生效：恒 false");

    cfg.input.correction.hint.correction = true;
    m.reload_from_config(&cfg);
    assert!(m.pinyin_hint_active(), "纠错来源勾上且纠错开：算提示");
    cfg.input.correction.enabled = false;
    m.reload_from_config(&cfg);
    assert!(!m.pinyin_hint_active(), "纠错关：纠错来源不生效");
}

/// 混输引用环的路径要能直接贴进日志：自引用、互指、下游成环、菱形（非环）四种形状。
#[test]
fn find_mixed_cycle_reports_path() {
    let graph = |pairs: &'static [(&'static str, &'static [&'static str])]| {
        move |id: &str| -> Vec<String> {
            pairs
                .iter()
                .find(|(k, _)| *k == id)
                .map(|(_, v)| v.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        }
    };
    let g = graph(&[("a", &["a"])]);
    assert_eq!(
        EngineManager::find_mixed_cycle("a", &g).unwrap(),
        ["a", "a"]
    );
    let g = graph(&[("a", &["b", "py"]), ("b", &["a"])]);
    assert_eq!(
        EngineManager::find_mixed_cycle("a", &g).unwrap(),
        ["a", "b", "a"]
    );
    // a 本身不在环上，但它引用的 b↔c 成环：递归照样无底，必须报。
    let g = graph(&[("a", &["b"]), ("b", &["c"]), ("c", &["b"])]);
    assert_eq!(
        EngineManager::find_mixed_cycle("a", &g).unwrap(),
        ["b", "c", "b"]
    );
    // 菱形共享子方案（primary 与 secondary 都落到 d）不是环。
    let g = graph(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &["d"])]);
    assert_eq!(EngineManager::find_mixed_cycle("a", &g), None);
}

/// 反查索引落在方案自己的目录下，且与主词库放得多深无关。
///
/// 目录不能取主库 `.wdat` 的父目录名：主库在 `schemas/ime_wubi86/Default/` 时会落到
/// `<cache>/Default/`，与任何方案都不对应，多个方案还会挤进同一个。
#[test]
fn reverse_index_path_is_scoped_by_schema_id_only() {
    let root = Path::new("cache");
    let p = EngineManager::reverse_index_cache_path_in(root, "ime_wubi86").unwrap();
    assert_eq!(p, root.join("ime_wubi86").join("ime_wubi86.wridx"));

    // 出厂扁平方案：与旧实现逐字节一致，存量索引不失效。
    assert_eq!(
        EngineManager::reverse_index_cache_path_in(root, "wubi86").unwrap(),
        root.join("wubi86").join("wubi86.wridx")
    );

    // 共用主库的两个方案（shuangpin 的主库在 pinyin/ 下）各归各的目录。
    let a = EngineManager::reverse_index_cache_path_in(root, "pinyin").unwrap();
    let b = EngineManager::reverse_index_cache_path_in(root, "shuangpin").unwrap();
    assert_ne!(a.parent(), b.parent());
}

#[test]
fn data_facts_are_cached_until_invalidated() {
    use std::sync::atomic::Ordering::Relaxed;
    let mgr = EngineManager::new(&Config::default(), None);
    let builds = || mgr.data_facts_builds.load(Relaxed);
    mgr.active_data_facts();
    mgr.active_data_facts();
    assert_eq!(builds(), 1, "活跃方案没变就不重算");
    mgr.invalidate_schema("pinyin");
    mgr.active_data_facts();
    assert_eq!(builds(), 2, "方案失效后重算");
}

/// 单字全码表与反查索引同键同目录、只差扩展名。
/// 两者同源于该方案启用的整组词库，一起生、一起废；分开放的话清缓存/换词库时漏掉一个就成了幽灵。
#[test]
fn single_char_codes_cache_sits_next_to_the_reverse_index() {
    let root = Path::new("cache");
    let w = EngineManager::single_char_codes_cache_path_in(root, "ime_wubi86").unwrap();
    assert_eq!(w, root.join("ime_wubi86").join("ime_wubi86.wscc"));
    let r = EngineManager::reverse_index_cache_path_in(root, "ime_wubi86").unwrap();
    assert_eq!(w.parent(), r.parent(), "同源的两份产物必须同目录");
    assert_ne!(w, r);
}

/// `max_code_length` 必须进单字全码表的指纹。
///
/// 它是这张表与反查索引的唯一分歧：反查索引与 cap 无关，单字全码表却拿 cap 筛超长码。
/// 只哈希词库的话，改了码长而词库未动时旧表被永久复用（重装也不好使，缓存还在）。
/// 空词库列表下摘要为空，剩下的恰好就是 cap 这一项，判据不受词库构造干扰。
#[test]
fn cap_is_part_of_the_single_char_cache_fingerprint() {
    let a = EngineManager::single_char_codes_source_digests(&[], 4).unwrap();
    let b = EngineManager::single_char_codes_source_digests(&[], 6).unwrap();
    assert_ne!(a, b, "改了 max_code_length，指纹必须跟着变");
    assert_ne!(
        EngineManager::reverse_index_source_digests(&[]).unwrap(),
        a,
        "两张表共用一个目录，指纹再相等就会互相判「新鲜」"
    );
}

/// 转义后同形的中文方案名不能共用一条路径——目录现在也由这个键决定。
#[test]
fn schema_cache_key_disambiguates_escaped_ids() {
    // ASCII id 不触发哈希：路径必须与旧实现保持逐字节一致。
    assert_eq!(
        EngineManager::schema_cache_key("wubi86_pinyin").as_deref(),
        Some("wubi86_pinyin")
    );

    let a = EngineManager::schema_cache_key("五笔").expect("非空 id 必有键");
    let b = EngineManager::schema_cache_key("拼音").expect("非空 id 必有键");
    assert_ne!(a, b, "两个中文方案名不能算出同一个键");
    assert!(
        a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "键会当目录名和文件名用，必须全是安全字符：{a}"
    );

    // 同一个 id 必须恒等——键进了路径，抖一下就是一批孤儿加一次全量重建。
    assert_eq!(EngineManager::schema_cache_key("五笔"), Some(a));

    assert_eq!(EngineManager::schema_cache_key(""), None);
}

/// 混输主引擎的整句开关不继承 `primary_schema` 的声明。
///
/// `wubi86` 开了整句不代表 `wubi86_pinyin` 也该开：后者的超码长区间归拼音管
/// （`MixedEngine::convert` 直接走 `convert_overflow`），继承过来只会得到「配置开着却不生效」。
#[test]
fn mixed_primary_does_not_inherit_sentence_input() {
    // 独立方案：用自己的声明。
    assert!(resolve_sentence_input(None, true));
    assert!(!resolve_sentence_input(None, false));

    // 混输主：`own=true`（primary_schema 开着）也要被压成混输方案自己的取值。
    assert!(!resolve_sentence_input(
        Some(MixedRole::Primary {
            sentence_input: false
        }),
        true
    ));
    // 混输方案自己声明了 ⇒ 开（当前只在没配拼音子引擎的退化混输下真正生效）。
    assert!(resolve_sentence_input(
        Some(MixedRole::Primary {
            sentence_input: true
        }),
        false
    ));

    // 混输次（拼音）走不到码表分支，取值等同独立方案即可。
    let sec = Some(MixedRole::Secondary(MixPinyinOpts { abbrev: true }));
    assert!(resolve_sentence_input(sec, true));
}

/// 逆切分在混输主引擎上一律关闭，谁声明都不行——见 [`resolve_split_input`]：
/// 切分候选的 `code` 是整串，会落进混输档位体系的档 0、无条件压过拼音精确候选。
#[test]
fn split_input_is_off_for_mixed_primary() {
    // 独立方案：用自己的声明。
    assert!(resolve_split_input(None, true));
    assert!(!resolve_split_input(None, false));

    // 混输主：两个来源都开着也压成 false——不是「不继承」，是「不生效」。
    for own in [true, false] {
        assert!(
            !resolve_split_input(
                Some(MixedRole::Primary {
                    sentence_input: false
                }),
                own
            ),
            "混输主引擎必须恒关（own={own}）"
        );
    }

    // 反向对照：整句不受这条影响，仍按混输方案自己的声明走，防两个开关被一起压死。
    let sentence_on = Some(MixedRole::Primary {
        sentence_input: true,
    });
    assert!(
        resolve_sentence_input(sentence_on, false),
        "整句仍按声明生效"
    );
    assert!(!resolve_split_input(sentence_on, true), "逆切分仍须关");

    // 混输次（拼音）走不到码表分支，取值等同独立方案即可。
    let sec = Some(MixedRole::Secondary(MixPinyinOpts { abbrev: true }));
    assert!(resolve_split_input(sec, true));
}

// ───────────────────────── overlay 方案注册表 ─────────────────────────

/// 造一个含三个方案的 data_dir：两个带 `[overlay]` 段、一个不带。
///
/// 不能断言 `overlay_modes().len()`：`installed_schemas` 还会扫描真实用户目录
/// `Config::user_config_dir()/schemas`，故断言只写「我造的这几个的相对关系」。
fn make_overlay_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_overlay_data_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    // zz_kf / zz_rare：overlay 方案。id 刻意用 `zz_` 前缀——注册表按 id 字典序，
    // 这样它们排在真实用户方案之后，相对顺序断言不受开发机上装了什么影响。
    std::fs::write(
        schemas.join("zz_kf.schema.toml"),
        "[schema]\nid = \"zz_kf\"\nname = \"快符\"\nicon_label = \"符\"\nhidden = true\n\
             [engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 1\n\
             [overlay]\nkind = \"special\"\nshow_all_on_enter = true\ncandidate_layout = \"vertical\"\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("zz_rare.schema.toml"),
        "[schema]\nid = \"zz_rare\"\nname = \"生僻字\"\nhidden = true\n\
             [engine]\ntype = \"codetable\"\n\
             [overlay]\n",
    )
    .unwrap();
    // 普通码表方案：无 [overlay] 段，不该进注册表。
    std::fs::write(
        schemas.join("zz_plain.schema.toml"),
        "[schema]\nid = \"zz_plain\"\nname = \"普通\"\n\
             [engine]\ntype = \"codetable\"\n",
    )
    .unwrap();
    dir
}

fn overlay_mgr(tag: &str) -> (EngineManager, std::path::PathBuf) {
    let dir = make_overlay_data_dir(tag);
    let ov = dir.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    let cfg = Config::default();
    let mgr = EngineManager::with_store_override(&cfg, Some(&dir), None, Some(ov.clone()));
    (mgr, ov)
}

/// 注册表的准入判据是 `[overlay]` 段存在，不是 `hidden`。
/// 两者正交：`hidden` 只说「不列进方案切换列表」，隐藏的码表方案也可能只是 mix 成员。
#[test]
fn overlay_modes_admits_by_section_presence() {
    let (mgr, _ov) = overlay_mgr("admit");
    let modes = mgr.overlay_modes();
    let ids: Vec<&str> = modes.iter().map(|e| e.schema_id.as_str()).collect();

    assert!(ids.contains(&"zz_kf"), "带 [overlay] 的方案应进表：{ids:?}");
    assert!(
        ids.contains(&"zz_rare"),
        "空 [overlay] 段同样算声明：{ids:?}"
    );
    assert!(
        !ids.contains(&"zz_plain"),
        "无 [overlay] 段的普通方案不该进表：{ids:?}"
    );
}

/// 下标 = 按 id 字典序，且 `overlay_index_of` 与之一致。
/// 钉住 `ModeKind::Special(u8)` 的下标语义来源：由本表给出，不是用户 config 数组的顺序。
#[test]
fn overlay_modes_sorted_by_id_and_index_matches() {
    let (mgr, _ov) = overlay_mgr("order");
    let modes = mgr.overlay_modes();
    let kf = modes.iter().position(|e| e.schema_id == "zz_kf").unwrap();
    let rare = modes.iter().position(|e| e.schema_id == "zz_rare").unwrap();
    assert!(kf < rare, "zz_kf 应排在 zz_rare 之前（id 字典序）");

    assert_eq!(mgr.overlay_index_of("zz_kf"), Some(kf as u8));
    assert_eq!(mgr.overlay_index_of("zz_rare"), Some(rare as u8));
    assert_eq!(
        mgr.overlay_index_of("zz_plain"),
        None,
        "不在表里的方案定位不到"
    );
}

/// 显示名/短称从方案文件 `[schema]` 派生，短称只取首字符。
#[test]
fn overlay_entry_derives_name_and_icon_from_schema() {
    let (mgr, _ov) = overlay_mgr("name");
    let modes = mgr.overlay_modes();
    let kf = modes.iter().find(|e| e.schema_id == "zz_kf").unwrap();
    assert_eq!(kf.name, "快符");
    // 一个汉字宽 2，恰好等于上限（`wind_config::ICON_LABEL_MAX_WIDTH`）、原样保留。
    // 这条测不到截断，截断由 `wind_config::schema` 的单元测试覆盖，别在这里重复测。
    assert_eq!(kf.icon_label, "符", "宽度在上限内，不截断");
    assert!(kf.spec.show_all_on_enter);
    assert_eq!(
        kf.spec.candidate_layout,
        wind_config::LayoutIntent::Vertical
    );

    let rare = modes.iter().find(|e| e.schema_id == "zz_rare").unwrap();
    assert_eq!(rare.icon_label, "", "未配 icon_label 时为空，不臆造");
    assert!(!rare.spec.show_all_on_enter, "空 [overlay] 段取字段默认值");
}

/// `schema_overrides/{id}.toml` 的 `[overlay]` 覆盖自动生效：注册表走静态 `read_schema`，
/// 其内部已做 `merge_toml` 深合并，无需为 overlay 另接一条线。
#[test]
fn overlay_spec_honors_schema_override_layer() {
    let (mgr, ov) = overlay_mgr("override");
    // 覆盖前：方案文件写的是 vertical。
    let before = mgr.overlay_modes();
    let kf = before.iter().find(|e| e.schema_id == "zz_kf").unwrap();
    assert_eq!(
        kf.spec.candidate_layout,
        wind_config::LayoutIntent::Vertical
    );

    // 用户在设置页改成横排 + 关掉进入即展示 → 落 override 层。
    std::fs::write(
        ov.join("zz_kf.toml"),
        "[overlay]\ncandidate_layout = \"horizontal\"\nshow_all_on_enter = false\n",
    )
    .unwrap();
    mgr.invalidate_schema("zz_kf"); // 整表失效（见 overlay_cache 的文档）

    let after = mgr.overlay_modes();
    let kf = after.iter().find(|e| e.schema_id == "zz_kf").unwrap();
    assert_eq!(
        kf.spec.candidate_layout,
        wind_config::LayoutIntent::Horizontal,
        "override 的 [overlay] 未生效"
    );
    assert!(!kf.spec.show_all_on_enter);
    assert_eq!(kf.name, "快符", "override 未提及的字段仍来自方案文件");
}

/// 辅助码设置缓存的回填竞态：未命中时先记代次再算，算的途中被清过（方案覆盖层写入 /
/// 重载），算出来的旧值不得回填。反向对照：代次没变时正常回填。
#[test]
fn aux_settings_cache_drops_stale_fill() {
    let mgr = EngineManager::new(&Config::default(), None);
    let cached = |m: &EngineManager| m.aux_settings_cache.lock().unwrap().contains_key("x");
    let before = mgr
        .aux_settings_gen
        .load(std::sync::atomic::Ordering::Acquire);
    mgr.invalidate_aux_settings_cache();
    assert!(!mgr.cache_aux_settings("x".into(), AuxCodeSettings::default(), before));
    assert!(!cached(&mgr), "算之前记的代次已过期，不得回填");
    let now = mgr
        .aux_settings_gen
        .load(std::sync::atomic::Ordering::Acquire);
    assert_ne!(now, before, "清表推进代次");
    assert!(mgr.cache_aux_settings("x".into(), AuxCodeSettings::default(), now));
    assert!(cached(&mgr), "代次未变照常回填");
}

fn codetable_cache_fixture(name: &str, auto_commit: bool) -> std::path::PathBuf {
    let base =
        std::env::temp_dir().join(format!("wind_eng_ct_cache-{name}-{}", std::process::id()));
    let schemas = base.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join("wb_c.schema.toml"),
        format!(
            "[schema]\nid = \"wb_c\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\nauto_commit_at_full = {auto_commit}\n"
        ),
    )
    .unwrap();
    base
}

/// 码表设置每次按键要取好几次（单字过滤、建候选、短语自动上屏），不能每次都重读方案文件。
#[test]
fn codetable_settings_are_cached_until_schema_invalidated() {
    let base = codetable_cache_fixture("inv", true);
    let mgr = EngineManager::new(&Config::default(), Some(&base));
    assert!(mgr.codetable_settings_of("wb_c").auto_commit_at_full);
    codetable_cache_fixture("inv", false);
    assert!(
        mgr.codetable_settings_of("wb_c").auto_commit_at_full,
        "命中缓存：不重读方案文件"
    );
    mgr.invalidate_schema("wb_c");
    assert!(
        !mgr.codetable_settings_of("wb_c").auto_commit_at_full,
        "方案失效后重读"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 全局 `schema.codetable` 是缓存值的基线：配置重载后必须重算（设置页改了要立即生效）。
#[test]
fn codetable_settings_follow_config_reload() {
    let base = codetable_cache_fixture("reload", true);
    let mut cfg = Config::default();
    cfg.schema.codetable.z_key_repeat = false;
    let mgr = EngineManager::new(&cfg, Some(&base));
    assert!(!mgr.codetable_settings_of("wb_c").z_key_repeat);
    cfg.schema.codetable.z_key_repeat = true;
    mgr.reload_from_config(&cfg);
    assert!(
        mgr.codetable_settings_of("wb_c").z_key_repeat,
        "重载后用新的全局基线"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 回填竞态同辅助码缓存：算的途中被清过，旧值不得回填。
#[test]
fn codetable_settings_cache_drops_stale_fill() {
    let mgr = EngineManager::new(&Config::default(), None);
    let key = ("x".to_string(), false);
    let cached = |m: &EngineManager| {
        m.codetable_settings_cache
            .lock()
            .unwrap()
            .contains_key(&key)
    };
    let before = mgr
        .codetable_settings_gen
        .load(std::sync::atomic::Ordering::Acquire);
    mgr.invalidate_codetable_settings_cache();
    assert!(!mgr.cache_codetable_settings(key.clone(), Default::default(), before));
    assert!(!cached(&mgr), "过期代次不得回填");
    let now = mgr
        .codetable_settings_gen
        .load(std::sync::atomic::Ordering::Acquire);
    assert!(mgr.cache_codetable_settings(key.clone(), Default::default(), now));
    assert!(cached(&mgr), "代次未变照常回填");
}

/// 半衰期回落只有两级：本段的值 > store 默认（72h）。
///
/// 码表段不回落到拼音段：设置页上那是两个独立控件，回落链会让用户改了拼音的、码表跟着变。
#[test]
fn half_life_falls_back_to_store_default_only() {
    let store_default = wind_store::freq::FreqProfile::default().half_life_hours;
    assert_eq!(store_default, 72.0, "store 默认值变了，本测试的基准需同步");

    // ① 本段有值 → 用它
    assert_eq!(EngineManager::resolve_half_life(6.0, store_default), 6.0);
    // ② 本段为 0 → store 默认。这是 ① 的反向对照：实现写成恒取第一个参数时 ① 仍会绿。
    assert_eq!(
        EngineManager::resolve_half_life(0.0, store_default),
        store_default
    );
    // 负值当未设置：手改配置写成负数不该得出负半衰期（会让 decay_factor 随时间增长）。
    assert_eq!(
        EngineManager::resolve_half_life(-1.0, store_default),
        store_default
    );
}

/// 码表衰减参数不读拼音段任何字段：以 `pinyin_freq_profile()` 为基只覆盖 half_life 的写法
/// 会让拼音段其余字段静默漏进来。
#[test]
fn codetable_profile_is_independent_of_pinyin_config() {
    let mut cfg = Config::default();
    // 把拼音段三个字段全设成可辨认的非默认值。
    cfg.schema.pinyin.frequency.half_life = 999.0;
    cfg.schema.pinyin.frequency.base_scale = 888.0;
    cfg.schema.pinyin.frequency.recency_peak = 777.0;
    cfg.schema.codetable.frequency.half_life = 0.0; // 码表未设 → 该走 store 默认
    let mgr = EngineManager::new(&cfg, None);

    let def = wind_store::freq::FreqProfile::default();
    let ct = mgr.codetable_freq_profile();
    assert_eq!(
        ct.half_life_hours, def.half_life_hours,
        "不得回落拼音的 999"
    );
    assert_eq!(ct.base_scale, def.base_scale, "不得取拼音的 888");
    assert_eq!(ct.recency_peak, def.recency_peak, "不得取拼音的 777");

    // 反向对照：拼音那侧确实读到了这些值，证明上面三条不是「配置根本没生效」。
    let py = mgr.pinyin_freq_profile();
    assert_eq!(py.half_life_hours, 999.0);
    assert_eq!(py.base_scale, 888.0);
    assert_eq!(py.recency_peak, 777.0);
}

/// 方案文件的 `[engine.codetable.frequency]` 能覆盖基线，且逐字段稀疏。
/// `freq_settings` 不能直读全局镜像，否则方案文件里写了调频也无人读。
#[test]
fn schema_level_frequency_overrides_baseline() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_schema_freq");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    // 方案只覆盖 enabled 与 strategy，其余（promote_prefix / protect_*）留空跟随基线。
    let mut f = std::fs::File::create(schemas.join("ct_freq.schema.toml")).unwrap();
    write!(
        f,
        "[schema]\nid = \"ct_freq\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\n[engine.codetable.frequency]\nenabled = true\nstrategy = \"position\"\n"
    )
    .unwrap();
    drop(f);

    let mut cfg = Config::default();
    // 全局：关、step、一简保护 3 位——三项都与方案要覆盖的不同。
    cfg.schema.codetable.frequency.enabled = false;
    cfg.schema.codetable.frequency.strategy = "step".to_string();
    cfg.schema.codetable.frequency.protect_top_n_len1 = 3;
    cfg.schema.available = vec!["ct_freq".to_string()];
    cfg.schema.active = "ct_freq".to_string();
    let mgr = EngineManager::new(&cfg, Some(&base_dir));

    let s = mgr.freq_settings_for("ct_freq");
    assert!(s.enabled, "方案写了 enabled = true 应覆盖全局的 false");
    assert_eq!(
        s.strategy,
        FreqStrategy::Position,
        "方案写了 position 应覆盖全局的 step"
    );
    assert_eq!(
        s.protect.by_len[0], 3,
        "方案没写的字段应跟随基线（全局一简保护 3 位）"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
}

/// 英文衰减参数与码表、拼音三者互不相干，各读各的 half_life。
#[test]
fn english_profile_is_independent_of_codetable_and_pinyin() {
    let mut cfg = Config::default();
    cfg.schema.pinyin.frequency.half_life = 999.0;
    cfg.schema.codetable.frequency.half_life = 888.0;
    cfg.schema.english.frequency.half_life = 111.0;
    let mgr = EngineManager::new(&cfg, None);

    assert_eq!(mgr.english_freq_profile().half_life_hours, 111.0);
    // 反向对照：另两侧读到的是各自的值，证明上面不是「三者碰巧都取了默认」。
    assert_eq!(mgr.codetable_freq_profile().half_life_hours, 888.0);
    assert_eq!(mgr.pinyin_freq_profile().half_life_hours, 999.0);

    // 英文段留 0 时走 store 默认，不回落码表的 888。
    cfg.schema.english.frequency.half_life = 0.0;
    let mgr2 = EngineManager::new(&cfg, None);
    let def = wind_store::freq::FreqProfile::default();
    assert_eq!(
        mgr2.english_freq_profile().half_life_hours,
        def.half_life_hours,
        "英文段为 0 应取内置默认，不得回落码表段"
    );
}

/// 英文记账口径对所有分支生效——混输/码表方案下混进来的英文候选同样按它记账。
/// 只在英文方案下读这个字段的话，混输里的英文词频写读口径不一致，写进去的记录读不到。
#[test]
fn english_code_scope_applies_to_every_schema_kind() {
    let mut cfg = Config::default();
    cfg.schema.english.frequency.code_scope = "input".to_string();
    // active 是码表方案（默认），英文候选仍应按 input 口径记账。
    let mgr = EngineManager::new(&cfg, None);
    assert!(
        mgr.freq_settings().english_code_by_input,
        "码表方案下英文候选也须按 schema.english 的口径"
    );

    // 反向对照：改回 candidate 时确实变 false，证明上面读的就是这个字段。
    cfg.schema.english.frequency.code_scope = "candidate".to_string();
    let mgr2 = EngineManager::new(&cfg, None);
    assert!(!mgr2.freq_settings().english_code_by_input);
}

#[test]
fn join_rel_dir_keeps_one_separator() {
    //! 子表路径要当稳定 id 用，两种分隔符混着写会让同一张表得到两个 id。
    assert_eq!(
        join_rel_dir(
            "pinyin/rime_frost.dict.yaml",
            "cn_dicts/corrections.dict.yaml"
        ),
        "pinyin/cn_dicts/corrections.dict.yaml"
    );
    assert_eq!(
        join_rel_dir(
            "pinyin\\rime_frost.dict.yaml",
            "cn_dicts/corrections.dict.yaml"
        ),
        "pinyin/cn_dicts/corrections.dict.yaml",
        "方案文件里写反斜杠时结果必须与写斜杠时一致"
    );
    assert_eq!(
        join_rel_dir("top.dict.yaml", "sub.dict.yaml"),
        "sub.dict.yaml",
        "主表就在 schemas 根下时没有目录段可拼"
    );
}

#[test]
fn read_dict_head_parses_name_and_imports() {
    //! 取自朗月拼音 `rime_frost.dict.yaml` 的真实形态：`import_tables` 块里混着
    //! 行内注释与整行注释，而带 `comment` 列的 `cn_dicts/corrections` 就在其中。
    let dir = std::env::temp_dir().join(format!("wind-dicthead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("rime_frost.dict.yaml");
    std::fs::write(
        &p,
        "# Rime dictionary\n\
             ---\n\
             name: rime_frost\n\
             version: \"2023-11-13\"\n\
             import_tables:\n\
             \x20 - cn_dicts/base     # 基础词库\n\
             \x20 # - cn_dicts/tencent  # 大词库，默认不启用\n\
             \n\
             \x20 - cn_dicts/corrections # 错音错字提示\n\
             columns:\n\
             \x20 - text\n\
             ...\n\
             import_tables:\n\
             \x20 - 正文里的这行不算\n",
    )
    .unwrap();

    let (name, imports) = EngineManager::read_dict_head(&p);
    assert_eq!(name, "rime_frost", "引号要剥掉，且只认顶格的 name:");
    assert_eq!(
        imports,
        ["cn_dicts/base", "cn_dicts/corrections"],
        "行内注释要剥、整行注释要跳、块内空行不算结束；`...` 之后是正文，不再解析"
    );

    // 顶格的 `columns:` 结束了 import 块——若把它也当成条目，会多出一个叫 `text` 的表。
    assert!(
        !imports.iter().any(|x| x == "text"),
        "回到另一个顶格键就该结束 import 块"
    );

    assert_eq!(
        EngineManager::read_dict_head(&dir.join("没有这个文件.dict.yaml")),
        (String::new(), Vec::new()),
        "文件不存在返回空，不 panic（子表可能只发了 wdat 没发 yaml）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 头里的 `group:` 是给发现目录的库自定义设置端分组用的；只认头部顶格的键。
#[test]
fn read_dict_head_parses_group_and_injection_carries_it() {
    let dir = std::env::temp_dir().join(format!("wind-dicthead-group-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("mine.dict.yaml");
    std::fs::write(
        &p,
        "---\nname: 我的词库\ngroup: \"工作\"\n...\ngroup: 正文里的不算\n",
    )
    .unwrap();
    let h = EngineManager::read_dict_head_full(&p);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((h.name.as_str(), h.group.as_str()), ("我的词库", "工作"));

    let mut base: toml::Value = toml::from_str(
        "[[dictionary_dirs]]\npath = \"pinyin/ext\"\n[[dictionaries]]\nid = \"main\"\npath = \"pinyin/m.dict.yaml\"\n",
    )
    .unwrap();
    EngineManager::inject_dir_dicts(
        &mut base,
        &|_| vec![("pinyin/ext/mine.dict.yaml".into(), "x".into())],
        &|_| DictHead {
            name: "我的词库".into(),
            group: "工作".into(),
            ..Default::default()
        },
    );
    let arr = base["dictionaries"].as_array().unwrap();
    assert_eq!(arr[1]["group"].as_str(), Some("工作"));
}

/// 词库路径解析的四级优先级。第三级（用户目录的 wdat 优先于安装目录）是关键：
/// 用户投放的 wdat-only 词库通常在用户目录，而兜底恒指向安装目录——Go 版正是为此
/// 才需要额外的 wdbOnlyHint 参数。
#[test]
fn resolve_dict_file_priority_order() {
    let base = std::env::temp_dir().join(format!("wind-resolve-{}", std::process::id()));
    let user = base.join("user/schemas");
    let sys = base.join("sys/schemas");
    std::fs::create_dir_all(user.join("s")).unwrap();
    std::fs::create_dir_all(sys.join("s")).unwrap();
    let rel = "s/d.dict.yaml";
    let touch = |p: &std::path::Path| std::fs::write(p, b"x").unwrap();
    let two = |u: Option<&std::path::Path>| -> Vec<wind_config::ResourceLayer> {
        let mut v: Vec<wind_config::ResourceLayer> = Vec::new();
        if let Some(u) = u {
            v.push(wind_config::ResourceLayer::new("user", u.to_path_buf()));
        }
        v.push(wind_config::ResourceLayer::new("data", sys.clone()));
        v
    };

    // 全空 → 兜底安装目录
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &two(Some(&user))),
        sys.join(rel),
        "全都没有时应兜底到安装目录（与改造前行为一致）"
    );

    // 只有安装目录的 wdat → 命中它
    touch(&sys.join("s/d.wdat"));
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &two(Some(&user))),
        sys.join(rel)
    );

    // 用户目录也有 wdat → 用户目录优先
    touch(&user.join("s/d.wdat"));
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &two(Some(&user))),
        user.join(rel),
        "用户目录的 wdat 必须优先于安装目录"
    );

    // 出现安装目录的 yaml → yaml 整体优先于 wdat
    touch(&sys.join(rel));
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &two(Some(&user))),
        sys.join(rel),
        "yaml 在场时不得被任何 wdat 抢走"
    );

    // 用户目录的 yaml → 最高优先级
    touch(&user.join(rel));
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &two(Some(&user))),
        user.join(rel)
    );

    // 无用户目录时不应 panic
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &two(None)),
        sys.join(rel)
    );

    std::fs::remove_dir_all(&base).ok();
}

/// 三层（user / custom / data）下的词库解析：两趟结构必须保住。
///
/// 反例组合：靠前的层只投了 `.wdat`、靠后的层有 `.yaml` ⇒ 靠后那层的 yaml 胜出（第 1 趟在各层
/// 找完 yaml）。改成一趟逐层「谁先命中算谁」会让靠前那层的 wdat 抢走；定制者只投 `.wdat` 很常见。
#[test]
fn resolve_dict_file_three_layers_keeps_two_pass_semantics() {
    let base = std::env::temp_dir().join(format!("wind-resolve3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let user = base.join("user/schemas");
    let custom = base.join("custom/schemas");
    let sys = base.join("sys/schemas");
    for d in [&user, &custom, &sys] {
        std::fs::create_dir_all(d.join("s")).unwrap();
    }
    let rel = "s/d.dict.yaml";
    let yaml = |d: &std::path::Path| d.join(rel);
    let wdat = |d: &std::path::Path| d.join("s/d.wdat");
    let touch = |p: std::path::PathBuf| std::fs::write(p, b"x").unwrap();
    let layers = vec![
        wind_config::ResourceLayer::new("user", user.clone()),
        wind_config::ResourceLayer::new("custom", custom.clone()),
        wind_config::ResourceLayer::new("data", sys.clone()),
    ];
    let got = || EngineManager::resolve_dict_file_in(rel, &layers);

    // 三层皆空 → 兜底 data 层（末位）
    assert_eq!(got(), yaml(&sys), "三层皆空应兜底到 data 层");

    // 反例组合一：user 只有 wdat、data 有 yaml ⇒ data 的 yaml 胜出（一趟逐层写法会返回 user）。
    touch(wdat(&user));
    touch(yaml(&sys));
    assert_eq!(
        got(),
        yaml(&sys),
        "第 1 趟要在各层找完 yaml：user 只有 wdat 时不得抢走 data 的 yaml"
    );

    // 反例组合二：中间层同理。custom 只有 wdat、data 有 yaml ⇒ 仍是 data 胜出。
    touch(wdat(&custom));
    assert_eq!(
        got(),
        yaml(&sys),
        "custom 只投 wdat 时同样不得抢走 data 的 yaml"
    );

    // custom 补上 yaml ⇒ 第 1 趟命中 custom（user 只有 wdat，轮不到它）
    touch(yaml(&custom));
    assert_eq!(
        got(),
        yaml(&custom),
        "第 1 趟按层序：custom 的 yaml 优先于 data 的 yaml"
    );

    // user 也补上 yaml ⇒ 最靠前的层胜出
    touch(yaml(&user));
    assert_eq!(got(), yaml(&user), "user 的 yaml 是最高优先级");

    // 第 2 趟的层序：三层都只有 wdat 时，按同一层序取最靠前的（此处 custom，
    // 因为 user 层这次不投）。
    let base2 = base.join("pass2");
    let u2 = base2.join("user/schemas");
    let c2 = base2.join("custom/schemas");
    let s2 = base2.join("sys/schemas");
    for d in [&u2, &c2, &s2] {
        std::fs::create_dir_all(d.join("s")).unwrap();
    }
    let layers2 = vec![
        wind_config::ResourceLayer::new("user", u2.clone()),
        wind_config::ResourceLayer::new("custom", c2.clone()),
        wind_config::ResourceLayer::new("data", s2.clone()),
    ];
    touch(wdat(&c2));
    touch(wdat(&s2));
    assert_eq!(
        EngineManager::resolve_dict_file_in(rel, &layers2),
        yaml(&c2),
        "第 2 趟（wdat-only）也要按层序：custom 的 wdat 优先于 data 的"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn freq_strategy_top_parsed() {
    assert_eq!(EngineManager::parse_freq_strategy("top"), FreqStrategy::Top);
}

#[test]
fn freq_strategy_step_and_unknown_fallback() {
    assert_eq!(
        EngineManager::parse_freq_strategy("step"),
        FreqStrategy::Step
    );
    // 未知策略值回退 step（稳健默认）。
    assert_eq!(
        EngineManager::parse_freq_strategy("bogus"),
        FreqStrategy::Step,
        "未知策略应回退 step"
    );
}

/// 无效辅助码来源的告警按「方案 + 条目」只报一次；换个条目 / 换个方案照报。
#[test]
fn aux_source_warn_is_once_per_schema_and_entry() {
    let mgr = EngineManager::new(&Config::default(), None);
    assert!(mgr.first_aux_source_warn("pinyin", "schema:nope"));
    assert!(!mgr.first_aux_source_warn("pinyin", "schema:nope"));
    assert!(mgr.first_aux_source_warn("pinyin", "schema:other"));
    assert!(mgr.first_aux_source_warn("shuangpin", "schema:nope"));
}

/// 临时拼音目标方案取自 schema.primary_pinyin（空=全拼）。
/// 回归防线：该字段一度只被定义/登记而无人读取，导致临时拼音恒为全拼。
#[test]
fn temp_pinyin_target_follows_primary_pinyin() {
    let ct = Some(EngineType::CodeTable);
    assert_eq!(
        EngineManager::resolve_temp_pinyin_target(true, ct, "shoudao"),
        Some("shoudao".to_string()),
        "配置的主拼音方案应作为临时拼音目标"
    );
    assert_eq!(
        EngineManager::resolve_temp_pinyin_target(true, ct, ""),
        Some("pinyin".to_string()),
        "primary_pinyin 空应定义为全拼，而非扫描 available"
    );
    assert_eq!(
        EngineManager::resolve_temp_pinyin_target(false, ct, "shoudao"),
        None,
        "总开关关闭时不进临时拼音"
    );
}

/// 临时拼音仅适用码表/混输方案。判据必须在 target 这个公共门卫上——
/// 曾只在引导键分支加判据，热键/顶屏进模式等入口直接漏网。
#[test]
fn temp_pinyin_target_scope_limited_to_codetable_and_mixed() {
    for ty in [EngineType::CodeTable, EngineType::Mixed] {
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(true, Some(ty), "shoudao"),
            Some("shoudao".to_string()),
            "{ty:?} 方案应支持临时拼音"
        );
    }
    // 拼音方案：本身就在打拼音，不再叠一层（引导符须留给标点输出）。
    assert_eq!(
        EngineManager::resolve_temp_pinyin_target(true, Some(EngineType::Pinyin), "shoudao"),
        None,
        "拼音方案不应进入临时拼音"
    );
    assert_eq!(
        EngineManager::resolve_temp_pinyin_target(true, Some(EngineType::English), "shoudao"),
        None,
        "英文引擎不应进入临时拼音"
    );
    assert_eq!(
        EngineManager::resolve_temp_pinyin_target(true, None, "shoudao"),
        None,
        "无活跃引擎时不应进入临时拼音"
    );
}

/// primary_pinyin 经 config 一路传到 manager 缓存（构造期接线防线）。
#[test]
fn primary_pinyin_wired_from_config() {
    let mut cfg = Config::default();
    cfg.schema.primary_pinyin = "shoudao".to_string();
    let mgr = EngineManager::with_store_override(&cfg, None, None, None);
    assert_eq!(
        *mgr.primary_pinyin.lock().unwrap(),
        "shoudao",
        "config.schema.primary_pinyin 应在构造期进入 manager"
    );
}

#[test]
fn codetable_inline_resolves_over_global() {
    // 全局基线 + 方案 [engine.codetable] 行为逐字段折叠（Some 覆盖 / None 回落）。
    let global = wind_config::CodetableGlobal {
        top_code_commit: false,
        z_key_repeat: false,
        ..Default::default()
    };
    let ov = wind_config::schema::CodeTableSpec {
        top_code_commit: Some(true),
        ..Default::default()
    };
    let eff = global.resolved(Some(&ov));
    assert!(eff.top_code_commit, "Some 字段应覆盖全局");
    assert!(!eff.z_key_repeat, "None 字段应回落全局");
    // 无方案行为（None）：整体回落全局。
    assert!(!global.resolved(None).top_code_commit, "无覆盖时应回落全局");
}

/// Fix 1+2 端到端：方案 `.schema.toml` 内联 `[engine.codetable]` 行为无需 override 文件即生效，
/// 且逐字段折叠到全局基线（内联给的覆盖，未给的回落）。
#[test]
fn resolve_codetable_reads_schema_inline_behavior() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_inline_data");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    let mut f = std::fs::File::create(schemas.join("wb_test.schema.toml")).unwrap();
    // 内联：自动上屏开、顶码关；z_key_repeat 不写 → 应回落全局。
    // 不写 hidden / overlay：回落全局是普通方案的语义，特殊方案另有一条
    // （见 special_schema_does_not_inherit_global_codetable）。
    write!(
        f,
        "[schema]\nid = \"wb_test\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 8\nauto_commit_at_full = true\ntop_code_commit = false\n"
    )
    .unwrap();
    drop(f);

    // 全局基线：auto_commit_at_full=false / top_code_commit=true / z_key_repeat=true。
    let global = wind_config::CodetableGlobal {
        auto_commit_at_full: false,
        top_code_commit: true,
        z_key_repeat: true,
        ..Default::default()
    };
    // override_dir 指向空目录：证明无 override 文件也能读到内联行为。
    let ov_dir = std::env::temp_dir().join("wind_eng_inline_empty_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);
    let eff = EngineManager::resolve_codetable("wb_test", Some(&base_dir), &global, Some(&ov_dir));
    assert!(eff.auto_commit_at_full, "内联 Some(true) 应覆盖全局 false");
    assert!(!eff.top_code_commit, "内联 Some(false) 应覆盖全局 true");
    assert!(eff.z_key_repeat, "内联未给的字段应回落全局 true");

    let _ = std::fs::remove_dir_all(&base_dir);
}

/// overlay 方案（带 `[overlay]` 段）不继承全局 `schema.codetable`：没写的字段取内置默认。
///
/// 快符是几十条的小符号表，全局基线按五笔数万条全码表调；继承的话用户改五笔的「精确匹配」，
/// 快符跟着变而毫无察觉。
#[test]
fn overlay_schema_does_not_inherit_global_codetable() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_special_baseline");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    // 两个方案除 `[overlay]` 段外逐字相同——差别只能来自那一段本身。
    for (id, overlay) in [("sp_overlay", true), ("sp_normal", false)] {
        let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
        let ov = if overlay { "[overlay]\n" } else { "" };
        write!(
            f,
            "[schema]\nid = \"{id}\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 8\n{ov}"
        )
        .unwrap();
    }

    // 基准是特殊方案基线，不是 CodetableGlobal::default()（结构体零值，见 special_schema_baseline）。
    let def = EngineManager::special_schema_baseline();
    // 全局把三个开关都拨到与该基线相反的一侧，这样「有没有继承」一眼可辨。
    let global = wind_config::CodetableGlobal {
        single_code_input: !def.single_code_input,
        z_key_repeat: !def.z_key_repeat,
        top_code_commit: !def.top_code_commit,
        ..Default::default()
    };
    let ov = std::env::temp_dir().join("wind_eng_special_baseline_ov");
    let _ = std::fs::remove_dir_all(&ov);

    let sp = EngineManager::resolve_codetable("sp_overlay", Some(&base_dir), &global, Some(&ov));
    assert_eq!(
        sp.single_code_input, def.single_code_input,
        "overlay 方案不该继承全局的精确匹配"
    );
    assert_eq!(sp.z_key_repeat, def.z_key_repeat);
    assert_eq!(sp.top_code_commit, def.top_code_commit);

    // 反向对照：同样的文件去掉 `[overlay]` 就该继承——否则上面三条在「全局基线整个
    // 失效」时也会通过。
    let np = EngineManager::resolve_codetable("sp_normal", Some(&base_dir), &global, Some(&ov));
    assert_eq!(
        np.single_code_input, global.single_code_input,
        "普通方案仍须继承全局"
    );
    assert_eq!(np.z_key_repeat, global.z_key_repeat);
    assert_eq!(np.top_code_commit, global.top_code_commit);

    let _ = std::fs::remove_dir_all(&base_dir);
}

/// 通配两项与其它码表行为同一条折叠链：方案写了覆盖、没写回落全局；overlay 方案
/// 取内置基线，不继承全局开关（快符那类小符号表里 `z` 多半是正经编码）。
#[test]
fn wildcard_folds_from_schema_and_overlay_does_not_inherit() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_wildcard_fold");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    for (id, body) in [
        ("wc_inline", "[engine.codetable]\nwildcard_key = \"?\"\n"),
        ("wc_follow", ""),
        ("wc_overlay", "[overlay]\n"),
    ] {
        let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
        write!(
            f,
            "[schema]\nid = \"{id}\"\n[engine]\ntype = \"codetable\"\n{body}"
        )
        .unwrap();
    }
    let global = wind_config::CodetableGlobal {
        wildcard: true,
        ..Default::default()
    };
    let ov = std::env::temp_dir().join("wind_eng_wildcard_fold_ov");
    let _ = std::fs::remove_dir_all(&ov);
    let key = |id: &str| {
        EngineManager::resolve_codetable(id, Some(&base_dir), &global, Some(&ov)).wildcard_char(id)
    };
    assert_eq!(
        key("wc_inline"),
        Some('?'),
        "方案写了键 ⇒ 覆盖；开关回落全局 true"
    );
    assert_eq!(key("wc_follow"), Some('z'), "都没写 ⇒ 全局");
    assert_eq!(key("wc_overlay"), None, "overlay 方案不继承全局开关");
    let _ = std::fs::remove_dir_all(&base_dir);
}

/// 出厂笔画方案显式 `wildcard = false`：它的 `z` 是「折」，全局开了通配（出厂键 `z`）也
/// 不跟随。读的是仓库 `data/` 里的出厂方案文件本身。对照：同一全局下五笔 86 跟随开启。
#[test]
fn factory_stroke_schema_opts_out_of_wildcard() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data");
    let ov = std::env::temp_dir().join("wind_eng_wildcard_stroke_ov");
    let _ = std::fs::remove_dir_all(&ov);
    let global = wind_config::CodetableGlobal {
        wildcard: true,
        ..Default::default()
    };
    let key = |id: &str| {
        EngineManager::resolve_codetable(id, Some(&data), &global, Some(&ov)).wildcard_char(id)
    };
    assert_eq!(key("stroke"), None, "笔画方案不得继承全局通配");
    assert_eq!(key("wubi86"), Some('z'), "对照：五笔 86 跟随全局");
}

/// 基线判据是 `[overlay]` 段而不是 `hidden`：钉住两个「反对角」组合。
///
/// | 方案 | 按 hidden 判（错） | 按 overlay 判（对） |
/// |---|---|---|
/// | 有 `[overlay]`、无 `hidden` | 全局，会继承五笔的精确匹配 | 内置基线 |
/// | 有 `hidden`、无 `[overlay]` | 内置基线 | 全局，它只是不列进切换列表的普通码表 |
///
/// 必须两条一起断言：只测其一的话，判据被改成恒真/恒假时另一半照样通过。
#[test]
fn codetable_baseline_keys_on_overlay_section_not_hidden() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_baseline_offdiag");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    // 两个方案的 hidden 与 overlay 刻意取反，两个属性由此可分辨。
    for (id, extra) in [("od_overlay_only", "[overlay]\n"), ("od_hidden_only", "")] {
        let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
        let h = if id == "od_hidden_only" {
            "hidden = true\n"
        } else {
            ""
        };
        write!(
            f,
            "[schema]\nid = \"{id}\"\n{h}[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 8\n{extra}"
        )
        .unwrap();
    }

    let def = EngineManager::special_schema_baseline();
    // 全局拨到与内置基线相反的一侧，「取了哪份基线」一眼可辨。
    let global = wind_config::CodetableGlobal {
        single_code_input: !def.single_code_input,
        z_key_repeat: !def.z_key_repeat,
        ..Default::default()
    };
    let ov = std::env::temp_dir().join("wind_eng_baseline_offdiag_ov");
    let _ = std::fs::remove_dir_all(&ov);

    // 有 [overlay] 但没写 hidden：仍取内置基线（旧判据在这里会去继承全局）。
    let a =
        EngineManager::resolve_codetable("od_overlay_only", Some(&base_dir), &global, Some(&ov));
    assert_eq!(
        a.single_code_input, def.single_code_input,
        "声明了 [overlay] 就该取内置基线，与写没写 hidden 无关"
    );
    assert_eq!(a.z_key_repeat, def.z_key_repeat);

    // hidden 但没有 [overlay]：它只是不进切换列表的普通码表，该跟随全局
    // （旧判据在这里会误给内置基线）。
    let b = EngineManager::resolve_codetable("od_hidden_only", Some(&base_dir), &global, Some(&ov));
    assert_eq!(
        b.single_code_input, global.single_code_input,
        "hidden 只是不列进切换列表，没有 [overlay] 就该跟随全局"
    );
    assert_eq!(b.z_key_repeat, global.z_key_repeat);

    let _ = std::fs::remove_dir_all(&base_dir);
}

/// `effective_codetable`（设置页初值快照）必须与 `resolve_codetable`（引擎构建用的折叠）同口径，
/// 否则用户看到「设置页显示精确匹配是关的，实际按开的跑」。
/// 顺带钉住基线分流：overlay 方案的初值必须来自 overlay 基线，否则一打开「自定义码表配置」
/// 就把全局的值写进了本不继承全局的方案。
#[test]
fn effective_codetable_matches_resolve_and_splits_by_overlay() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_effective_ct");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    // 与 overlay_schema_does_not_inherit_global_codetable 同构：除 `[overlay]` 外逐字相同。
    for (id, overlay) in [("eff_overlay", true), ("eff_normal", false)] {
        let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
        let ov = if overlay { "[overlay]\n" } else { "" };
        write!(
            f,
            "[schema]\nid = \"{id}\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\nsingle_code_complete = false\n{ov}"
        )
        .unwrap();
    }

    let def = EngineManager::special_schema_baseline();
    let mut cfg = Config::default();
    // 全局三项都拨到与特殊基线相反的一侧：继承与否一眼可辨。
    cfg.schema.codetable.single_code_input = !def.single_code_input;
    cfg.schema.codetable.z_key_repeat = !def.z_key_repeat;
    cfg.schema.codetable.punct_commit = !def.punct_commit;
    cfg.schema.available = vec!["eff_normal".to_string()];
    cfg.schema.active = "eff_normal".to_string();
    let mgr = EngineManager::new(&cfg, Some(&base_dir));

    let sp = mgr.effective_codetable("eff_overlay");
    assert_eq!(
        sp.single_code_input, def.single_code_input,
        "overlay 方案的初值快照该来自 overlay 基线，不该继承全局"
    );
    assert_eq!(sp.z_key_repeat, def.z_key_repeat);
    assert_eq!(sp.punct_commit, def.punct_commit);
    assert!(
        !sp.single_code_complete,
        "方案文件显式写了 false，应压过基线的 true"
    );

    // 反向对照：去掉 `[overlay]` 就该继承全局——否则上面三条在「全局整个失效」时也通过。
    let np = mgr.effective_codetable("eff_normal");
    assert_eq!(
        np.single_code_input, cfg.schema.codetable.single_code_input,
        "普通方案的初值快照仍须继承全局"
    );
    assert_eq!(np.z_key_repeat, cfg.schema.codetable.z_key_repeat);

    // 与引擎构建实际用的那条折叠路径逐字段比对（同口径守护）。
    for id in ["eff_overlay", "eff_normal"] {
        let via_resolve = EngineManager::resolve_codetable(
            id,
            Some(&base_dir),
            &cfg.schema.codetable,
            mgr.override_dir.as_deref(),
        );
        assert_eq!(
            mgr.effective_codetable(id),
            via_resolve,
            "{id}: 设置页快照与引擎实际折叠必须一致"
        );
    }

    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn merge_toml_table_recurse_and_scalar_replace() {
    let mut base: toml::Value = toml::from_str("a = 1\n[t]\nx = 1\ny = 2\n").unwrap();
    let over: toml::Value = toml::from_str("a = 9\n[t]\ny = 20\nz = 30\n").unwrap();
    merge_toml(&mut base, over);
    assert_eq!(base.get("a").unwrap().as_integer(), Some(9));
    let t = base.get("t").unwrap();
    assert_eq!(t.get("x").unwrap().as_integer(), Some(1), "未覆盖键保留");
    assert_eq!(t.get("y").unwrap().as_integer(), Some(20), "覆盖键替换");
    assert_eq!(t.get("z").unwrap().as_integer(), Some(30), "新增键加入");
}

/// 键集合可变的表（键名是数据，如 `key_actions` 的按键名）同样逐键合并。
/// 这是 `docs/design/schema-key-actions.md` §3「逐键合并」覆盖语义成立的前提。
///
/// 顺带钉住能力缺口：override 无法表达「删除 base 的某个键」，合并只会新增/覆盖。
/// 「本方案禁用某个全局绑定」只能靠显式哨兵值（`none`）。
#[test]
fn merge_toml_merges_tables_with_arbitrary_key_sets() {
    // base = 方案作者内联；over = 用户 override。键名均为按键名，非结构字段。
    let mut base: toml::Value =
        toml::from_str("[key_actions]\nbackslash = \"special:fuhao\"\nz = \"temp_pinyin\"\n")
            .unwrap();
    let over: toml::Value =
        toml::from_str("[key_actions]\nz = \"temp_english\"\nrshift = \"toggle_schema:english\"\n")
            .unwrap();
    merge_toml(&mut base, over);

    let ka = base.get("key_actions").unwrap();
    assert_eq!(
        ka.get("backslash").unwrap().as_str(),
        Some("special:fuhao"),
        "override 未提及的键保留——这是逐键合并而非整段替换的判据"
    );
    assert_eq!(
        ka.get("z").unwrap().as_str(),
        Some("temp_english"),
        "同名键被 override 覆盖"
    );
    assert_eq!(
        ka.get("rshift").unwrap().as_str(),
        Some("toggle_schema:english"),
        "override 新增的键加入"
    );
    assert_eq!(
        ka.as_table().unwrap().len(),
        3,
        "合并结果 = 两侧键集合的并集"
    );
}

/// dictionaries 的 override 是按 id 的稀疏合并，不是数组整体替换：
/// 只有 enabled 会被覆盖，结构定义（顺序/path/label/base_order）恒以方案文件为准。
#[test]
fn merge_toml_dictionaries_only_overrides_enabled_by_id() {
    // 方案文件：三个库，其中 ext2 是"方案后续新增"的（override 写入时还不存在）。
    let mut base: toml::Value = toml::from_str(
        "[[dictionaries]]\nid = \"main\"\npath = \"a.yaml\"\ndefault = true\nbase_order = 0\n\
             [[dictionaries]]\nid = \"ext1\"\npath = \"new/b.yaml\"\nlabel = \"新标签\"\nbase_order = 1\n\
             [[dictionaries]]\nid = \"ext2\"\npath = \"c.yaml\"\nbase_order = 2\n",
    )
    .unwrap();
    // override：老格式整表快照（含 path/label 副本）+ 一个方案已删除的库 gone。
    let over: toml::Value = toml::from_str(
        "[[dictionaries]]\nid = \"ext1\"\nenabled = false\npath = \"old/b.yaml\"\nlabel = \"旧标签\"\nbase_order = 99\n\
             [[dictionaries]]\nid = \"gone\"\nenabled = true\npath = \"gone.yaml\"\n",
    )
    .unwrap();
    merge_toml(&mut base, over);

    let arr = base.get("dictionaries").unwrap().as_array().unwrap();
    assert_eq!(arr.len(), 3, "方案文件的库集合与顺序不受 override 影响");
    assert_eq!(arr[2].get("id").unwrap().as_str(), Some("ext2"));
    assert!(
        arr[2].get("enabled").is_none(),
        "override 未提及的库保持无显式 enabled（继承 default_enabled）"
    );

    let ext1 = &arr[1];
    assert_eq!(
        ext1.get("enabled").unwrap().as_bool(),
        Some(false),
        "enabled 被覆盖"
    );
    assert_eq!(
        ext1.get("path").unwrap().as_str(),
        Some("new/b.yaml"),
        "path 以方案文件为准——老快照里的副本必须被忽略"
    );
    assert_eq!(ext1.get("label").unwrap().as_str(), Some("新标签"));
    assert_eq!(ext1.get("base_order").unwrap().as_integer(), Some(1));

    assert!(
        !arr.iter()
            .any(|d| d.get("id").and_then(|v| v.as_str()) == Some("gone")),
        "方案已删除的库不因 override 复活"
    );
}

/// 发现目录注入的夹具：`files` 是目录里的文件（相对 schemas/），`heads` 是各词库头部。
fn inject_with(base_src: &str, files: &[&str], heads: &[(&str, &str, &[&str])]) -> toml::Value {
    let mut base: toml::Value = toml::from_str(base_src).unwrap();
    let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
    let heads: Vec<(String, String, Vec<String>)> = heads
        .iter()
        .map(|(r, n, i)| {
            (
                r.to_string(),
                n.to_string(),
                i.iter().map(|x| x.to_string()).collect(),
            )
        })
        .collect();
    EngineManager::inject_dir_dicts(
        &mut base,
        &|sub| {
            files
                .iter()
                .filter(|f| f.starts_with(&format!("{sub}/")))
                .map(|f| (f.clone(), std::path::PathBuf::from(f)))
                .collect()
        },
        &|rel| {
            heads
                .iter()
                .find(|(r, _, _)| r == rel)
                .map(|(_, n, i)| DictHead {
                    name: n.clone(),
                    imports: i.clone(),
                    ..Default::default()
                })
                .unwrap_or_default()
        },
    );
    base
}

fn dict_ids(base: &toml::Value) -> Vec<String> {
    base.get("dictionaries")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.get("id")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

const PINYIN_WITH_DIR: &str = "[[dictionaries]]\nid = \"main\"\npath = \"pinyin/rime_frost.dict.yaml\"\n\
        type = \"rime_pinyin\"\ndefault = true\n\
        [[dictionary_dirs]]\npath = \"pinyin/cn_dicts_cell\"\ntype = \"rime_pinyin\"\n";

/// 目录里的词库登记成出厂关的扩展库，id 带前缀、label 取词库头 name（空则文件名）。
#[test]
fn dir_dicts_are_injected_disabled_with_name_or_file_label() {
    let base = inject_with(
        PINYIN_WITH_DIR,
        &[
            "pinyin/cn_dicts_cell/idiom.dict.yaml",
            "pinyin/cn_dicts_cell/food.dict.yaml",
        ],
        &[("pinyin/cn_dicts_cell/idiom.dict.yaml", "成语", &[])],
    );
    assert_eq!(
        dict_ids(&base),
        [
            "main",
            "dir:pinyin/cn_dicts_cell/idiom",
            "dir:pinyin/cn_dicts_cell/food"
        ]
    );
    let schema: Schema = base.try_into().unwrap();
    let idiom = &schema.dictionaries[1];
    assert_eq!(idiom.label, "成语");
    assert_eq!(idiom.path, "pinyin/cn_dicts_cell/idiom.dict.yaml");
    assert_eq!(idiom.dict_type, "rime_pinyin");
    assert!(!idiom.default);
    assert!(!idiom.is_enabled(), "出厂关：开不开由用户在扩展词库里决定");
    assert_eq!(
        schema.dictionaries[2].label, "food",
        "词库头没写 name 时退回文件名"
    );
}

/// 两类不登记：方案已手写声明的同一文件；已被已声明词库的 import_tables 引用的文件
/// （用户拿上游完整 rime_frost 主表覆盖时，它们就是主表的一部分，再登记就加载两遍）。
#[test]
fn dir_dicts_skip_declared_and_imported_files() {
    let src = format!(
        "{PINYIN_WITH_DIR}[[dictionaries]]\nid = \"hand\"\npath = \"pinyin/cn_dicts_cell/hand.dict.yaml\"\n\
             type = \"rime_pinyin\"\nlabel = \"手写的\"\nbase_order = 5\n"
    );
    let base = inject_with(
        &src,
        &[
            "pinyin/cn_dicts_cell/hand.dict.yaml",
            "pinyin/cn_dicts_cell/idiom.dict.yaml",
            "pinyin/cn_dicts_cell/place.dict.yaml",
        ],
        &[(
            "pinyin/rime_frost.dict.yaml",
            "rime_frost",
            &["cn_dicts/base", "cn_dicts_cell/idiom"],
        )],
    );
    assert_eq!(
        dict_ids(&base),
        ["main", "hand", "dir:pinyin/cn_dicts_cell/place"],
        "hand 已手写、idiom 已被主表 import，只有 place 是新的"
    );
    let place = &base.get("dictionaries").unwrap().as_array().unwrap()[2];
    assert_eq!(
        place.get("base_order").unwrap().as_integer(),
        Some(6),
        "排在已声明的库之后（码表按 base_order 分层）"
    );
}

/// 用户在扩展词库里翻的开关（override 的 `{id, enabled}`）必须对得上注入的库——
/// 这正是注入要排在 merge_toml 之前的原因。
#[test]
fn dir_dict_toggle_from_override_applies_after_injection() {
    let mut base = inject_with(
        PINYIN_WITH_DIR,
        &["pinyin/cn_dicts_cell/idiom.dict.yaml"],
        &[],
    );
    let over: toml::Value = toml::from_str(
        "[[dictionaries]]\nid = \"dir:pinyin/cn_dicts_cell/idiom\"\nenabled = true\n",
    )
    .unwrap();
    merge_toml(&mut base, over);
    let schema: Schema = base.try_into().unwrap();
    assert!(schema.dictionaries[1].is_enabled());
}

/// 没声明发现目录 ⇒ 原样不动；声明了但方案没有 `[[dictionaries]]` ⇒ 照样能登记。
#[test]
fn dir_dicts_noop_without_dirs_and_work_without_declared_dicts() {
    let plain = "[[dictionaries]]\nid = \"main\"\npath = \"a.dict.yaml\"\ndefault = true\n";
    let before: toml::Value = toml::from_str(plain).unwrap();
    assert_eq!(inject_with(plain, &["x/a.dict.yaml"], &[]), before);

    let only_dir = "[[dictionary_dirs]]\npath = \"wubi86/ext/\"\ntype = \"rime_codetable\"\n";
    let base = inject_with(only_dir, &["wubi86/ext/a.dict.yaml"], &[]);
    assert_eq!(
        dict_ids(&base),
        ["dir:wubi86/ext/a"],
        "path 的首尾斜杠不影响扫描"
    );
}

const CODETABLE_NO_DIR: &str = "[engine]\ntype = \"codetable\"\n\
        [[dictionaries]]\nid = \"extra\"\npath = \"shared/extra.dict.yaml\"\nbase_order = 1\n\
        [[dictionaries]]\nid = \"main\"\npath = \"wubi86/main.dict.yaml\"\ntype = \"rime_codetable\"\n\
        default = true\nbase_order = 0\n";

/// 码表方案没写 dictionary_dirs ⇒ 兜底扫主词库目录下的 ext/（取 default=true 的那张，
/// 不是第一张），兜底目录写回方案、扫到的库排在已声明的之后。
#[test]
fn codetable_without_dirs_falls_back_to_main_dict_ext() {
    let base = inject_with(
        CODETABLE_NO_DIR,
        &["wubi86/ext/chengyu.dict.yaml", "wubi86/stray.dict.yaml"],
        &[],
    );
    assert_eq!(
        dict_ids(&base),
        ["extra", "main", "dir:wubi86/ext/chengyu"],
        "只扫 ext/，方案目录本层的未声明文件不登记"
    );
    let schema: Schema = base.try_into().unwrap();
    assert_eq!(schema.dictionary_dirs.len(), 1);
    assert_eq!(schema.dictionary_dirs[0].path, "wubi86/ext");
    assert_eq!(schema.dictionary_dirs[0].dict_type, "rime_codetable");
    assert_eq!(schema.dictionaries[2].base_order, 2);
    assert!(!schema.dictionaries[2].is_enabled());
}

/// 拼音方案同样兜底：出厂全拼 / 双拼的随包细胞库就在 `pinyin/ext`，类型取 rime_pinyin。
#[test]
fn pinyin_without_dirs_falls_back_to_main_dict_ext() {
    let src = "[engine]\ntype = \"pinyin\"\n\
            [[dictionaries]]\nid = \"main\"\npath = \"pinyin/rime_frost.dict.yaml\"\n\
            type = \"rime_pinyin\"\ndefault = true\n";
    let base = inject_with(src, &["pinyin/ext/idiom.dict.yaml"], &[]);
    assert_eq!(dict_ids(&base), ["main", "dir:pinyin/ext/idiom"]);
    let schema: Schema = base.try_into().unwrap();
    assert_eq!(schema.dictionary_dirs[0].path, "pinyin/ext");
    assert_eq!(schema.dictionary_dirs[0].dict_type, "rime_pinyin");
}

/// 英文方案同样兜底：出厂主库在 `english/en.dict.yaml`，类型取 english，扫到的库排在
/// 内置 en_ext 之后、出厂关。
#[test]
fn english_without_dirs_falls_back_to_main_dict_ext() {
    let src = "[engine]\ntype = \"english\"\n\
            [[dictionaries]]\nid = \"en_main\"\npath = \"english/en.dict.yaml\"\n\
            type = \"english\"\ndefault = true\nbase_order = 0\n\
            [[dictionaries]]\nid = \"en_ext\"\npath = \"english/en_ext.dict.yaml\"\n\
            type = \"english\"\ndefault_enabled = true\nbase_order = 1\n";
    let base = inject_with(src, &["english/ext/en_it.dict.yaml"], &[]);
    assert_eq!(
        dict_ids(&base),
        ["en_main", "en_ext", "dir:english/ext/en_it"]
    );
    let schema: Schema = base.try_into().unwrap();
    assert_eq!(schema.dictionary_dirs[0].path, "english/ext");
    assert_eq!(schema.dictionary_dirs[0].dict_type, "english");
    assert_eq!(schema.dictionaries[2].dict_type, "english");
    assert_eq!(schema.dictionaries[2].base_order, 2);
    assert!(!schema.dictionaries[2].is_enabled());
}

/// 不兜底的三种：显式写了 `dictionary_dirs = []`；混输等没有词库类型的方案；主词库在 schemas/ 根下。
#[test]
fn default_dict_dir_only_when_undeclared_and_main_has_a_dir() {
    let files = ["wubi86/ext/a.dict.yaml", "ext/a.dict.yaml"];
    let opted_out = format!("dictionary_dirs = []\n{CODETABLE_NO_DIR}");
    assert_eq!(
        dict_ids(&inject_with(&opted_out, &files, &[])),
        ["extra", "main"]
    );

    let mixed = CODETABLE_NO_DIR.replace("\"codetable\"", "\"mixed\"");
    let base = inject_with(&mixed, &files, &[]);
    assert_eq!(dict_ids(&base), ["extra", "main"]);
    assert!(base.get("dictionary_dirs").is_none());

    let at_root = "[engine]\ntype = \"codetable\"\n\
            [[dictionaries]]\nid = \"main\"\npath = \"main.dict.yaml\"\ndefault = true\n";
    let base = inject_with(at_root, &files, &[]);
    assert_eq!(dict_ids(&base), ["main"]);
    assert!(base.get("dictionary_dirs").is_none());
}

/// 主表清单里写了、但主表目录里没有的子表，不算「已被主表引用」：子表不走分层，它其实
/// 不会被加载。只看清单就跳过，用户目录里那张同名细胞库就两头落空。
#[test]
fn dict_head_resolved_keeps_only_imports_present_beside_main() {
    let dir = std::env::temp_dir().join(format!("wind-headres-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cells")).unwrap();
    let main = dir.join("main.dict.yaml");
    std::fs::write(
        &main,
        "---\nname: main\nimport_tables:\n  - cells/here\n  - cells/elsewhere\n...\n",
    )
    .unwrap();
    std::fs::write(dir.join("cells/here.dict.yaml"), "---\nname: here\n...\n").unwrap();

    let (name, imports) = EngineManager::dict_head_resolved(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(name, "main");
    assert_eq!(
        imports,
        ["cells/here"],
        "cells/elsewhere 不在主表目录，不会被主表加载"
    );
}

/// 端到端：真实 `read_schema` 走分层扫描 + 读词库头 + 合并 override。
/// 目录名取一个用户目录里不可能有的，免得本机真实的用户层混进来。
#[test]
fn read_schema_registers_dir_dicts_and_applies_override() {
    let root = std::env::temp_dir().join(format!("wind-dirdicts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = root.join("data");
    let schemas = data.join("schemas");
    let cells = schemas.join("zz_dirdicts_probe/cells");
    std::fs::create_dir_all(&cells).unwrap();
    std::fs::write(
        schemas.join("zzprobe.schema.toml"),
        "[schema]\nid = \"zzprobe\"\nname = \"探针\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"zz_dirdicts_probe/main.dict.yaml\"\ndefault = true\n\
             [[dictionary_dirs]]\npath = \"zz_dirdicts_probe/cells\"\ntype = \"rime_codetable\"\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("zz_dirdicts_probe/main.dict.yaml"),
        "---\nname: main\nimport_tables:\n  - cells/inmain  # 已被主表引用\n...\n",
    )
    .unwrap();
    std::fs::write(cells.join("inmain.dict.yaml"), "---\nname: inmain\n...\n").unwrap();
    std::fs::write(cells.join("extra.dict.yaml"), "---\nname: \"额外\"\n...\n").unwrap();
    std::fs::write(cells.join("notes.txt"), "不是词库").unwrap();
    let ov = root.join("ov");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("zzprobe.toml"),
        "[[dictionaries]]\nid = \"dir:zz_dirdicts_probe/cells/extra\"\nenabled = true\n",
    )
    .unwrap();

    let schema = EngineManager::read_schema("zzprobe", Some(&data), Some(&ov)).unwrap();
    let _ = std::fs::remove_dir_all(&root);

    let ids: Vec<&str> = schema.dictionaries.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["main", "dir:zz_dirdicts_probe/cells/extra"]);
    let extra = &schema.dictionaries[1];
    assert_eq!(extra.label, "额外");
    assert!(extra.is_enabled(), "override 的开关落到注入的库上");
}

/// base 侧没有 dictionaries 时，override 的稀疏项无 path、造不出可用词库，应整键忽略。
#[test]
fn merge_toml_dictionaries_not_created_from_override_alone() {
    let mut base: toml::Value = toml::from_str("[schema]\nid = \"x\"\n").unwrap();
    let over: toml::Value =
        toml::from_str("[[dictionaries]]\nid = \"ext1\"\nenabled = true\n").unwrap();
    merge_toml(&mut base, over);
    assert!(base.get("dictionaries").is_none());
}

/// 方案级 `[session_actions]`：逐键合并 + 显式 `none` 留在表里 + 并集滤掉 `none`。
///
/// - override 逐键合并（整段替换会让方案作者内联的其余键消失）；
/// - 显式 `none` 必须留在表里：滤掉它，消费点会回落全局，用户写的禁用失效；
/// - 并集要滤掉 `none`：并集决定装不装 CapsLock 全局钩子、要不要让 TSF 转发。
///   两处方向相反，合并成一套就必错一头。
#[test]
fn active_session_actions_merges_per_key_and_union_drops_none() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_sa_data");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    let mut f = std::fs::File::create(schemas.join("sa.schema.toml")).unwrap();
    write!(
        f,
        "[schema]\nid = \"sa\"\n[engine]\ntype = \"codetable\"\n\
             [session_actions]\nminus = \"page_prev\"\nequal = \"page_next\"\n"
    )
    .unwrap();
    drop(f);

    let ov_dir = std::env::temp_dir().join("wind_eng_sa_overrides");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mut cfg = Config::default();
    cfg.schema.active = "sa".to_string();
    cfg.schema.available = vec!["sa".to_string()];
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    let inline = mgr.active_session_actions();
    assert_eq!(inline.get("minus").map(String::as_str), Some("page_prev"));
    assert_eq!(inline.get("equal").map(String::as_str), Some("page_next"));

    // 用户 override：改 minus 的动作、禁用 equal、不提方案作者写的其余键。
    let ov: toml::Value =
        toml::from_str("[session_actions]\nminus = \"page_next\"\nequal = \"none\"\n").unwrap();
    mgr.write_schema_override("sa", &ov).unwrap();

    let merged = mgr.active_session_actions();
    assert_eq!(
        merged.get("minus").map(String::as_str),
        Some("page_next"),
        "override 应逐键覆盖"
    );
    assert_eq!(
        merged.get("equal").map(String::as_str),
        Some("none"),
        "显式 none 必须留在表里——滤掉它，消费点会回落全局，用户的禁用就失效了"
    );

    let union = mgr.all_session_action_keys();
    assert!(union.contains("minus"), "启用的键要进可达性并集");
    assert!(
        !union.contains("equal"),
        "显式 none 的键不该进并集——否则白装一个全局钩子 / 白转发一个不动作的键"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// 方案级 `[key_actions]`：方案文件内联 + override 逐键合并，不是整段替换。
/// `docs/design/schema-key-actions.md` §3 的端到端确认：贯通到 `active_key_actions()` 的返回值。
#[test]
fn active_key_actions_merges_schema_file_and_override_per_key() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_ka_data");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    let mut f = std::fs::File::create(schemas.join("ka.schema.toml")).unwrap();
    // 方案作者内联：`\` 进快符，z 进临拼。
    write!(
        f,
        "[schema]\nid = \"ka\"\n[engine]\ntype = \"codetable\"\n\
             [key_actions]\nbackslash = \"special:fuhao\"\nz = \"temp_pinyin\"\n"
    )
    .unwrap();
    drop(f);

    let ov_dir = std::env::temp_dir().join("wind_eng_ka_overrides");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mut cfg = Config::default();
    cfg.schema.active = "ka".to_string();
    cfg.schema.available = vec!["ka".to_string()];
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    let inline = mgr.active_key_actions();
    assert_eq!(
        inline.get("backslash").map(String::as_str),
        Some("special:fuhao")
    );
    assert_eq!(inline.get("z").map(String::as_str), Some("temp_pinyin"));

    // 用户 override：改 z 的去向、给 grave 加禁用、不提 backslash。
    let ov: toml::Value =
        toml::from_str("[key_actions]\nz = \"temp_english\"\ngrave = \"none\"\n").unwrap();
    mgr.write_schema_override("ka", &ov).unwrap();

    let merged = mgr.active_key_actions();
    assert_eq!(
        merged.get("backslash").map(String::as_str),
        Some("special:fuhao"),
        "override 未提及的键必须保留——整段替换会让它消失"
    );
    assert_eq!(
        merged.get("z").map(String::as_str),
        Some("temp_english"),
        "同名键被 override 覆盖"
    );
    assert_eq!(
        merged.get("grave").map(String::as_str),
        Some("none"),
        "override 新增的禁用项加入"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// `[[dictionaries]]` 下的 `font_path` / `font_family` 不是受支持的键：要进 `degraded_items`
/// （toast 的数据源），且方案其余部分照常读出。
#[test]
fn font_keys_under_dictionaries_are_reported_not_silently_dropped() {
    let base_dir =
        std::env::temp_dir().join(format!("wind_eng_dict_font_data_{}", std::process::id()));
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join("toli.schema.toml"),
        "[schema]\nid = \"toli\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"toli_main\"\npath = \"toli/toli.dict.yaml\"\n\
             font_path = \"toli/Menk.ttf\"\nfont_family = \"Menk\"\n",
    )
    .unwrap();

    let schema = EngineManager::read_schema("toli", Some(&base_dir), None)
        .expect("多写两个键不许让方案读不出来");
    assert_eq!(schema.dictionaries.len(), 1);
    assert_eq!(
        schema.degraded_items,
        vec![
            "[[dictionaries]] toli_main.font_path（请改用方案设置里的候选字体）".to_string(),
            "[[dictionaries]] toli_main.font_family（请改用方案设置里的候选字体）".to_string(),
        ]
    );

    let _ = std::fs::remove_dir_all(&base_dir);
}

/// 写在正确位置（`[engine.chaizi]`）的字体不许被误报。
#[test]
fn font_keys_in_engine_chaizi_are_not_reported() {
    let v: toml::Value = toml::from_str(
        "[engine.chaizi]\nfont_path = \"a.ttf\"\nfont_family = \"A\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"a.dict.yaml\"\n",
    )
    .unwrap();
    assert!(misplaced_dict_font_keys(&v).is_empty());
}

/// 用户 override 里写了值域外的值（如 `text_orientation = ""`），不许让整个方案读不出来。
///
/// `read_schema` 有约 30 个调用点，返回 `None` 时这个方案在系统里等于不存在且不报错。
/// 三条断言缺一不可：① 方案还在；② 同段的其它设置没被连累；③ 降级被记下来了（toast 的数据源）。
#[test]
fn bad_enum_value_in_override_degrades_instead_of_killing_the_schema() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_bad_enum_data");
    let ov_dir = std::env::temp_dir().join("wind_eng_bad_enum_ov");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::create_dir_all(&ov_dir).unwrap();
    let mut f = std::fs::File::create(schemas.join("toli.schema.toml")).unwrap();
    write!(
        f,
        "[schema]
id = \"toli\"
name = \"托利\"
[engine]
type = \"codetable\"
"
    )
    .unwrap();
    drop(f);
    // 用户的 override，逐字取自事故现场。
    std::fs::write(
        ov_dir.join("toli.toml"),
        "[candidate]\nfont_family = \"Noto Sans Mongolian\"\ntext_orientation = \"\"\n",
    )
    .unwrap();

    let schema = EngineManager::read_schema("toli", Some(&base_dir), Some(&ov_dir))
        .expect("① 一个字段写错不许让整个方案读不出来");

    assert_eq!(
        schema.candidate.text_orientation,
        wind_config::config::TextOrientation::Normal,
        "写错的那一项回落出厂默认"
    );
    assert_eq!(
        schema.candidate.font_family, "Noto Sans Mongolian",
        "② 同段的其它设置不许被连累——这正是字段级容错优于段级降级的地方"
    );
    assert!(
        !schema.degraded_items.is_empty(),
        "③ 降级必须被记下来，否则 toast 无从弹起，用户仍然是「写了没反应」"
    );
    assert!(
        schema.degraded_items.iter().any(|s| s.contains("\"\"")),
        "记录里要带上用户写的原值，他才搜得到自己文件里的哪一行：{:?}",
        schema.degraded_items
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// 方案级 `[punct]` / `[candidate]` / `[phrases]`：内联 + override 合并，三段同批失效。
/// 额外钉住三段共用一个缓存条目时的失效正确性：override 只改一段，另两段不能被清成默认值。
#[test]
fn active_behavior_merges_schema_file_and_override() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_beh_data");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    let mut f = std::fs::File::create(schemas.join("beh.schema.toml")).unwrap();
    write!(
        f,
        "[schema]
id = \"beh\"
[engine]
type = \"codetable\"
             [punct]
mode = \"english\"
             [candidate]
layout = \"vertical\"
             [phrases]
enabled = false
"
    )
    .unwrap();
    drop(f);

    let ov_dir = std::env::temp_dir().join("wind_eng_beh_overrides");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mut cfg = Config::default();
    cfg.schema.active = "beh".to_string();
    cfg.schema.available = vec!["beh".to_string()];
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    let inline = mgr.active_behavior();
    assert_eq!(inline.punct, wind_config::PunctIntent::English);
    assert_eq!(inline.candidate_layout, wind_config::LayoutIntent::Vertical);
    assert!(!inline.phrases.enabled);

    // 用户 override：只改布局，不提另外两段。
    let ov: toml::Value = toml::from_str(
        "[candidate]
layout = \"horizontal\"
",
    )
    .unwrap();
    mgr.write_schema_override("beh", &ov).unwrap();

    let merged = mgr.active_behavior();
    assert_eq!(
        merged.candidate_layout,
        wind_config::LayoutIntent::Horizontal,
        "override 覆盖同段"
    );
    assert_eq!(
        merged.punct,
        wind_config::PunctIntent::English,
        "override 未提及的段必须保留——三段合订若写成「读一段存一段」这里会回落 Follow"
    );
    assert!(!merged.phrases.enabled, "override 未提及的段必须保留");

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// 读不到方案（文件缺失）时三段全部回落默认值 = 一段都不覆盖。
/// 否则装歪的方案包会静默改掉用户的标点态与候选方向。
#[test]
fn active_behavior_defaults_to_no_override_when_schema_missing() {
    let mut cfg = Config::default();
    cfg.schema.active = "nonexistent_schema".to_string();
    let dir = std::env::temp_dir().join("wind_eng_beh_missing");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    let mgr = EngineManager::with_store_override(&cfg, Some(&dir), None, Some(dir.clone()));

    let b = mgr.active_behavior();
    assert_eq!(b.punct, wind_config::PunctIntent::Follow);
    assert_eq!(b.candidate_layout, wind_config::LayoutIntent::Follow);
    assert!(
        b.phrases.enabled,
        "短语默认加载——默认关会让所有方案静默失去短语"
    );
    assert!(b.phrases.categories.is_empty());
    assert!(b.phrases.exclude_categories.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// `schema_code_char_set` 必须与引擎构建同规则：`leading_chars` 为空时等于全集。
/// 它服务于设置页的「这个键是不是本方案首码」提示，规则不一致就会指向内核并不认可的冲突。
#[test]
fn schema_code_char_set_follows_engine_rules() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_ccs_data");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    // input_chars 含 `;`，leading_chars 未写 → 首码集 = 全集，故 `;` 可起头。
    let mut f = std::fs::File::create(schemas.join("ccs.schema.toml")).unwrap();
    write!(
        f,
        "[schema]
id = \"ccs\"
[engine]
type = \"codetable\"
             [engine.codetable]
input_chars = \"a-z;\"
"
    )
    .unwrap();
    drop(f);

    let mut cfg = Config::default();
    cfg.schema.active = "ccs".to_string();
    cfg.schema.available = vec!["ccs".to_string()];
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, None);

    let set = mgr.schema_code_char_set("ccs").expect("方案应可读");
    assert!(set.contains_leading(';'), "leading_chars 为空时应等于全集");
    assert!(set.contains_leading('a'));
    assert!(!set.contains_leading('/'), "没配的符号不该在首码集里");

    // 读不到的方案返回 None，而不是默认 a-z——拿默认值去提示等于凭空报冲突。
    assert!(mgr.schema_code_char_set("no_such_schema").is_none());

    let _ = std::fs::remove_dir_all(&base_dir);
}

/// 符号通配键只算纯码表方案：拼音方案经折叠也继承全局 `wildcard` 但不消费它，
/// 算进去会让「空闲透传标点」在拼音下也白吃这个符号。字母键不算（字母本不在透传集）。
#[test]
fn schema_lead_inputs_counts_symbol_wildcard_only_for_codetable() {
    let base_dir = std::env::temp_dir().join(format!("wind_eng_lead_in_{}", std::process::id()));
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    for (id, ty) in [("lct", "codetable"), ("lpy", "pinyin")] {
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!("[schema]\nid = \"{id}\"\n[engine]\ntype = \"{ty}\"\n"),
        )
        .unwrap();
    }
    let mk = |key: &str| {
        let mut cfg = Config::default();
        cfg.schema.active = "lct".to_string();
        cfg.schema.available = vec!["lct".to_string()];
        cfg.schema.codetable.wildcard = true;
        cfg.schema.codetable.wildcard_key = key.to_string();
        EngineManager::with_store_override(&cfg, Some(&base_dir), None, None)
    };
    let m = mk("/");
    assert_eq!(m.schema_lead_inputs("lct").unwrap().1, Some('/'));
    assert_eq!(m.schema_lead_inputs("lpy").unwrap().1, None, "拼音方案不算");
    assert_eq!(
        mk("z").schema_lead_inputs("lct").unwrap().1,
        None,
        "字母键不算"
    );
    let _ = std::fs::remove_dir_all(&base_dir);
}

/// 方案没写 `[key_actions]` 时返回空表——各键照常走全局引导键链。
#[test]
fn active_key_actions_empty_when_schema_declares_none() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_ka_none_data");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    let mut f = std::fs::File::create(schemas.join("kan.schema.toml")).unwrap();
    write!(
        f,
        "[schema]\nid = \"kan\"\n[engine]\ntype = \"codetable\"\n"
    )
    .unwrap();
    drop(f);

    let mut cfg = Config::default();
    cfg.schema.active = "kan".to_string();
    cfg.schema.available = vec!["kan".to_string()];
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, None);
    assert!(mgr.active_key_actions().is_empty());

    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn schema_override_merge_and_delete() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_eng_ov_data");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    let mut f = std::fs::File::create(schemas.join("tcfg.schema.toml")).unwrap();
    write!(
        f,
        "[schema]\nid = \"tcfg\"\nname = \"基础名\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\n"
    )
    .unwrap();
    drop(f);

    let ov_dir = std::env::temp_dir().join("wind_eng_ov_overrides");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let cfg = Config::default();
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    // 基础层
    let base = mgr.schema_base("tcfg").unwrap();
    assert_eq!(base.schema.name, "基础名");
    assert_eq!(base.engine.codetable.max_code_length, 4);

    // 写 override：覆盖 name + max_code_length
    let ov: toml::Value =
        toml::from_str("[schema]\nname = \"覆盖名\"\n[engine.codetable]\nmax_code_length = 5\n")
            .unwrap();
    mgr.write_schema_override("tcfg", &ov).unwrap();

    let merged = mgr.schema_merged("tcfg").unwrap();
    assert_eq!(merged.schema.name, "覆盖名", "override 覆盖 name");
    assert_eq!(merged.engine.codetable.max_code_length, 5);
    assert_eq!(merged.schema.id, "tcfg", "未覆盖字段保留基础值");
    // base 不受 override 影响
    assert_eq!(mgr.schema_base("tcfg").unwrap().schema.name, "基础名");

    // 删除 override → 回到基础层
    assert!(mgr.delete_schema_override("tcfg").unwrap());
    assert_eq!(mgr.schema_merged("tcfg").unwrap().schema.name, "基础名");

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// shuangpin 方案在 EngineManager 层的 available 过滤中被放行，不支持的双拼布局被滤掉。
///
/// 夹具：`dummy_ct`（codetable，active）、`sp_test`（scheme="shuangpin"，留下）、
/// `sp_unsupported`（scheme="ziranma_xxx"，滤掉）。不触发词库加载，无词库环境可跑。
#[test]
fn shuangpin_available_not_filtered_out() {
    use std::io::Write;

    // 建 temp 数据目录
    let base_dir = std::env::temp_dir().join("wind_eng_sp_available_test");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();

    // active schema：最小 codetable，无词库（ensure_loaded 失败 = warn，manager 仍构造成功）
    {
        let mut f = std::fs::File::create(schemas.join("dummy_ct.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"dummy_ct\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
    }

    // shuangpin schema：engine.type="pinyin" + scheme="shuangpin" → is_supported()=true
    {
        let mut f = std::fs::File::create(schemas.join("sp_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"sp_test\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"shuangpin\"\n"
        )
        .unwrap();
    }

    // 不支持的双拼变体：engine.type="pinyin" + scheme="ziranma_xxx" → is_supported()=false
    {
        let mut f = std::fs::File::create(schemas.join("sp_unsupported.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"sp_unsupported\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"ziranma_xxx\"\n"
        )
        .unwrap();
    }

    // 构造 config：active = dummy_ct（首个 available 即为 active）
    let mut cfg = Config::default();
    cfg.schema.active = "dummy_ct".into();
    cfg.schema.available = vec!["dummy_ct".into(), "sp_test".into(), "sp_unsupported".into()];

    let ov_dir = std::env::temp_dir().join("wind_eng_sp_available_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    let available = mgr.available_schemas();

    // shuangpin 方案应通过过滤，进入 available 列表
    assert!(
        available.contains(&"sp_test".to_string()),
        "shuangpin schema 应在 available 中，实际 available={available:?}"
    );

    // 不支持的 scheme 应被过滤掉（过滤仍有效）
    assert!(
        !available.contains(&"sp_unsupported".to_string()),
        "ziranma_xxx schema 应被过滤，实际 available={available:?}"
    );

    // active 方案始终保留（不论 schema_supported 结果如何）
    assert!(
        available.contains(&"dummy_ct".to_string()),
        "active schema 应始终保留，实际 available={available:?}"
    );

    // 清理
    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// 双拼布局的非字母键（mspy `;` = ing）必须成为码元、但不可作首码。
///
/// 协调器只认 `active_is_code_char`：这里为 false 时 `;` 会被次选键 / quick_mix 引导键 /
/// 标点流水线依次拦下。首码为 false 同样是硬要求：`;` 能起头的话空缓冲按 `;` 归码表，
/// 快捷输入再也进不去。必须用 `build_dev/data`：码元集挂在活跃引擎上，引擎建不起来就回落 `a-z`。
#[test]
fn shuangpin_symbol_final_is_code_char_but_not_leading() {
    let data_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data_dir.join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺 build_dev/data（未构建）");
        return;
    }

    // override 把内置 shuangpin 方案的布局换成 mspy（`;` = ing），不动真实方案文件。
    let ov_dir = std::env::temp_dir().join("wind_eng_sp_charset_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);
    std::fs::create_dir_all(&ov_dir).unwrap();
    std::fs::write(
        ov_dir.join("shuangpin.toml"),
        "[engine.pinyin.shuangpin]\nlayout = \"mspy\"\n",
    )
    .unwrap();

    let mut cfg = Config::default();
    cfg.schema.active = "shuangpin".into();
    cfg.schema.available = vec!["shuangpin".into()];

    let mgr = EngineManager::with_store_override(&cfg, Some(&data_dir), None, Some(ov_dir.clone()));

    // mspy `;` = ing → 码元，但只能作第二码
    assert!(mgr.active_is_code_char(';'), "mspy `;` 应是码元（= ing）");
    assert!(
        !mgr.active_is_leading_char(';'),
        "mspy `;` 只作韵母（第二码），不得进首码集——否则夺走 quick_mix 引导键"
    );
    // 字母键不受影响：仍是码元且可起头
    assert!(mgr.active_is_code_char('k'), "字母 k 应是码元");
    assert!(mgr.active_is_leading_char('n'), "声母 n 应可作首码");
    // 布局没用到的符号不得混进来
    assert!(!mgr.active_is_code_char('['), "mspy 未用 `[`，不应是码元");

    // 反向对照：换回全字母布局（小鹤）后必须回落默认集——否则上面的断言可能只是
    // 「某处无条件把 `;` 放进码元集」，而不是真的从布局推导出来的。
    let ov2 = std::env::temp_dir().join("wind_eng_sp_charset_ov_xh");
    let _ = std::fs::remove_dir_all(&ov2);
    std::fs::create_dir_all(&ov2).unwrap();
    std::fs::write(
        ov2.join("shuangpin.toml"),
        "[engine.pinyin.shuangpin]\nlayout = \"xiaohe\"\n",
    )
    .unwrap();
    let mgr2 = EngineManager::with_store_override(&cfg, Some(&data_dir), None, Some(ov2.clone()));
    assert!(
        !mgr2.active_is_code_char(';'),
        "小鹤布局无符号键 → `;` 不得是码元"
    );
    assert!(
        mgr2.active_input_chars().is_default_alpha(),
        "全字母布局应回落内置 a-z"
    );

    let _ = std::fs::remove_dir_all(&ov_dir);
    let _ = std::fs::remove_dir_all(&ov2);
}

/// 对照：非双拼方案（codetable，未配 `input_chars`）码元集回落内置 `a-z`，
/// `;` 不得因为「别的方案用它作韵母」而变成码元——码元集是方案级的。
#[test]
fn non_shuangpin_schema_keeps_default_alpha_charset() {
    use std::io::Write;

    let base_dir = std::env::temp_dir().join("wind_eng_sp_finalkey_ct_test");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    {
        let mut f = std::fs::File::create(schemas.join("wubi.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"wubi\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
    }

    let mut cfg = Config::default();
    cfg.schema.active = "wubi".into();
    cfg.schema.available = vec!["wubi".into()];

    let ov_dir = std::env::temp_dir().join("wind_eng_sp_finalkey_ct_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    assert!(
        !mgr.active_is_code_char(';'),
        "codetable 方案未配 input_chars → `;` 不是码元"
    );
    assert!(mgr.active_is_code_char('k'), "字母仍是码元（回落 a-z）");
    assert!(
        mgr.active_input_chars().is_default_alpha(),
        "未配 input_chars 的方案必须恰好回落内置 a-z"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// installed_schemas 返回所有已安装且受支持的方案，不受 available 限制。
///
/// 夹具：`dummy_active`（active）、`sp_installed` 与 `ct_installed`（已装但不在 available ⇒
/// installed 含、available 不含）、`unsupported_installed`（不受支持 ⇒ installed 不含）。
#[test]
fn installed_schemas_includes_all_supported_not_just_available() {
    use std::io::Write;

    let base_dir = std::env::temp_dir().join("wind_eng_installed_schemas_test");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();

    // active（codetable）
    {
        let mut f = std::fs::File::create(schemas.join("dummy_active.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"dummy_active\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
    }

    // 已安装双拼（shuangpin），未在 available → 应出现在 installed_schemas
    {
        let mut f = std::fs::File::create(schemas.join("sp_installed.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"sp_installed\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"shuangpin\"\n"
        )
        .unwrap();
    }

    // 已安装但不受支持（scheme="ziranma_xxx"）→ 应被过滤
    {
        let mut f =
            std::fs::File::create(schemas.join("unsupported_installed.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"unsupported_installed\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"ziranma_xxx\"\n"
        )
        .unwrap();
    }

    // 已安装 codetable，未在 available → 应出现在 installed_schemas
    {
        let mut f = std::fs::File::create(schemas.join("ct_installed.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"ct_installed\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
    }

    // config.available 只含 active，其余方案未启用
    let mut cfg = Config::default();
    cfg.schema.active = "dummy_active".into();
    cfg.schema.available = vec!["dummy_active".into()];

    let ov_dir = std::env::temp_dir().join("wind_eng_installed_schemas_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    let available = mgr.available_schemas();
    let installed = mgr.installed_schemas();

    // available 只含 active，未启用方案不在其中
    assert_eq!(available, vec!["dummy_active".to_string()]);

    // installed 含 active
    assert!(
        installed.contains(&"dummy_active".to_string()),
        "active 应在 installed_schemas 中，实际={installed:?}"
    );

    // installed 含未启用的双拼方案
    assert!(
        installed.contains(&"sp_installed".to_string()),
        "已安装 shuangpin 方案应在 installed_schemas 中，实际={installed:?}"
    );

    // installed 含未启用的 codetable 方案
    assert!(
        installed.contains(&"ct_installed".to_string()),
        "已安装 codetable 方案应在 installed_schemas 中，实际={installed:?}"
    );

    // 不受支持的方案被过滤掉
    assert!(
        !installed.contains(&"unsupported_installed".to_string()),
        "不支持的方案不应在 installed_schemas 中，实际={installed:?}"
    );

    // 结果有序（字典序）
    let mut sorted = installed.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        installed, sorted,
        "installed_schemas 应按字典序排序且无重复"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// scan_shuangpin_layouts：合并扫描多层、靠前的层优先（user > custom > data）、
/// 跳过解析失败（缺 [finals]）的布局、按 id 字典序排序。
#[test]
fn scan_shuangpin_layouts_merges_user_priority() {
    use std::io::Write;

    // 带 pid：多 worktree / 多会话并行跑测试时，固定名 + `remove_dir_all` 会互删夹具。
    let base =
        std::env::temp_dir().join(format!("wind_eng_sp_layouts_test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let install = base.join("install");
    let custom = base.join("custom");
    let user = base.join("user");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&custom).unwrap();
    std::fs::create_dir_all(&user).unwrap();

    let write_layout = |dir: &std::path::Path, file: &str, id: &str, name: &str, finals: bool| {
        let mut f = std::fs::File::create(dir.join(file)).unwrap();
        let finals_sec = if finals {
            "[finals]\na = [\"a\"]\n"
        } else {
            ""
        };
        write!(f, "[meta]\nid = \"{id}\"\nname = \"{name}\"\n{finals_sec}").unwrap();
    };

    // 安装目录：xiaohe、mspy
    write_layout(&install, "xiaohe.toml", "xiaohe", "小鹤双拼", true);
    write_layout(&install, "mspy.toml", "mspy", "微软双拼", true);
    // 用户目录：新增 shoudao + 同名覆盖 xiaohe（改显示名）
    write_layout(&user, "shoudao.toml", "shoudao", "手道双拼", true);
    write_layout(&user, "xiaohe.toml", "xiaohe", "小鹤(用户版)", true);
    // 用户目录：损坏布局（缺 [finals]）应被跳过
    write_layout(&user, "broken.toml", "broken", "坏的", false);
    // 定制层：新增 huma；mspy 盖住安装层；xiaohe 自己被用户层盖住（夹在中间的一层
    // 既能盖人也能被盖，正是「两层扩成层序」最容易漏掉的那一格）。
    write_layout(&custom, "huma.toml", "huma", "虎码双拼", true);
    write_layout(&custom, "mspy.toml", "mspy", "微软双拼(定制版)", true);
    write_layout(&custom, "xiaohe.toml", "xiaohe", "小鹤(定制版)", true);

    // 层序：user > custom > data
    let layers = vec![
        wind_config::ResourceLayer::new("user", user.clone()),
        wind_config::ResourceLayer::new("custom", custom.clone()),
        wind_config::ResourceLayer::new("data", install.clone()),
    ];
    let got = EngineManager::scan_shuangpin_layouts(&layers);

    assert_eq!(
        got,
        vec![
            ("xiaohe".to_string(), "小鹤(用户版)".to_string()),
            ("mspy".to_string(), "微软双拼(定制版)".to_string()),
            ("huma".to_string(), "虎码双拼".to_string()),
            ("shoudao".to_string(), "手道双拼".to_string()),
        ],
        "布局枚举应合并、靠前的层优先、跳过损坏、内置方案按流行度排序，实际={got:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// data_schema_id 拼音族折叠为 "pinyin"；码表与未知方案返回自身 id。
#[test]
fn data_schema_id_folds_pinyin_and_returns_self() {
    use std::io::Write;

    let base_dir = std::env::temp_dir().join("wind_eng_data_schema_id_test");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();

    // 拼音方案
    {
        let mut f = std::fs::File::create(schemas.join("py_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"py_test\"\n[engine]\ntype = \"pinyin\"\n"
        )
        .unwrap();
    }

    // 码表方案
    {
        let mut f = std::fs::File::create(schemas.join("ct_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"ct_test\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
    }

    let mut cfg = Config::default();
    cfg.schema.active = "ct_test".into();
    cfg.schema.available = vec!["ct_test".into(), "py_test".into()];

    let ov_dir = std::env::temp_dir().join("wind_eng_data_schema_id_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    // 拼音方案折叠为 "pinyin"
    assert_eq!(
        mgr.data_schema_id("py_test"),
        "pinyin",
        "拼音方案 data_schema_id 应返回 pinyin"
    );

    // 码表方案返回自身 id
    assert_eq!(
        mgr.data_schema_id("ct_test"),
        "ct_test",
        "码表方案 data_schema_id 应返回自身 id"
    );

    // 未知方案（schema_engine_type=None）返回自身 id
    assert_eq!(
        mgr.data_schema_id("nonexistent"),
        "nonexistent",
        "未知方案 data_schema_id 应返回自身 id"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// 按键路径取的方案级字段（混输成员、分隔符策略、辅助码设置）只在首次读盘，之后走缓存；
/// 方案失效后重读并取到新值。计数按线程，故并行测试互不干扰。
#[test]
fn hot_path_schema_fields_read_disk_once() {
    let base_dir = std::env::temp_dir().join("wind_eng_hot_path_schema_reads");
    let schemas = base_dir.join("schemas");
    let ov_dir = std::env::temp_dir().join("wind_eng_hot_path_schema_reads_ov");
    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::create_dir_all(&ov_dir).unwrap();
    let write = |name: &str, body: &str| {
        std::fs::write(schemas.join(format!("{name}.schema.toml")), body).unwrap()
    };
    write(
        "py_hp",
        "[schema]\nid = \"py_hp\"\n[engine]\ntype = \"pinyin\"\n",
    );
    write(
        "ct_hp",
        "[schema]\nid = \"ct_hp\"\n[engine]\ntype = \"codetable\"\n",
    );
    write(
        "mx_hp",
        "[schema]\nid = \"mx_hp\"\n[engine]\ntype = \"mixed\"\n[engine.mixed]\n\
             primary_schema = \"ct_hp\"\nsecondary_schema = \"py_hp\"\n",
    );
    let mut cfg = Config::default();
    cfg.schema.active = "mx_hp".into();
    cfg.schema.available = vec!["mx_hp".into(), "ct_hp".into(), "py_hp".into()];
    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    let touch = || {
        assert_eq!(
            mgr.write_data_schema_id("mx_hp", CandidateSource::CodeTable),
            Some("ct_hp".to_string())
        );
        let _ = mgr.pinyin_separator_mode_of("mx_hp");
        let _ = mgr.aux_code_settings_cached_of("mx_hp");
        mgr.mixed_secondary_schema("mx_hp")
    };
    assert_eq!(touch(), Some("py_hp".to_string()));
    let warmed = schema_reads_on_current_thread();
    for _ in 0..3 {
        touch();
    }
    assert_eq!(
        schema_reads_on_current_thread(),
        warmed,
        "预热后不应再读方案文件"
    );

    std::fs::write(
        ov_dir.join("mx_hp.toml"),
        "[engine.mixed]\nsecondary_schema = \"py_hp2\"\n",
    )
    .unwrap();
    mgr.invalidate_schema("mx_hp");
    assert_eq!(
        touch(),
        Some("py_hp2".to_string()),
        "失效后应取到 override 新值"
    );
    assert!(schema_reads_on_current_thread() > warmed);

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// P2d Task 1：write_data_schema_id 混输按候选来源分流；非混输忽略 source。
#[test]
fn write_data_schema_id_routes_mixed_by_source() {
    use std::io::Write;

    let base_dir = std::env::temp_dir().join("wind_eng_write_data_schema_id_test");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();

    // 拼音方案
    {
        let mut f = std::fs::File::create(schemas.join("py_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"py_test\"\n[engine]\ntype = \"pinyin\"\n"
        )
        .unwrap();
    }
    // 码表方案
    {
        let mut f = std::fs::File::create(schemas.join("ct_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"ct_test\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
    }
    // 混输方案
    {
        let mut f = std::fs::File::create(schemas.join("mx_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"mx_test\"\n[engine]\ntype = \"mixed\"\n[engine.mixed]\nprimary_schema = \"ct_test\"\nsecondary_schema = \"py_test\"\n"
        )
        .unwrap();
    }

    let mut cfg = Config::default();
    cfg.schema.active = "mx_test".into();
    cfg.schema.available = vec!["mx_test".into(), "ct_test".into(), "py_test".into()];

    let ov_dir = std::env::temp_dir().join("wind_eng_write_data_schema_id_ov");
    let _ = std::fs::remove_dir_all(&ov_dir);

    let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

    // 非混输：忽略 source，等价 data_schema_id
    assert_eq!(
        mgr.write_data_schema_id("py_test", CandidateSource::None),
        Some("pinyin".to_string()),
        "拼音方案忽略 source，折叠为 pinyin"
    );
    assert_eq!(
        mgr.write_data_schema_id("ct_test", CandidateSource::Pinyin),
        Some("ct_test".to_string()),
        "码表方案忽略 source，返回自身 id"
    );

    // 混输：按来源分流
    assert_eq!(
        mgr.write_data_schema_id("mx_test", CandidateSource::CodeTable),
        Some("ct_test".to_string()),
        "混输 + CodeTable → 主码表方案 id"
    );
    assert_eq!(
        mgr.write_data_schema_id("mx_test", CandidateSource::Pinyin),
        Some("pinyin".to_string()),
        "混输 + Pinyin → pinyin"
    );
    assert_eq!(
        mgr.write_data_schema_id("mx_test", CandidateSource::None),
        None,
        "混输 + None → 无法归因，跳过"
    );
    assert_eq!(
        mgr.write_data_schema_id("mx_test", CandidateSource::Phrase),
        None,
        "混输 + Phrase → 无法归因，跳过"
    );
    assert_eq!(
        mgr.write_data_schema_id("mx_test", CandidateSource::English),
        None,
        "混输 + English → 无法归因，跳过"
    );

    // mixed_primary_schema
    assert_eq!(
        mgr.mixed_primary_schema("mx_test"),
        Some("ct_test".to_string()),
        "混输方案的主码表方案 id"
    );
    assert_eq!(
        mgr.mixed_primary_schema("ct_test"),
        None,
        "非混输方案 mixed_primary_schema 返回 None"
    );
    assert_eq!(
        mgr.mixed_primary_schema("nonexistent"),
        None,
        "未知方案 mixed_primary_schema 返回 None"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
    let _ = std::fs::remove_dir_all(&ov_dir);
}

/// 反查索引被 build_guard 跳过后，本进程不得再反复尝试：否则协调器「预热 → Skip →
/// 通知重绘 → 再预热」无限循环。缓存失效（如重建词库缓存）后才给一次重试机会。
#[test]
fn reverse_index_skip_is_remembered_until_invalidated() {
    use std::sync::atomic::Ordering::Relaxed;
    let base = std::env::temp_dir().join(format!("wind_eng_riskip-{}", std::process::id()));
    let schemas = base.join("schemas");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(schemas.join("riskip")).unwrap();
    let sid = format!("riskip_{}", std::process::id());
    std::fs::write(
        schemas.join(format!("{sid}.schema.toml")),
        format!(
            "[schema]\nid = \"{sid}\"\n[engine]\ntype = \"codetable\"\n\
                 [[dictionaries]]\nid = \"d\"\npath = \"riskip/d.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join("riskip/d.dict.yaml"),
        "---\nname: d\n...\n工\ta\t100\n",
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec![sid.clone()];
    cfg.schema.active = sid.clone();
    let mgr = EngineManager::new(&cfg, Some(&base));

    let Some(cache) = EngineManager::reverse_index_cache_path(&sid) else {
        eprintln!("!!! 无缓存根，build_guard 不参与，本测试没有真正运行");
        return;
    };
    let schema = EngineManager::read_schema(&sid, Some(&base), mgr.override_dir.as_deref())
        .expect("方案可读");
    let dicts = EngineManager::load_dicts_individually(&schema, &schemas);
    let dg = EngineManager::reverse_index_source_digests(&dicts).expect("词库有源文件");
    let key = wind_dict::cache_fp::derived_build_key(&dg, wind_dict::cache_fp::REVERSE_INDEX_TAG);
    let mut marker = cache.clone().into_os_string();
    marker.push(".building");
    let marker = std::path::PathBuf::from(marker);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    struct Cleanup(Vec<std::path::PathBuf>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = std::fs::remove_dir_all(p);
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let _cleanup = Cleanup(vec![base.clone(), cache.parent().unwrap().to_path_buf()]);
    // 已连续死满 MAX_ATTEMPTS 次、且无活着的持有者。
    std::fs::write(
        &marker,
        format!("{key}\n{}\n", wind_dict::build_guard::MAX_ATTEMPTS),
    )
    .unwrap();

    assert!(!mgr.prewarm_reverse_index(&sid), "被跳过时不得报「建成」");
    assert!(mgr.reverse_index_skipped(&sid));
    let calls = mgr.reverse_index_build_calls.load(Relaxed);
    assert_eq!(calls, 1);
    assert!(!mgr.prewarm_reverse_index(&sid));
    assert_eq!(
        mgr.reverse_index_build_calls.load(Relaxed),
        calls,
        "已记下跳过，第二次预热不得再走构建（再读方案、再过 build_guard）"
    );

    // 失效后重试：清掉死亡记录再重建即成功。
    let _ = std::fs::remove_file(&marker);
    mgr.invalidate_schema(&sid);
    assert!(!mgr.reverse_index_skipped(&sid));
    assert!(mgr.prewarm_reverse_index(&sid), "失效后应重试并建成");
    assert_eq!(
        mgr.word_codes_in(&sid, "工").as_deref(),
        Some("a"),
        "重试建成的是真索引"
    );
}

/// A1-7：merged 缓存的构建若连续死在中途（内存不足 abort，不可捕获），下次启动必须
/// 跳过这次必败的构建而不是再死一次；源文件一变就恢复尝试。
#[test]
fn merged_pinyin_build_skipped_after_repeated_deaths() {
    /// 无论断言成败都清掉：源目录，以及 merged 缓存那一组（`CACHE_DIR` 若已被同进程别的
    /// 用例设成共享缓存根，它们就不在源目录下）。
    struct Cleanup(Vec<std::path::PathBuf>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = std::fs::remove_dir_all(p);
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("wind_eng_a17-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dict = dir.join("py.dict.yaml");
    std::fs::write(&dict, "---\nname: py\n...\n你好\tni hao\t100\n").unwrap();
    let merged = cache_path(&dict, "merged.wdat");
    let key = wind_dict::cache_fp::build_key(&[dict.as_path()], MERGED_CACHE_TAG);
    let sibling = |suffix: &str| {
        let mut s = merged.clone().into_os_string();
        s.push(suffix);
        std::path::PathBuf::from(s)
    };
    let marker = sibling(".building");
    let _cleanup = Cleanup(vec![dir.clone(), marker.clone(), sibling(".st")]);
    // 内容寻址：产物是 `<stem>-<fp>.wdat` 变体，不是槽位名本身
    let variants = |slot: &std::path::Path| -> Vec<std::path::PathBuf> {
        let prefix = format!("{}-", slot.file_stem().unwrap().to_str().unwrap());
        std::fs::read_dir(slot.parent().unwrap())
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".wdat"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for v in variants(&merged) {
        let _ = std::fs::remove_file(&v);
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }

    // 模拟前 MAX_ATTEMPTS 次启动都死在构建里：标记留在盘上、且没有活着的持有者
    // （无持有者行＝持有者已死；本进程自己登记的会被判为「在建」而不计数）。
    std::fs::write(
        &marker,
        format!("{key}\n{}\n", wind_dict::build_guard::MAX_ATTEMPTS),
    )
    .unwrap();
    assert!(
        EngineManager::load_rime_pinyin_dict(&dict).is_none(),
        "同一输入已连续死在构建中，本次应降级而不是再建"
    );
    assert!(variants(&merged).is_empty(), "跳过时不应产出缓存");

    // 源变了 → 重新尝试并成功，且成功后不再留记录。
    std::fs::write(&dict, "---\nname: py\n...\n你好\tni hao\t200\n").unwrap();
    assert!(EngineManager::load_rime_pinyin_dict(&dict).is_some());
    assert!(!marker.exists());
}

/// 子表各自成段 → k 路归并 → 流式写入器的 merged 产物，必须与「并行解析全部子表 → HashMap 聚合
/// → `WdatWriter`」的聚合算法逐字节相同；后者在本用例里原样复刻作 oracle。夹具覆盖：跨表同 code、
/// 跨表同 (text, code)（不去重）、同码多条等权（稳定序＝子表序）、单音节（无简拼）、多音节简拼跨表取最大权重。
#[test]
fn merge_from_segments_matches_aggregate_build() {
    let dir = std::env::temp_dir().join(format!("wind_eng_mergeeq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cells")).unwrap();
    let body_a = "你好\tni hao\t100\n你\tni\t500\n拟好\tni hao\t64\n你好吗\tni hao ma\t20\n\
                      中国\tzhong guo\t1000\n逆\tni\t64\n西安\txi an\t1000\n先\txian\t500\n\
                      习惯\txi guan\t300\n啊\ta\t1\n";
    let body_b = "你好呀\tni hao ya\t30\n尼豪\tni hao\t64\n中国风\tzhong guo feng\t50\n\
                      泥\tni\t64\n你好\tni hao\t300\n鲜\txian\t500\n西瓜\txi gua\t300\n\
                      你好\tni hao\t100\n\
                      安安\tan an\t261\n凹凹\tao ao\t261\n爱爱\tai ai\t93\n按按\tan an\t75\n\
                      挨挨\tai ai\t66\n欸\tê\t5\n欸哟\tê yo\t7\n女吕\tnü lü\t3\n";
    std::fs::write(
        dir.join("a.dict.yaml"),
        format!("---\nname: a\n...\n{body_a}"),
    )
    .unwrap();
    std::fs::write(
        dir.join("cells/b.dict.yaml"),
        format!("---\nname: b\n...\n{body_b}"),
    )
    .unwrap();
    let main = dir.join("main.dict.yaml");
    std::fs::write(
        &main,
        "---\nname: main\nimport_tables:\n  - a\n  - cells/b\n...\n",
    )
    .unwrap();

    // 新路径
    let got = EngineManager::load_rime_pinyin_dict(&main).unwrap();
    let got_path = got.source_file().expect("merged 应落盘").to_path_buf();
    let got_bytes = std::fs::read(&got_path).unwrap();

    // oracle：旧算法原样复刻
    let mut agg: HashMap<String, Vec<(String, i32, u64)>> = HashMap::new();
    let mut agg_ab: HashMap<String, HashMap<String, i32>> = HashMap::new();
    for sub in EngineManager::rime_source_paths(&main, "rime_pinyin") {
        let (fulls, abbrevs) =
            wind_dict::codetable::parse_rime_entries_parallel(&sub, false).unwrap();
        for (code, text, weight, boundary) in fulls {
            agg.entry(code).or_default().push((text, weight, boundary));
        }
        for (ab, code, weight) in abbrevs {
            let slot = agg_ab
                .entry(ab)
                .or_default()
                .entry(code)
                .or_insert(i32::MIN);
            *slot = (*slot).max(weight);
        }
    }
    let mut writer = wind_dict::datformat::WdatWriter::new();
    for (code, mut entries) in agg {
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        let with_order: Vec<(String, i32, u32, u64)> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (text, weight, boundary))| (text, weight, i as u32, boundary))
            .collect();
        writer.add_with_boundary(code, with_order);
    }
    for (ab, codes) in agg_ab {
        let mut entries: Vec<(String, i32)> = codes.into_iter().collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        writer.add_abbrev(ab, entries);
    }
    let oracle = dir.join("oracle.wdat");
    writer.write(&oracle).unwrap();
    let want = std::fs::read(&oracle).unwrap();

    assert_eq!(got_bytes.len(), want.len(), "长度不同");
    assert!(got_bytes == want, "段归并产物与旧算法逐字节不同");
    // 顺手验内容：跨表同 (text, code) 保留两条；简拼取跨表最大权重
    assert_eq!(got.search("nihao").len(), 5);
    let ab = got.search_abbrev("nh", 10);
    assert_eq!(ab.first().map(String::as_str), Some("nihao"));
    drop(got);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 分层视图（主库 + 细胞库各一层）与「主表 import 细胞库合成一份 merged」查询结果逐项
/// 一致：精确查、前缀查、按音节截断的前缀查、简拼、更长码存在性。
///
/// 夹具里刻意放的三类陷阱：
/// - 跨层同权（主库「拟好」与细胞库「尼豪」都是 64）：`order` 是层内名次，跨层直接比会排反；
/// - 被音节过滤掉的同码高权词（主库「西安」xi'an 1000 与单音节「先」同码 `xian`）：
///   读端先取名次再过滤，合并缓存里「先」的名次照样排在「西安」之后，拿候选池重算会算小；
/// - 跨层同一个词（两层都有「你好」）：合并缓存保留两条，分层也不去重。
///
/// 简拼 `xg` 的跨层同权码（主库 xiguan、细胞库 xigua 都是 300）验证按码字典序而非层序。
#[test]
fn layered_pinyin_dict_matches_merged_cache() {
    let dir = std::env::temp_dir().join(format!("wind_eng_layered-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cells")).unwrap();
    let body_main = "你好\tni hao\t100\n你\tni\t500\n拟好\tni hao\t64\n你好吗\tni hao ma\t20\n\
                         中国\tzhong guo\t1000\n逆\tni\t64\n\
                         西安\txi an\t1000\n先\txian\t500\n想\txiang\t500\n小\txiao\t10\n\
                         习惯\txi guan\t300\n";
    let body_cell = "你好呀\tni hao ya\t30\n尼豪\tni hao\t64\n中国风\tzhong guo feng\t50\n\
                         厚积薄发\thou ji bo fa\t5\n泥\tni\t64\n你好\tni hao\t300\n\
                         虾啊\txia a\t10\n鲜\txian\t500\n西瓜\txi gua\t300\n";
    let main = dir.join("main.dict.yaml");
    std::fs::write(&main, format!("---\nname: main\n...\n{body_main}")).unwrap();
    let cell = dir.join("cells/cell.dict.yaml");
    std::fs::write(&cell, format!("---\nname: cell\n...\n{body_cell}")).unwrap();
    // 对照组：同一批词经 import_tables 合成一份（旧的整合方式）
    std::fs::write(
        dir.join("mainbody.dict.yaml"),
        format!("---\nname: mb\n...\n{body_main}"),
    )
    .unwrap();
    let both = dir.join("both.dict.yaml");
    std::fs::write(
        &both,
        "---\nname: both\nimport_tables:\n  - mainbody\n  - cells/cell\n...\n",
    )
    .unwrap();

    let merged = EngineManager::load_rime_pinyin_dict(&both).unwrap();
    let layered = CachedDict::Layered(vec![
        EngineManager::load_rime_pinyin_dict(&main).unwrap(),
        EngineManager::load_rime_pinyin_dict(&cell).unwrap(),
    ]);

    let hits = |v: Vec<wind_dict::cached::DictHit>| {
        v.into_iter()
            .map(|h| (h.code, h.text, h.weight, h.order, h.boundary))
            .collect::<Vec<_>>()
    };
    for code in [
        "nihao",
        "ni",
        "zhongguo",
        "houjibofa",
        "nihaoya",
        "xian",
        "none",
    ] {
        assert_eq!(
            hits(layered.search_with_boundary(code)),
            hits(merged.search_with_boundary(code)),
            "精确查 {code}"
        );
        assert_eq!(
            layered.search(code),
            merged.search(code),
            "精确查(元组) {code}"
        );
    }
    for (prefix, limit) in [
        ("ni", 100),
        ("ni", 3),
        ("nihao", 2),
        ("zhong", 10),
        ("h", 5),
        ("xi", 10),
        ("x", 3),
    ] {
        assert_eq!(
            hits(layered.search_prefix_with_boundary(prefix, limit)),
            hits(merged.search_prefix_with_boundary(prefix, limit)),
            "前缀查 {prefix} limit={limit}"
        );
    }
    for (prefix, limit, syl, done) in [
        ("ni", 10, 2, 2),
        ("nihao", 10, 3, 5),
        ("zhongguo", 5, 2, 8),
        ("xia", 10, 1, 0),
        ("xi", 10, 1, 0),
        ("x", 4, 1, 0),
    ] {
        assert_eq!(
            hits(layered.search_prefix_with_boundary_syllable_capped(prefix, limit, syl, done)),
            hits(merged.search_prefix_with_boundary_syllable_capped(prefix, limit, syl, done)),
            "音节截断前缀查 {prefix}"
        );
    }
    for ab in ["nh", "nhy", "zg", "zgf", "hjbf", "xa", "xg", "xx"] {
        assert_eq!(
            layered.search_abbrev(ab, 10),
            merged.search_abbrev(ab, 10),
            "简拼 {ab}"
        );
    }
    for p in ["nihao", "zhongguo", "houjibofa", "ni"] {
        assert_eq!(
            layered.has_longer_code(p),
            merged.has_longer_code(p),
            "更长码 {p}"
        );
    }

    drop((merged, layered));
    for f in [&main, &cell, &both] {
        for ext in ["merged.wdat", "merged.wdat.fp"] {
            let _ = std::fs::remove_file(cache_path(f, ext));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn purge_cache_files_only_removes_cache_extensions() {
    let dir = std::env::temp_dir().join(format!("wind_eng_purge-{}", std::process::id()));
    let sub = dir.join("wubi86");
    std::fs::create_dir_all(&sub).unwrap();
    // 缓存产物（应删）：wdat / 多点 combined.wdat / fp 指纹 / wdb
    std::fs::write(sub.join("main.wdat"), b"x").unwrap();
    std::fs::write(sub.join("main.combined.wdat"), b"x").unwrap();
    std::fs::write(sub.join("main.wdat.fp"), b"x").unwrap();
    std::fs::write(dir.join("unigram.wdb"), b"x").unwrap();
    // 构建死亡记录（应删：「重建缓存」即手动重试，见 build_guard）
    std::fs::write(sub.join("main.wdat.building"), b"x").unwrap();
    // 英文词组索引及其写盘临时文件（应删，`english_phrase::is_phrase_cache_file`）
    std::fs::write(sub.join("phrase-x.wphr"), b"x").unwrap();
    std::fs::write(sub.join("phrase-x.wphr.123-0.tmp"), b"x").unwrap();
    // 非缓存文件（应留）——别人的 `.tmp` 也不归我们删
    std::fs::write(dir.join("note.txt"), b"x").unwrap();
    std::fs::write(sub.join("raw.dict.yaml"), b"x").unwrap();
    std::fs::write(sub.join("other.tmp"), b"x").unwrap();

    let (mut removed, mut failed) = (0usize, 0usize);
    purge_cache_files(&dir, &mut removed, &mut failed);
    assert_eq!((removed, failed), (7, 0));
    assert!(dir.join("note.txt").exists());
    assert!(sub.join("raw.dict.yaml").exists());
    assert!(sub.join("other.tmp").exists());
    assert!(!sub.join("phrase-x.wphr").exists());
    assert!(!sub.join("phrase-x.wphr.123-0.tmp").exists());
    assert!(!sub.join("main.wdat").exists());
    assert!(!dir.join("unigram.wdb").exists());

    let _ = std::fs::remove_dir_all(&dir);
}
/// 区间累加的两条规则：字符去重升序，词条数按条去重。
/// 写成「各字符命中数之和」的话，一条含两个注音的词条会被数两遍，「会影响 N 条词」随即失真。
#[test]
fn range_scan_counts_entries_once_per_entry() {
    let (start, end) = (0x3100, 0x312F); // 注音符号
    let mut s = super::RangeScan::default();
    s.tally("\u{3105}", start, end); // ㄅ：命中
    s.tally("\u{3105}\u{3106}", start, end); // ㄅㄆ：两个字符，仍只算一条词条
    s.tally("\u{3105}\u{3105}", start, end); // 同字符两次，也只算一条
    s.tally("\u{6211}", start, end); // 我：区间外，不计
    s.tally("", start, end);
    s.finish();

    assert_eq!(
        s.chars,
        vec!['\u{3105}', '\u{3106}'],
        "字符去重且按码位升序"
    );
    assert_eq!(s.entries, 3, "三条词条命中；若按字符数累加会得到 4");
}

/// 扫描不收空白与控制字符，哪怕它们落在区间里。
///
/// `ASCII` 块整块允许批量，而词库里含空格的词条成百上千；不挡的话「整类设为生僻」会给空格登记一条
/// 覆盖，含空格的候选全判非常用，且导出写得进去、导入却被拒。
#[test]
fn range_scan_skips_unmarkable_chars() {
    let (start, end) = (0x0020, 0x007F); // ASCII
    let mut s = super::RangeScan::default();
    s.tally("a b", start, end); // 命中 a、b，**不收**中间的空格
    s.tally("\t", start, end); // 整条只有制表符 ⇒ 一个都不收
    s.finish();

    assert_eq!(s.chars, vec!['a', 'b'], "空白不该进候选清单");
    assert_eq!(s.entries, 1, "只有第一条真正命中了可登记字符");
}

/// 空区间（`charblock` 的「其它」用 `start > end` 表示）不该命中任何东西。
#[test]
fn range_scan_empty_range_matches_nothing() {
    let mut s = super::RangeScan::default();
    s.tally("\u{3105}\u{6211}", 1, 0);
    s.finish();
    assert!(s.chars.is_empty() && s.entries == 0);
}

/// 影子层来源与 load_codetable_layers 同口径：主库除外、扩展库全收（含已启用），
/// 两边选库规则一旦漂移，就会出现「某库既不在主层也不在影子层」。
#[test]
fn disabled_extra_sources_cover_every_declared_extra() {
    let schema: Schema = toml::from_str(
        "[schema]\nid = \"t\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"t/m.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"on\"\npath = \"t/on.dict.yaml\"\ndefault_enabled = true\n\
             [[dictionaries]]\nid = \"off\"\npath = \"t/off.dict.yaml\"\ndefault_enabled = false\n\
             [[dictionaries]]\nid = \"nopath\"\npath = \"\"\n",
    )
    .unwrap();
    let ids: Vec<String> =
        EngineManager::disabled_extra_sources(&schema, Path::new("/nonexistent"))
            .into_iter()
            .map(|s| s.id)
            .collect();
    assert_eq!(ids, ["on", "off"]);
}

/// 无 `default` 时主库 = 首个可用库（同 load_codetable_layers），它同样不进影子层来源。
#[test]
fn disabled_extra_sources_exclude_fallback_main_without_default() {
    let schema: Schema = toml::from_str(
        "[schema]\nid = \"t\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"nopath\"\npath = \"\"\n\
             [[dictionaries]]\nid = \"first\"\npath = \"t/f.dict.yaml\"\n\
             [[dictionaries]]\nid = \"off\"\npath = \"t/off.dict.yaml\"\ndefault_enabled = false\n",
    )
    .unwrap();
    let ids: Vec<String> =
        EngineManager::disabled_extra_sources(&schema, Path::new("/nonexistent"))
            .into_iter()
            .map(|s| s.id)
            .collect();
    assert_eq!(ids, ["off"]);
}

/// 实际进影子集合的只有「本次没挂进主 dm 的扩展库」——主库、已加载的扩展库
/// 都不读。四个库各放一个独有词，用 `??` 通配把影子层整个掏出来比对；另有一个文件缺失的
/// 未启用库，证明 loader 失败只 warn 跳过、不 panic。
#[test]
fn disabled_dict_layers_load_only_unloaded_extras() {
    let dir = std::env::temp_dir().join(format!("wind-ddl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("t")).unwrap();
    let dict = |name: &str, body: &str| {
        format!(
            "---\nname: {name}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n{body}"
        )
    };
    for (f, body) in [
        ("m", "aa\t主库\t1\n"),
        ("on", "ab\t已启用\t1\n"),
        ("off", "ac\t未启用\t1\n"),
    ] {
        std::fs::write(dir.join(format!("t/{f}.dict.yaml")), dict(f, body)).unwrap();
    }
    let schema: Schema = toml::from_str(
        "[schema]\nid = \"t\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"t/m.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"on\"\npath = \"t/on.dict.yaml\"\ndefault_enabled = true\n\
             [[dictionaries]]\nid = \"off\"\npath = \"t/off.dict.yaml\"\ndefault_enabled = false\n\
             [[dictionaries]]\nid = \"gone\"\npath = \"t/gone.dict.yaml\"\ndefault_enabled = false\n",
    )
    .unwrap();
    let d = EngineManager::disabled_dict_layers(&schema, &dir, vec!["on".to_string()])
        .expect("有扩展库 ⇒ 挂影子层");
    assert_eq!(d.load_count(), 0, "构造不读盘");
    let pat = format!("{0}{0}", wind_dict::WILDCARD_SLOT);
    let texts: Vec<String> = d
        .search_pattern(&pat, wind_dict::WILDCARD_SLOT, 50, true)
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(texts, ["未启用"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 方案没有任何扩展库 ⇒ 不挂影子层（零开销）。
#[test]
fn disabled_dict_layers_none_without_extras() {
    let schema: Schema = toml::from_str(
        "[schema]\nid = \"t\"\n[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"t/m.dict.yaml\"\ndefault = true\n",
    )
    .unwrap();
    assert!(
        EngineManager::disabled_dict_layers(&schema, Path::new("/nonexistent"), Vec::new())
            .is_none()
    );
}

/// 混输代际测试的夹具。方案 id 前缀 `t` = 标签 + 进程号：派生缓存按词库所在目录名分组、
/// 落在进程级 `CACHE_DIR`（真实用户缓存根）下，不带进程号的话并发进程互踩。
///
/// 方案：码表 `{t}c`（主库 `a` → 甲；默认启用的扩展库 `ext`，`b` → 戊）、混输 `{t}b`
/// （primary = `{t}c`）、混输 `{t}a`（primary = `{t}b`），以及码表类型的 `english`
/// （词库在 `{t}en`，不与真英文方案的缓存撞路径）。Drop 时删数据目录与那几组缓存。
struct MixFixture {
    t: String,
    root: std::path::PathBuf,
}

impl MixFixture {
    fn new(tag: &str) -> Self {
        let t = format!("{tag}{}", std::process::id());
        let root = std::env::temp_dir().join(format!("wind_eng_mixgen_{t}"));
        let _ = std::fs::remove_dir_all(&root);
        let fx = Self { t, root };
        let t = &fx.t;
        let schemas = fx.data().join("schemas");
        for (dir, line) in [("c", "甲\ta"), ("x", "戊\tb"), ("en", "乙\ta")] {
            let id = format!("{t}{dir}");
            std::fs::create_dir_all(schemas.join(&id)).unwrap();
            std::fs::write(
                fx.dict(dir),
                format!("---\nname: {id}\nversion: \"1\"\n...\n{line}\n"),
            )
            .unwrap();
        }
        let codetable = |id: &str, dict: &str, extra: &str| {
            format!(
                "[schema]\nid = \"{id}\"\nname = \"{id}\"\n[engine]\ntype = \"codetable\"\n\
                     [engine.codetable]\nmax_code_length = 4\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{dict}/{dict}.dict.yaml\"\ndefault = true\n\
                     {extra}"
            )
        };
        let mixed = |id: &str, primary: &str| {
            format!(
                "[schema]\nid = \"{id}\"\nname = \"{id}\"\n[engine]\ntype = \"mixed\"\n\
                     [engine.mixed]\nprimary_schema = \"{primary}\"\nsecondary_schema = \"\"\n"
            )
        };
        let ext = format!(
            "[[dictionaries]]\nid = \"ext\"\npath = \"{t}x/{t}x.dict.yaml\"\n\
                 default_enabled = true\n"
        );
        let files = [
            (fx.id("c"), codetable(&fx.id("c"), &fx.id("c"), &ext)),
            (fx.id("b"), mixed(&fx.id("b"), &fx.id("c"))),
            (fx.id("a"), mixed(&fx.id("a"), &fx.id("b"))),
            (
                ENGLISH_SCHEMA.to_string(),
                codetable(ENGLISH_SCHEMA, &fx.id("en"), ""),
            ),
        ];
        for (id, body) in files {
            std::fs::write(schemas.join(format!("{id}.schema.toml")), body).unwrap();
        }
        fx
    }

    fn id(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.t)
    }

    fn data(&self) -> std::path::PathBuf {
        self.root.join("data")
    }

    /// `{t}{dir}` 目录下那份词库源文件。
    fn dict(&self, dir: &str) -> std::path::PathBuf {
        let id = self.id(dir);
        self.data().join(format!("schemas/{id}/{id}.dict.yaml"))
    }

    fn manager(&self, active: &str, available: &[&str], enable_english: bool) -> EngineManager {
        let mut cfg = Config::default();
        cfg.schema.active = active.into();
        cfg.schema.available = available.iter().map(|s| s.to_string()).collect();
        cfg.schema.mix.enable_english = enable_english;
        cfg.input.temp_pinyin.enabled = false;
        cfg.input.temp_english.show_candidates = false;
        EngineManager::with_store_override(
            &cfg,
            Some(&self.data()),
            None,
            Some(self.root.join("ov")),
        )
    }
}

impl Drop for MixFixture {
    fn drop(&mut self) {
        for dir in ["c", "x", "en"] {
            if let Some(p) = cache_path(&self.dict(dir), "wdat").parent() {
                let _ = std::fs::remove_dir_all(p);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 扇出要沿 primary / secondary **传递**：A 的 primary 是混输 B、B 的 primary 是码表 C，
/// 改 C 之后 A 手里捧着的是旧 C，必须连带摘掉。
#[test]
fn invalidating_leaf_member_drops_mixed_two_levels_up() {
    let fx = MixFixture::new("fan");
    let (a, b, c) = (fx.id("a"), fx.id("b"), fx.id("c"));
    let mgr = fx.manager(&a, &[&a, &b, &c], false);
    assert!(mgr.is_loaded(&a), "夹具：混输 A 应能建起来");
    let v: toml::Value = "[engine.codetable]\nmax_code_length = 3\n".parse().unwrap();
    mgr.write_schema_override(&c, &v).unwrap();
    assert!(!mgr.is_loaded(&a), "C 是 A 的间接成员，改 C 必须连带失效 A");
}

/// 用户分库扇出同理：可用列表里只有 A 时，C 的分库变了也要沿 A → B → C 找到 C 并失效 A。
#[test]
fn user_dict_invalidation_reaches_indirect_member() {
    let fx = MixFixture::new("udm");
    let (a, c) = (fx.id("a"), fx.id("c"));
    let mgr = fx.manager(&a, &[&a], false);
    assert!(mgr.is_loaded(&a), "夹具：混输 A 应能建起来");
    assert_eq!(mgr.data_schema_id(&c), c);
    mgr.invalidate_user_dicts(&c);
    assert!(
        !mgr.is_loaded(&a),
        "C 的分库变了，间接引用它的混输 A 必须失效"
    );
}

fn set_hook(mgr: &EngineManager, f: impl Fn(&EngineManager, &str) + Send + Sync + 'static) {
    *mgr.before_publish_hook.lock().unwrap() = Some(Arc::new(f));
}

/// 装一个只在 `stage` 第一次出现时触发的钩子。
fn hook_once(mgr: &EngineManager, stage: String, f: impl FnOnce(&EngineManager) + Send + 'static) {
    let f = Mutex::new(Some(f));
    set_hook(mgr, move |m, s| {
        if s == stage {
            let f = f.lock().unwrap().take();
            if let Some(f) = f {
                f(m);
            }
        }
    });
}

fn first_text(mgr: &EngineManager, id: &str, code: &str) -> Option<String> {
    mgr.convert_with(id, code, 5)
        .candidates
        .first()
        .map(|c| c.text.clone())
}

/// 锁外构建期间方案被失效（词库换了内容），建好的旧引擎不许写回表里：
/// 下次取到的必须是按新内容建的。`invalidate` 即构建窗口里插入的那次失效。
fn assert_mid_build_invalidation_discarded(
    tag: &str,
    invalidate: impl FnOnce(&EngineManager, &str) + Send + 'static,
) {
    let fx = MixFixture::new(tag);
    let c = fx.id("c");
    let mgr = fx.manager(&c, &[&c], false);
    assert_eq!(first_text(&mgr, &c, "a").as_deref(), Some("甲"), "夹具");
    mgr.invalidate_schema(&c);
    let dict = fx.dict("c");
    let id = c.clone();
    hook_once(&mgr, c.clone(), move |m| {
        std::fs::write(&dict, "---\nname: changed\nversion: \"2\"\n...\n丙丁\ta\n").unwrap();
        invalidate(m, &id);
    });
    assert!(mgr.prewarm_schema(&c));
    assert_eq!(
        first_text(&mgr, &c, "a").as_deref(),
        Some("丙丁"),
        "构建期间被失效的旧引擎被写回了表里"
    );
}

#[test]
fn engine_invalidated_mid_build_is_not_published() {
    assert_mid_build_invalidation_discarded("rce", |m, id| m.invalidate_schema(id));
}

#[test]
fn engine_reloaded_mid_build_is_not_published() {
    assert_mid_build_invalidation_discarded("rcr", |m, id| {
        // 活跃方案不能是正在建的这个：`reload_from_config` 末尾会 `ensure_loaded(活跃)`，
        // 而钩子跑在持着它构建锁的同一线程上。
        let mut cfg = Config::default();
        cfg.schema.active = ENGLISH_SCHEMA.into();
        cfg.schema.available = vec![ENGLISH_SCHEMA.into(), id.into()];
        cfg.schema.mix.enable_english = false;
        cfg.input.temp_pinyin.enabled = false;
        cfg.input.temp_english.show_candidates = false;
        m.reload_from_config(&cfg);
    });
}

/// 禁用扩展库只对已加载引擎摘层；正在锁外构建的那份（已按旧 override 读到库开着）
/// 不许带着这个库写回。
#[test]
fn dict_disabled_mid_build_is_not_published() {
    let fx = MixFixture::new("dde");
    let c = fx.id("c");
    let mgr = fx.manager(ENGLISH_SCHEMA, &[ENGLISH_SCHEMA, &c], false);
    assert_eq!(first_text(&mgr, &c, "b").as_deref(), Some("戊"), "夹具");
    mgr.invalidate_schema(&c);
    let id = c.clone();
    hook_once(&mgr, c.clone(), move |m| {
        let ov: toml::Value = "[[dictionaries]]\nid = \"ext\"\nenabled = false\n"
            .parse()
            .unwrap();
        m.persist_schema_override(&id, &ov).unwrap();
        m.set_dict_enabled_live(&id, "ext", false);
    });
    assert!(mgr.prewarm_schema(&c));
    assert_eq!(
        first_text(&mgr, &c, "b"),
        None,
        "构建期间被禁用的扩展库随旧引擎写回了表里"
    );
}

/// 共享英文引擎同理：取到之后、写回缓存之前 `invalidate_schema("english")`，
/// 缓存不许留着那份已被移出表的旧实例。
#[test]
fn shared_english_invalidated_before_cache_write_is_not_cached() {
    let fx = MixFixture::new("rcs");
    let c = fx.id("c");
    let mgr = fx.manager(&c, &[&c, ENGLISH_SCHEMA], false);
    hook_once(&mgr, "@shared_english".into(), |m| {
        m.invalidate_schema(ENGLISH_SCHEMA)
    });
    let old = mgr
        .shared_english_engine()
        .expect("夹具：英文引擎应能建起来");
    assert!(!mgr.is_loaded(ENGLISH_SCHEMA), "钩子里已失效");
    let now = mgr.shared_english_engine().expect("重建");
    assert!(!Arc::ptr_eq(&old, &now), "失效后缓存仍握着旧英文引擎");
    assert!(Arc::ptr_eq(
        &now,
        &mgr.loaded_engine(ENGLISH_SCHEMA).unwrap()
    ));
}

/// 混输构建被 `invalidate_schema("english")` 打断后重来的那一轮，不许从共享英文缓存里
/// 取到还没清掉的旧实例：清缓存必须与推进代次在同一段 `engines` 锁里。
///
/// 时序靠两个线程 + 钩子钉死：混输 B 建好、写回前（钩子 B）放行线程 A 去失效英文；
/// A 推进代次、放开 `engines` 锁后停在 `@after_engines_bump`；B 比对失败重来、取共享英文；
/// B 发布完才放 A 走完。
#[test]
fn mixed_retry_does_not_pick_up_uncleared_shared_english() {
    use std::sync::mpsc;
    use std::time::Duration;
    let fx = MixFixture::new("rse");
    let (b, c) = (fx.id("b"), fx.id("c"));
    let mgr = fx.manager(&c, &[&c, &b, ENGLISH_SCHEMA], true);
    let old = Arc::downgrade(&mgr.shared_english_engine().expect("夹具：英文引擎"));
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (bump_tx, bump_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let go_tx = Mutex::new(Some(go_tx));
    let (bump_tx, bump_rx) = (Mutex::new(bump_tx), Mutex::new(bump_rx));
    let release_rx = Mutex::new(release_rx);
    let stage = b.clone();
    let wait = Duration::from_secs(20);
    set_hook(&mgr, move |_, s| {
        if s == stage {
            let go = go_tx.lock().unwrap().take();
            if let Some(go) = go {
                go.send(()).unwrap();
                bump_rx.lock().unwrap().recv_timeout(wait).unwrap();
            }
        } else if s == "@after_engines_bump" {
            bump_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(wait).unwrap();
        }
    });
    // 判据要在放 A 走完之前取：A 收尾时的扇出会把 B 连同它捧着的英文一起摘掉。
    let (published, stale_alive) = std::thread::scope(|sc| {
        let m = &mgr;
        let a = sc.spawn(move || {
            go_rx.recv_timeout(wait).unwrap();
            m.invalidate_schema(ENGLISH_SCHEMA);
        });
        let published = mgr.prewarm_schema(&b) && mgr.is_loaded(&b);
        let stale_alive = old.upgrade().is_some();
        release_tx.send(()).unwrap();
        a.join().unwrap();
        (published, stale_alive)
    });
    *mgr.before_publish_hook.lock().unwrap() = None;
    assert!(published, "混输 B 应已建好发布");
    assert!(!stale_alive, "重来那一轮的混输捧着已失效的旧英文引擎");
}
