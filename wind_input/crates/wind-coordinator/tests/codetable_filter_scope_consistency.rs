//! 「检索范围」智能档的**过滤一致性**与**临时放宽**（端到端，真实词库）。
//!
//! 两层语义：
//!
//! 1. **一致性**（`fix(filter): 去重并入码位` 修的）：生僻字在前缀输入与全码输入下可见性相同。
//!    现场＝五笔「桜」(sivg) 打 `siv` 能出、打全 `sivg` 却消失。根因在过滤器上游的按 text
//!    去重：`sivg` 码位另有常用字「档」，而「档」还有简码 `siv`，打 `siv` 时「档」以
//!    `code="siv"` 入列、它在 `sivg` 的那条被去重丢弃，`sivg` 组只剩「桜」成孤儿码而放行。
//! 2. **临时放宽**（`smart-filter-scope-relax.md`）：候选窗内翻到末页再按一次向后翻页键，
//!    把被滤候选**追加到末尾**；本次组合结束后自动恢复。**唯一入口，全靠用户主动触发**。
//!
//! ⚠️ 曾做过「候选不足一页就自动补充」，实测平白改变了智能档的既有观感，**已删除**。
//! `smart_list_unchanged_without_explicit_relax` 就是钉住这一点的——不主动触发就什么都不变。
//!
//! ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**（判据是耗时，正常秒级 vs 0.0x s）。
//! 这不是假想风险：本文件开发期间该目录被清掉过一次，用例全部跳过却报「3 passed」，
//! 是靠一个无守卫的临时探针打印出空候选才发现的。恢复命令 `.\scripts\dev.ps1 gd`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_NEXT: u32 = 0x22; // PageDown，默认翻页键组 "pageupdown"
const VK_SPACE: u32 = 0x20;
const VK_BACK: u32 = 0x08;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn wubi_config(filter_mode: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = filter_mode.into();
    // 含生僻字的**词**怎么处置，显式钉住出厂档（见 `rare_phrase_scope.rs`）。本族素材全是
    // 单字，两档结果相同 —— 正因如此，跟着 `Config::default()` 走的话，将来出厂档一变，
    // 这一族的语义会**静默**换成另一档而断言照绿。
    cfg.input.rare_phrase = "keep".into();
    cfg
}

fn coord_with(filter_mode: &str) -> std::sync::Arc<Coordinator> {
    Coordinator::new_headless(wubi_config(filter_mode), Some(&data_dir()))
}

/// 按键走**生产入口** `handle_key_event_policed`：临时放宽的失效收口挂在那里，
/// 直接调内部的 `handle_key_event` 会绕过它，等于验证一条真实不存在的路径。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 敲入 `code` 后的候选文本列表。满码自动上屏默认关（`auto_commit_at_full=false`），
/// 故 4 码输入仍停在候选态可供检查。
fn candidates_for(filter_mode: &str, code: &str) -> Vec<String> {
    let coord = coord_with(filter_mode);
    press(&coord, code);
    coord.debug_all_candidate_texts()
}

/// 精确翻到**末页**（不多按）。多按会翻过头，视口相关断言就测不准了。
fn page_to_last(coord: &Coordinator) {
    for _ in 0..50 {
        let (cur, _, total) = coord.debug_page_info();
        if cur + 1 >= total {
            return;
        }
        coord.handle_key_event_policed(&key_event(VK_NEXT));
    }
    panic!("翻页未收敛：页数异常");
}

/// 翻到末页后**再按一次**——这一下才是触发放宽的那次。
fn page_to_end_and_relax(coord: &Coordinator) {
    page_to_last(coord);
    coord.handle_key_event_policed(&key_event(VK_NEXT));
}

// ─────────────────────── 层 1：过滤一致性 ───────────────────────

/// 主用例：生僻字在前缀输入与全码输入下同样不可见。
#[test]
fn rare_char_hidden_consistently() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let siv = candidates_for("smart", "siv");
    let sivg = candidates_for("smart", "sivg");

    // 前置：常用字「档」两次都在，确认输入链路正常（否则下面的「不含桜」毫无意义）
    assert!(
        siv.contains(&"档".to_string()),
        "打 siv 应出「档」: {siv:?}"
    );
    assert!(
        sivg.contains(&"档".to_string()),
        "打 sivg 应出「档」: {sivg:?}"
    );

    assert!(
        !siv.contains(&"桜".to_string()),
        "打 siv 时 sivg 码位的生僻字不该露出——此前因去重丢失遮蔽关系而露出，\
         导致打全 sivg 反而消失。实际: {siv:?}"
    );
    assert!(
        !sivg.contains(&"桜".to_string()),
        "打全 sivg 时「桜」应被同码常用字「档」遮蔽。实际: {sivg:?}"
    );
}

