use super::*;

fn build(tmp_name: &str, data: &[(&str, &[(&str, i32)])]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(tmp_name);
    let mut w = WdatWriter::new();
    for (code, ents) in data {
        let v: Vec<(String, i32)> = ents.iter().map(|(t, wt)| (t.to_string(), *wt)).collect();
        w.add(code.to_string(), v);
    }
    w.write(&p).expect("write wdat");
    p
}

/// 按 (code, entries) 构建 wdat（owned 版，供批量生成用例）。走 `WdatWriter::add`
/// → `with_local_order`，即 order = code 内序号——与 `export_to_writer` / combined
/// 合并路径同语义，单条 code 时 order 恒 0（等权即全打平，最坏 tie-break 场景）。
fn build_owned(tmp_name: &str, data: &[(String, Vec<(String, i32)>)]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(tmp_name);
    let mut w = WdatWriter::new();
    for (code, ents) in data {
        w.add(code.clone(), ents.clone());
    }
    w.write(&p).expect("write wdat");
    p
}

/// 参考实现：「DFS 全量收集 → 稳定排序 → 截断」。top-N 堆的输出必须与它逐条相同（含顺序）。
fn reference_prefix(r: &WdatReader, prefix: &str, limit: usize) -> Vec<DictEntry> {
    let v = &r.main;
    let Some(start) = r.walk(v, prefix) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut path: Vec<u8> = prefix.as_bytes().to_vec();
    r.for_each_leaf(v, start, &mut path, &mut |code, leaf| {
        r.read_leaf_entries(v, leaf, &mut |text, weight, order, boundary| {
            out.push(DictEntry {
                code: code.to_string(),
                text: text.to_string(),
                weight,
                order,
                boundary,
            });
        });
    });
    out.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
    out.truncate(limit);
    out
}

/// 通配查询的全遍历参考实现：全树 DFS 收集全部条目 → 按 pattern 过滤 →
/// 等长 / 更长两档各自按 (weight 降, order 升) 稳定排序 → 等长先取满 `limit`，余额给更长档。
/// DFS 序即叶号序（构建期按 code 字典序分配），故稳定排序的 tie-break 与 `RankKey` 一致。
fn reference_pattern(
    r: &WdatReader,
    pattern: &str,
    wildcard: char,
    limit: usize,
    with_prefix: bool,
) -> Vec<DictEntry> {
    let v = &r.main;
    let n = pattern.len();
    let mut exact: Vec<DictEntry> = Vec::new();
    let mut longer: Vec<DictEntry> = Vec::new();
    let mut path: Vec<u8> = Vec::new();
    r.for_each_leaf(v, 0, &mut path, &mut |code, leaf| {
        if !crate::layer::pattern_matches(pattern, wildcard, code, with_prefix) {
            return;
        }
        let bucket = if code.len() == n {
            &mut exact
        } else {
            &mut longer
        };
        r.read_leaf_entries(v, leaf, &mut |text, weight, order, boundary| {
            bucket.push(DictEntry {
                code: code.to_string(),
                text: text.to_string(),
                weight,
                order,
                boundary,
            });
        });
    });
    let key = |a: &DictEntry, b: &DictEntry| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order));
    exact.sort_by(key);
    longer.sort_by(key);
    exact.truncate(limit);
    let rest = limit - exact.len();
    longer.truncate(rest);
    exact.extend(longer);
    exact
}

/// 通配查询 == 全遍历参考实现，逐条相同（含顺序）。覆盖多通配、首位通配、`with_prefix`、
/// 转移空洞（`i % 5` 挖掉的码）与大量等权 tie-break。
#[test]
fn search_pattern_matches_full_scan() {
    let alphabet = ['a', 'b', 'c'];
    let mut data: Vec<(String, Vec<(String, i32)>)> = Vec::new();
    let mut i = 0u32;
    for len in 1..=4u32 {
        for k in 0..3usize.pow(len) {
            let mut code = String::new();
            let mut x = k;
            for _ in 0..len {
                code.push(alphabet[x % 3]);
                x /= 3;
            }
            i += 1;
            if i.is_multiple_of(5) {
                continue; // 挖空洞：逼出「该位无此转移」的分支
            }
            let w = ((i * 7919) % 7) as i32 * 100; // 权重大量重复，逼出 tie-break
            let mut ents = vec![(format!("甲{i}"), w)];
            if i.is_multiple_of(4) {
                ents.push((format!("乙{i}"), w)); // 一码多词
            }
            data.push((code, ents));
        }
    }
    let p = build_owned("wdat_pattern_full_scan.wdat", &data);
    let r = WdatReader::open(&p).unwrap();
    for pat in ["?", "a?", "?b", "??", "a?c", "??c?", "????", "c??a", "?a"] {
        for with_prefix in [false, true] {
            for limit in [1usize, 3, 10, 1000] {
                assert_same(
                    &r.search_pattern(pat, '?', limit, with_prefix),
                    &reference_pattern(&r, pat, '?', limit, with_prefix),
                    &format!("pat={pat} with_prefix={with_prefix} limit={limit}"),
                );
            }
        }
    }
    let _ = std::fs::remove_file(&p);
}

/// 等长档不得被更长的高权重编码挤出配额。
#[test]
fn search_pattern_equal_length_never_crowded_out() {
    let p = build(
        "wdat_pattern_quota.wdat",
        &[
            ("ab", &[("等长", 1)]),
            ("abcd", &[("长码", 9999)]),
            ("ac", &[("等长二", 2)]),
        ],
    );
    let r = WdatReader::open(&p).unwrap();
    let texts = |v: Vec<DictEntry>| v.into_iter().map(|e| e.text).collect::<Vec<_>>();
    assert_eq!(
        texts(r.search_pattern("a?", '?', 1, true)),
        ["等长二"],
        "limit=1 时名额先给等长档"
    );
    assert_eq!(
        texts(r.search_pattern("a?", '?', 3, true)),
        ["等长二", "等长", "长码"]
    );
    assert_eq!(
        texts(r.search_pattern("a?", '?', 3, false)),
        ["等长二", "等长"],
        "不带前缀补全时只出等长"
    );
    assert!(
        r.search_pattern("x?", '?', 3, true).is_empty(),
        "字面位无转移 ⇒ 空"
    );
    let _ = std::fs::remove_file(&p);
}

