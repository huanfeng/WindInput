use super::*;

fn heads(v: &[(String, usize)]) -> Vec<&str> {
    v.iter().map(|(h, _)| h.as_str()).collect()
}

#[test]
fn readings_list_default_then_other_prefixes_then_initial() {
    // xian：引擎默认 xian；另一种切法 xi'an；再加只取声母 x
    let r = readings_of("xian", 4, &[]);
    assert_eq!(heads(&r), ["xian", "xi", "x"]);
    assert_eq!(r[1], ("xi".to_string(), 2));
}

#[test]
fn readings_include_fuzzy_siblings_with_original_length() {
    let r = readings_of("fangxiang", 4, &["fan".to_string()]);
    assert_eq!(heads(&r), ["fang", "fan", "f"]);
    assert_eq!(
        r[1],
        ("fan".to_string(), 4),
        "模糊兄弟仍占原始缓冲的 4 个字节"
    );
}

#[test]
fn digraph_initials_are_one_unit() {
    let r = readings_of("zhongguo", 5, &[]);
    assert_eq!(heads(&r), ["zhong", "zh"]);
}

#[test]
fn a_single_reading_means_no_choice() {
    assert!(readings_of("a", 1, &[]).is_empty());
    assert!(readings_of("xian", 0, &[]).is_empty(), "拆不出首音节");
}

#[test]
fn rewrite_inserts_one_separator_after_the_head() {
    assert_eq!(rewrite_query("xianzai", "xi", 2), "xi'anzai");
    assert_eq!(rewrite_query("fangxiang", "fan", 4), "fan'xiang");
    assert_eq!(rewrite_query("nihao", "n", 1), "n'ihao");
    assert_eq!(rewrite_query("ni", "ni", 2), "ni", "没有后半截不加隔音符");
    assert_eq!(
        rewrite_query("ni'hao", "ni", 2),
        "ni'hao",
        "已有隔音符不重复"
    );
}

#[test]
fn consumed_maps_back_over_the_separator() {
    // fang|xiang 读成 fan：查询 fan'xiang（8 字节含 '），原始缓冲 fangxiang（9）
    assert_eq!(
        unmap_consumed(3, 3, 4, 9),
        4,
        "只吃首单元 → 原始缓冲里的 4 字节"
    );
    assert_eq!(unmap_consumed(4, 3, 4, 9), 4, "吃到隔音符 → 仍是首单元");
    assert_eq!(
        unmap_consumed(8, 3, 4, 9),
        8,
        "fan'xian（8）→ 原始 fangxian（8）"
    );
    assert_eq!(unmap_consumed(9, 3, 4, 9), 9, "整串");
    assert_eq!(unmap_consumed(0, 3, 4, 9), 0, "0 = 未标注，约定为整串");
}

#[test]
fn shuangpin_readings_show_pinyin_but_query_with_keys() {
    // 小鹤 dg = deng；开了 en/eng 与 an/ang 模糊：兄弟 dang 对应击键 dh
    let sib = vec![("dang".to_string(), "dh".to_string())];
    let r = sp_readings_of("deng", "dgfg", 2, &sib);
    let view: Vec<_> = r
        .iter()
        .map(|(l, h, n)| (l.as_str(), h.as_str(), *n))
        .collect();
    assert_eq!(view, [("deng", "dg", 2), ("dang", "dh", 2), ("d", "d", 1)]);
}

#[test]
fn shuangpin_digraph_initial_is_one_key() {
    // 小鹤 zh = v：zhong = vs，只取声母显示 zh、查询用击键 v
    let r = sp_readings_of("zhong", "vsxx", 2, &[]);
    assert_eq!(r[1], ("zh".to_string(), "v".to_string(), 1));
}

#[test]
fn shuangpin_zero_initial_has_no_initial_reading() {
    // ang 是零声母音节，没有声母可取；也没有兄弟时就没得选
    assert!(sp_readings_of("ang", "ahxx", 2, &[]).is_empty());
    let sib = vec![("an".to_string(), "aj".to_string())];
    assert_eq!(sp_readings_of("ang", "ahxx", 2, &sib).len(), 2);
}