/// ★ 反向对照：放开检索范围后「桜」必须两次都出现。
///
/// 排除「桜 压根不在词库/检索不到」这种假绿——没有它，上面那条连词库为空都能通过。
#[test]
fn gb18030_reveals_rare_char_in_both_inputs() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    for code in ["siv", "sivg"] {
        let got = candidates_for("gb18030", code);
        assert!(
            got.contains(&"桜".to_string()),
            "全部字符档下打 {code} 必须出「桜」。实际: {got:?}"
        );
    }
}

/// ★ 边界对照：无同码常用字的孤儿码生僻字，智能档下本就保留。
///
/// 「樑」(sivs) 与「档」(siv/sivg) 不共码位。若实现退化成「有常用字就滤掉所有生僻字」，
/// 这条会红。
#[test]
fn orphan_rare_char_survives_smart_filter() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let sivs = candidates_for("smart", "sivs");
    assert!(
        sivs.contains(&"樑".to_string()),
        "sivs 下无常用字，孤儿码生僻字应保留。实际: {sivs:?}"
    );
}

// ─────────────────────── 层 2：临时放宽 ───────────────────────

/// ★★ **不主动触发就什么都不变**——智能档的既有表现不被任何自动行为改动。
///
/// 这是本功能的默认行为契约。曾实现过「候选不足一页自动补充」，实测下来平白改变了智能档
/// 的观感（用户没要求却凭空多出生僻字），已删除。若日后有人再加自动补充类逻辑，这条会红。
#[test]
fn smart_list_unchanged_without_explicit_relax() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    // sivg 候选远不足一页，正是「自动补充」最想介入的场景——这里必须岿然不动
    let sivg = candidates_for("smart", "sivg");
    assert!(
        !sivg.contains(&"桜".to_string()),
        "未主动放宽时不得凭空多出被滤的字。实际: {sivg:?}"
    );
    // 敲键、退格等日常操作都不应触发放宽
    let coord = coord_with("smart");
    press(&coord, "sivg");
    coord.handle_key_event_policed(&key_event(VK_BACK));
    press(&coord, "g");
    assert!(
        !coord
            .debug_all_candidate_texts()
            .contains(&"桜".to_string()),
        "退格重打不该触发放宽"
    );
}

/// ★ 候选**不足一页**时，同样靠翻页键放宽——一条路径覆盖两种场景。
///
/// `sivg` 智能档下只剩「档」一条，连一页都不到；此时 `page_next` 一样翻不动
/// （`current_page + 1 < total_pages` 为假），照样落到放宽分支。
#[test]
fn short_list_relaxes_via_page_key_too() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("smart");
    press(&coord, "sivg");
    let before = coord.debug_all_candidate_texts();
    assert!(
        before.len() < 7 && !before.contains(&"桜".to_string()),
        "前提：sivg 智能档下候选不足一页且无「桜」。实际: {before:?}"
    );

    coord.handle_key_event_policed(&key_event(VK_NEXT));

    let after = coord.debug_all_candidate_texts();
    assert!(
        after.contains(&"桜".to_string()),
        "不足一页时按向后翻页键应放宽出「桜」。实际: {after:?}"
    );
    assert_eq!(
        after.first().map(|s| s.as_str()),
        Some("档"),
        "放宽不得改动首选。实际: {after:?}"
    );
}

/// ★★ **常用字档不参与放宽**——放宽是智能档专属的补偿。
///
/// 智能档按「同码位有常用字」滤掉生僻字，才需要一条把它们放回来的出路；常用字档要的
/// 正是一个稳定只出常用字的列表，若它也能翻到底放宽，两档的差异就被抹平了，用户也就
/// 没有理由再选常用字档。
///
/// 门禁曾写作「排除 Gb18030」，等价于常用字档照样放宽——与设计文档（全篇以
/// `filter_mode = "smart"` 为前提）不符。这条钉住修正后的语义。
#[test]
fn general_scope_does_not_relax_on_page_end() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("general");
    press(&coord, "sivg");
    let before = coord.debug_all_candidate_texts();
    assert!(
        !before.is_empty(),
        "前提：常用字档下 sivg 应有候选，否则本用例验不出东西"
    );
    assert!(
        !before.contains(&"桜".to_string()),
        "前提：常用字档下不该出生僻字「桜」。实际: {before:?}"
    );

    // 候选不足一页时按一次向后翻页键，就是「翻到底再按一次」那条路径
    // （同 short_list_relaxes_via_page_key_too 的前提）。
    coord.handle_key_event_policed(&key_event(VK_NEXT));

    let after = coord.debug_all_candidate_texts();
    assert!(
        !after.contains(&"桜".to_string()),
        "常用字档不得放宽出生僻字。实际: {after:?}"
    );
    assert_eq!(
        before, after,
        "常用字档的候选列表不该因向后翻页键发生任何变化"
    );
}

