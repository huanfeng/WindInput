use super::*;
use std::io::Write;

fn e(text: &str, code: &str, weight: i64) -> Entry {
    Entry::new(text.into(), code.into(), weight, 0)
}

fn rule(code: &str, text: &str, mode: BoostMode) -> BoostRule {
    BoostRule {
        code: code.into(),
        text: text.into(),
        mode,
    }
}

fn nolog() -> impl FnMut(String) {
    |_| {}
}

#[test]
fn top_puts_entry_above_current_best() {
    let mut v = vec![e("甲", "abcd", 500), e("乙", "abcd", 900)];
    let mut l = nolog();
    let (applied, missing) =
        apply_boost_rules(&mut v, &[rule("abcd", "甲", BoostMode::Top)], &mut l);
    assert_eq!((applied, missing), (1, 0));
    assert_eq!(v[0].weight, 901, "顶置 = 当前最高 +1");
}

#[test]
fn absolute_sets_weight_verbatim() {
    let mut v = vec![e("甲", "abcd", 500)];
    let mut l = nolog();
    apply_boost_rules(
        &mut v,
        &[rule("abcd", "甲", BoostMode::Absolute(1234))],
        &mut l,
    );
    assert_eq!(v[0].weight, 1234);
}

#[test]
fn relative_up_inserts_above_target_position() {
    // 排序后: 丙900 > 乙700 > 甲500；把「甲」上移 1 位 → 越过乙
    let mut v = vec![
        e("甲", "abcd", 500),
        e("乙", "abcd", 700),
        e("丙", "abcd", 900),
    ];
    let mut l = nolog();
    apply_boost_rules(
        &mut v,
        &[rule("abcd", "甲", BoostMode::Relative(1))],
        &mut l,
    );
    assert_eq!(v[0].weight, 701, "上移到目标位置权重 +1");
}

#[test]
fn relative_down_falls_below_target() {
    let mut v = vec![
        e("丙", "abcd", 900),
        e("乙", "abcd", 700),
        e("甲", "abcd", 500),
    ];
    let mut l = nolog();
    apply_boost_rules(
        &mut v,
        &[rule("abcd", "丙", BoostMode::Relative(-1))],
        &mut l,
    );
    assert_eq!(v[0].weight, 699, "下移到目标位置权重 -1");
}

#[test]
fn relative_clamps_at_list_bounds() {
    let mut v = vec![e("甲", "abcd", 500), e("乙", "abcd", 900)];
    let mut l = nolog();
    // 上移 99 位，列表只有 2 条 → 夹到首位
    apply_boost_rules(
        &mut v,
        &[rule("abcd", "甲", BoostMode::Relative(99))],
        &mut l,
    );
    assert_eq!(v[0].weight, 901);
}

#[test]
fn sequential_rules_on_same_code_see_prior_changes() {
    // 两条顶置规则依次作用：第二条应看到第一条的结果
    let mut v = vec![
        e("甲", "abcd", 500),
        e("乙", "abcd", 600),
        e("丙", "abcd", 900),
    ];
    let mut l = nolog();
    apply_boost_rules(
        &mut v,
        &[
            rule("abcd", "甲", BoostMode::Top),
            rule("abcd", "乙", BoostMode::Top),
        ],
        &mut l,
    );
    assert_eq!(v[0].weight, 901, "甲先顶置到 901");
    assert_eq!(v[1].weight, 902, "乙再顶置须越过甲");
}

#[test]
fn unmatched_rule_is_counted_not_fatal() {
    let mut v = vec![e("甲", "abcd", 500)];
    let mut logs = Vec::new();
    let (applied, missing) = apply_boost_rules(
        &mut v,
        &[rule("abcd", "不存在", BoostMode::Top)],
        &mut |s| logs.push(s),
    );
    assert_eq!((applied, missing), (0, 1));
    assert!(logs.iter().any(|l| l.contains("未匹配")));
}

#[test]
fn parses_three_adjust_forms() {
    let p = std::env::temp_dir().join("gen_dict_boost_test.txt");
    let mut f = std::fs::File::create(&p).unwrap();
    writeln!(f, "# 注释").unwrap();
    writeln!(f, "abcd\t甲").unwrap();
    writeln!(f, "abcd\t乙\t+2").unwrap();
    writeln!(f, "abcd\t丙\t-3").unwrap();
    writeln!(f, "abcd\t丁\t777").unwrap();
    drop(f);
    let r = load_boost_rules(&p).unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(r[0].mode, BoostMode::Top, "留空 = 顶置");
    assert_eq!(r[1].mode, BoostMode::Relative(2));
    assert_eq!(r[2].mode, BoostMode::Relative(-3));
    assert_eq!(r[3].mode, BoostMode::Absolute(777), "裸数字 = 绝对权重");
}
