use super::*;

fn schema_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin")
}

fn conv(id: &str) -> ShuangpinConverter {
    let p = schema_dir().join(format!("{id}.toml"));
    let layout = Layout::from_toml(&p).unwrap_or_else(|e| panic!("加载 {id} 失败: {e}"));
    ShuangpinConverter::new(layout)
}

// --- TestXiaoheBasic ---
#[test]
fn xiaohe_basic() {
    let c = conv("xiaohe");
    let cases = [
        ("ni", "ni", false),
        ("nihc", "nihao", false),
        ("womf", "women", false),
        ("n", "n", true),
        ("", "", false),
    ];
    for (input, want, want_partial) in cases {
        let r = c.convert(input);
        assert_eq!(r.full_pinyin(), want, "convert({input:?}).full_pinyin");
        assert_eq!(
            r.has_partial, want_partial,
            "convert({input:?}).has_partial"
        );
    }
}

/// 直接辅助码的前缀闸门：整串恰好切成完整双拼音节才给出音节数。
#[test]
fn full_syllable_count_requires_whole_pairs() {
    let c = conv("xiaohe");
    assert_eq!(c.full_syllable_count("uidu"), Some(2), "shi du");
    assert_eq!(c.full_syllable_count("goqk"), Some(2), "guo qing");
    assert_eq!(c.full_syllable_count("xl"), Some(1));
    assert_eq!(c.full_syllable_count("uidupl"), None, "pl 不是小鹤合法音节");
    assert_eq!(c.full_syllable_count("uid"), None, "尾部落单键");
    assert_eq!(c.full_syllable_count("ui'du"), None, "含手动分隔符");
    assert_eq!(c.full_syllable_count(""), None);
}

// --- TestXiaoheSyllables ---
#[test]
fn xiaohe_syllables() {
    let c = conv("xiaohe");
    let r = c.convert("nihc");
    assert_eq!(r.syllables.len(), 2);
    assert_eq!(r.syllables[0].pinyin, "ni");
    assert_eq!(r.syllables[1].pinyin, "hao");
    assert_eq!((r.syllables[0].raw_start, r.syllables[0].raw_end), (0, 2));
    assert_eq!((r.syllables[1].raw_start, r.syllables[1].raw_end), (2, 4));
}

// --- TestXiaoheZhChSh ---
#[test]
fn xiaohe_zh_ch_sh() {
    let c = conv("xiaohe");
    let cases = [
        ("vs", "zhong"),
        ("ig", "cheng"),
        ("uf", "shen"),
        ("vv", "zhui"),
        ("dv", "dui"),
        ("gv", "gui"),
        ("go", "guo"),
        ("ho", "huo"),
        ("xp", "xie"),
        ("bp", "bie"),
        ("zz", "zou"),
        ("dz", "dou"),
        ("nv", "nv"),
        ("lv", "lv"),
    ];
    for (input, want) in cases {
        assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
    }
}

// --- TestXiaoheZeroInitial ---
#[test]
fn xiaohe_zero_initial() {
    let c = conv("xiaohe");
    let cases = [
        ("aa", "a"),
        ("oo", "o"),
        ("ee", "e"),
        ("ai", "ai"),
        ("an", "an"),
        ("ei", "ei"),
        ("en", "en"),
        ("ou", "ou"),
    ];
    for (input, want) in cases {
        assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
    }
}

// --- TestConsumedLengthMapping ---
#[test]
fn consumed_length_mapping() {
    let c = conv("xiaohe");
    let r = c.convert("nihc");
    assert_eq!(r.map_consumed_length(0), 0);
    assert_eq!(r.map_consumed_length(2), 2);
    assert_eq!(r.map_consumed_length(5), 4);
}

// --- TestConsumedLengthAbbrev ---
#[test]
fn consumed_length_abbrev() {
    let c = conv("xiaohe");
    let r = c.convert("bzd");
    assert_eq!(r.map_consumed_length(3), 3);

    let r2 = c.convert("nihcbzd");
    assert_eq!(r2.map_consumed_length(8), 7);
    assert_eq!(r2.map_consumed_length(5), 4);
}

// --- TestPartialInput ---
#[test]
fn partial_input() {
    let c = conv("xiaohe");
    let r = c.convert("nih");
    assert_eq!(r.syllables.len(), 1);
    assert!(r.has_partial);
    assert_eq!(r.partial_initial.as_deref(), Some("h"));
}

// --- TestZiranmaVKey ---
#[test]
fn ziranma_v_key() {
    let c = conv("ziranma");
    let cases = [("dv", "dui"), ("gv", "gui"), ("nv", "nv"), ("lv", "lv")];
    for (input, want) in cases {
        assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
    }
}

