//! 方案级 `[key_actions]` 的端到端分派测试。
//!
//! 用 `new_headless_with_override` 指定临时 override 目录：`new_headless` 会让
//! `EngineManager` 取真实用户目录，测试写进去会污染用户配置。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
}

/// 建一个隔离的 override 目录，写入指定方案的 `[key_actions]`。
fn make_override(tag: &str, schema_id: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_ka_ov_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{schema_id}.toml")),
        format!("[key_actions]\n{body}\n"),
    )
    .unwrap();
    dir
}

/// 造一个自带 `zz` 开头编码的临时方案数据目录。
///
/// 真机上 z 是活码前缀靠的是 `system.phrases.toml` 的 `zz*` 标点短语，而测试里 `store` 是
/// None、短语层为空，z 会成死码、走不到夺取路径。这里用码表自带 `zz` 编码复现同构场景。
fn make_data_dir_with_z_code(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_ka_data_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("zt")).unwrap();
    std::fs::write(
        schemas.join("zt.schema.toml"),
        "[schema]\nid = \"zt\"\nname = \"Z测试\"\n\
         [engine]\ntype = \"codetable\"\n\
         [engine.codetable]\nmax_code_length = 4\n\
         [[dictionaries]]\nid = \"main\"\npath = \"zt/zt.dict.yaml\"\ndefault = true\n",
    )
    .unwrap();
    // rime .dict.yaml：`---` 头之后是 `文本\t编码`。zz* 模拟系统短语占位，a 保证词库非空可用。
    std::fs::write(
        schemas.join("zt/zt.dict.yaml"),
        "---\nname: zt\nversion: \"1\"\n...\n阿\ta\n甲\tzzbd\n乙\tzzsz\n",
    )
    .unwrap();
    dir
}

fn cfg_for_z_schema() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["zt".into()];
    c.schema.active = "zt".into();
    c.input.default.chinese_mode = true;
    c
}

fn cfg_for(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into()];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c
}

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

const VK_OEM_1: u32 = 0xBA; // ;
const VK_Z: u32 = 0x5A;

/// `none`：本方案禁用该键的全局引导，既不进模式、也不回落全局 `trigger_keys`。
///
/// `;` 是 `quick_mix` 的全局触发键。方案里写 `semicolon = "none"` 后，空码按 `;`
/// 必须落普通输入（后续由标点流水线出分号），而不是进快捷输入。
#[test]
fn schema_none_blocks_global_trigger_key() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("none", "wubi86", "semicolon = \"none\"");
    let mut cfg = cfg_for("wubi86");
    // 全局把 ; 配成 quick_mix 引导键（出厂即如此，这里显式写清前提）。
    cfg.schema.mix_modes[0].trigger_keys = vec!["semicolon".into()];
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));

    let act = coord.handle_key_event(&key(VK_OEM_1));
    // 进了 mix 会得到 UpdateComposition（组合区开前缀 ";"）；被 none 拦住则不会。
    if let KeyAction::UpdateComposition { text, .. } = &act {
        panic!("`;` 被 none 禁用后不该进快捷输入，实际开了组合区: {text:?}");
    }
    let _ = std::fs::remove_dir_all(&ov);
}

/// 对照组：不写 `none` 时，`;` 照常进快捷输入。
///
/// 没有这一条，上面那个用例在「`;` 本来就进不去」时也会绿。
#[test]
fn without_none_semicolon_still_enters_mix() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("ctrl", "wubi86", "backslash = \"none\"");
    let mut cfg = cfg_for("wubi86");
    cfg.schema.mix_modes[0].trigger_keys = vec!["semicolon".into()];
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));

    let act = coord.handle_key_event(&key(VK_OEM_1));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "未被 none 禁用时 `;` 应进快捷输入，实际: {act:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// `z_key_repeat` 只压得住有夺取回路的目标。
///
/// `temp_pinyin` 有 `try_z_fallback`，首键让位给 repeat 后仍能被夺取进临拼；special 等只支持
/// 首键进入，让位一次就再也进不去。
///
/// 本用例先真上屏一次（喂出 repeat 历史），再按 z：绑 special 时必须照进不误。
#[test]
fn z_repeat_does_not_steal_targets_without_rescue_path() {
    if !has_schemas() {
        return;
    }
    // 目标取内置 quick_mix：它与快符同属「只支持首键进入、没有夺取回路」那一类，验证的是
    // 同一条判据。不用 special 是因为快符类方案不在 build_dev/data 里，`ensure_schema`
    // 门卫过不了，测出来的会是「方案缺失」而不是「被 repeat 抢走」——两者在结果上同形。
    let ov = make_override("zrepeat", "wubi86", "z = \"mix:quick_mix\"");
    let mut cfg = cfg_for("wubi86");
    cfg.schema.codetable.z_key_repeat = true;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));

    // 先上屏一次，喂出 repeat 历史（无历史时 repeat 本就不生效，测不出让位）。
    for c in ['a', 'a'] {
        coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
    }
    coord.handle_key_event(&key(0x20)); // 空格上屏

    coord.handle_key_event(&key(VK_Z));
    // 判据必须是「进没进模式」，不能看 KeyAction 的形状——让位后 z 落普通输入、
    // buffer 变 "z"，返回的同样是 UpdateComposition，两种结局在那一层完全同形。
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "z 绑无夺取回路的目标时不该被 repeat 抢走：让位即这个方案里永久进不去"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 反向对照：绑 `temp_pinyin` 时 repeat 仍然优先（它有 z-fallback 补救）。
///
/// 没有这条，上面那个用例在「repeat 整个失效」时也会绿。
#[test]
fn z_repeat_still_wins_for_temp_pinyin() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("zrepeat_tp", "wubi86", "z = \"temp_pinyin\"");
    let mut cfg = cfg_for("wubi86");
    cfg.schema.codetable.z_key_repeat = true;
    cfg.input.temp_pinyin.enabled = true;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));

    for c in ['a', 'a'] {
        coord.handle_key_event(&key(c.to_ascii_uppercase() as u32));
    }
    coord.handle_key_event(&key(0x20));

    coord.handle_key_event(&key(VK_Z));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "绑 temp_pinyin 时 repeat 仍优先，z 应落普通输入而非进模式（后续字母由 z-fallback 补救）"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// z 夺取回路推广到 mix：首键让位（保住 `zz*` 系统短语），下一键破前缀时夺取进快捷输入。