fn assert_same(actual: &[DictEntry], expect: &[DictEntry], ctx: &str) {
    let a: Vec<_> = actual
        .iter()
        .map(|e| (&e.code, &e.text, e.weight))
        .collect();
    let b: Vec<_> = expect
        .iter()
        .map(|e| (&e.code, &e.text, e.weight))
        .collect();
    assert_eq!(a, b, "{ctx}");
}

/// top-N 堆 == 全量排序取前 N，在权重与 order 全部打平时也成立。
///
/// 对应无 weight 列、每 code 单条的拼字词库形状：两级排序键全部失效，顺序只由「保持 DFS 序」
/// 兜底；堆本身不稳定，靠 `RankKey::seq` 复现，这正是最容易与全排序分歧之处。
#[test]
fn topn_prefix_matches_full_sort_when_all_keys_tie() {
    let data: Vec<(String, Vec<(String, i32)>)> = (0..500u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            (format!("ok{c1}{c2}"), vec![(format!("字{i}"), 0)])
        })
        .collect();
    let p = build_owned("wdat_topn_all_tie.wdat", &data);
    let r = WdatReader::open(&p).unwrap();

    for limit in [1usize, 5, 90, 499, 500, 501, 1000] {
        assert_same(
            &r.search_prefix("ok", limit),
            &reference_prefix(&r, "ok", limit),
            &format!("全打平 limit={limit}"),
        );
    }
    let _ = std::fs::remove_file(&p);
}

/// top-N 堆 == 全量排序取前 N，混合权重 + 同 code 多条（order 0,1,2）时也成立。
#[test]
fn topn_prefix_matches_full_sort_mixed_weights() {
    let data: Vec<(String, Vec<(String, i32)>)> = (0..200u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            // 权重故意大量重复（i % 7），并让部分 code 带 2 条（order 0/1）。
            let mut ents = vec![(format!("甲{i}"), (i % 7) as i32 * 100)];
            if i % 3 == 0 {
                ents.push((format!("乙{i}"), (i % 7) as i32 * 100));
            }
            (format!("ok{c1}{c2}"), ents)
        })
        .collect();
    let p = build_owned("wdat_topn_mixed.wdat", &data);
    let r = WdatReader::open(&p).unwrap();

    for limit in [1usize, 3, 50, 200, 999] {
        assert_same(
            &r.search_prefix("ok", limit),
            &reference_prefix(&r, "ok", limit),
            &format!("混合权重 limit={limit}"),
        );
    }
    let _ = std::fs::remove_file(&p);
}

/// 剪枝确实发生：权重有区分度时，小 limit 的访问状态数须远低于全遍历。
///
/// 对拍测试证明不了这件事：`..._all_keys_tie` 里 weight 全为 0，`bound < worst.weight` 恒假，
/// 剪枝退化成全遍历，剪枝逻辑被误删也照样全绿。这里直接钉 `states_visited`。
#[test]
fn bnb_actually_prunes_when_weights_differ() {
    let data: Vec<(String, Vec<(String, i32)>)> = (0..676u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            // 伪随机但确定的权重，让高权重分散到不同子树（逼近真实词库形状）。
            let w = ((i * 7919) % 10000) as i32;
            (format!("ok{c1}{c2}"), vec![(format!("字{i}"), w)])
        })
        .collect();
    let p = build_owned("wdat_bnb_prune.wdat", &data);
    let r = WdatReader::open(&p).unwrap();

    let (_, full) = r.search_prefix_stats("ok", 100_000);
    let (_, few) = r.search_prefix_stats("ok", 5);
    assert!(
        few.states_visited * 4 < full.states_visited,
        "limit=5 应因剪枝远少于全遍历：few={} full={}",
        few.states_visited,
        full.states_visited
    );
    // 剪枝不得损害正确性。
    for limit in [1usize, 5, 40, 676, 999] {
        assert_same(
            &r.search_prefix("ok", limit),
            &r.search_prefix_scan("ok", limit),
            &format!("剪枝对拍 limit={limit}"),
        );
    }
    let _ = std::fs::remove_file(&p);
}