// --- TestSogouVKey ---
#[test]
fn sogou_v_key() {
    let c = conv("sogou");
    let cases = [("dv", "dui"), ("gv", "gui"), ("ny", "nv"), ("ly", "lv")];
    for (input, want) in cases {
        assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
    }
}

// --- TestZiguangScheme ---
#[test]
fn ziguang_scheme() {
    let c = conv("ziguang");
    let cases = [
        ("ut", "zheng"),
        ("ux", "zhua"),
        ("ir", "shan"),
        ("ik", "shei"),
        ("aq", "chao"),
        ("nb", "niao"),
        ("mw", "men"),
        ("ds", "dang"),
        ("gh", "gong"),
        ("jj", "jiu"),
        ("lk", "lei"),
        ("ll", "luan"),
        ("xy", "xin"),
        ("gz", "gou"),
        ("nn", "nve"),
        ("ln", "lve"),
    ];
    for (input, want) in cases {
        assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
    }
}

// --- TestGb2017Scheme ---
/// GB/T 34947-2017 的 ü 与同键双韵母。
///
/// ü 在布局里一律写 `v`（j/q/x/y 后由 `normalize_pinyin` 折回 u），写成字面 `ü`/`üe`/`ün` 时
/// jue/que/xue/yue、lve/nve、lv/nv 整片打不出。一键双韵母只取第一个合法者，`["o", "uo"]` 会让
/// `lo`（咯）抢走 `luo`。零声母按标准 4.1.1 以 `a` 引导——`ac`=ao、`ao`=o，不是首字母直接重复。
#[test]
fn gb2017_scheme() {
    let c = conv("gb2017");
    let cases = [
        ("jx", "jue"),
        ("qx", "que"),
        ("xx", "xue"),
        ("yx", "yue"),
        ("lx", "lve"),
        ("nx", "nve"),
        ("jv", "ju"),
        ("lv", "lv"),
        ("nv", "nv"),
        ("lo", "luo"),
        ("jw", "juan"),
        ("jz", "jun"),
        ("aa", "a"),
        ("af", "an"),
        ("al", "er"),
        ("ao", "o"),
        ("ac", "ao"),
    ];
    for (input, want) in cases {
        assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
    }
}

// --- TestZeroInitialAo ---
#[test]
fn zero_initial_ao() {
    let cases = [
        ("xiaohe", "ac", "ao"),
        ("xiaohe", "aa", "a"),
        ("xiaohe", "ai", "ai"),
        ("xiaohe", "an", "an"),
        ("xiaohe", "ah", "ang"),
        ("ziranma", "ak", "ao"),
        ("ziranma", "aa", "a"),
        ("ziranma", "al", "ai"),
        ("ziranma", "aj", "an"),
        ("ziranma", "ah", "ang"),
        // 微软/搜狗零声母以 `o` 引导（不是首字母引导——那是自然码/小鹤的规则）。
        ("mspy", "ok", "ao"),
        ("mspy", "oa", "a"),
        ("mspy", "ol", "ai"),
        ("mspy", "oj", "an"),
        ("mspy", "oh", "ang"),
        ("sogou", "ok", "ao"),
        ("sogou", "oa", "a"),
        ("sogou", "ol", "ai"),
        ("sogou", "oj", "an"),
        ("sogou", "oh", "ang"),
    ];
    for (scheme, input, want) in cases {
        let c = conv(scheme);
        assert_eq!(
            c.convert(input).full_pinyin(),
            want,
            "{scheme} convert({input:?})"
        );
    }
}

// --- TestZeroInitialLiteralAo ---
// 测的是零声母的「字面匹配」路径（convert_pair 路径 b）：击键本身就是音节全拼。
// 只对首字母引导的方案成立；微软/搜狗是纯 `o` 引导，其 `ao` 走常规声母路径，由官方击键表覆盖。
#[test]
fn zero_initial_literal_ao() {
    for scheme in ["xiaohe", "ziranma"] {
        let c = conv(scheme);
        assert_eq!(
            c.convert("ao").full_pinyin(),
            "ao",
            "{scheme} convert(\"ao\")"
        );
    }
}

// --- TestPreeditDisplay ---
#[test]
fn preedit_display() {
    let c = conv("xiaohe");
    let r = c.convert("nihc");
    assert_eq!(r.preedit_display, "ni'hao");
}

// ===== converter_fuzzy_test.go =====

