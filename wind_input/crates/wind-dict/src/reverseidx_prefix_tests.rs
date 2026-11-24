//! `texts_with_prefix`：词语联想的取数口。行为必须与常驻/mmap 无关，故每条都两路都跑。
use super::*;

fn both(
    pairs: Vec<(String, String, i32)>,
    tag: &str,
) -> (ReverseIndex, ReverseIndex, std::path::PathBuf) {
    let mem = ReverseIndex::build(pairs);
    let p = std::env::temp_dir().join(format!(
        "wind_wridx_pfx_{}_{}.wridx",
        std::process::id(),
        tag
    ));
    write_wridx(&p, mem.image()).unwrap();
    let mapped = ReverseIndex::open(&p, 0).unwrap();
    (mem, mapped, p)
}

fn sample() -> Vec<(String, String, i32)> {
    // 刻意让**字典序**与**权重序**相反——两者一致的话，「按权重排」和「按字典序排」
    // 会得出同样的结果，本组测试就什么都没验到。
    [
        ("中", "a", 1000),
        ("中一", "aa", 1),
        ("中丁", "ab", 2),
        ("中国", "ac", 900),
        ("中国人", "acd", 800),
        ("中间", "ad", 700),
        ("丰", "b", 500),
        ("串", "c", 400),
    ]
    .iter()
    .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
    .collect()
}

#[test]
fn ranks_by_weight_not_dictionary_order() {
    let (mem, mapped, p) = both(sample(), "rank");
    for i in [&mem, &mapped] {
        let texts: Vec<_> = i
            .texts_with_prefix("中", 3)
            .iter()
            .map(|(t, _)| *t)
            .collect();
        assert_eq!(
            texts,
            ["中国", "中国人", "中间"],
            "取的是最常用的三个，不是字典序前三（那会是 中一/中丁/中国）"
        );
    }
    drop((mem, mapped));
    let _ = std::fs::remove_file(&p);
}

/// ★ 前缀词**自己**必须排除：「中」的联想不该包含「中」。
///
/// 它的权重恰恰常常是全场最高（单字比词常用），不排除就会稳定占据首位，
/// 而选中它等于上屏一个空串。
#[test]
fn excludes_the_prefix_itself() {
    let (mem, mapped, p) = both(sample(), "self");
    for i in [&mem, &mapped] {
        let out = i.texts_with_prefix("中", 9);
        assert!(
            !out.iter().any(|(t, _)| *t == "中"),
            "前缀本身不是它自己的联想"
        );
        assert_eq!(out.len(), 5, "中一/中丁/中国/中国人/中间");
    }
    drop((mem, mapped));
    let _ = std::fs::remove_file(&p);
}

/// 扫描必须在第一个不匹配处停下，且不越界到相邻前缀。
#[test]
fn does_not_leak_into_neighbouring_prefixes() {
    let (mem, mapped, p) = both(sample(), "leak");
    for i in [&mem, &mapped] {
        assert!(
            i.texts_with_prefix("丰", 9).is_empty(),
            "「丰」没有更长的词"
        );
        assert!(i.texts_with_prefix("串", 9).is_empty());
        assert!(i.texts_with_prefix("龘", 9).is_empty(), "不存在的前缀");
        assert!(i.texts_with_prefix("", 9).is_empty());
        assert!(i.texts_with_prefix("中", 0).is_empty());
    }
    drop((mem, mapped));
    let _ = std::fs::remove_file(&p);
}

/// 同权时短词优先——「中国」比「中国人民」更适合当联想。
#[test]
fn same_weight_prefers_shorter() {
    let i = ReverseIndex::build(vec![
        ("中国人民".into(), "a".into(), 5),
        ("中国".into(), "b".into(), 5),
    ]);
    assert_eq!(i.texts_with_prefix("中", 2)[0].0, "中国");
}

/// 权重取该词全部条目里的**最大**值——同一个词在主库与扩展库各有一条时，
/// 取到低的那个会让它在联想里莫名靠后。
#[test]
fn weight_is_max_across_codes() {
    let i = ReverseIndex::build(vec![
        ("中国".into(), "a".into(), 1),
        ("中国".into(), "bb".into(), 900),
        ("中间".into(), "c".into(), 500),
    ]);
    assert_eq!(i.texts_with_prefix("中", 2)[0].0, "中国", "取 900 而不是 1");
}

/// ★ 回归：**同一个 (词, 码) 跨库重复、权重不同**时，聚合必须取到高的那个。
///
/// 上一条只覆盖「同词**不同**码」，而真正出问题的是同词**同码**：按 (词, 码) 去重
/// 会整条丢掉重复项，若聚合发生在去重之后，被丢的那条所带的高权重一并消失。
/// 2026-08-24 实测：`中` 在两个库里都是 `khk`（权重 10 / 999），逐库并集算出 10、
/// 先合并再构建算出 999——两条路径就此分叉，而当时的等价性判据（比索引总字节数）
/// 对权重差异完全不敏感。
///
/// 两个方向都要断言：**颠倒输入顺序结果必须相同**。只测一个方向的话，
/// 「取第一条」这种错误实现会有一半概率蒙对。
#[test]
fn weight_is_max_even_when_the_same_code_repeats_across_dicts() {
    // 两个方向都跑：只测一个方向的话，「取第一条权重」这种错误实现有一半概率蒙对。
    for (w1, w2) in [(1, 999), (999, 1)] {
        let i = ReverseIndex::build(vec![
            ("甲乙".into(), "x".into(), w1),
            ("甲乙".into(), "x".into(), w2), // 同码重复：必有一条被去重丢掉
            ("甲丙".into(), "y".into(), 500),
        ]);
        assert_eq!(
            i.codes_of("甲乙").unwrap().iter().collect::<Vec<_>>(),
            ["x"],
            "同 (词, 码) 仍须去重成一条"
        );
        assert_eq!(
            i.texts_with_prefix("甲", 2)
                .iter()
                .map(|(t, w)| (*t, *w))
                .collect::<Vec<_>>(),
            [("甲乙", 999), ("甲丙", 500)],
            "被去重丢掉的那条所带的权重必须参与聚合（w1={w1}, w2={w2}）"
        );
    }
}
