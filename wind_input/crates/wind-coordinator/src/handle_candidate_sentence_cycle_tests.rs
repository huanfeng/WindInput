//! 整句切换键的窗口滚动：`pool[(w+i) % K]`，到头回卷，名次跟着窗口重编。
use super::*;

fn c(text: &str, rank: u8) -> Candidate {
    Candidate {
        text: text.into(),
        sentence_rank: rank,
        is_sentence: rank > 0,
        ..Default::default()
    }
}

fn pool(texts: &[&str]) -> Vec<Candidate> {
    texts
        .iter()
        .enumerate()
        .map(|(i, t)| c(t, (i + 1) as u8))
        .collect()
}

fn texts(v: &[Candidate]) -> Vec<&str> {
    v.iter().map(|x| x.text.as_str()).collect()
}

/// 露 1 算 3：同一个位置轮流换三种解读，第三次回卷到最优解。
#[test]
fn single_slot_rotates_through_pool_and_wraps() {
    let p = pool(&["甲句", "乙句", "丙句"]);
    let mut v = vec![c("甲句", 1), c("词", 0)];
    let mut w = 0;
    let mut base = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let first = cycle_sentence_window(&mut v, &mut base, &p, &mut w).expect("池子 ≥2 应当切换");
        seen.push(v[first].text.clone());
    }
    assert_eq!(seen, vec!["乙句", "丙句", "甲句"], "滚到头回卷");
    assert_eq!(texts(&v), vec!["甲句", "词"], "普通候选不动");
}

/// 露 2 算 3：两条一组往后看，名次按窗口内位置重编（块内顺序与滚动方向一致）。
#[test]
fn multi_slot_window_slides_and_ranks_follow_window() {
    let p = pool(&["甲句", "乙句", "丙句"]);
    let mut v = vec![c("甲句", 1), c("乙句", 2), c("词", 0)];
    let mut w = 0;
    let mut base = Vec::new();
    cycle_sentence_window(&mut v, &mut base, &p, &mut w).unwrap();
    assert_eq!(texts(&v), vec!["乙句", "丙句", "词"]);
    assert_eq!(
        v.iter().map(|x| x.sentence_rank).collect::<Vec<_>>(),
        vec![1, 2, 0]
    );
    cycle_sentence_window(&mut v, &mut base, &p, &mut w).unwrap();
    assert_eq!(texts(&v), vec!["丙句", "甲句", "词"], "窗口回卷");
}

/// 池子不足两条 ⇒ 不切换（返回 None），调用方据此不吃键，Tab 落回翻页。
#[test]
fn pool_below_two_does_not_cycle() {
    let mut v = vec![c("甲句", 1), c("词", 0)];
    let mut w = 0;
    let mut base = Vec::new();
    assert_eq!(
        cycle_sentence_window(&mut v, &mut base, &pool(&["甲句"]), &mut w),
        None
    );
    assert_eq!(cycle_sentence_window(&mut v, &mut base, &[], &mut w), None);
    assert_eq!(w, 0, "没切换就不该动窗口");
}

/// 候选里没有露出的整句（比如 shadow 把它删了）⇒ 不切换。
#[test]
fn no_visible_sentence_does_not_cycle() {
    let mut v = vec![c("词", 0)];
    let mut w = 0;
    let mut base = Vec::new();
    assert_eq!(
        cycle_sentence_window(&mut v, &mut base, &pool(&["甲句", "乙句"]), &mut w),
        None
    );
}

/// 换进来的整句恰好与某条普通候选同文 ⇒ 删掉那条普通候选，一屏不出两条同字。
#[test]
fn incoming_sentence_replaces_same_text_normal_candidate() {
    let p = pool(&["甲句", "你好"]);
    let mut v = vec![c("甲句", 1), c("你好", 0), c("词", 0)];
    let mut w = 0;
    let mut base = Vec::new();
    let first = cycle_sentence_window(&mut v, &mut base, &p, &mut w).unwrap();
    assert_eq!(texts(&v), vec!["你好", "词"]);
    assert_eq!(first, 0);
    assert_eq!(v[0].sentence_rank, 1, "留下的是窗口里那条");
}

/// 转满一圈回到原列表：同文去重删掉的普通候选要回来（审查查出：曾在当前列表上累积改，
/// 删掉的就再也回不来）。
#[test]
fn full_round_restores_the_original_list() {
    let p = pool(&["甲句", "你好"]);
    let orig = vec![c("甲句", 1), c("你好", 0), c("词", 0)];
    let mut v = orig.clone();
    let mut w = 0;
    let mut base = Vec::new();
    cycle_sentence_window(&mut v, &mut base, &p, &mut w).unwrap();
    assert_eq!(texts(&v), vec!["你好", "词"]);
    cycle_sentence_window(&mut v, &mut base, &p, &mut w).unwrap();
    assert_eq!(texts(&v), texts(&orig), "转满一圈应回到原列表");
    assert_eq!(w, 0);
}