// --- TestXiaoheFuzzy_SLNeedsFuzzyToShuang ---
#[test]
fn xiaohe_fuzzy_sl_needs_fuzzy_to_shuang() {
    let mut c = conv("xiaohe");
    // fuzzy 关：s+l 无合法 → fallback "sl"
    assert_eq!(c.convert("sl").full_pinyin(), "sl");
    // fuzzy s↔sh 开启：s+l 应被 sh+l 模糊补救 → "shuang"
    c.set_fuzzy(&FuzzyConfig {
        sh_s: true,
        ..Default::default()
    });
    assert_eq!(c.convert("sl").full_pinyin(), "shuang");
    // 整段 slpb → shuangpin
    assert_eq!(c.convert("slpb").full_pinyin(), "shuangpin");
}

// --- TestXiaoheFuzzy_OriginalLegalNotShadowed ---
#[test]
fn xiaohe_fuzzy_original_legal_not_shadowed() {
    let mut c = conv("xiaohe");
    c.set_fuzzy(&initials_fuzzy());
    assert_eq!(c.convert("zi").full_pinyin(), "zi");
    assert_eq!(c.convert("zisi").full_pinyin(), "zisi");
}

// --- TestXiaoheFuzzy_Bidirectional ---
#[test]
fn xiaohe_fuzzy_bidirectional() {
    let mut c = conv("xiaohe");
    c.set_fuzzy(&FuzzyConfig {
        zh_z: true,
        ..Default::default()
    });
    let results = c.convert_pair(b'v', b'd');
    assert!(results.iter().any(|s| s == "zhai"), "缺 zhai: {results:?}");
    assert!(
        results.iter().any(|s| s == "zai"),
        "应含 zai 候选: {results:?}"
    );
    assert_eq!(results[0], "zhai", "原始声母合法时应排首位: {results:?}");
}

// --- TestXiaoheFuzzy_DisabledSwitch ---
#[test]
fn xiaohe_fuzzy_disabled_switch() {
    let c = conv("xiaohe");
    let results = c.convert_pair(b's', b'l');
    assert!(results.is_empty(), "fuzzy 关时 s+l 应为空: {results:?}");
}

/// 同键韵母：`q = ["iu", "ui"]` 时 `dq` 两个读音互为对端；`w` 上 `ia`/`o` 同键时
/// `lw` 得 `lia`/`lo`。零声母引导键不造对（`o` 不会和 `ia` 配上）。
/// `x` 同时放 uo/o/ia：出厂固有的 luo/lo 按对排除，lia 与两者的对照常生效。
const SHARED_KEY: &str = r#"
[meta]
id = "shared"
name = "同键测试"
[finals]
q = ["iu", "ui"]
w = ["ia", "ua", "o"]
x = ["uo", "o", "ia"]
f = ["en"]
[zero_initials]
o = ["o", "ou"]
"#;

#[test]
fn shared_key_syllables_pairs_same_key_finals() {
    let c = ShuangpinConverter::new(Layout::from_toml_str(SHARED_KEY).unwrap());
    let m = c.shared_key_syllables();
    assert_eq!(m.get("diu"), Some(&vec!["dui".to_string()]));
    assert_eq!(m.get("dui"), Some(&vec!["diu".to_string()]));
    let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(m.get("lia"), Some(&v(&["lo", "luo"])));
    assert_eq!(
        m.get("lo"),
        Some(&v(&["lia"])),
        "luo/lo 是出厂固有对，不得互配"
    );
    assert_eq!(m.get("luo"), Some(&v(&["lia"])));
    assert!(!m.contains_key("o"), "零声母路径不造对: {:?}", m.get("o"));
    // 只有一个合法音节的键对不进表（`gua` 的 g+w：gia/go 都不成音节）。
    assert!(!m.contains_key("gua"), "{:?}", m.get("gua"));
    assert!(!m.contains_key("den"));
    // 转换器本身行为不变：仍只交出首个读音，其余靠引擎展开。
    assert_eq!(c.convert("dq").full_pinyin, "diu");
}

#[test]
fn pair_edges_agree_with_convert_first_syllable() {
    let c = conv("xiaohe");
    for keys in ["nihaoma", "ni'hao", "nihc"] {
        let r = c.convert(keys);
        let edges = c.pair_edges(keys);
        assert_eq!(edges.len(), r.syllables.len(), "{keys}");
        for (e, s) in edges.iter().zip(&r.syllables) {
            assert_eq!(e.syllables[0], s.pinyin, "{keys}");
            assert_eq!((e.start, e.end), (s.raw_start, s.raw_end), "{keys}");
            assert_eq!(e.literal, s.literal.is_some(), "{keys}");
        }
    }
}

#[test]
fn pair_edges_keep_all_readings_for_shared_key() {
    let c = ShuangpinConverter::new(Layout::from_toml_str(SHARED_KEY).unwrap());
    let edges = c.pair_edges("dq");
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].syllables,
        vec!["diu".to_string(), "dui".to_string()]
    );
}