/// 裸声母（`sh`、cap=1）的形状：单字稀疏且权重低，每棵子树都挂着高权重的多音节词，
/// 按权重上界剪不动。码长上限的深度剪枝须既剪得动、又与全量过滤逐条一致。
#[test]
fn syllable_capped_prunes_by_code_len() {
    let firsts = [
        "sha", "shan", "shang", "she", "shen", "sheng", "shi", "shu", "shuang",
    ];
    let seconds = ["de", "men", "hou", "jian", "zhong", "guo", "ren", "zhuang"];
    let mut data: Vec<(String, Vec<WriteEntry>)> = Vec::new();
    for (i, s1) in firsts.iter().enumerate() {
        data.push((s1.to_string(), vec![(format!("单{i}"), i as i32 + 1, 0, 1)]));
        for (j, s2) in seconds.iter().enumerate() {
            let b2 = 1u64 | 1 << s1.len();
            data.push((
                format!("{s1}{s2}"),
                vec![(format!("词{i}{j}"), 1000, 0, b2)],
            ));
            for s3 in &seconds {
                let b3 = b2 | 1 << (s1.len() + s2.len());
                data.push((
                    format!("{s1}{s2}{s3}"),
                    vec![(format!("长{i}{j}"), 2000, 0, b3)],
                ));
            }
        }
    }
    // 无边界信息：码长 ≤ 6 的放行，超过的拦下。
    data.push(("shix".into(), vec![("短无界".into(), 50, 0, 0)]));
    data.push(("shishishi".into(), vec![("长无界".into(), 5000, 0, 0)]));
    data.sort_by(|a, b| a.0.cmp(&b.0));
    data.dedup_by(|a, b| a.0 == b.0);
    let p = std::env::temp_dir().join("wdat_capped_len.wdat");
    let mut w = WdatWriter::new();
    for (code, ents) in data {
        w.add_with_boundary(code, ents);
    }
    w.write(&p).expect("write wdat");
    let r = WdatReader::open(&p).unwrap();

    let expect = |limit: usize| -> Vec<DictEntry> {
        let mut all = reference_prefix(&r, "sh", 100_000);
        all.retain(|e| {
            e.code.len() <= crate::cached::prefix_code_len_cap(1)
                && crate::cached::prefix_entry_keep(e.boundary, e.code.len(), 1, 0)
        });
        all.truncate(limit);
        all
    };
    let got = r.search_prefix_syllable_capped("sh", 300, 1, 0);
    assert!(got.iter().any(|e| e.text == "短无界"));
    assert!(got.iter().all(|e| e.text != "长无界"));
    for limit in [1usize, 5, 10, 300] {
        assert_same(
            &r.search_prefix_syllable_capped("sh", limit, 1, 0),
            &expect(limit),
            &format!("码长剪枝对拍 limit={limit}"),
        );
    }
    let (_, capped) = r.search_prefix_inner("sh", 300, Some((1, 0)));
    let (_, full) = r.search_prefix_stats("sh", 100_000);
    assert!(
        capped.states_visited * 4 < full.states_visited,
        "cap=1 应按码长剪掉深层子树：capped={} full={}",
        capped.states_visited,
        full.states_visited
    );
    let _ = std::fs::remove_file(&p);
}

/// 剪枝判据必须是严格小于，放宽成 `<=` 即静默漏候选。
///
/// 上界只覆盖 RankKey 的首要键 weight，`bound == 第 limit 名的 weight` 时子树内仍可能有同
/// weight 但 order/leaf 更小的条目应当取代第 limit 名。
///
/// 构造让出队顺序完全确定（否则差异随 tie 顺序时隐时现）：`za` 一个叶子内含两条（300/100）
/// 一次性填满 limit=2，worst 的 weight=100 由它独自决定；唯一待展开的 `a` 子树 maxw 恰为 100，
/// 其中 `甲`(order=0, leaf=0) 优于 `丙`(order=1, leaf=1)，必须能取代它。
#[test]
fn pruning_bound_must_be_strictly_less() {
    let p = build(
        "wdat_bnb_strict.wdat",
        &[
            ("ab", &[("甲", 100)][..]),
            ("za", &[("乙", 300), ("丙", 100)][..]),
        ],
    );
    let r = WdatReader::open(&p).unwrap();
    let got: Vec<String> = r
        .search_prefix("", 2)
        .iter()
        .map(|e| e.text.clone())
        .collect();
    assert_eq!(
        got,
        vec!["乙".to_string(), "甲".to_string()],
        "同 weight 时 order 更小的「甲」应取代「丙」；若剪枝误用 <=，「甲」所在子树被整棵跳过"
    );
    assert_same(
        &r.search_prefix("", 2),
        &r.search_prefix_scan("", 2),
        "严格小于对拍",
    );
    let _ = std::fs::remove_file(&p);
}

/// `maxw` 必须是真实上界：偏大只是剪枝变弱，偏小则静默漏候选。
/// 逐前缀与暴力遍历子树求得的最大 weight 对拍。
#[test]
fn maxw_equals_brute_force_subtree_max() {
    let p = build(
        "wdat_maxw_bound.wdat",
        &[
            ("ab", &[("甲", 100)][..]),
            ("abc", &[("乙", 700)][..]),
            ("abd", &[("丙", 300)][..]),
            ("az", &[("丁", 50)][..]),
            ("z", &[("戊", 900)][..]),
        ],
    );
    let r = WdatReader::open(&p).unwrap();
    let v = &r.main;
    for prefix in ["", "a", "ab", "abc", "abd", "az", "z"] {
        let Some(s) = r.walk(v, prefix) else {
            panic!("前缀 {prefix:?} 应可达");
        };
        let mut brute = NO_MAXW;
        let mut path: Vec<u8> = prefix.as_bytes().to_vec();
        r.for_each_leaf(v, s, &mut path, &mut |_code, leaf| {
            r.read_leaf_entries(v, leaf, &mut |_t, w, _o, _b| {
                brute = brute.max(w);
            });
        });
        assert_eq!(
            r.maxw(v, s),
            brute,
            "前缀 {prefix:?} 的子树权重上界与暴力结果不符"
        );
    }
    let _ = std::fs::remove_file(&p);
}

/// 护栏：DFS 必须遍历完整棵子树，不得早停。
///
/// 最高权重的词放在字典序最末。若改成 `codetable.rs` / `binformat.rs` 那种「攒够 limit(*2)
/// 就 break」，DFS 按字典序推进，这条永远进不了结果集。
#[test]
fn topn_prefix_keeps_high_weight_entry_at_lexical_tail() {
    let mut data: Vec<(String, Vec<(String, i32)>)> = (0..400u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            (format!("ok{c1}{c2}"), vec![(format!("庸{i}"), 10)])
        })
        .collect();
    data.push(("okzz".to_string(), vec![("压轴".to_string(), 9999)]));
    let p = build_owned("wdat_topn_tail.wdat", &data);
    let r = WdatReader::open(&p).unwrap();

    let out = r.search_prefix("ok", 5);
    assert_eq!(out.len(), 5);
    assert_eq!(
        out[0].text, "压轴",
        "字典序最末的高权重词被漏掉 ⇒ DFS 提前中止了"
    );
    assert_eq!(out[0].code, "okzz");
    let _ = std::fs::remove_file(&p);
}