/// 放宽把被滤候选**追加到末尾**，原有顺序纹丝不动。
///
/// **追加而非按真实顺序插入**是刻意的——翻页是线性前进的动作，翻到末尾再翻却让新字插到
/// 第 1 页（`dwi` 放宽出的字权重 8999 占三简位，正好排在很前面），体验很突兀。
#[test]
fn relax_appends_filtered_at_end() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("smart");
    press(&coord, "siv");
    let before = coord.debug_all_candidate_texts();
    assert!(!before.contains(&"桜".to_string()), "前提：放宽前无「桜」");

    page_to_end_and_relax(&coord);
    let after = coord.debug_all_candidate_texts();

    assert!(
        after.len() > before.len(),
        "放宽应带来新候选：{} → {}",
        before.len(),
        after.len()
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "放宽**不得改动原有候选的顺序**（首选位置尤其不能变）"
    );
    assert!(
        after[before.len()..].contains(&"桜".to_string()),
        "被滤的「桜」应出现在追加段里。追加段: {:?}",
        &after[before.len()..]
    );
    // 内容上等价于「全部字符」档（只是次序不同：被滤的集中在末尾）
    let all: std::collections::HashSet<String> =
        candidates_for("gb18030", "siv").into_iter().collect();
    let got: std::collections::HashSet<String> = after.iter().cloned().collect();
    assert_eq!(got, all, "放宽后的候选**集合**应与全部字符档一致");
}

/// ★ 放宽后**当前页必须能看到新增候选**，否则用户按了翻页却什么变化都没有。
///
/// 「追加末尾 + 翻到下一页」这套组合的最终验收。历史上出过两种错法：按真实顺序插入导致
/// 新字落在第 1 页（用户在末页看不到），以及视口定位跳回页首（翻着翻着回到开头）。
/// 用 `dwi`：它放宽出的字权重 8999 占三简位，若改回「按真实顺序插入」这条立刻变红。
#[test]
fn relax_keeps_new_candidate_visible_on_current_page() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("smart");
    press(&coord, "dwi");
    let before: std::collections::HashSet<String> =
        coord.debug_all_candidate_texts().into_iter().collect();

    page_to_end_and_relax(&coord);

    let after = coord.debug_all_candidate_texts();
    let new_ones: Vec<String> = after
        .iter()
        .filter(|t| !before.contains(*t))
        .cloned()
        .collect();
    assert!(
        !new_ones.is_empty(),
        "前提：dwi 放宽后应出现原本被滤掉的候选（放宽前 {} 条）",
        before.len()
    );

    let page = coord.debug_page_texts();
    assert!(
        page.iter().any(|t| new_ones.contains(t)),
        "放宽后当前页必须能看到新增候选，否则用户什么变化都看不到。\
         当前页: {page:?}，新增: {new_ones:?}"
    );
    // 高亮不特殊化：与普通翻页一致地归零到页首
    let (_, sel, _) = coord.debug_page_info();
    assert_eq!(sel, 0, "翻页后高亮应照常归零（与普通翻页一致）");
}

/// 临时放宽在**本次组合结束后**失效，下次输入回到智能档。
///
/// 失效收口在 `handle_key_event_policed`（判据＝缓冲已空），覆盖上屏/取消/切焦点等全部
/// 结束路径，无需逐路径接线。
#[test]
fn relax_expires_after_composition_ends() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("smart");
    press(&coord, "siv");
    page_to_end_and_relax(&coord);
    assert!(
        coord
            .debug_all_candidate_texts()
            .contains(&"桜".to_string()),
        "前提：放宽后应能看到「桜」"
    );

    coord.handle_key_event_policed(&key_event(VK_SPACE)); // 上屏 → 组合结束
    press(&coord, "siv");
    assert!(
        !coord
            .debug_all_candidate_texts()
            .contains(&"桜".to_string()),
        "组合结束后临时放宽须失效，下次输入回到智能档"
    );
}

/// 放宽期间**改编码不丢状态**：找生僻字常要退格重打，此时若失效就得反复触发。
#[test]
fn relax_survives_backspace_within_composition() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：五笔词库不存在");
        return;
    }
    let coord = coord_with("smart");
    press(&coord, "sivg");
    page_to_end_and_relax(&coord);
    assert!(
        coord
            .debug_all_candidate_texts()
            .contains(&"桜".to_string())
    );

    coord.handle_key_event_policed(&key_event(VK_BACK)); // sivg → siv，缓冲仍非空
    assert!(
        coord
            .debug_all_candidate_texts()
            .contains(&"桜".to_string()),
        "退格改码期间放宽状态须保持（缓冲非空＝组合未结束）"
    );
}