#[test]
fn pair_edges_skip_unpairable_and_tail_key() {
    let c = conv("xiaohe");
    assert!(c.pair_edges("pl").is_empty(), "pl 小鹤拼不出");
    assert!(c.pair_edges("n").is_empty());
    let e = c.pair_edges("nihcn");
    assert_eq!(e.len(), 2, "尾键不成边");
    let e = c.pair_edges("n'hc");
    assert_eq!((e.len(), e[0].start, e[0].end), (1, 2, 4), "偏移含 '");
}

/// 每个偏移都试，含重叠；不跨 `'`；是 `pair_edges` 的超集。
#[test]
fn pair_edges_all_tries_every_offset() {
    let c = conv("xiaohe");
    let spans = |keys: &str| -> Vec<(usize, usize, String)> {
        c.pair_edges_all(keys)
            .into_iter()
            .map(|e| (e.start, e.end, e.syllables[0].clone()))
            .collect()
    };
    // 小鹤：ui = shi，iu = ?（拼不出），uu = shu
    let all = spans("uiuu");
    assert!(all.contains(&(0, 2, "shi".to_string())), "{all:?}");
    assert!(all.contains(&(2, 4, "shu".to_string())), "{all:?}");
    assert!(all.iter().any(|e| e.0 == 1) == !c.convert_pair(b'i', b'u').is_empty());
    let sep = c.pair_edges_all("ni'hc");
    assert!(sep.iter().all(|e| e.start != 1 && e.start != 2), "{sep:?}");
    for keys in ["nihaoma", "ni'hao", "nihcn", "qiddn"] {
        let all = c.pair_edges_all(keys);
        for e in c.pair_edges(keys) {
            assert!(all.contains(&e), "{keys}: {e:?}");
        }
    }
}

/// 每个韵母键只放一个韵母的布局不产生任何对（出厂布局的情形见下一个用例）。
#[test]
fn shared_key_syllables_empty_for_single_final_keys() {
    let single = r#"
[meta]
id = "single"
name = "单韵母键"
[finals]
q = ["iu"]
v = ["ui"]
"#;
    let c = ShuangpinConverter::new(Layout::from_toml_str(single).unwrap());
    assert!(c.shared_key_syllables().is_empty());
}

/// 出厂布局的同键情形全部落在排除表里：出厂布局推不出任何同键对。
///
/// 守护排除表的完整性：按不排除的口径重算各布局的同键音节对，逐对断言在表里。新增/修改出厂布局
/// 引入新的固有对时这里变红：要么补进表，要么评估让它生效的排序影响。
#[test]
fn builtin_layouts_shared_key_pairs_are_all_excluded() {
    let mut seen: Vec<(String, String)> = Vec::new();
    for id in all_builtin_ids() {
        let c = conv(&id);
        assert!(c.shared_key_syllables().is_empty(), "{id} 不得产生同键对");
        for initial in c.layout.initials.values() {
            for finals in c.layout.finals.values() {
                let mut syls: Vec<String> = Vec::new();
                for f in finals {
                    let s = normalize_pinyin(&format!("{initial}{f}"));
                    // ue/ve 归一化后同形，同一音节不成对。
                    if c.is_valid(&s) && !syls.contains(&s) {
                        syls.push(s);
                    }
                }
                for (i, a) in syls.iter().enumerate() {
                    for b in &syls[i + 1..] {
                        assert!(
                            is_builtin_shared_pair(a, b),
                            "{id}: 固有同键对 {a}/{b} 不在 BUILTIN_SHARED_KEY_PAIRS 里"
                        );
                        seen.push((a.clone(), b.clone()));
                    }
                }
            }
        }
    }
    // 表里每一对都确有出厂布局用到，免得表只增不减、悄悄吞掉自定义布局的对。
    for &(x, y) in BUILTIN_SHARED_KEY_PAIRS {
        assert!(
            seen.iter()
                .any(|(a, b)| (a == x && b == y) || (a == y && b == x)),
            "排除表里的 {x}/{y} 已无出厂布局使用"
        );
    }
}

fn initials_fuzzy() -> FuzzyConfig {
    FuzzyConfig {
        zh_z: true,
        ch_c: true,
        sh_s: true,
        ..Default::default()
    }
}

fn all_fuzzy() -> FuzzyConfig {
    FuzzyConfig {
        zh_z: true,
        ch_c: true,
        sh_s: true,
        n_l: true,
        f_h: true,
        r_l: true,
        an_ang: true,
        en_eng: true,
        in_ing: true,
        ian_iang: true,
        uan_uang: true,
        un_ong: true,
        eng_ong: true,
        ..Default::default()
    }
}

