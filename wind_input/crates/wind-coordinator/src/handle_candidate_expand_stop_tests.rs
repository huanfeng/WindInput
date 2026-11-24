//! 翻页扩容的「到底」判据（spec codetable-wildcard §11 契约 3）：看**引擎条数**，不看可见
//! 条数。自造码表、**非通配**——这条判据对所有引擎生效，通配只是更容易撞上。
use crate::charset_test_support::copy_factory_charsets;
use crate::coordinator::Coordinator;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_A: u32 = 0x41;
const VK_NEXT: u32 = 0x22; // PageDown，出厂翻页键组 "pageupdown"
const VK_Z: u32 = 0x5A;

/// 清掉夹具目录与码表方案在共享缓存根下的产物（同 `handle_aux_code` 的 `Cleanup`）。
struct Cleanup {
    id: String,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(cache) = Config::cache_dir() {
            let _ = std::fs::remove_dir_all(cache.join(&self.id));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 常用 / 生僻各取 `n` 个真实汉字：常用取出厂 `common_han.yaml` 名单里 CJK 基本区的头部，
/// 生僻取 U+4E00 起**不在名单里**的字（该类 `outside: rare`：汉字域内名单外即生僻）。
fn common_and_rare(dir: &Path, n: usize) -> (Vec<char>, Vec<char>) {
    let yaml = std::fs::read_to_string(dir.join("charsets/common_han.yaml")).unwrap();
    let body = yaml
        .split("\n...\n")
        .nth(1)
        .expect("common_han.yaml 缺文档体");
    let listed: Vec<char> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .filter_map(|l| {
            let mut cs = l.chars();
            let c = cs.next()?;
            cs.next().is_none().then_some(c)
        })
        .collect();
    let set: HashSet<char> = listed.iter().copied().collect();
    let common: Vec<char> = listed
        .into_iter()
        .filter(|c| ('\u{4E00}'..='\u{9FFF}').contains(c))
        .take(n)
        .collect();
    let rare: Vec<char> = (0x4E00u32..=0x9FFF)
        .filter_map(char::from_u32)
        .filter(|c| !set.contains(c))
        .take(n)
        .collect();
    assert_eq!((common.len(), rare.len()), (n, n), "前置：字表够取");
    (common, rare)
}

/// 码表 `a` 前缀下三段（按权重降序）：
/// - `ab` 99 个常用字 + `ac` 1 个常用字 → 首批 100 条全可见；
/// - `ac` 150 个生僻字 → 与上面那个常用字同组，智能档全滤（第 2 批引擎 +100、可见 +0）；
/// - `ad` 50 个常用字 → 第 3 批才出现。
fn coord(tag: &str) -> (Arc<Coordinator>, Cleanup) {
    coord_with(tag, false, |common, rare| {
        let mut dict = String::new();
        for (i, ch) in common[..99].iter().enumerate() {
            dict += &format!("ab\t{ch}\t{}\n", 10_000 - i);
        }
        dict += &format!("ac\t{}\t9800\n", common[99]);
        for (i, ch) in rare.iter().enumerate() {
            dict += &format!("ac\t{ch}\t{}\n", 5_000 - i);
        }
        for (i, ch) in common[100..150].iter().enumerate() {
            dict += &format!("ad\t{ch}\t{}\n", 100 - i);
        }
        dict
    })
}

/// 自造码表夹具：`dict` 拿（常用 150 字，生僻 150 字）写词条；`wildcard` 开通配（键 `z`）。
fn coord_with(
    tag: &str,
    wildcard: bool,
    dict: impl FnOnce(&[char], &[char]) -> String,
) -> (Arc<Coordinator>, Cleanup) {
    let id = format!("zz_expand_{tag}_{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("wind_expand_stop_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let guard = Cleanup {
        id: id.clone(),
        dir: dir.clone(),
    };
    copy_factory_charsets(&dir);
    let (common, rare) = common_and_rare(&dir, 150);
    let dict = dict(&common, &rare);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join(&id)).unwrap();
    std::fs::write(
        schemas.join(format!("{id}.schema.toml")),
        format!(
            "[schema]\nid = \"{id}\"\nname = \"扩容\"\n[engine]\ntype = \"codetable\"\n\
                 [engine.codetable]\nmax_code_length = 4\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join(format!("{id}/{id}.dict.yaml")),
        format!(
            "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n{dict}"
        ),
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec![id.clone()];
    cfg.schema.active = id;
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = "smart".into();
    cfg.input.rare_phrase = "keep".into();
    // 关掉末页放宽：本用例只看扩容，不许「翻到底再按一次」把被滤的生僻字追加回来。
    cfg.input.scope_relax.page_end_key = false;
    cfg.schema.codetable.wildcard = wildcard;
    cfg.schema.codetable.wildcard_key = "z".into();
    (Coordinator::new_headless(cfg, Some(&dir)), guard)
}

fn press(c: &Coordinator, vk: u32) {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    });
}

/// 一路往后翻到「末页且不再有更多」为止（不多按：末页多按一下就是放宽意图）。
fn page_to_last(c: &Coordinator) {
    for _ in 0..200 {
        let (cur, _, total) = c.debug_page_info();
        if cur + 1 >= total && !c.debug_has_more() {
            return;
        }
        press(c, VK_NEXT);
    }
    panic!("翻页未收敛");
}

/// ★ Review Focus 2：第 2 批 100 条全被检索范围滤掉（可见条数不变），扩容不得就此判到底——
/// 引擎条数涨了，第 3 批还带着常用字。旧判据「可见条数未增」在这里停在 100 条。
#[test]
fn expansion_continues_past_an_all_filtered_batch() {
    let (c, _guard) = coord("allfiltered");
    press(&c, VK_A);
    assert_eq!(
        c.debug_candidate_count(),
        100,
        "前置：首批 100 条全是常用字"
    );
    assert!(c.debug_has_more(), "前置：引擎回满首批上限（码表单码 100）");
    page_to_last(&c);
    assert_eq!(
        c.debug_candidate_count(),
        150,
        "第 2 批全被滤，第 3 批带出 ad 下 50 个常用字"
    );
    assert!(!c.debug_has_more(), "引擎 300 条 < 上限 400，到底");
}

/// ★ 扩容后可见列表**变短**时页码/高亮须夹回范围（Task 2 审查 Important）。
/// 通配 `a?`：`ab` 150 个生僻字在前、`ac` 1 个常用字垫底，全是同一通配组。首批 100 条
/// 组里没有常用字 ⇒ 全放行；翻到倒数第二页触发扩容，第 2 批带进那个常用字 ⇒ 整组生僻字
/// 被滤、只剩 1 条。旧实现页码停在十几页上，按页切片 `start > end` 当场 panic。
#[test]
fn expansion_that_shrinks_the_list_clamps_page_and_selection() {
    let (c, _guard) = coord_with("shrink", true, |common, rare| {
        let mut dict = String::new();
        for (i, ch) in rare.iter().enumerate() {
            dict += &format!("ab\t{ch}\t{}\n", 10_000 - i);
        }
        dict += &format!("ac\t{}\t1\n", common[0]);
        dict
    });
    press(&c, VK_A);
    press(&c, VK_Z);
    assert_eq!(
        c.debug_candidate_count(),
        100,
        "前置：首批 100 条全是生僻字、全放行"
    );
    assert!(c.debug_has_more(), "前置：首批回满 100 ⇒ 还有更多");
    let first_pages = c.debug_page_info().2;
    assert!(
        first_pages > 2,
        "前置：首批要多于两页才能停在非首页：{first_pages}"
    );
    for _ in 0..first_pages {
        if c.debug_candidate_count() != 100 {
            break;
        }
        press(&c, VK_NEXT);
    }
    assert_eq!(
        c.debug_candidate_count(),
        1,
        "第 2 批带进同组常用字，生僻字整组被滤"
    );
    // 先按页切片（同渲染路径）：不夹页码时这里 `start > end` panic。
    assert_eq!(c.debug_page_texts().len(), 1, "当页可渲染、不越界");
    let (page, sel, total) = c.debug_page_info();
    assert_eq!(total, 1);
    assert_eq!((page, sel), (0, 0), "页码/高亮夹回新列表范围内");

    // 高亮单独越界（页码在范围内）也要夹：键盘路径上扩容总发生在高亮 0 的时刻，
    // 走不到这一支，故直接喂越界状态。
    let mut st = c.state.lock().unwrap();
    st.selected_index = 7;
    c.clamp_candidate_view(&mut st);
    assert_eq!(
        (st.current_page, st.selected_index),
        (0, 0),
        "高亮夹到当页末项"
    );
}