/// `has_longer_code` 的基本语义：只认严格更长的后继，前缀自身成词不算。
#[test]
fn has_longer_code_semantics() {
    let p = build(
        "wdat_has_longer.wdat",
        &[
            ("ok", &[("好", 10)]),     // 自身成词，且下面还有更长的
            ("okz", &[("仄", 10)]),    // 中间节点，自身也成词
            ("okzz", &[("最长", 10)]), // 该支最长
            ("om", &[("某", 10)]),     // 自身成词，无更长后继
        ],
    );
    let r = WdatReader::open(&p).unwrap();

    assert!(r.has_longer_code(""), "空前缀：词库非空即有更长码");
    assert!(r.has_longer_code("o"), "o 下有 ok/om");
    assert!(r.has_longer_code("ok"), "ok 自身成词，但下面还有 okz/okzz");
    assert!(r.has_longer_code("okz"), "okz 自身成词，但下面还有 okzz");
    assert!(!r.has_longer_code("okzz"), "okzz 是该支最长，无更长后继");
    assert!(!r.has_longer_code("om"), "om 自身成词且无后继");
    assert!(!r.has_longer_code("xyz"), "前缀不存在");
    assert!(!r.has_longer_code("okzzz"), "越过最长码");
    let _ = std::fs::remove_file(&p);
}

/// `has_longer_code` 与旧判据（`search_prefix(_, 64)` 再 `any(code 更长)`）在未触发截断时
/// 一致——覆盖「前缀自身成词 / 中间节点 / 叶子 / 不存在」四类位置。
///
/// 截断时旧判据会因长码权重低被挤出前 64 名而漏判；新实现直接问 trie，故新 ⊇ 旧，只会更
/// 保守，这正是「更长后继」作为上屏安全阀所需的方向。
#[test]
fn has_longer_code_agrees_with_legacy_predicate_when_untruncated() {
    let data: Vec<(String, Vec<(String, i32)>)> = (0..300u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            // 一半是 3 码（叶），一半再挂 4 码后继 → 覆盖有/无更长后继两类。
            if i % 2 == 0 {
                (format!("z{c1}{c2}"), vec![(format!("甲{i}"), 10)])
            } else {
                (format!("z{c1}{c2}q"), vec![(format!("乙{i}"), 10)])
            }
        })
        .collect();
    let p = build_owned("wdat_has_longer_agree.wdat", &data);
    let r = WdatReader::open(&p).unwrap();

    let legacy = |prefix: &str| -> bool {
        let n = prefix.chars().count();
        r.search_prefix(prefix, 64)
            .iter()
            .any(|e| e.code.chars().count() > n)
    };
    // 枚举全部编码的全部前缀 + 空串 + 若干不存在的码。
    let mut prefixes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    prefixes.insert(String::new());
    for extra in ["zzzz", "q", "z"] {
        prefixes.insert(extra.to_string());
    }
    r.for_each_entry(&mut |code, _t, _w| {
        for i in 1..=code.len() {
            prefixes.insert(code[..i].to_string());
        }
    });
    for pre in &prefixes {
        assert_eq!(
            r.has_longer_code(pre),
            legacy(pre),
            "前缀 {pre:?} 上新旧判据分歧"
        );
    }
    let _ = std::fs::remove_file(&p);
}