/// 韵母模糊音：自然码 `n` = in，`dn` 解出 `din`——它不是音节，但开着 in↔ing 时
/// 是登记过的错音串，交给拼音引擎按 `ding` 展开（与全拼打 `din` 同路）。
#[test]
fn ziranma_fuzzy_final_keeps_misspelling() {
    let mut c = conv("ziranma");
    assert_eq!(c.convert("dn").full_pinyin(), "dn", "模糊音关：dn 无解");
    c.set_fuzzy(&FuzzyConfig {
        in_ing: true,
        ..Default::default()
    });
    assert_eq!(c.convert("dn").full_pinyin(), "din");
    assert_eq!(c.convert("tn").full_pinyin(), "tin");
    assert_eq!(c.convert("dnyi").full_pinyin(), "dinyi");
    // 本身合法的不受影响。
    assert_eq!(c.convert("bn").full_pinyin(), "bin");
}

// --- 边界测试 6.1-4：符号键 `;` e2e 转换（mspy 布局，;=ing）---
// mspy.toml 显式声明 ";" = ["ing"]，输入 "n;" 应转换为 "ning"（n 声母 + ; 韵母 ing）。
#[test]
fn mspy_semicolon_final_e2e() {
    let c = conv("mspy");
    // 单音节：n + ; → ning
    let r = c.convert("n;");
    assert_eq!(r.full_pinyin(), "ning", "mspy: n; 应转换为 ning");
    assert_eq!(r.syllables.len(), 1, "应有 1 个音节");
    assert_eq!(r.syllables[0].pinyin, "ning");
    assert_eq!((r.syllables[0].raw_start, r.syllables[0].raw_end), (0, 2));

    // 多音节：n; + ni → ning + ni（第二对是普通双拼）
    let r2 = c.convert("n;ni");
    assert_eq!(r2.full_pinyin(), "ningni", "mspy: n;ni 应转换为 ningni");
    assert_eq!(r2.syllables.len(), 2);
    assert_eq!(r2.syllables[0].pinyin, "ning");
    assert_eq!(r2.syllables[1].pinyin, "ni");
}

// ===== 首道双拼（zero_pairs）=====

// 零声母：12 条显式映射全覆盖
#[test]
fn shoudao_zero_pairs_all() {
    let c = conv("shoudao");
    let cases = [
        ("aa", "a"),
        ("ai", "ai"),
        ("an", "an"),
        ("ay", "ang"),
        ("ao", "ao"),
        ("ue", "e"),
        ("ui", "ei"),
        ("en", "en"),
        ("uf", "eng"),
        ("er", "er"),
        ("oo", "o"),
        ("ou", "ou"),
    ];
    for (input, want) in cases {
        assert_eq!(
            c.convert(input).full_pinyin(),
            want,
            "首道 convert({input:?})"
        );
    }
}

// 常规声母+韵母不受 zero_pairs 影响
#[test]
fn shoudao_regular_initials() {
    let c = conv("shoudao");
    let cases = [
        ("vs", "zhou"),  // v=zh, s=ou
        ("ef", "sheng"), // e=sh, f=eng
        ("id", "chao"),  // i=ch, d=ao
        ("ni", "ni"),    // n=n, i=i
        ("hd", "hao"),   // h=h, d=ao
    ];
    for (input, want) in cases {
        assert_eq!(
            c.convert(input).full_pinyin(),
            want,
            "首道 convert({input:?})"
        );
    }
}

// ef 只产出 sheng，不应有零声母 eng 污染
#[test]
fn shoudao_ef_no_eng_pollution() {
    let c = conv("shoudao");
    let results = c.convert_pair(b'e', b'f');
    assert_eq!(results, vec!["sheng"], "ef 应仅产出 sheng: {results:?}");
}

// ee 走常规路径 sh+e=she，不产出零声母 e
#[test]
fn shoudao_ee_is_she() {
    let c = conv("shoudao");
    assert_eq!(c.convert("ee").full_pinyin(), "she", "首道 ee → she");
}

// 多音节混合
#[test]
fn shoudao_multi_syllable() {
    let c = conv("shoudao");
    // ni + ue(→e) → nie? no: ni=ni, ue=e → "nie"? Let me think...
    // convert processes in pairs: (n,i)=ni, (u,e)=e → "nie"
    let r = c.convert("niue");
    assert_eq!(r.full_pinyin(), "nie", "首道 niue → ni+e");
    assert_eq!(r.syllables.len(), 2);
    assert_eq!(r.syllables[0].pinyin, "ni");
    assert_eq!(r.syllables[1].pinyin, "e");

    // hd + uf(→eng) → hao+eng
    let r2 = c.convert("hduf");
    assert_eq!(r2.full_pinyin(), "haoeng", "首道 hduf → hao+eng");
    assert_eq!(r2.syllables.len(), 2);
}