///
/// 出厂 `system.phrases.toml` 的 `zz*` 标点短语让 z 恒为活码、首键必然让位；
/// 不补这条夺取回路，`z = "mix:…"` 配了也永不生效。
#[test]
fn z_fallback_hijacks_into_mix() {
    let dd = make_data_dir_with_z_code("zmix");
    let ov = make_override("zmix", "zt", "z = \"mix:quick_mix\"");
    let coord =
        Coordinator::new_headless_with_override(cfg_for_z_schema(), Some(&dd), Some(ov.clone()));

    coord.handle_key_event(&key(VK_Z));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "首键 z 应让位（zz* 使 z 成活码前缀），否则下面测的就不是夺取路径了"
    );
    // r：zr 不是任何编码的前缀 → 破前缀，夺取。
    coord.handle_key_event(&key(0x52));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "z + 破前缀字母应夺取进 mix"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 对照组：`zz` 仍走活码路径、不夺取，出厂 `zz*` 标点短语必须照打。
/// 没有它，「z 无条件夺取」的错误实现也能让上一条绿。
#[test]
fn z_fallback_keeps_zz_system_phrases() {
    let dd = make_data_dir_with_z_code("zz");
    let ov = make_override("zz", "zt", "z = \"mix:quick_mix\"");
    let coord =
        Coordinator::new_headless_with_override(cfg_for_z_schema(), Some(&dd), Some(ov.clone()));

    coord.handle_key_event(&key(VK_Z));
    coord.handle_key_event(&key(VK_Z)); // zz —— 仍是活码前缀（zzbd/zzsz）
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "zz 是 zzbd/zzsz 的前缀，必须留在正常输入流，不能被夺取（真机上对应那 37 条系统标点短语）"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 夺取后退到边界再退格 → 还原正常码流，不是停在半残的模式里。
#[test]
fn z_fallback_into_mix_can_rewind() {
    let dd = make_data_dir_with_z_code("zrw");
    let ov = make_override("zrw", "zt", "z = \"mix:quick_mix\"");
    let coord =
        Coordinator::new_headless_with_override(cfg_for_z_schema(), Some(&dd), Some(ov.clone()));

    coord.handle_key_event(&key(VK_Z));
    coord.handle_key_event(&key(0x52)); // zr → 夺取进 mix，残余 "r"
    assert_eq!(coord.debug_active_mode(), Some("mix"));

    coord.handle_key_event(&key(0x08)); // Backspace：退到夺取边界
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "退到边界应撤销夺取、回到正常码表输入流（active_hijack_buffer 与 rewind_hijack 都要认得 mix）"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 方案表里的 `z` 必须压过全局 `schema.codetable.z_key_action`。
///
/// 全局配 `z_key_action = "temp_pinyin"`，方案表配 `z = "temp_english"`。
/// 按 z 应进临时英文——进了临拼就说明方案表没被优先。
#[test]
fn schema_table_overrides_global_z_key_action() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("zover", "wubi86", "z = \"temp_english\"");
    let mut cfg = cfg_for("wubi86");
    cfg.schema.codetable.z_key_action = "temp_pinyin".into();
    cfg.input.temp_pinyin.enabled = true;
    cfg.input.temp_english.enabled = true;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));

    let act = coord.handle_key_event(&key(VK_Z));
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "z 应进某个模式，实际: {act:?}"
    );
    // 临英缓冲吃字母原文：打 "ab" 后组合区应含 ab；临拼会把 ab 转成候选/拼音串。
    coord.handle_key_event(&key(0x41)); // a
    let act2 = coord.handle_key_event(&key(0x42)); // b
    if let KeyAction::UpdateComposition { text, .. } = &act2 {
        assert!(
            text.contains("ab"),
            "应进临时英文（缓冲存英文原文 ab），实际组合区: {text:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&ov);
}

// ──────────────── 四期：修饰键 keyup 通路 + C 类 toggle_schema ────────────────

const VK_RSHIFT: u32 = 0xA1;
const VK_LSHIFT: u32 = 0xA0;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;

/// 修饰键的 keyup 事件（TSF 只在「干净单击」后转发这类事件，见 KeyEventSink.cpp）。
fn key_up(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_UP,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 从 `KeyAction` 里取中英文态（切换类动作的应答都带这个字段）。
fn chinese_of(act: &KeyAction) -> Option<bool> {
    match act {
        KeyAction::StatusUpdate(s) => Some(s.chinese_mode),
        KeyAction::InsertText { chinese_mode, .. } => Some(*chinese_mode),
        _ => None,
    }
}

/// 英文半角态下按键功能表跳过方案级层，只认全局配置。
///
/// 拼音方案级 `lshift = toggle_mode`、全局 `lshift = switch_schema:wubi86`：中文态按左 Shift
/// 命中方案级进英文；英文态再按跳过方案级、命中全局切到五笔。
/// 判据取活跃方案：两种动作都会让 `chinese_mode` 回到 true，只看中英文态测不出来。
#[test]
fn english_mode_skips_schema_layer_for_modifier_bindings() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("en_layer", "pinyin", "lshift = \"toggle_mode\"");
    let mut cfg = cfg_for("pinyin");
    cfg.keys
        .key_actions
        .insert("lshift".into(), "switch_schema:wubi86".into());
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
    assert_eq!(coord.active_schema_id(), "pinyin");

    // ① 中文态：方案级那条生效 —— 进系统英文，方案不动。
    let a1 = coord.handle_key_event(&key_up(VK_LSHIFT));
    assert_eq!(
        chinese_of(&a1),
        Some(false),
        "中文态按左 Shift 应进英文半角，实际: {a1:?}"
    );
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "方案级 toggle_mode 只切中英文态，不该动方案"
    );

    // ② 英文态：跳过方案级 ⇒ 命中全局的单向切换。
    let a2 = coord.handle_key_event(&key_up(VK_LSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "wubi86",
        "英文态应走全局表切到五笔（修复前会命中方案级 toggle_mode、方案原地不动）"
    );
    assert_eq!(
        chinese_of(&a2),
        Some(true),
        "切方案自带中文态归位（finish_user_schema_switch 的既有断言）"
    );

    let _ = std::fs::remove_dir_all(&ov);
}

/// 反向对照：中文态下方案级层照常优先，全局那条不得抢走。
///
/// 与上一条共用同一份配置，只差不进英文态。少了它，把分层判据写反（中文态跳过方案级）
/// 同样能让上一条变绿。
#[test]
fn chinese_mode_still_prefers_schema_layer() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("cn_layer", "pinyin", "lshift = \"toggle_mode\"");
    let mut cfg = cfg_for("pinyin");
    cfg.keys
        .key_actions
        .insert("lshift".into(), "switch_schema:wubi86".into());
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));

    let act = coord.handle_key_event(&key_up(VK_LSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "中文态必须命中方案级 toggle_mode，全局的 switch_schema 不得抢走"
    );
    assert_eq!(chinese_of(&act), Some(false));

    let _ = std::fs::remove_dir_all(&ov);
}