/// limit=0 → 空（与改造前 `truncate(0)` 语义一致，且不做无谓的 DFS）。
#[test]
fn topn_prefix_limit_zero_is_empty() {
    let data = vec![("oka".to_string(), vec![("甲".to_string(), 10)])];
    let p = build_owned("wdat_topn_zero.wdat", &data);
    let r = WdatReader::open(&p).unwrap();
    assert!(r.search_prefix("ok", 0).is_empty());
    assert_eq!(r.search_prefix("ok", 1).len(), 1);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn exact_match_multi_key() {
    let p = build(
        "wdat_exact_test.wdat",
        &[
            ("a", &[("工", 9999), ("戈", 100)]),
            ("ni", &[("你", 800), ("尼", 50)]),
            ("nihao", &[("你好", 1200)]),
            ("zhongguo", &[("中国", 2000)]),
        ],
    );
    let r = WdatReader::open(&p).unwrap();
    assert_eq!(r.key_count(), 4);

    let a = r.search("a");
    assert_eq!(a.len(), 2);
    assert!(a.iter().any(|e| e.text == "工" && e.weight == 9999));
    assert!(a.iter().any(|e| e.text == "戈"));

    let nihao = r.search("nihao");
    assert_eq!(nihao.len(), 1);
    assert_eq!(nihao[0].text, "你好");
    assert_eq!(nihao[0].weight, 1200);
    assert_eq!(nihao[0].code, "nihao");

    let zg = r.search("zhongguo");
    assert_eq!(zg.len(), 1);
    assert_eq!(zg[0].text, "中国");

    // 不存在的 code。
    assert!(r.search("xyz").is_empty());
    assert!(r.search("n").is_empty()); // "n" 非终止（是 ni/nihao 前缀但无 n 自身词）
    let _ = std::fs::remove_file(&p);
}

#[test]
fn prefix_collects_subtree_with_codes() {
    let p = build(
        "wdat_prefix_test.wdat",
        &[
            ("ni", &[("你", 800)]),
            ("nihao", &[("你好", 1200)]),
            ("nihaoma", &[("你好吗", 300)]),
            ("nin", &[("您", 600)]),
            ("zhong", &[("中", 500)]),
        ],
    );
    let r = WdatReader::open(&p).unwrap();
    // 前缀 "ni" → ni/nihao/nihaoma/nin（不含 zhong）。
    let res = r.search_prefix("ni", 10);
    let texts: Vec<&str> = res.iter().map(|e| e.text.as_str()).collect();
    assert!(texts.contains(&"你"));
    assert!(texts.contains(&"你好"));
    assert!(texts.contains(&"你好吗"));
    assert!(texts.contains(&"您"));
    assert!(!texts.contains(&"中"), "前缀 ni 不应含 zhong: {texts:?}");
    // 重建的 code 正确。
    let nihao = res.iter().find(|e| e.text == "你好").unwrap();
    assert_eq!(nihao.code, "nihao");
    let nihaoma = res.iter().find(|e| e.text == "你好吗").unwrap();
    assert_eq!(nihaoma.code, "nihaoma");
    // 按权重降序。
    assert_eq!(res[0].text, "你好", "最高权重 1200 应排首: {texts:?}");
    // limit 截断。
    assert_eq!(r.search_prefix("ni", 2).len(), 2);
    let _ = std::fs::remove_file(&p);
}

/// 无权重（全同权）时，跨编码前缀查询应按全局 order（词库出现顺序）排列，而非退化为
/// 编码字母序。构造 order 与字母序相反以区分两者。
#[test]
fn prefix_no_weight_sorts_by_global_order_not_code() {
    let p = std::env::temp_dir().join("wdat_global_order_test.wdat");
    let mut w = WdatWriter::new();
    // 出现顺序 za → ma → aa（order 0,1,2），编码字母序则相反 aa < ma < za；权重全 0。
    w.add_with_order("za".to_string(), vec![("Z".to_string(), 0, 0)]);
    w.add_with_order("ma".to_string(), vec![("M".to_string(), 0, 1)]);
    w.add_with_order("aa".to_string(), vec![("A".to_string(), 0, 2)]);
    w.write(&p).expect("write wdat");
    let r = WdatReader::open(&p).unwrap();
    let res = r.search_prefix("", 10);
    let texts: Vec<&str> = res.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["Z", "M", "A"],
        "无权重应按全局 order（出现序）排序而非编码字母序，实际: {texts:?}"
    );
    // 单编码多条同权：前缀查询（会按 weight desc、order asc 排序）亦按 order 升序恢复出现序。
    // （精确 search 不排序、返回叶内存储序——生产路径写入前已预排序，故存储序即展示序。）
    let mut w2 = WdatWriter::new();
    w2.add_with_order(
        "aa".to_string(),
        vec![
            ("三".to_string(), 0, 2),
            ("一".to_string(), 0, 0),
            ("二".to_string(), 0, 1),
        ],
    );
    let p2 = std::env::temp_dir().join("wdat_global_order_test2.wdat");
    w2.write(&p2).expect("write wdat");
    let r2 = WdatReader::open(&p2).unwrap();
    let t2: Vec<String> = r2
        .search_prefix("aa", 10)
        .into_iter()
        .map(|e| e.text)
        .collect();
    assert_eq!(
        t2,
        vec!["一", "二", "三"],
        "同码同权前缀查询应按 order 升序"
    );
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(&p2);
}

#[test]
fn for_each_enumerates_all() {
    let p = build(
        "wdat_foreach_test.wdat",
        &[("a", &[("工", 9999), ("戈", 100)]), ("aaaa", &[("叕", 50)])],
    );
    let r = WdatReader::open(&p).unwrap();
    let mut got: Vec<(String, String, i32)> = Vec::new();
    r.for_each_entry(&mut |c, t, w| got.push((c.to_string(), t.to_string(), w)));
    assert_eq!(got.len(), 3, "应枚举 3 条: {got:?}");
    assert!(got.contains(&("a".to_string(), "工".to_string(), 9999)));
    assert!(got.contains(&("a".to_string(), "戈".to_string(), 100)));
    assert!(got.contains(&("aaaa".to_string(), "叕".to_string(), 50)));
    let _ = std::fs::remove_file(&p);
}

/// 对拍：同一份数据分别建 wdb(binformat) 与 wdat，比对精确/前缀/全枚举查询结果一致
/// （按 (code,text,weight) 集合比较，忽略 natural_order 这一格式差异）。
#[test]
fn parity_with_wdb() {
    use crate::binformat::{DictReader, DictWriter};
    let data: Vec<(&str, Vec<(&str, i32)>)> = vec![
        ("a", vec![("工", 9999), ("戈", 100)]),
        ("ni", vec![("你", 800), ("尼", 50)]),
        ("nihao", vec![("你好", 1200)]),
        ("nihaoma", vec![("你好吗", 300)]),
        ("nin", vec![("您", 600)]),
        ("zhong", vec![("中", 500)]),
        ("zhongguo", vec![("中国", 2000)]),
        ("zhi", vec![("之", 700), ("知", 690)]),
    ];
    let to_owned = |e: &Vec<(&str, i32)>| -> Vec<(String, i32)> {
        e.iter().map(|(t, w)| (t.to_string(), *w)).collect()
    };

    let wdb_path = std::env::temp_dir().join("wdat_parity.wdb");
    let mut dw = DictWriter::new();
    for (c, e) in &data {
        dw.add(c.to_string(), to_owned(e));
    }
    dw.write(&wdb_path).unwrap();
    let wdb = DictReader::open(&wdb_path).unwrap();

    let wdat_path = std::env::temp_dir().join("wdat_parity.wdat");
    let mut ww = WdatWriter::new();
    for (c, e) in &data {
        ww.add(c.to_string(), to_owned(e));
    }
    ww.write(&wdat_path).unwrap();
    let wdat = WdatReader::open(&wdat_path).unwrap();

    assert_eq!(wdb.key_count(), wdat.key_count(), "key_count 应一致");

    // 精确：每个 code 的 (text,weight) 集合一致。
    for (c, _) in &data {
        let mut a: Vec<(String, i32)> = wdb
            .search(c)
            .into_iter()
            .map(|e| (e.text, e.weight))
            .collect();
        let mut b: Vec<(String, i32)> = wdat
            .search(c)
            .into_iter()
            .map(|e| (e.text, e.weight))
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "精确查询 '{c}' 不一致");
    }
    // 不存在 code 两者都空。
    assert!(wdb.search("xyz").is_empty() && wdat.search("xyz").is_empty());

    // 前缀：(code,text,weight) 集合一致（含空前缀=全量、单字母、整码）。
    for pre in ["ni", "n", "nih", "zhong", "z", "a", "zh", ""] {
        let mut a: Vec<(String, String, i32)> = wdb
            .search_prefix(pre, 100000)
            .into_iter()
            .map(|e| (e.code, e.text, e.weight))
            .collect();
        let mut b: Vec<(String, String, i32)> = wdat
            .search_prefix(pre, 100000)
            .into_iter()
            .map(|e| (e.code, e.text, e.weight))
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "前缀查询 '{pre}' 不一致");
    }

    // for_each_entry：全量 (code,text,weight) 集合一致。
    let mut ea = Vec::new();
    wdb.for_each_entry(&mut |c, t, w| ea.push((c.to_string(), t.to_string(), w)));
    let mut eb = Vec::new();
    wdat.for_each_entry(&mut |c, t, w| eb.push((c.to_string(), t.to_string(), w)));
    ea.sort();
    eb.sort();
    assert_eq!(ea, eb, "全枚举不一致");

    let _ = std::fs::remove_file(&wdb_path);
    let _ = std::fs::remove_file(&wdat_path);
}