// 奇数尾键 partial
#[test]
fn shoudao_partial() {
    let c = conv("shoudao");
    let r = c.convert("aal");
    assert_eq!(r.syllables.len(), 1);
    assert_eq!(r.syllables[0].pinyin, "a");
    assert!(r.has_partial);
    assert_eq!(r.partial_initial.as_deref(), Some("l"));
}

// consumed_length 回映射
#[test]
fn shoudao_consumed_length() {
    let c = conv("shoudao");
    // ue → "e"（全拼 1 字节 → 双拼 2 字节）
    let r = c.convert("ue");
    assert_eq!(r.map_consumed_length(1), 2);

    // aa + ue → "a" + "e"（全拼各 1 字节）
    let r2 = c.convert("aaue");
    assert_eq!(r2.map_consumed_length(1), 2); // "a" 消耗双拼 2 字节
    assert_eq!(r2.map_consumed_length(2), 4); // "ae" 消耗双拼 4 字节
}

// 旧方案 zero_initials 不受影响（回归）
#[test]
fn xiaohe_zero_initials_unchanged() {
    let c = conv("xiaohe");
    let layout = c.layout();
    assert!(!layout.has_zero_pairs(), "小鹤不应有 zero_pairs");
    let cases = [
        ("aa", "a"),
        ("oo", "o"),
        ("ee", "e"),
        ("ac", "ao"),
        ("ah", "ang"),
        ("ew", "ei"),
        ("ef", "en"),
    ];
    for (input, want) in cases {
        assert_eq!(
            c.convert(input).full_pinyin(),
            want,
            "小鹤 convert({input:?}) 回归"
        );
    }
}

// ========================================================================
// 反向表（音节 → 双拼键对）
// ========================================================================

/// 全部内置布局。新增布局文件后这里会自动带上——`schema_dir` 下的每个 `.toml`。
fn all_builtin_ids() -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(schema_dir())
        .expect("读取双拼布局目录")
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "toml").then(|| p.file_stem()?.to_str().map(str::to_string))?
        })
        .collect();
    ids.sort();
    assert!(ids.len() >= 8, "内置布局应至少 8 个，实得 {}", ids.len());
    ids
}

/// 反向表的正确性定义：表里说音节 S 敲 `[k1,k2]`，敲下这个键对就必须能得到 S，
/// 即 S ∈ `convert_pair(k1, k2)`。
///
/// 用「包含」而非「恒等」：建表第二遍会给只能作次选出现的冷僻音节兜底（往下翻一格才选得到，
/// 但编码没错）。恒等那一档由 `reverse_common_syllables_are_first_choice` 单独钉。
#[test]
fn reverse_table_is_decodable_for_all_builtin_layouts() {
    for id in all_builtin_ids() {
        let c = conv(&id);
        let rev = c.build_reverse();
        assert!(!rev.is_empty(), "{id}：反向表不应为空");
        // 全部内置布局恒为 414 个音节。留点余量给布局差异，掉到 400 以下就是枚举漏了键。
        assert!(
            rev.len() >= 400,
            "{id}：反向表只收录了 {} 个音节，疑似枚举漏键",
            rev.len()
        );
        for (syl, keys) in &rev.map {
            let decoded = c.convert_pair(keys[0], keys[1]);
            assert!(
                decoded.iter().any(|d| d == syl),
                "{id}：反向表说 {syl:?} 敲 {:?}，但该键对解出的是 {decoded:?}",
                std::str::from_utf8(keys).unwrap()
            );
        }
    }
}

/// 常用音节必须是首选（敲完即得，不用往下翻）。
/// 注释里显示的编码若要翻一格才对得上，用户会认为编码显示错了。
#[test]
fn reverse_common_syllables_are_first_choice() {
    // 覆盖各条取码路径：常规声母韵母、zh/ch/sh 独立键、ü 归一化、零声母、整体认读。
    const COMMON: &[&str] = &[
        "ni", "hao", "wo", "men", "zhong", "guo", "shi", "chi", "zhuang", "chuang", "shuang", "nv",
        "lv", "ju", "qu", "xu", "yu", "jue", "que", "xue", "yue", "jun", "qun", "xun", "a", "e",
        "o", "ai", "ei", "ou", "an", "en", "ang", "eng", "er", "ao", "ying", "yin",
    ];
    for id in all_builtin_ids() {
        let c = conv(&id);
        let rev = c.build_reverse();
        for syl in COMMON {
            let Some(keys) = rev.encode(syl) else {
                panic!("{id}：常用音节 {syl:?} 不在反向表里");
            };
            let decoded = c.convert_pair(keys[0], keys[1]);
            assert_eq!(
                decoded.first().map(String::as_str),
                Some(*syl),
                "{id}：{syl:?} 编码为 {:?}，但该键对的首选是 {:?}",
                std::str::from_utf8(&keys).unwrap(),
                decoded.first()
            );
        }
    }
}