/// 方案级单向 `switch_schema`：切过去就完事，再按不回程（回程可由别的键负责）。
///
/// 第二次按须断言 `KeyAction::Consumed`：出厂 `toggle_mode_keys` 含 `rshift`，键若落回全局链
/// 会被 `is_toggle_mode_keycode` 接住去切中英文。只断言「仍在 pinyin」测不出这个。
#[test]
fn schema_level_switch_schema_is_one_way() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("sw_oneway", "wubi86", "rshift = \"switch_schema:pinyin\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert_eq!(coord.active_schema_id(), "wubi86");
    // 前置：出厂 toggle_mode_keys 含 rshift —— 没有这个前提，下面那条吞键断言就失去意义。
    assert!(
        Config::default()
            .keys
            .toggle_mode_keys
            .iter()
            .any(|k| k == "rshift"),
        "前置：rshift 出厂应是 toggle_mode 键"
    );

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "方案级单向应生效（放开前这里会被让位吞掉、停在 wubi86）"
    );

    // pinyin 的 override 里没有任何 key_actions ⇒ 走 NotBound。
    let act = coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "单向没有回程，再按应留在目标方案"
    );
    assert!(
        matches!(act, KeyAction::Consumed),
        "再按必须被吞掉，绝不能漏回全局链去切中英文，实际: {act:?}"
    );

    let _ = std::fs::remove_dir_all(&ov);
}

/// 全局 `switch_schema` 不受放开影响：在目标方案里仍命中全局表走 `Act` 分支（幂等归位），
/// 而不是被单向送达记录吞掉。
///
/// 处置不同是刻意的：全局绑定在所有方案下都在作用域内，再按走幂等归位；方案级绑定在目标
/// 方案里不存在，才需要送达记录兜底。
#[test]
fn global_switch_schema_unaffected_by_one_way_arrival() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let mut cfg = cfg_for("wubi86");
    cfg.keys
        .key_actions
        .insert("rshift".into(), "switch_schema:pinyin".into());
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin");
    // 再按：全局表仍命中 ⇒ 幂等分支，方案不变且不回程。
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "全局单向再按应原地不动（幂等），不回程也不切走"
    );
}

/// C 类 `toggle_schema` 的往返：五笔按右 Shift 去拼音，再按回五笔。
///
/// 回程不要求目标方案配对称的绑定：本例 pinyin 的 override 里没有任何 `key_actions`。
/// 单向 `switch_schema` 的处置见 `schema_level_switch_schema_is_one_way`。
#[test]
fn toggle_schema_on_modifier_round_trips() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("tgl_rt", "wubi86", "rshift = \"toggle_schema:pinyin\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert_eq!(coord.active_schema_id(), "wubi86");

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin", "右 Shift 应切到 pinyin");

    // 回程靠运行时来源，与 pinyin 有没有配 rshift 无关。
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "wubi86", "再按应回到来源方案");

    let _ = std::fs::remove_dir_all(&ov);
}