/// `pool_hash` 的低位必须分布开——开放寻址取槽用的是 `h & mask`。
///
/// 去重测试守不住这条：判等是逐字节的，哈希再烂结果也对，只是线性探测退化成长链扫描。
/// 这里直接数不同落点：两字中文串在 2^17 槽上应接近随机分布。
#[test]
fn pool_hash_spreads_low_bits_for_short_cjk_strings() {
    let mask = (1usize << 17) - 1;
    let mut homes = std::collections::HashSet::new();
    let mut n = 0usize;
    // 常用汉字区里取 256×256 个两字组合，头一个字只有 256 种——正是会把坏哈希
    // 打回原形的那种输入。
    for a in 0x4E00u32..0x4F00 {
        for b in 0x5000u32..0x5100 {
            let s: String = [char::from_u32(a).unwrap(), char::from_u32(b).unwrap()]
                .iter()
                .collect();
            homes.insert((pool_hash(s.as_bytes()) as usize) & mask);
            n += 1;
        }
    }
    // 65536 个键扔进 131072 个槽，理想随机约落 51.6k 个不同槽（1-e^-0.5 的期望）。
    // 下限取 45k 留足余量；去掉终混的 `rotate_left` 时只落 128 个槽。
    assert!(
        homes.len() > 45_000,
        "{n} 个两字串只落在 {} 个不同的槽——低位没混开，线性探测会退化成长链",
        homes.len()
    );
}

/// 字符串池必须真的去重：同一文本无论来自哪个 code、哪个段，都返回同一个偏移，且 `buf`
/// 里只存一份。roundtrip 用例守不住这条（去重失效时读出来照样对，只是文件变大）；
/// 去重错（返回别人的偏移）则会读出另一个词。两个方向这里都断言。
#[test]
fn string_pool_dedups_and_never_returns_a_foreign_offset() {
    let mut pool = StringPool::new();

    // 同串多次 add → 同一偏移，且 buf 不增长。
    let a1 = pool.add("你好");
    let a2 = pool.add("你好");
    assert_eq!(a1, a2, "同一文本必须复用同一偏移");
    assert_eq!(pool.buf.len(), "你好".len(), "重复文本不该再占 buf");

    // 不同串 → 不同偏移，且各自读回来是自己。
    let mut offs = Vec::new();
    // 刻意混入：空串、单字节、长度相同但内容不同、互为前缀、以及足够多条触发多次 grow。
    let mut words: Vec<String> = vec![
        String::new(),
        "a".into(),
        "b".into(),
        "ab".into(),
        "abc".into(),
        "你好".into(),
        "你坏".into(),
    ];
    for i in 0..5000 {
        words.push(format!("词{i}"));
    }
    for w in &words {
        offs.push((w.clone(), pool.add(w)));
    }
    // 再来一遍：必须全部命中已有偏移。
    for (w, off) in &offs {
        assert_eq!(pool.add(w), *off, "第二次 add {w:?} 应命中原偏移");
    }
    // 逐条按偏移取回，必须是自己——去重表把偏移串了的话这里立刻红。
    for (w, off) in &offs {
        let a = *off as usize;
        let got = std::str::from_utf8(&pool.buf[a..a + w.len()]).expect("utf8");
        assert_eq!(got, w.as_str(), "偏移 {off} 处应是 {w:?}");
    }
    // 唯一文本数 == buf 里应有的总字节数（去重没漏）。
    let uniq: std::collections::HashSet<&String> = words.iter().collect();
    let expect: usize = uniq.iter().map(|w| w.len()).sum();
    assert_eq!(pool.buf.len(), expect, "buf 应恰好装下每个唯一文本一份");
}