/// 整串往返：`encode_all` 出来的击键串喂回 `convert`，必须还原成同一串全拼。
/// 连带验证「每音节恒 2 键」这个隐含前提，哪天有布局的零声母要敲 3 键，这里会红。
#[test]
fn reverse_roundtrip_through_convert() {
    const WORDS: &[(&[&str], &str)] = &[
        (&["ni", "hao"], "nihao"),
        (&["wo", "men"], "women"),
        (&["zhong", "guo"], "zhongguo"),
        (&["shu", "ru", "fa"], "shurufa"),
        (&["a", "yi"], "ayi"),
    ];
    for id in all_builtin_ids() {
        let c = conv(&id);
        let rev = c.build_reverse();
        for (syls, want_full) in WORDS {
            let keys = rev
                .encode_all(syls)
                .unwrap_or_else(|| panic!("{id}：{syls:?} 编码失败"));
            assert_eq!(
                keys.len(),
                syls.len() * 2,
                "{id}：{syls:?} 应得 {} 键，实得 {keys:?}",
                syls.len() * 2
            );
            assert_eq!(
                c.convert(&keys).full_pinyin(),
                *want_full,
                "{id}：{syls:?} → {keys:?} → 回解不符"
            );
        }
    }
}

/// 建表必须无视模糊音开关。
///
/// `chua` 在小鹤下的真值键对是 `ix`。开着 ch↔c 模糊音时 `cx` 的模糊对偶 "chua" 合法，成了 `cx` 的
/// 首选；枚举序 `c` 在 `i` 之前，建表若跟随模糊开关，"chua" 就会被登记成 `cx`。
/// 模糊音是输入时的宽容，不是这个字的打法。
#[test]
fn reverse_ignores_fuzzy_initials() {
    let plain = conv("xiaohe").build_reverse();

    let mut fuzzy_conv = conv("xiaohe");
    fuzzy_conv.set_fuzzy(&all_fuzzy());
    let fuzzy = fuzzy_conv.build_reverse();

    for syl in [
        "chua", "shua", "zhuang", "chuang", "shuang", "zhi", "chi", "shi",
    ] {
        assert_eq!(
            plain.encode(syl),
            fuzzy.encode(syl),
            "{syl:?}：模糊音开关不应影响反向表"
        );
    }
    // 钉死具体值，免得两边一起错还互相印证。
    assert_eq!(plain.encode("chua"), Some(*b"ix"), "小鹤 chua 应为 ix");
    assert_eq!(plain.encode("shua"), Some(*b"ux"), "小鹤 shua 应为 ux");
    assert_eq!(plain.encode("zhuang"), Some(*b"vl"), "小鹤 zhuang 应为 vl");
}

/// 三个零声母流派各自按自己的规矩出码：小鹤=首字母引导(`aa`)、微软=O 引导(`oa`)、
/// 首道=显式 `zero_pairs`(`aa`)。同时守着 `convert_pair` 里「单韵母重复键只对把该键配成零声母
/// 引导键的方案生效」那道闸：无条件放行的话微软的 `a` 会变成 `aa`。
#[test]
fn reverse_zero_initial_schools() {
    assert_eq!(
        conv("xiaohe").build_reverse().encode("a"),
        Some(*b"aa"),
        "小鹤：首字母引导"
    );
    assert_eq!(
        conv("mspy").build_reverse().encode("a"),
        Some(*b"oa"),
        "微软：O 引导"
    );
    assert_eq!(
        conv("shoudao").build_reverse().encode("a"),
        Some(*b"aa"),
        "首道：显式 zero_pairs"
    );
}

/// 查不到的音节返回 `None`；整串里任一音节查不到即整串 `None`。
/// 半截击键串是错的答案而不是不完整的答案。
#[test]
fn reverse_rejects_unknown_syllables() {
    let rev = conv("xiaohe").build_reverse();
    assert_eq!(rev.encode("zzz"), None);
    assert_eq!(rev.encode(""), None);
    assert_eq!(rev.encode_all(&[]), None, "空串没有编码可言");
    assert_eq!(
        rev.encode_all(&["ni", "zzz"]),
        None,
        "一个音节查不到，整串就不该给答案"
    );
    assert!(rev.encode_all(&["ni", "hao"]).is_some());
}