/// 修饰键的绑定必须进 `key_up` 转发集，否则 TSF 压根不发这个 keyup ——
/// 绑定在配置里躺着但永远不触发，是「配了不生效」里最难查的一种。
///
/// 断言的是推给 C++ 的白名单，不是内部结构：这是可达性的唯一来源。
#[test]
fn modifier_binding_enters_key_up_forward_set() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("tgl_fwd", "wubi86", "rshift = \"toggle_schema:pinyin\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    let hashes = coord.debug_key_up_hotkeys();
    assert!(
        hashes.iter().any(|h| (h & 0xFFFF) == VK_RSHIFT),
        "rshift 应在 key_up 转发集里，实际: {hashes:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 转发集取所有方案的并集：即便当前活跃的是 pinyin，wubi86 里绑的 rshift
/// 也要在集合里。按活跃方案裁剪就得在每次切方案后重推白名单，漏一次的表现是
/// 「刚切完方案这个键不灵、点下别的窗口又灵了」。
#[test]
fn key_up_forward_set_is_union_across_schemas() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("tgl_union", "wubi86", "rshift = \"toggle_schema:pinyin\"");
    // 活跃方案是 pinyin，它自己没配任何 key_actions。
    let coord = Coordinator::new_headless_with_override(
        cfg_for("pinyin"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    let hashes = coord.debug_key_up_hotkeys();
    assert!(
        hashes.iter().any(|h| (h & 0xFFFF) == VK_RSHIFT),
        "别的方案绑的修饰键也要在转发集里，实际: {hashes:?}"
    );
    // 但不动作：活跃方案没绑，keyup 落回全局链。
    assert_eq!(coord.active_schema_id(), "pinyin");
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "活跃方案没绑该键，不应切方案"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// `toggle_schema` 绑在有字符的键上不生效（core 侧忽略 + warn）。
///
/// 不是遗漏：它必须在英文模式下也按得动（否则切到英文方案就回不来），而有字符的键
/// 走的 keydown 链在英文模式分水岭之后。设置页对这个组合给行内提示。
#[test]
fn toggle_schema_ignored_on_character_key() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("tgl_char", "wubi86", "backslash = \"toggle_schema:pinyin\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    coord.handle_key_event(&key(0xDC)); // backslash
    assert_eq!(
        coord.active_schema_id(),
        "wubi86",
        "有字符的键上的 toggle_schema 应被忽略"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 回程记录用掉即失效：连按三次是「去 → 回 → 再去」，不是在两边反复横跳时
/// 拿陈旧记录乱送。
///
/// 第三次按下时活跃方案已回到 wubi86，该方案里 rshift 有绑定，故走的是正常去程；
/// 若回程记录没被 take 掉，这一次会被当成回程处理。
#[test]
fn return_authorization_is_consumed_once() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("tgl_once", "wubi86", "rshift = \"toggle_schema:pinyin\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin", "第一次：去程");
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "wubi86", "第二次：回程");
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "第三次应是新的去程，而非拿用掉的记录再回一次"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 去程之后用别的方式切了中英文状态，再按不该弹回来源。
///
/// 切中英文不动 `schema_generation`，故落点快照要把 `chinese_mode` 也算进去：落点被扰动则
/// 这把键退回「去目标」重新落地，来源保留，第四次按仍回得去。本例同时压在
/// `BoundKeyDecision::NotBound` 的临时授权路径上（pinyin 没配 rshift），两条路径须共用判据。
///
/// 扰动用左 Shift：`default_toggle_mode_keys` 出厂即 `["lshift", "rshift"]`，故它落回
/// 全局链就是中英切换，不需要额外配置。
#[test]
fn disturbed_landing_relands_instead_of_returning() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("tgl_dist", "wubi86", "rshift = \"toggle_schema:pinyin\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin", "去程");
    assert!(coord.is_chinese_mode(), "去程收尾会归位中文态");

    // 用别的方式切英文状态 —— 落点被扰动，但活跃方案没动，代际不变。
    coord.handle_key_event(&key_up(VK_LSHIFT));
    assert!(!coord.is_chinese_mode(), "左 Shift 应切到英文态");

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "落点被扰动后再按应重新落地，而不是弹回 wubi86"
    );
    assert!(coord.is_chinese_mode(), "重新落地必须把中文态归位回来");

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "wubi86",
        "重新落地保留来源，落点复原后再按仍应回得去"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 期间用别的方式切过方案（代际已变）⇒ 来源作废，按键完全无操作，且不复活。
///
/// 不做归位：此时无从判断用户想去哪，随便挑会把往返键变成随机跳转键。第二次按验记录确实
/// 被 take 掉。用全局 `keys.key_actions`：两次按都走 `Act` 分支，能干净断言「无操作」。
#[test]
fn origin_dropped_after_schema_changed_by_other_means() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let mut cfg = cfg_for("wubi86");
    for (k, v) in [
        ("rshift", "toggle_schema:pinyin"),
        ("lctrl", "switch_schema:wubi86"),
        ("rctrl", "switch_schema:pinyin"),
    ] {
        cfg.keys.key_actions.insert(k.to_string(), v.to_string());
    }
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), None);

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin", "去程");

    // 用方案直达热键（不经 toggle_schema_by_id）来回切一遍：代际 +2，活跃方案又是 pinyin。
    coord.handle_key_event(&key_up(VK_LCONTROL));
    assert_eq!(coord.active_schema_id(), "wubi86");
    coord.handle_key_event(&key_up(VK_RCONTROL));
    assert_eq!(coord.active_schema_id(), "pinyin");

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "来源已随代际失效，这一按应完全无操作"
    );
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_eq!(
        coord.active_schema_id(),
        "pinyin",
        "失效的记录必须已被丢弃，不能在下一按复活"
    );
}

// ──────────────── 五期：A 类状态切换 ────────────────