/// 简拼 AbbrevSection 往返：简拼查得到、按权重排序，且不污染全拼精确/前缀查询。
/// 条目内容是全拼码（二级索引指向主键），`DictEntry::text` 装的是码，调用方拿它去主表装配候选。
#[test]
fn abbrev_section_roundtrip() {
    let p = std::env::temp_dir().join("wdat_abbrev_test.wdat");
    let mut w = WdatWriter::new();
    w.add("nihao".into(), vec![("你好".into(), 1200)]);
    w.add("beijing".into(), vec![("北京".into(), 2000)]);
    // 存码不存词；同一简拼下多个码按权重降序（权重只用于截断时取舍）。
    w.add_abbrev(
        "nh".into(),
        vec![("nihao".into(), 1200), ("nihuo".into(), 5)],
    );
    w.add_abbrev("bj".into(), vec![("beijing".into(), 2000)]);
    w.add_abbrev("nhm".into(), vec![("nihaoma".into(), 300)]);
    w.write(&p).unwrap();

    let r = WdatReader::open(&p).unwrap();
    assert!(r.has_abbrev());
    // 简拼命中 + 权重降序；取出的是全拼码。
    let nh = r.search_abbrev("nh", 10);
    assert_eq!(nh.len(), 2);
    assert_eq!(nh[0].text, "nihao", "按权重降序，且存的是码: {nh:?}");
    assert_eq!(nh[0].code, "nh");
    assert_eq!(r.search_abbrev("bj", 10)[0].text, "beijing");
    assert_eq!(r.search_abbrev("nhm", 10)[0].text, "nihaoma");
    assert!(r.search_abbrev("zzz", 10).is_empty());
    // 不污染全拼：全拼 search/search_prefix 不应命中简拼码。
    assert!(r.search("nh").is_empty(), "全拼精确不应命中简拼码 nh");
    assert_eq!(r.search("nihao")[0].text, "你好");
    let pre = r.search_prefix("n", 100);
    assert!(pre.iter().any(|e| e.text == "你好"));
    assert!(
        !pre.iter().any(|e| e.code == "nh" || e.code == "nhm"),
        "前缀查询不应含简拼码: {:?}",
        pre.iter().map(|e| &e.code).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_file(&p);
}

/// 无简拼区段时 search_abbrev 返回空、has_abbrev=false。
#[test]
fn no_abbrev_section() {
    let p = build("wdat_noabbrev_test.wdat", &[("a", &[("工", 1)])]);
    let r = WdatReader::open(&p).unwrap();
    assert!(!r.has_abbrev());
    assert!(r.search_abbrev("nh", 10).is_empty());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn shared_prefix_and_branching() {
    // 强分支：同前缀大量分叉，验证 base/check 冲突解决正确。
    let data: Vec<(String, Vec<(String, i32)>)> = (0..200)
        .map(|i| (format!("code{i:03}"), vec![(format!("词{i}"), i)]))
        .collect();
    let p = std::env::temp_dir().join("wdat_branch_test.wdat");
    let mut w = WdatWriter::new();
    for (c, e) in &data {
        w.add(c.clone(), e.clone());
    }
    w.write(&p).unwrap();
    let r = WdatReader::open(&p).unwrap();
    assert_eq!(r.key_count(), 200);
    for i in 0..200 {
        let res = r.search(&format!("code{i:03}"));
        assert_eq!(res.len(), 1, "code{i:03} 应命中");
        assert_eq!(res[0].text, format!("词{i}"));
        assert_eq!(res[0].weight, i);
    }
    // 前缀 code0 → code000..code099（100 条）。
    let pre = r.search_prefix("code0", 1000);
    assert_eq!(pre.len(), 100, "code0 前缀应 100 条");
    let _ = std::fs::remove_file(&p);
}

/// 复刻「深、密、共享前缀」的 key 形态：编码几乎全落在 6/8 位、字母表仅 a-z（形如百万级
/// key 的形码扩展词库）。线性探测 `find_base` 在这种输入下退化为 O(状态数²)（见 [`FreeList`]
/// 顶部说明）。这里用 30 万 key 验证在合理时间内完成且结果正确。
#[test]
fn large_scale_build_stays_fast_and_correct() {
    fn gen_code(mut n: u64, len: usize) -> String {
        let mut buf = vec![b'a'; len];
        for i in (0..len).rev() {
            buf[i] = b'a' + (n % 26) as u8;
            n /= 26;
        }
        String::from_utf8(buf).unwrap()
    }
    let total = 300_000usize;
    let mut data: Vec<(String, Vec<(String, i32)>)> = Vec::with_capacity(total);
    let (mut c4, mut c6, mut c8) = (0u64, 0u64, 0u64);
    for i in 0..total {
        // 大致复刻真实文件里 4/6/8 位码的比例（约 1:4:4）。
        let code = match i % 9 {
            0 => {
                let c = gen_code(c4, 4);
                c4 += 1;
                c
            }
            1..=4 => {
                let c = gen_code(c6, 6);
                c6 += 1;
                c
            }
            _ => {
                let c = gen_code(c8, 8);
                c8 += 1;
                c
            }
        };
        data.push((code, vec![(format!("词{i}"), i as i32)]));
    }

    let p = std::env::temp_dir().join("wdat_large_scale_test.wdat");
    let start = std::time::Instant::now();
    let mut w = WdatWriter::new();
    for (c, e) in &data {
        w.add(c.clone(), e.clone());
    }
    w.write(&p).unwrap();
    let elapsed = start.elapsed();
    eprintln!("built {total} keys in {elapsed:?}");
    assert!(
        elapsed.as_secs() < 30,
        "{total} 个 key 构建耗时 {elapsed:?}——若退化回平方级复杂度，这里会是\
             几分钟甚至更久（历史故障见 FreeList 顶部说明）",
    );

    let r = WdatReader::open(&p).unwrap();
    assert_eq!(r.key_count() as usize, total);
    // 抽样点查验证正确性（量太大，不做全量校验）。
    for i in (0..total).step_by(997) {
        let (code, _) = &data[i];
        let res = r.search(code);
        assert_eq!(res.len(), 1, "{code} 应命中");
        assert_eq!(res[0].text, format!("词{i}"));
    }
    let _ = std::fs::remove_file(&p);
}

// ───────────── S3：流式写入器与有序游标 ─────────────

/// 一份带简拼区、边界、order 与 meta 的输入，分别走批量写入器与流式写入器，
/// 产物逐字节相同。这是「两条路径共用一套构建」的对拍基础。
#[test]
fn stream_writer_matches_batch_writer_byte_for_byte() {
    type Entries<'a> = Vec<(&'a str, i32, u32, u64)>;
    let data: Vec<(&str, Entries)> = vec![
        (
            "nihao",
            vec![("你好", 100, 0, 0b101), ("拟好", 20, 1, 0b101)],
        ),
        ("ni", vec![("你", 500, 0, 0b1), ("泥", 30, 1, 0b1)]),
        ("xian", vec![("先", 80, 0, 0b1), ("西安", 70, 1, 0b101)]),
        ("a", vec![("啊", 1, 0, 0b1)]),
        ("nihaoma", vec![("你好吗", 10, 0, 0b10101)]),
    ];
    let abbrevs: Vec<(&str, Vec<(&str, i32)>)> = vec![
        ("nh", vec![("nihao", 100)]),
        ("xa", vec![("xian", 70)]),
        ("nhm", vec![("nihaoma", 10)]),
    ];
    let mut batch = WdatWriter::new();
    for (code, ents) in &data {
        batch.add_with_boundary(
            code.to_string(),
            ents.iter()
                .map(|(t, w, o, b)| (t.to_string(), *w, *o, *b))
                .collect(),
        );
    }
    for (ab, ents) in &abbrevs {
        batch.add_abbrev(
            ab.to_string(),
            ents.iter().map(|(c, w)| (c.to_string(), *w)).collect(),
        );
    }
    batch.set_meta(b"meta-bytes".to_vec());
    let pb = std::env::temp_dir().join("wind_stream_eq_batch.wdat");
    batch.write(&pb).unwrap();

    let mut sorted = data.clone();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut sw = WdatStreamWriter::new();
    for (code, ents) in &sorted {
        sw.push_code(code, ents.iter().map(|(t, w, o, b)| (*t, *w, *o, *b)));
    }
    let mut ab_sorted = abbrevs.clone();
    ab_sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (ab, ents) in &ab_sorted {
        sw.push_abbrev_sorted(ab, ents.iter().map(|(c, w)| (*c, *w)));
    }
    sw.set_meta(b"meta-bytes".to_vec());
    let ps = std::env::temp_dir().join("wind_stream_eq_stream.wdat");
    sw.build().emit(&ps).unwrap();

    assert_eq!(
        std::fs::read(&pb).unwrap(),
        std::fs::read(&ps).unwrap(),
        "批量与流式两条路径产物必须逐字节相同"
    );
    let _ = std::fs::remove_file(&pb);
    let _ = std::fs::remove_file(&ps);
}

/// 同一 code 超过 65535 条候选：计数截断、偏移按实际条数前进，后续叶不错位，
/// 且与批量写入器逐字节相同。
#[test]
fn stream_writer_caps_leaf_count_without_misaligning_later_leaves() {
    let big: Vec<(String, i32)> = (0..70_000).map(|i| (format!("w{i}"), 1)).collect();
    let mut batch = WdatWriter::new();
    batch.add("a".into(), big.clone());
    batch.add("b".into(), vec![("乙".into(), 9)]);
    let pb = std::env::temp_dir().join("wind_stream_cap_batch.wdat");
    batch.write(&pb).unwrap();
    let mut sw = WdatStreamWriter::new();
    sw.push_code(
        "a",
        big.iter()
            .enumerate()
            .map(|(i, (t, w))| (t.as_str(), *w, i as u32, 0)),
    );
    sw.push_code("b", [("乙", 9, 0, 0)]);
    let ps = std::env::temp_dir().join("wind_stream_cap_stream.wdat");
    sw.build().emit(&ps).unwrap();
    assert_eq!(std::fs::read(&pb).unwrap(), std::fs::read(&ps).unwrap());
    let r = WdatReader::open(&ps).unwrap();
    assert_eq!(r.search("a").len(), u16::MAX as usize);
    assert_eq!(r.search("b").first().map(|e| e.text.as_str()), Some("乙"));
    let _ = std::fs::remove_file(&pb);
    let _ = std::fs::remove_file(&ps);
}

/// 乱序喂码必须 panic：这是 DAT 正确性的不变量，不能静默建错。
#[test]
#[should_panic(expected = "严格递增")]
fn stream_writer_rejects_unsorted_codes() {
    let mut sw = WdatStreamWriter::new();
    sw.push_code("b", [("乙", 1, 0, 0)]);
    sw.push_code("a", [("甲", 1, 0, 0)]);
}

/// 游标按字节序严格递增、短码先于长码，且与 `for_each_entry` 的 (code,text,weight)
/// 序列完全一致（含同码多条与跨段重复）；`leaf_entries` 与精确查询一致。
#[test]
fn code_cursor_is_sorted_and_matches_for_each_entry() {
    let p = build(
        "wind_cursor.wdat",
        &[
            ("nihao", &[("你好", 100), ("拟好", 20)]),
            ("ni", &[("你", 500)]),
            ("", &[("空码", 1)]),
            ("nihaoma", &[("你好吗", 10), ("你好吗", 10)]),
            ("a", &[("啊", 1)]),
            ("ab", &[("阿布", 2)]),
        ],
    );
    let r = WdatReader::open(&p).unwrap();
    let mut via_cursor: Vec<(String, String, i32)> = Vec::new();
    let mut prev: Option<String> = None;
    for (code, leaf) in r.codes() {
        if let Some(pv) = &prev {
            assert!(code.as_bytes() > pv.as_bytes(), "{pv:?} -> {code:?} 不递增");
        }
        prev = Some(code.clone());
        let ents = r.leaf_entries(leaf);
        let direct = r.search(&code);
        assert_eq!(
            ents.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(),
            direct
                .iter()
                .map(|e| (e.text.as_str(), e.weight))
                .collect::<Vec<_>>(),
            "leaf_entries 与精确查询应一致: {code}"
        );
        for (t, w, _, _) in ents {
            via_cursor.push((code.clone(), t.to_string(), w));
        }
    }
    let mut via_each: Vec<(String, String, i32)> = Vec::new();
    r.for_each_entry(&mut |c, t, w| via_each.push((c.to_string(), t.to_string(), w)));
    assert_eq!(via_cursor, via_each);
    assert_eq!(via_cursor.len(), 8);
    assert_eq!(via_cursor[0].0, "", "空码（根为终止态）先出");
    let _ = std::fs::remove_file(&p);
}