/// 建表第二遍（给只能作次选出现的音节兜底）的受益者：`lo`（全部布局）与手道的 `dia`。
///
/// `o` 键在全部内置布局下都配 `["uo", "o"]`，`l` + `o` 解出 `["luo", "lo"]`，「咯」只能从第二遍
/// 拿到同一个键对；手道 `k` 键配 `["en", "ia"]`，`d` + `k` 解出 `["den", "dia"]`。
/// 没有这条，删掉第二遍扫描其余断言照常全绿（反向表只是从 414 个音节掉到 413）。
#[test]
fn reverse_second_pass_rescues_secondary_syllables() {
    for id in all_builtin_ids() {
        let c = conv(&id);
        assert_eq!(
            c.convert_pair(b'l', b'o'),
            vec!["luo".to_string(), "lo".to_string()],
            "{id}：`lo` 键对应解出两个音节，luo 在前"
        );
        assert_eq!(
            c.build_reverse().encode("lo"),
            Some(*b"lo"),
            "{id}：次选音节 lo 也要能反查到键对"
        );
    }
    let shoudao = conv("shoudao");
    assert_eq!(
        shoudao.convert_pair(b'd', b'k'),
        vec!["den".to_string(), "dia".to_string()]
    );
    assert_eq!(shoudao.build_reverse().encode("dia"), Some(*b"dk"));
}

/// 两遍扫描的顺序保障：同一个音节既是 A 键对的次选、又是 B 键对的首选时，反向表必须给 B，
/// 否则用户照着注释显示的编码敲下去，出来的是另一个字。
///
/// 现有内置布局触发不了这个差异（`lo`/`dia` 在别处不是谁的首选），故用构造布局造出来，
/// 否则「先扫首选、再扫次选」这个顺序无法被任何测试区分。
#[test]
fn reverse_prefers_first_choice_pair_over_secondary() {
    // `a` 键配两个韵母（ang 在前、a 在后），`z` 键只配 `a`。于是 "ba" 是 `ba` 键对的次选
    // （首选 "bang"），却是 `bz` 键对的首选；而枚举序里 `ba` 在 `bz` 之前。
    const DUAL: &str = r#"
[meta]
id = "test_dual"
name = "测试夹具：同一音节既是次选又是首选"

[finals]
a = ["ang", "a"]
z = ["a"]
"#;
    let c = ShuangpinConverter::new(Layout::from_toml_str(DUAL).unwrap());
    assert_eq!(
        c.convert_pair(b'b', b'a'),
        vec!["bang".to_string(), "ba".to_string()],
        "夹具前提：`ba` 键对解出两个音节，ba 排第二"
    );
    assert_eq!(
        c.convert_pair(b'b', b'z'),
        vec!["ba".to_string()],
        "夹具前提：`bz` 键对只解出 ba，是首选"
    );

    let rev = c.build_reverse();
    assert_eq!(
        rev.encode("ba"),
        Some(*b"bz"),
        "ba 应取「一敲即得」的 bz，而不是要翻一格的 ba"
    );
    assert_eq!(rev.encode("bang"), Some(*b"ba"), "bang 本就是 ba 的首选");
}

/// 枚举必须按排序后的键走，否则反向表随哈希序漂移。
///
/// 每个内置布局都有若干音节被多个键对同时当作首选（如 ü 归一化造成的 `ju ← [ju, jv]`、
/// 微软/搜狗的 `[ju, jy]`、零声母 abc 的 `ou ← [ob, ou]`），取哪个完全由枚举顺序决定。
/// `HashMap` 的哈希序跨进程随机，不排序的话同一个字重启后显示的编码会变。
#[test]
fn reverse_table_is_deterministic_under_first_choice_conflicts() {
    for id in all_builtin_ids() {
        let rev = conv(&id).build_reverse();
        for syl in ["ju", "qu", "xu", "yu"] {
            let keys = rev
                .encode(syl)
                .unwrap_or_else(|| panic!("{id}：{syl} 不在反向表里"));
            let initial = syl.as_bytes()[0];
            assert_eq!(keys[0], initial, "{id}：{syl} 的首码应是声母键本身");
            // 排序后字典序小者胜：`u` < `v`（abc/佳佳/首道）、`u` < `y`（微软/搜狗）。
            // 这也正是各方案文档里写的标准打法。
            assert_eq!(
                keys[1], b'u',
                "{id}：{syl} 应取 {}u，而不是 v/y 键那条等价路径",
                initial as char
            );
        }
    }
}