/// A 类绑在有字符键上：中文态按下即切换标点，不需要修饰键。
///
/// 与 C 类刻意不同——`toggle_punct` 本就只在中文态有意义（全局那份也带
/// CHINESE_ONLY），不存在「切过去回不来」的问题，故 keydown 路径可用。
#[test]
fn dispatch_action_on_character_key_toggles_punct() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("act_punct", "wubi86", "backslash = \"toggle_punct\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    let before = coord.is_chinese_punct();
    coord.handle_key_event(&key(0xDC)); // backslash
    assert_ne!(
        coord.is_chinese_punct(),
        before,
        "绑在 backslash 上的 toggle_punct 应切换中英标点"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// A 类里「用来离开英文态」的那几个（`toggle_mode` / `switch_engine`）限修饰键。
///
/// 绑在有字符的键上是单程票：那条 keydown 链在英文模式分水岭之后，切到英文态就
/// 再也按不动了。core 侧忽略并 warn，判据见 `BoundAction::requires_modifier_key`。
#[test]
fn toggle_mode_ignored_on_character_key() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("act_mode_char", "wubi86", "backslash = \"toggle_mode\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert!(coord.is_chinese_mode());
    coord.handle_key_event(&key(0xDC));
    assert!(
        coord.is_chinese_mode(),
        "有字符键上的 toggle_mode 应被忽略（否则切到英文就回不来）"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 同一个动作绑到修饰键上则生效，且能来回切——这正是「限修饰键」要保住的能力。
#[test]
fn toggle_mode_works_on_modifier_key() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("act_mode_mod", "wubi86", "rshift = \"toggle_mode\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    assert!(coord.is_chinese_mode());
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert!(!coord.is_chinese_mode(), "右 Shift 应切到英文");
    // 回程：英文态下同一个键仍走 keyup 路径，按得动。
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert!(coord.is_chinese_mode(), "再按应切回中文");
    let _ = std::fs::remove_dir_all(&ov);
}

/// 缓冲非空时 A 类不接管：打字打到一半按下绑定键，意图多半是输入而非切状态。
#[test]
fn dispatch_action_yields_while_typing() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let ov = make_override("act_typing", "wubi86", "backslash = \"toggle_punct\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    coord.handle_key_event(&key(0x41)); // a：缓冲非空
    let before = coord.is_chinese_punct();
    coord.handle_key_event(&key(0xDC));
    assert_eq!(
        coord.is_chinese_punct(),
        before,
        "缓冲非空时不该被 A 类接管"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// z 夺取进 mix 后，数字要能进缓冲，而不是被当成选词键。
///
/// `z = "mix:quick_mix"` 时按 `z1+2`：z 是活码、首键让位，第二键才夺取；`try_z_fallback`
/// 须同样挂在数字臂上，否则数字走「数字选词」臂、到不了夺取判定。
#[test]
fn z_fallback_hijacks_into_mix_on_digit() {
    let dd = make_data_dir_with_z_code("zmixdigit");
    let ov = make_override("zmixdigit", "zt", "z = \"mix:quick_mix\"");
    let coord =
        Coordinator::new_headless_with_override(cfg_for_z_schema(), Some(&dd), Some(ov.clone()));

    coord.handle_key_event(&key(VK_Z));
    let act = coord.handle_key_event(&key(0x31)); // 数字 1
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "z1 应夺取进 mix，实际: {act:?}"
    );
    assert_eq!(coord.debug_active_mode(), Some("mix"), "应处于 mix 模式");
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// z 夺取进 mix 后，算式符号要归 mix 处理（`z1+2` 应得到 `1+2` 的计算）。
///
/// 符号自己触发夺取（`z` 直接接 `-`）刻意不做：`-`/`=` 默认是翻页键，在更上游被消费。
#[test]
fn z_fallback_mix_accepts_operators_after_hijack() {
    let dd = make_data_dir_with_z_code("zmixcalc");
    let ov = make_override("zmixcalc", "zt", "z = \"mix:quick_mix\"");
    let coord =
        Coordinator::new_headless_with_override(cfg_for_z_schema(), Some(&dd), Some(ov.clone()));

    coord.handle_key_event(&key(VK_Z));
    coord.handle_key_event(&key(0x31)); // 1 —— 触发夺取
    assert_eq!(coord.debug_active_mode(), Some("mix"), "z1 应已进 mix");

    coord.handle_key_event(&key(0xBB)); // = 键（Shift 未按下时是 `=`，算式里用 +）
    let act = coord.handle_key_event(&key(0x32)); // 2
    assert!(
        matches!(act, KeyAction::UpdateComposition { .. }),
        "进 mix 后的按键应由 mix 处理，实际: {act:?}"
    );
    assert_eq!(coord.debug_active_mode(), Some("mix"), "不该被踢出 mix");
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 在 `make_data_dir_with_z_code` 基础上让 `zzbd` 有两条候选——真机上它是一整屏分组，
/// 只有一条的话 4 码唯一会自动上屏，候选列表根本留不住，测不到选词。
fn make_data_dir_with_zz_group(tag: &str) -> PathBuf {
    let dir = make_data_dir_with_z_code(tag);
    std::fs::write(
        dir.join("schemas/zt/zt.dict.yaml"),
        "---\nname: zt\nversion: \"1\"\n...\n阿\ta\n甲\tzzbd\n丙\tzzbd\n乙\tzzsz\n",
    )
    .unwrap();
    dir
}

fn inserted_text(act: &KeyAction) -> Option<&str> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text),
        _ => None,
    }
}

/// 缓冲沿活码长到 `zzbd`、候选已列出后，数字是选词键，不能被 z 夺取吃成 mix 残余码
/// （`zzbd2` 破前缀，夺取会把 `2` 连同 `zbd` 塞进快捷输入缓冲）。
#[test]
fn z_fallback_keeps_digit_select_when_candidates_listed() {
    let dd = make_data_dir_with_zz_group("zzgroupdigit");
    let ov = make_override("zzgroupdigit", "zt", "z = \"mix:quick_mix\"");
    let coord =
        Coordinator::new_headless_with_override(cfg_for_z_schema(), Some(&dd), Some(ov.clone()));

    for vk in [VK_Z, VK_Z, 'B' as u32, 'D' as u32] {
        coord.handle_key_event(&key(vk));
    }
    assert_eq!(
        coord.debug_all_candidate_texts(),
        vec!["甲", "丙"],
        "前提：zzbd 应列出两条候选"
    );
    let act = coord.handle_key_event(&key(0x32)); // 数字 2
    assert_eq!(coord.debug_active_mode(), None, "不该夺取进 mix");
    assert_eq!(
        inserted_text(&act),
        Some("丙"),
        "2 应选第二候选，实际: {act:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 同一守卫对 `temp_english` 目标同样生效（`z_fallback_accepts` 里它与 mix 同一分支）。
#[test]
fn z_fallback_temp_english_keeps_digit_select_when_candidates_listed() {
    let dd = make_data_dir_with_zz_group("zzgroupten");
    let ov = make_override("zzgroupten", "zt", "z = \"temp_english\"");
    let mut cfg = cfg_for_z_schema();
    cfg.input.temp_english.enabled = true;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&dd), Some(ov.clone()));

    for vk in [VK_Z, VK_Z, 'B' as u32, 'D' as u32] {
        coord.handle_key_event(&key(vk));
    }
    let act = coord.handle_key_event(&key(0x32)); // 数字 2
    assert_eq!(coord.debug_active_mode(), None, "不该夺取进临英");
    assert_eq!(
        inserted_text(&act),
        Some("丙"),
        "2 应选第二候选，实际: {act:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 同一守卫的标点面：候选已列出时标点走顶屏，不夺取。
#[test]
fn z_fallback_keeps_punct_top_commit_when_candidates_listed() {
    let dd = make_data_dir_with_zz_group("zzgrouppunct");
    let ov = make_override("zzgrouppunct", "zt", "z = \"mix:quick_mix\"");
    let mut cfg = cfg_for_z_schema();
    // 出厂 punct_commit 关（有编码时标点吞键），关着就测不出顶屏。
    cfg.schema.codetable.punct_commit = true;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&dd), Some(ov.clone()));

    for vk in [VK_Z, VK_Z, 'B' as u32, 'D' as u32] {
        coord.handle_key_event(&key(vk));
    }
    // 用 `/` 而不用 `,`：出厂 `,` `.` 是以词定字键，走不到标点顶屏。
    let act = coord.handle_key_event(&key(0xBF)); // /
    assert_eq!(coord.debug_active_mode(), None, "不该夺取进 mix");
    assert!(
        inserted_text(&act).is_some_and(|t| t.starts_with('甲')),
        "`/` 应顶屏首选，实际: {act:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 临拼的残余码只可能是拼音字母，故数字不该夺取——`z1` 里的 1 仍是选词键。
/// 判据按目标模式的「残余码语义」分，与设计文档 §4.2 那张表同源。
#[test]
fn z_fallback_does_not_hijack_digits_for_temp_pinyin() {
    let dd = make_data_dir_with_z_code("zpydigit");
    let ov = make_override("zpydigit", "zt", "z = \"temp_pinyin\"");
    let mut cfg = cfg_for_z_schema();
    cfg.input.temp_pinyin.enabled = true;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&dd), Some(ov.clone()));

    coord.handle_key_event(&key(VK_Z));
    coord.handle_key_event(&key(0x31));
    assert_ne!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "数字不该把临拼夺取进来——拼音里数字没有意义"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 出厂 `system.phrases.toml` 那批 `zz*` 标点短语的最小复刻（码长均 ≥2）。
///
/// 真机上正是它们让 z 成活码；headless 的 `store` 是 `None`、短语层恒空，不显式装载就只测到
/// 「z 是死码」那条分支。
fn seed(code: &str, text: &str) -> wind_phrase::PhraseSeed {
    wind_phrase::PhraseSeed {
        code: code.into(),
        text: text.into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    }
}

fn zz_system_phrases() -> Vec<wind_phrase::PhraseSeed> {
    vec![seed("zzbd", "、"), seed("zzsz", "…")]
}

/// `input.phrase.min_prefix` 对绑了动作的字母同样有效，让位帧的反馈不能拿短语顶上。
///
/// 把绑定字母的枚举门槛降到 1 会让按 z 就弹出整屏 `$SS` 分组导航。`has_code_prefix`（z 是不是
/// 活码）与 `build_candidates`（现在显示什么）回答的不是同一个问题，别强行对齐。
/// 让位帧的反馈见 `bound_letter_yield_frame_shows_repeat`。
#[test]
fn min_prefix_respected_on_bound_letter() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("zminpfx", "wubi86", "z = \"mix:quick_mix\"");
    let cfg = cfg_for("wubi86"); // input.phrase.min_prefix 默认 2
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
    coord.debug_install_phrases(zz_system_phrases());

    coord.handle_key_event(&key(VK_Z));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "z 在本方案是活码前缀（有 `zz*`），首键应让位给正常输入"
    );
    let texts = coord.debug_all_candidate_texts();
    assert!(
        !texts.iter().any(|t| t == "、"),
        "按 z 那一帧不该列出 `zz*`——那是 `zz` 那一级的导航，min_prefix=2 挡着。实际: {texts:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 对照：没绑动作的字母同样受 `min_prefix` 约束。
///
/// 与上一条合起来说明「绑不绑动作，短语门槛都一视同仁」。
#[test]
fn min_prefix_respected_on_unbound_letter() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("zrelax", "wubi86", "z = \"mix:quick_mix\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    let mut phrases = zz_system_phrases();
    phrases.push(seed("ccbd", "○"));
    coord.debug_install_phrases(phrases);

    coord.handle_key_event(&key('C' as u32));
    assert!(
        !coord.debug_all_candidate_texts().iter().any(|t| t == "○"),
        "c 没绑动作，单字母帧同样受 min_prefix=2 约束"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 让位那一帧的反馈＝重复上屏，且资格来自目标 mix 的 members。
///
/// 优先级：先显示重复上屏，没有才显示快捷模式提示；本用例锁前者。不开 `z_key_repeat` 也该有：
/// 走让位路径的引导键到不了 mix 的空缓冲帧，members 里的 `quick_input.repeat` 只能在这一帧生效。
/// 上屏内容用 `zzbd` 打出的「、」，结果确定，断言才不必退化成「非空」。
#[test]
fn bound_letter_yield_frame_shows_repeat() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("zrepeat_member", "wubi86", "z = \"mix:quick_mix\"");
    let cfg = cfg_for("wubi86"); // 刻意不开 z_key_repeat
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
    coord.debug_install_phrases(zz_system_phrases());

    // 先用 `zzbd` 上屏一个确定的内容。
    for vk in [VK_Z, VK_Z, 'B' as u32, 'D' as u32] {
        coord.handle_key_event(&key(vk));
    }
    coord.handle_key_event(&key(0x20)); // 空格上屏「、」

    coord.handle_key_event(&key(VK_Z));
    let texts = coord.debug_all_candidate_texts();
    assert_eq!(
        texts.first().map(String::as_str),
        Some("、"),
        "让位那一帧首选应是「重复上屏」（上次上屏的「、」），实际: {texts:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// z 的三条路必须同时走得通（临拼、临英、mix 目标同理）：
///
/// | 打法 | 归属 | 机制 |
/// |---|---|---|
/// | `z` | 让位（反馈＝重复上屏） | 活码前缀判据 + `leading_letter_repeat_text` |
/// | `zzbd` | 系统标点短语 | 正常码表输入，精确命中 |
/// | `z1` / `zri` | 目标模式 | `try_z_fallback` 破前缀夺取 |
///
/// 三条各测一遍，任何一条断掉都说明某一路被挤掉了。
#[test]
fn z_three_paths_coexist() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("z3path", "wubi86", "z = \"mix:quick_mix\"");
    let coord = Coordinator::new_headless_with_override(
        cfg_for("wubi86"),
        Some(&data_dir()),
        Some(ov.clone()),
    );
    coord.debug_install_phrases(zz_system_phrases());

    // ① 首键 z：让位给正常输入（不进模式、也不抢编码）。
    coord.handle_key_event(&key(VK_Z));
    assert_eq!(coord.debug_active_mode(), None, "首键 z 应让位");

    // ② `zzbd`：走正常码表输入，精确命中系统短语。
    for vk in [VK_Z, 'B' as u32, 'D' as u32] {
        coord.handle_key_event(&key(vk));
    }
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.first().map(String::as_str) == Some("、"),
        "`zzbd` 应精确命中系统标点短语并居首，实际: {texts:?}"
    );
    coord.handle_key_event(&key(0x1B)); // ESC 清空缓冲

    // ③ `z1`：破前缀，夺取进 mix。
    coord.handle_key_event(&key(VK_Z));
    coord.handle_key_event(&key(0x31));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "`z1` 破了活码前缀，应由夺取回路接进 mix"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 精确码短语不受 `min_prefix` 约束（`lookup` 无门槛），故码就是 `z` 的短语必须照常让位。
///
/// 把修复的边界钉死：改的是前缀枚举那一条，不是把短语判据整个放宽。
#[test]
fn exact_code_phrase_still_yields_regardless_of_min_prefix() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("zexact", "wubi86", "z = \"mix:quick_mix\"");
    let cfg = cfg_for("wubi86"); // min_prefix = 2
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
    coord.debug_install_phrases(vec![seed("z", "◎")]);

    coord.handle_key_event(&key(VK_Z));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "码就是 `z` 的短语是精确命中、那一帧确有候选——必须让位"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 隐式的 repeat 来源（mix members）只填空帧，不抢首选。
///
/// 绑了动作的字母可能是活码（这正是它让位的原因），那一帧有真候选，重复上屏插到顶上会让
/// 空格上屏上次内容。
///
/// 用 `zt` 方案（码表自带 `zzbd`→甲 / `zzsz`→乙）：z 是活码前缀，按 z 有真候选。
/// 与 `bound_letter_yield_frame_shows_repeat`（wubi86 的 z 是死码、帧为空）构成受控对比,
/// 唯一变量就是「这一帧有没有候选」。
///
/// 显式开关 `z_key_repeat` 不受此限：用户开了它就是要 z 干这个，抢首选是本意。
#[test]
fn implicit_repeat_does_not_outrank_real_candidates() {
    let dd = make_data_dir_with_z_code("zrepeatrank");
    let ov = make_override("zrepeatrank", "zt", "z = \"mix:quick_mix\"");
    let cfg = cfg_for_z_schema(); // 不开 z_key_repeat
    let coord = Coordinator::new_headless_with_override(cfg, Some(&dd), Some(ov.clone()));

    // 先上屏「阿」（码 a）喂出历史。
    coord.handle_key_event(&key('A' as u32));
    coord.handle_key_event(&key(0x20));

    coord.handle_key_event(&key(VK_Z));
    assert_eq!(coord.debug_active_mode(), None, "z 是活码前缀，应让位");
    let texts = coord.debug_all_candidate_texts();
    assert!(
        !texts.is_empty(),
        "前提不成立：zt 方案按 z 本该有码表前缀候选，否则本用例测不出「抢首选」"
    );
    assert_ne!(
        texts.first().map(String::as_str),
        Some("阿"),
        "这一帧有真候选，隐式 repeat 不该插到首位，实际: {texts:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
    let _ = std::fs::remove_dir_all(&dd);
}

/// 没绑动作、也没开 `z_key_repeat` 的字母，那一帧不该凭空多出重复候选。
///
/// 反向守卫：`leading_letter_repeat_text` 的资格判定若写漏（比如只看「单字母」不看绑定），
/// 每个死码字母按下去都会冒出上次上屏的内容。
#[test]
fn unbound_letter_gets_no_repeat_candidate() {
    if !has_schemas() {
        return;
    }
    let ov = make_override("znorepeat", "wubi86", "z = \"mix:quick_mix\"");
    let cfg = cfg_for("wubi86"); // 不开 z_key_repeat
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov.clone()));
    coord.debug_install_phrases(zz_system_phrases());

    for vk in [VK_Z, VK_Z, 'B' as u32, 'D' as u32] {
        coord.handle_key_event(&key(vk));
    }
    coord.handle_key_event(&key(0x20)); // 上屏「、」，喂出历史

    // c 没绑动作：即使有上屏历史，也不该出现重复候选。
    coord.handle_key_event(&key('C' as u32));
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.first().map(String::as_str) != Some("、"),
        "没绑动作的字母不该出重复上屏候选，实际: {texts:?}"
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 拼音方案绑字母引导键：只在空缓冲进模式，组码中间的同一个字母照常是编码。
///
/// 全拼里 `u`/`v` 不能起头，可在拼音方案级表绑成引导键；非码表引擎对字母绑定不做合理性判断。
/// `nv` / `lv` 里的 `v` 是组码中间，不能被夺。
/// 断言用 `debug_active_mode`：不进模式时 `u` 也会开出组合区，只看动作类型分不出来。
#[test]
fn pinyin_letter_lead_enters_only_on_empty_buffer() {
    if !has_schemas() {
        return;
    }
    let ov = make_override(
        "pinyin_letter",
        "pinyin",
        "u = \"mix:quick_mix\"\nv = \"mix:quick_mix\"",
    );
    let coord = Coordinator::new_headless_with_override(
        cfg_for("pinyin"),
        Some(&data_dir()),
        Some(ov.clone()),
    );

    // 组码中间：n 之后的 v 是编码，不得进模式。
    coord.handle_key_event(&key(0x4E)); // N
    coord.handle_key_event(&key(0x56)); // V
    assert_eq!(coord.debug_active_mode(), None, "nv 中间的 v 不应作引导键");
    assert_eq!(coord.debug_preedit(), "nv");
    coord.handle_key_event(&key(0x1B)); // Esc 清空

    // 空缓冲：u 进快捷输入。
    coord.handle_key_event(&key(0x55)); // U
    assert_eq!(coord.debug_active_mode(), Some("mix"), "空缓冲 u 应进模式");
    let _ = std::fs::remove_dir_all(&ov);
}

/// GH#264：短语编码 `crr`，拼音下敲 `cr` 默认会前缀列举出来；`min_prefix = 0` 关掉后只剩精确码。
#[test]
fn phrase_prefix_listing_can_be_turned_off() {
    if !has_schemas() {
        return;
    }
    let texts = |prefix_match: bool| {
        let mut cfg = cfg_for("pinyin");
        cfg.input.phrase.prefix_match = prefix_match;
        let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), None);
        coord.debug_install_phrases(vec![seed("crr", "○短语")]);
        coord.handle_key_event(&key('C' as u32));
        coord.handle_key_event(&key('R' as u32));
        let t = coord.debug_all_candidate_texts();
        let exact = {
            coord.handle_key_event(&key('R' as u32));
            coord.debug_all_candidate_texts()
        };
        (t, exact)
    };
    let (on, _) = texts(true);
    assert!(
        on.iter().any(|t| t == "○短语"),
        "前提：默认敲 cr 会列出 crr 的短语"
    );
    let (off, exact) = texts(false);
    assert!(
        !off.iter().any(|t| t == "○短语"),
        "prefix_match=false 时 cr 不该列出"
    );
    assert!(exact.iter().any(|t| t == "○短语"), "精确码 crr 仍要出");
}

/// `min_prefix` 只管前缀列举的输入长度；短于它的短语编码，敲全码仍要出。
#[test]
fn short_phrase_code_ignores_min_prefix() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg_for("pinyin");
    cfg.input.phrase.min_prefix = 3;
    let coord = Coordinator::new_headless_with_override(cfg, Some(&data_dir()), None);
    coord.debug_install_phrases(vec![seed("c", "○单码"), seed("cr", "○双码")]);
    coord.handle_key_event(&key('C' as u32));
    let one = coord.debug_all_candidate_texts();
    assert!(one.iter().any(|t| t == "○单码"), "1 字符编码应出: {one:?}");
    coord.handle_key_event(&key('R' as u32));
    let two = coord.debug_all_candidate_texts();
    assert!(two.iter().any(|t| t == "○双码"), "2 字符编码应出: {two:?}");
}
