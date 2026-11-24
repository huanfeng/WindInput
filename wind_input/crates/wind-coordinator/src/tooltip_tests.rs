use super::*;
use wind_config::config::{LegacyTooltipFlags, tooltip_sections_from_legacy};
use wind_reverse::ReverseLookup;

// ───────────────────────── 夹具 ─────────────────────────

/// 夹具反查表。每个字的设定都对应旧实现的一条分支：
///
/// | 字 | 拆字 | 读音 | 用途 |
/// |---|---|---|---|
/// | 好 | 女子 [vbg] | hǎo/hào | 多音字、合并行 |
/// | 重 | 丿一日一土 [tgjf] | zhòng/chóng/tóng | 多音字，读音数截断 |
/// | 你 | —— | nǐ | 拆字库未收录：合并段里补在末尾 |
/// | 人 | 人 [] | rén | 有字根无编码：`[编码]` 可选段消失 |
/// | 㐀 | 丿一 [tgd] | —— | 无拼音有拆字：合并行不带 `\t` |
/// | 𠀀 | 一丨 [ghk] | hē | 扩展 B（代理对） |
/// | 龘 | —— | —— | 两表都没有 |
/// | 丂 | （空） [gnv] | kǎo | 有编码无字根：只有用户自备拆字库会出现（出厂库 0 条） |
fn fixture_reverse() -> ReverseLookup {
    // 每次调用独占一个目录：测试并行跑，共用目录会被别的用例的 remove_dir_all 删掉。
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("wind-tooltip-fixture-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let chaizi = dir.join("chaizi.txt");
    std::fs::write(
        &chaizi,
        "好\t女子\tvbg\n重\t丿一日一土\ttgjf\n人\t人\t\n㐀\t丿一\ttgd\n𠀀\t一丨\tghk\n丂\t\tgnv\n",
    )
    .unwrap();
    let pinyin = dir.join("pinyin_map.txt");
    std::fs::write(
        &pinyin,
        "U+597D: hǎo,hào  # 好\nU+91CD: zhòng,chóng,tóng  # 重\nU+4F60: nǐ  # 你\n\
             U+4EBA: rén  # 人\nU+20000: hē  # 𠀀\nU+4E02: kǎo  # 丂\n",
    )
    .unwrap();
    let rl = ReverseLookup::load(Some(&pinyin), Some(&chaizi));
    let _ = std::fs::remove_dir_all(&dir);
    rl
}

/// 夹具取数的字面量锚：对拍的新旧两侧共用 `readings_of` / `radicals_of` /
/// `chaizi_code_of`，它们错了两侧会一起错、对拍照样绿，故在典型字上钉死输出。
#[test]
fn fixture_lookups_are_pinned() {
    let rl = fixture_reverse();
    assert_eq!(
        rl.readings_of('重', 0, "/"),
        "zhòng/chóng/tóng",
        "多音字按表序"
    );
    assert_eq!(rl.readings_of('重', 2, "/"), "zhòng/chóng");
    assert_eq!(rl.readings_of('你', 0, "/"), "nǐ", "单读音");
    assert_eq!(rl.readings_of('𠀀', 0, "/"), "hē", "扩展 B");
    assert_eq!(rl.readings_of('㐀', 0, "/"), "");
    assert_eq!(rl.radicals_of("好", ""), "女子");
    assert_eq!(rl.chaizi_code_of("好"), "vbg");
    assert_eq!(rl.radicals_of("𠀀", ""), "一丨");
    assert_eq!(rl.chaizi_code_of("𠀀"), "ghk");
    assert_eq!(rl.radicals_of("人", ""), "人");
    assert_eq!(rl.chaizi_code_of("人"), "", "有字根无编码");
    assert_eq!(rl.radicals_of("丂", ""), "", "有编码无字根");
    assert_eq!(rl.chaizi_code_of("丂"), "gnv");
    assert_eq!(rl.radicals_of("你", ""), "");
}

/// 生产同一份的逐字求值（去掉只有协调器才有的引擎类变量，对拍的段列表用不到它们）。
fn per_char(rl: &ReverseLookup) -> impl Fn(char, &str, Option<&str>) -> Option<VarValue> + '_ {
    move |c, name, arg| {
        char_var(name, arg, c, rl)
            .map(VarValue::from)
            .or_else(|| crate::comment::reverse_text_var(name, arg, &c.to_string(), rl))
    }
}

struct Cand<'a> {
    disp: &'a str,
    /// 完整原文；与 `disp` 不同即「显示被截断」。
    full: &'a str,
    word_code: Option<&'a str>,
    code_source: Option<&'a str>,
    debug: &'a str,
}

fn cand_eval<'a>(c: &'a Cand<'a>) -> impl Fn(&str, Option<&str>) -> Option<VarValue> + 'a {
    move |name, arg| {
        Some(VarValue::from(match name {
            "word_code" => c.word_code.unwrap_or_default().to_string(),
            "code_source" => c.code_source.unwrap_or_default().to_string(),
            "debug" => c.debug.to_string(),
            // 注释库只给英文词配一条，够测「非汉字候选靠注释库出气泡」。
            "dict" if c.full == "hello" => "问候".to_string(),
            "dict" => String::new(),
            // 两个库都收了 hello（与 `comments_of` 同形：每库一条、按挂载顺序）。
            "dict_all" if c.full == "hello" => {
                ["问候", "[IT]你好世界"].join(arg.unwrap_or(crate::comment::DICT_ALL_SEP))
            }
            "dict_all" => String::new(),
            _ => return None,
        }))
    }
}

/// 出厂的长度保护（200 字 / 40 列）+ 给定段列表：对拍按生产实际口径跑。
fn compile(sections: &[SectionConfig]) -> CompiledTooltip {
    CompiledTooltip::compile(&TooltipConfig {
        sections: sections.to_vec(),
        ..TooltipConfig::default()
    })
}

fn render_new(rl: &ReverseLookup, sections: &[SectionConfig], c: &Cand) -> String {
    compile(sections)
        .render(c.disp, c.full, &cand_eval(c), &per_char(rl))
        .doc
        .to_plain_text()
}

// ───────────────── 参照实现：段列表引入前的气泡 ─────────────────

/// 旧 `ReverseLookup::tooltip_for` + `merge_chaizi_pinyin` + 协调器追加调试段，逐行照搬
/// （只把私有表访问换成等价的公开方法）。对拍的基准，不要「顺手修」它。
fn legacy(rl: &ReverseLookup, f: LegacyTooltipFlags, c: &Cand) -> String {
    struct Sec {
        label: String,
        lines: Vec<String>,
    }
    let mut tooltip = (|| {
        if rl.is_empty() && c.word_code.is_none() {
            return String::new();
        }
        let chars: Vec<char> = c.disp.chars().filter(|c| (*c as u32) >= 0x3400).collect();
        if chars.is_empty() {
            return String::new();
        }
        let mut sections: Vec<Sec> = Vec::new();
        if f.code
            && let Some(code) = c.word_code.filter(|c| !c.is_empty())
        {
            let label = match c.code_source.filter(|s| !s.is_empty()) {
                Some(src) => format!("编码({src})"),
                None => "编码".to_string(),
            };
            sections.push(Sec {
                label,
                lines: vec![code.to_string()],
            });
        }
        if f.pinyin {
            let mut lines = Vec::new();
            for &ch in &chars {
                let all = rl.readings_of(ch, 0, "/");
                if all.is_empty() {
                    continue;
                }
                let len = all.split('/').count();
                let n = if !f.heteronyms {
                    1
                } else if f.max_readings > 0 {
                    f.max_readings.min(len)
                } else {
                    len
                };
                let shown = rl.readings_of(ch, n, "/");
                if !shown.is_empty() {
                    lines.push(format!("{ch}：{shown}"));
                }
            }
            if !lines.is_empty() {
                sections.push(Sec {
                    label: "拼音".into(),
                    lines,
                });
            }
        }
        if f.chaizi {
            let mut lines = Vec::new();
            for &ch in &chars {
                let s = ch.to_string();
                let rad = rl.radicals_of(&s, "");
                if rad.is_empty() {
                    continue;
                }
                let code = rl.chaizi_code_of(&s);
                lines.push(if code.is_empty() {
                    format!("{ch}：{rad}")
                } else {
                    format!("{ch}：{rad} [{code}]")
                });
            }
            if !lines.is_empty() {
                sections.push(Sec {
                    label: "拆字".into(),
                    lines,
                });
            }
        }
        // merge_chaizi_pinyin
        let ci = sections.iter().position(|s| s.label == "拆字");
        let pi = sections.iter().position(|s| s.label == "拼音");
        if let (Some(ci), Some(pi)) = (ci, pi) {
            let mut pin_map = std::collections::HashMap::new();
            let mut pin_full = std::collections::HashMap::new();
            let mut pin_order = Vec::new();
            for line in &sections[pi].lines {
                let head = line.chars().next().unwrap();
                pin_full.insert(head, line.clone());
                let reading = line
                    .find('：')
                    .map(|i| line[i + '：'.len_utf8()..].to_string())
                    .unwrap_or_else(|| line.clone());
                pin_map.insert(head, reading);
                pin_order.push(head);
            }
            let mut used = std::collections::HashSet::new();
            let mut merged = Vec::new();
            for cz in &sections[ci].lines {
                match cz.chars().next() {
                    Some(h) if pin_map.contains_key(&h) => {
                        used.insert(h);
                        merged.push(format!("{}\t{}", cz, pin_map[&h]));
                    }
                    _ => merged.push(cz.clone()),
                }
            }
            for r in &pin_order {
                if !used.contains(r) {
                    merged.push(pin_full[r].clone());
                }
            }
            let combined = Sec {
                label: "拆字 / 拼音".into(),
                lines: merged,
            };
            let mut combined = Some(combined);
            sections = sections
                .into_iter()
                .enumerate()
                .filter(|(i, _)| *i != pi)
                .map(|(i, s)| if i == ci { combined.take().unwrap() } else { s })
                .collect();
        }
        // format_sections（旧气泡的段全是 always_expand）
        let mut parts = Vec::new();
        for sec in sections {
            parts.push(format!("[{}]", sec.label));
            parts.extend(sec.lines);
        }
        parts.join("\n")
    })();
    if f.debug {
        if !tooltip.is_empty() {
            tooltip.push('\n');
        }
        tooltip.push_str(&format!("[调试]\n{}", c.debug));
    }
    tooltip
}

// ───────────────────────── 对拍 ─────────────────────────

fn all_flags() -> Vec<LegacyTooltipFlags> {
    let mut out = Vec::new();
    for bits in 0..32u32 {
        for max_readings in [0, 1, 2] {
            out.push(LegacyTooltipFlags {
                code: bits & 1 != 0,
                pinyin: bits & 2 != 0,
                heteronyms: bits & 4 != 0,
                chaizi: bits & 8 != 0,
                debug: bits & 16 != 0,
                max_readings,
            });
        }
    }
    out
}

const DEBUG: &str = "来源: 码表·五笔\n码 vbg · 权 100 · 序 0 · 用 0次";

fn fixtures() -> Vec<Cand<'static>> {
    let c = |disp, word_code, code_source| Cand {
        disp,
        full: disp,
        word_code,
        code_source,
        debug: DEBUG,
    };
    vec![
        c("好", Some("vbg"), None),
        c("好", Some("v/vb/vbg"), Some("五笔")),
        c("重要", Some("tgsv"), None),
        c("你好", Some("wqvb"), Some("五笔")), // 你无拆字 ⇒ 合并段里排到好之后
        c("你你好人", None, None),             // 重复字 + 有字根无编码
        c("好好", Some("vbvb"), None),
        c("人", None, None),
        c("㐀", None, None), // 无拼音有拆字
        c("𠀀", Some("ghk"), None),
        c("好𠀀你", None, Some("五笔")),
        c("龘", Some("xyz"), None), // 两表都没有，只剩编码
        c("丂", None, None),        // 有编码无字根
        c("好a人", Some("x"), None),
        Cand {
            full: "你好世界",
            ..c("你好…", None, None) // 截断：多出「完整原文」段，逐字段不含 …
        },
        c("abc", Some("abc"), None), // 纯非 CJK：P2 起也有气泡
        c("", None, None),
    ]
}

/// 新渲染的预期：旧输出，外加设计 §8.3 已确认的差异。
///
/// 1. 旧开关只有一边有内容时，旧实现不合并、保留那一边的标题（`[拼音]` / `[拆字]`），
///    而迁移出的合并段标题恒为「拆字 / 拼音」。内容行逐字节一致，差异只在这一行标题。
/// 2. 拆字库里「字根空、编码非空」的字（夹具「丂」）：旧实现按字根判存在、整行跳过；
///    新模板 `${chaizi}{ [${chaizi_code}]}` 表达不了「编码只跟着字根出现」，于是多出
///    ` [gnv]`。只有用户自备拆字库会出现（出厂库 0 条），且显示的信息更多而非更少。
///    这类用例不走「旧输出 + 改写」，直接逐字节写出新输出。
/// 3. （P2）非汉字候选不再整体没有气泡：编码段照常出（逐字段本就没有汉字可出）。
/// 4. （P2）候选显示被截断时，首段多出「完整原文」。
///
/// 3、4 同样直接写出新输出，不从旧输出改写；中文且未截断的夹具仍须与旧输出逐字节一致。
fn expected(old: &str, f: LegacyTooltipFlags, c: &Cand) -> String {
    let debug = f.debug.then(|| format!("[调试]\n{}", c.debug));
    let body = if !c.disp.chars().any(is_han) {
        let code = c.word_code.filter(|w| f.code && !w.is_empty()).map(|w| {
            match c.code_source.filter(|s| !s.is_empty()) {
                Some(src) => format!("[编码({src})]\n{w}"),
                None => format!("[编码]\n{w}"),
            }
        });
        [code, debug]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    } else if c.disp == "丂" && f.chaizi {
        let body = if f.pinyin {
            "[拆字 / 拼音]\n丂： [gnv]\tkǎo"
        } else {
            "[拆字]\n丂： [gnv]"
        };
        [Some(body.to_string()), debug]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    } else if f.chaizi && f.pinyin {
        old.replacen("[拼音]\n", "[拆字 / 拼音]\n", 1)
            .replacen("[拆字]\n", "[拆字 / 拼音]\n", 1)
    } else {
        old.to_string()
    };
    if c.disp != c.full {
        [format!("[完整原文]\n{}", c.full), body]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        body
    }
}

/// 验收核心（设计 §8.2）：全部旧开关组合 × 夹具，「旧开关 → 迁移出的段列表 →
/// 新渲染」与旧实现逐字节相同（唯一例外见 [`expected`]，每一例都单独计数并核对）。
#[test]
fn migrated_sections_reproduce_legacy_tooltip_byte_for_byte() {
    let rl = fixture_reverse();
    let mut checked = 0;
    let mut deviations = 0;
    for f in all_flags() {
        let sections = tooltip_sections_from_legacy(f);
        for c in fixtures() {
            let old = legacy(&rl, f, &c);
            let new = render_new(&rl, &sections, &c);
            let want = expected(&old, f, &c);
            assert_eq!(
                new, want,
                "\n开关 {f:?}\n候选 {:?} word_code={:?} code_source={:?}\n旧输出:\n{old}\n",
                c.disp, c.word_code, c.code_source
            );
            checked += 1;
            if want != old {
                deviations += 1;
            }
        }
    }
    assert_eq!(checked, 96 * fixtures().len());
    // 差异只出现在 [`expected`] 列出的几类用例上（标题、丂、非汉字、截断）；数目为零
    // 说明夹具退化了，对拍不再覆盖那些分支。
    assert!(deviations > 0);
}

/// 已确认的唯一标题差异（设计 §8.3）：拆字、拼音同开而某一边整段为空时，旧实现不合并、
/// 保留那一边的标题；合并段标题恒为「拆字 / 拼音」。
/// 两种退化各断言一次：标题按新口径，内容行仍须与旧实现逐字节一致。
#[test]
fn merged_section_degenerate_cases_differ_only_in_title() {
    let rl = fixture_reverse();
    let f = LegacyTooltipFlags {
        code: false,
        chaizi: true,
        ..Default::default()
    };
    let sections = tooltip_sections_from_legacy(f);
    for (disp, old_title, body) in [
        // 显示的字全都没有拆字（当前方案没配拆字库时的常态）→ 旧标题 [拼音]
        ("你", "[拼音]", "你：nǐ"),
        // 显示的字全都没有读音 → 旧标题 [拆字]，且行尾不留 `\t`
        ("㐀", "[拆字]", "㐀：丿一 [tgd]"),
    ] {
        let c = Cand {
            disp,
            full: disp,
            word_code: None,
            code_source: None,
            debug: "",
        };
        assert_eq!(legacy(&rl, f, &c), format!("{old_title}\n{body}"));
        assert_eq!(
            render_new(&rl, &sections, &c),
            format!("[拆字 / 拼音]\n{body}")
        );
    }
}

/// 行序专项（用户明确要求不变）：「你好」里「你」无拆字，旧实现先出「好」再补「你」。
#[test]
fn merged_section_keeps_legacy_row_order() {
    let rl = fixture_reverse();
    let f = LegacyTooltipFlags {
        chaizi: true,
        ..Default::default()
    };
    let c = Cand {
        disp: "你好人㐀",
        full: "你好人㐀",
        word_code: None,
        code_source: None,
        debug: "",
    };
    assert_eq!(
        render_new(&rl, &tooltip_sections_from_legacy(f), &c),
        "[拆字 / 拼音]\n好：女子 [vbg]\thǎo/hào\n人：人\trén\n㐀：丿一 [tgd]\n你：nǐ"
    );
}

// ───────────────────────── 新变量与段语义 ─────────────────────────

fn section(label: &str, each: &str, template: &str) -> SectionConfig {
    SectionConfig {
        enabled: true,
        label: label.into(),
        template: template.into(),
        each: each.into(),
        promote: String::new(),
        inline: false,
    }
}

fn cand(disp: &str) -> Cand<'_> {
    Cand {
        disp,
        full: disp,
        word_code: Some("vbg"),
        code_source: Some("五笔"),
        debug: "来源: 拼音",
    }
}

#[test]
fn unicode_vars_cover_bmp_and_astral() {
    let rl = ReverseLookup::default();
    let s = [section("Unicode", "char", "${char}：${unicode}")];
    assert_eq!(
        render_new(&rl, &s, &cand("好 𠀀")),
        "[Unicode]\n好：U+597D\n𠀀：U+20000",
        "非 BMP 照写五位；空白字符不出行"
    );
    let all = [
        section("码位", "", "${unicode_all}"),
        section("", "", "${unicode_all:,}"),
    ];
    assert_eq!(
        render_new(&rl, &all, &cand("好𠀀")),
        "[码位]\nU+597D U+20000\nU+597D,U+20000"
    );
}

#[test]
fn readings_arg_limits_count_and_bad_arg_means_all() {
    let rl = fixture_reverse();
    let one = |t: &str| render_new(&rl, &[section("", "han", t)], &cand("重"));
    assert_eq!(one("${readings}"), "zhòng/chóng/tóng");
    assert_eq!(one("${readings:2}"), "zhòng/chóng");
    assert_eq!(one("${readings:x}"), "zhòng/chóng/tóng");
}

#[test]
fn word_code_and_code_source_feed_label_and_content() {
    let rl = ReverseLookup::default();
    let s = [section("编码{(${code_source})}", "", "${word_code}")];
    assert_eq!(render_new(&rl, &s, &cand("好")), "[编码(五笔)]\nvbg");
    let direct = Cand {
        code_source: None,
        ..cand("好")
    };
    assert_eq!(
        render_new(&rl, &s, &direct),
        "[编码]\nvbg",
        "段名的字面文字不随变量全空而消失"
    );
}

/// `${char}` 不计入「有值」：一行里除它以外全空即丢行；整段无行即整段不显示。
#[test]
fn char_alone_does_not_keep_a_row_or_section() {
    let rl = fixture_reverse();
    let s = [section("拼音", "han", "${char}：${readings}")];
    assert_eq!(render_new(&rl, &s, &cand("龘好")), "[拼音]\n好：hǎo/hào");
    assert_eq!(render_new(&rl, &s, &cand("龘")), "");
}

/// P2 起非汉字候选与汉字候选同一口径：有任一非空段就有气泡。多行变量天然拆成多行。
#[test]
fn non_cjk_candidate_gets_every_non_empty_section() {
    let rl = ReverseLookup::default();
    let s = [
        section("编码", "", "${word_code}"),
        section("Unicode", "char", "${char}：${unicode}"),
        section("调试", "", "${debug}"),
    ];
    let c = Cand {
        debug: "来源: 英文\n码 abc",
        ..cand("ab")
    };
    assert_eq!(
        render_new(&rl, &s, &c),
        "[编码]\nvbg\n[Unicode]\na：U+0061\nb：U+0062\n[调试]\n来源: 英文\n码 abc"
    );
}

/// 英文候选配了注释库变量就能出气泡（旧口径下非汉字一律没有气泡）。
#[test]
fn english_candidate_with_dict_gets_a_tooltip() {
    let rl = ReverseLookup::default();
    let s = [section("释义", "", "${dict}")];
    let c = Cand {
        word_code: None,
        ..cand("hello")
    };
    assert_eq!(render_new(&rl, &s, &c), "[释义]\n问候");
    let other = Cand {
        word_code: None,
        ..cand("world")
    };
    assert_eq!(render_new(&rl, &s, &other), "", "查不到就没有气泡");
}

/// 出厂「释义」段 `${dict_all:\n}`：分隔符是真换行，各库释义各占一行；查不到整段不显示。
#[test]
fn dict_all_section_splits_into_rows() {
    let rl = ReverseLookup::default();
    let d = TooltipConfig::default();
    let s: Vec<SectionConfig> = d
        .sections
        .iter()
        .filter(|s| s.label == "释义")
        .cloned()
        .collect();
    assert_eq!(s.len(), 1, "出厂段列表应含「释义」段");
    assert_eq!(s[0].template, "${dict_all:\n}");
    let c = Cand {
        word_code: None,
        ..cand("hello")
    };
    assert_eq!(render_new(&rl, &s, &c), "[释义]\n问候\n[IT]你好世界");
    let other = Cand {
        word_code: None,
        ..cand("world")
    };
    assert_eq!(render_new(&rl, &s, &other), "");
}

// ───────────────────────── P2：完整原文、截断、折行 ─────────────────────────

fn truncated<'a>(disp: &'a str, full: &'a str) -> Cand<'a> {
    Cand {
        full,
        word_code: None,
        code_source: None,
        ..cand(disp)
    }
}

/// 出厂段列表：未截断时没有完整原文段，截断时置首出现、原始行就是完整原文。
#[test]
fn full_text_section_appears_only_when_truncated() {
    let rl = fixture_reverse();
    let t = CompiledTooltip::compile(&TooltipConfig::default());
    let plain = cand("你好");
    let r = t.render(plain.disp, plain.full, &cand_eval(&plain), &per_char(&rl));
    assert_eq!(
        r.doc.to_plain_text(),
        "[编码(五笔)]\nvbg\n[拼音]\n你：nǐ\n好：hǎo/hào"
    );

    let c = truncated("你好…", "你好世界");
    let r = t.render(c.disp, c.full, &cand_eval(&c), &per_char(&rl));
    assert_eq!(
        r.doc.to_plain_text(),
        "[完整原文]\n你好世界\n[拼音]\n你：nǐ\n好：hǎo/hào",
        "逐字段只遍历显示出来的字，… 不出行"
    );
    assert_eq!(r.raw[0], ["你好世界"]);
}

/// 截断标记不是候选的字：逐字段（含 each=char）不为它出行，`${unicode_all}` 也不含它。
#[test]
fn truncation_mark_is_not_a_character_of_the_candidate() {
    let rl = ReverseLookup::default();
    let s = [
        section("Unicode", "char", "${char}：${unicode}"),
        section("", "", "${unicode_all}"),
    ];
    assert_eq!(
        render_new(&rl, &s, &truncated("好人…", "好人们")),
        "[Unicode]\n好：U+597D\n人：U+4EBA\nU+597D U+4EBA"
    );
    // 未截断时原文里自带的 … 是真字符，照常出行。
    assert_eq!(
        render_new(&rl, &s[..1], &cand("好…")),
        "[Unicode]\n好：U+597D\n…：U+2026"
    );
}

fn limited(max_chars: usize, wrap_width: usize, sections: &[SectionConfig]) -> CompiledTooltip {
    CompiledTooltip::compile(&TooltipConfig {
        max_chars,
        wrap_width,
        sections: sections.to_vec(),
        ..TooltipConfig::default()
    })
}

fn render_limited(t: &CompiledTooltip, c: &Cand) -> RenderedTooltip {
    t.render(
        c.disp,
        c.full,
        &cand_eval(c),
        &per_char(&ReverseLookup::default()),
    )
}

/// 超过 `max_chars` 的原始行显示时截断加 …，原始行保持完整。
#[test]
fn max_chars_truncates_display_only() {
    let t = limited(5, 0, &[section("完整原文", "", "${full_text}")]);
    let r = render_limited(&t, &truncated("一二…", "一二三四五六七"));
    assert_eq!(r.doc.to_plain_text(), "[完整原文]\n一二三四五…");
    assert_eq!(r.raw[0], ["一二三四五六七"]);
    let off = limited(0, 0, &[section("完整原文", "", "${full_text}")]);
    let r = render_limited(&off, &truncated("一二…", "一二三四五六七"));
    assert_eq!(
        r.doc.to_plain_text(),
        "[完整原文]\n一二三四五六七",
        "0 = 不限"
    );
}

/// 折行按显示列：汉字 2 列、ASCII 1 列；折出的显示行都指回同一条原始行。
#[test]
fn wrap_counts_display_columns_for_mixed_text() {
    let t = limited(0, 10, &[section("", "", "${full_text}")]);
    let r = render_limited(&t, &truncated("a…", "abc你好defg世界"));
    let lines: Vec<(&str, u16)> = r.doc.sections[0]
        .lines
        .iter()
        .map(|l| (l.text.as_str(), l.raw))
        .collect();
    assert_eq!(lines, [("abc你好def", 0), ("g世界", 0)]);
    // 宽度不足一个全角字时，那个字独占一行（不丢字、不死循环）。
    let narrow = limited(0, 1, &[section("", "", "${full_text}")]);
    let r = render_limited(&narrow, &truncated("a…", "你a"));
    assert_eq!(r.doc.to_plain_text(), "你\na");
}

/// 含 `\t` 的分列行不折：折开会把第二列甩到下一行行首。
#[test]
fn tab_separated_rows_are_not_wrapped() {
    let rl = fixture_reverse();
    let merged = tooltip_sections_from_legacy(LegacyTooltipFlags {
        code: false,
        chaizi: true,
        ..Default::default()
    });
    let t = CompiledTooltip::compile(&TooltipConfig {
        wrap_width: 4,
        sections: merged,
        ..TooltipConfig::default()
    });
    let c = cand("好");
    let r = t.render(c.disp, c.full, &cand_eval(&c), &per_char(&rl));
    assert_eq!(
        r.doc.to_plain_text(),
        "[拆字 / 拼音]\n好：女子 [vbg]\thǎo/hào"
    );
}

/// `raw` 下标：多行变量拆出的每条原始行各有下标，折行产生的显示行共用所属原始行的下标。
#[test]
fn raw_index_maps_display_lines_back_to_raw_lines() {
    let t = limited(0, 5, &[section("调试", "", "${debug}")]);
    let c = Cand {
        debug: "aaaaaaaaaaaa\nbb",
        ..cand("x")
    };
    let r = render_limited(&t, &c);
    let lines: Vec<(&str, u16)> = r.doc.sections[0]
        .lines
        .iter()
        .map(|l| (l.text.as_str(), l.raw))
        .collect();
    assert_eq!(lines, [("aaaaa", 0), ("aaaaa", 0), ("aa", 0), ("bb", 1)]);
    assert_eq!(r.raw, [vec!["aaaaaaaaaaaa".to_string(), "bb".to_string()]]);
}

fn texts(r: &RenderedTooltip, sec: usize) -> Vec<(&str, u16)> {
    r.doc.sections[sec]
        .lines
        .iter()
        .map(|l| (l.text.as_str(), l.raw))
        .collect()
}

/// 原始行保真：段内空行、行首缩进（空格 / `\t`）、行尾空白都原样保留，按 `\n` 连回去
/// 就是完整原文。显示行照旧跳过空行。
#[test]
fn raw_lines_round_trip_the_full_text() {
    let t = limited(0, 0, &[section("完整原文", "", "${full_text}")]);
    for full in [
        "第一段\n\n\t第二段 缩进\n  第三段 ",
        "\t首行就是制表符缩进",
        "  两个空格缩进\n\n\n隔两空行",
    ] {
        let r = render_limited(&t, &truncated("第…", full));
        assert_eq!(r.raw[0].join("\n"), full);
        assert!(
            r.doc.sections[0]
                .lines
                .iter()
                .all(|l| !l.text.trim().is_empty())
        );
    }
    // 首尾的空白行不算内容；整段全是空白即空段。
    let r = render_limited(&t, &truncated("第…", "\n  \n正文\n\t\n"));
    assert_eq!(r.raw[0], ["正文"]);
    assert!(
        render_limited(&t, &truncated("第…", " \n\t "))
            .doc
            .is_empty()
    );
}

/// 只有模板字面写了 `\t` 的分列段才免折；完整原文里自带的 `\t` 照常折。
#[test]
fn tab_from_variable_value_is_still_wrapped() {
    let t = limited(0, 6, &[section("", "", "${full_text}")]);
    let r = render_limited(&t, &truncated("a…", "abc\tdefghij"));
    assert_eq!(r.doc.to_plain_text(), "abc\tde\nfghij");
}

/// 截断与折行都按字素簇计：ZWJ 序列、组合字符不会被切开，emoji 记 2 列。
#[test]
fn truncation_and_wrap_respect_grapheme_clusters() {
    let family = "👨\u{200D}👩\u{200D}👧";
    let t = limited(1, 0, &[section("", "", "${full_text}")]);
    let r = render_limited(&t, &truncated("a…", &format!("{family}x")));
    assert_eq!(r.doc.to_plain_text(), format!("{family}…"));

    let t = limited(0, 4, &[section("", "", "${full_text}")]);
    let r = render_limited(&t, &truncated("a…", &format!("ab{family}cd")));
    assert_eq!(
        r.doc.to_plain_text(),
        format!("ab{family}\ncd"),
        "emoji 记 2 列"
    );
    let r = render_limited(
        &t,
        &truncated("a…", "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}"),
    );
    assert_eq!(
        r.doc.to_plain_text(),
        "e\u{301}e\u{301}e\u{301}e\u{301}\ne\u{301}",
        "组合字符随基字走"
    );
}

/// 连续 ASCII 优先在空格、`/`、`·` 之后断开；全是空白的显示行丢掉。
#[test]
fn wrap_prefers_ascii_break_points() {
    let t = limited(0, 10, &[section("", "", "${full_text}")]);
    let lines = |full: &str| {
        render_limited(&t, &truncated("a…", full))
            .doc
            .to_plain_text()
    };
    assert_eq!(lines("a/ab/abc/abcd/abcde"), "a/ab/abc/\nabcd/abcde");
    assert_eq!(lines("hello world foo"), "hello\nworld foo");
    assert_eq!(lines("abc·defghijk"), "abc·\ndefghijk");
    // 找不到断点才硬折。
    assert_eq!(lines("abcdefghijklm"), "abcdefghij\nklm");
    // 溢出点是汉字时不回退找 ASCII 断点。
    assert_eq!(lines("ab/cdefgh你好"), "ab/cdefgh\n你好");
    // 空格恰落在折点：不留下只有空白的行，也不把空格带到下一行行首。
    assert_eq!(lines("abcdefghij          k"), "abcdefghij\nk");
}

/// inline 按原始行数判：一条长内容折成多条显示行仍写成 `标题: …`，前缀宽度计入首行。
#[test]
fn inline_counts_raw_lines_and_prefix_width() {
    let mut s = section("码", "", "${full_text}");
    s.inline = true;
    let t = limited(0, 10, &[s]);
    let r = render_limited(&t, &truncated("a…", "abcdefghijkl"));
    assert!(r.doc.sections[0].inline);
    assert_eq!(texts(&r, 0), [("abcdef", 0), ("ghijkl", 0)]);
    assert_eq!(r.doc.to_plain_text(), "码: abcdef\nghijkl");
    let r = render_limited(&t, &truncated("a…", "ab\ncd"));
    assert!(!r.doc.sections[0].inline, "两条原始行就不 inline");
    assert_eq!(r.doc.to_plain_text(), "[码]\nab\ncd");
}

/// DirectWrite 认作断行的 `\r`、U+0085、U+2028、U+2029 在显示行里归一成换行（否则渲染
/// 多出的行会让命中换算错位）；原始行保留原字符。
#[test]
fn exotic_line_breaks_are_normalized_for_display_only() {
    let t = limited(0, 0, &[section("", "", "${full_text}")]);
    let full = "a\rb\u{85}c\u{2028}d\u{2029}e\r\nf";
    let r = render_limited(&t, &truncated("a…", full));
    assert_eq!(
        texts(&r, 0),
        [("a", 0), ("b", 0), ("c", 0), ("d", 0), ("e", 0), ("f", 1)]
    );
    assert_eq!(r.raw[0], ["a\rb\u{85}c\u{2028}d\u{2029}e\r", "f"]);
    assert_eq!(r.raw[0].join("\n"), full);
}

/// 截断标记与候选窗同源：拿 `truncate_display` 的真实产物喂进来，逐字段不为 … 出行。
#[test]
fn real_truncate_display_output_yields_no_mark_row() {
    let mut cfg = wind_config::Config::default().ui.candidate;
    cfg.max_chars = 2;
    let full = "好人们";
    let disp = cfg.truncate_display(full);
    let s = [section("Unicode", "char", "${char}：${unicode}")];
    let out = render_new(&ReverseLookup::default(), &s, &truncated(&disp, full));
    assert_eq!(out, "[Unicode]\n好：U+597D\n人：U+4EBA");
    assert!(!out.contains("U+2026"));
}

/// 「是否截断」判据（`disp != full`）跟着 `truncate_display` 的字素簇口径走：
/// 恰好 max_chars 簇（含 ZWJ 序列）不算截断，`${full_text}` 段不出；多一簇才出。
#[test]
fn full_text_follows_grapheme_truncation() {
    let mut cfg = wind_config::Config::default().ui.candidate;
    cfg.max_chars = 3;
    let s = [section("完整原文", "", "${full_text}")];
    let rl = ReverseLookup::default();
    let fit = "ab👨\u{200D}👩\u{200D}👧";
    let disp = cfg.truncate_display(fit);
    assert_eq!(
        render_new(&rl, &s, &truncated(&disp, fit)),
        "",
        "恰好 3 簇不截"
    );
    let over = format!("{fit}c");
    let disp = cfg.truncate_display(&over);
    assert_eq!(
        render_new(&rl, &s, &truncated(&disp, &over)),
        format!("[完整原文]\n{over}")
    );
}

/// 出厂口径（200 字 / 40 列）端到端：超长短语的完整原文先截到 200 字，再每 20 个汉字一行。
#[test]
fn factory_limits_apply_end_to_end() {
    let t = CompiledTooltip::compile(&TooltipConfig::default());
    let long: String = "长".repeat(250);
    let c = truncated("长…", &long);
    let r = render_limited(&t, &c);
    let first = &r.doc.sections[0];
    assert_eq!(first.title.as_ref().map(|t| t.as_str()), Some("完整原文"));
    assert_eq!(
        first.lines.len(),
        11,
        "200 字 ÷ 20 字/行 = 10 行，另加 … 落在第 11 行"
    );
    assert!(first.lines.iter().all(|l| l.raw == 0));
    assert_eq!(first.lines[10].text.as_str(), "…");
    assert_eq!(r.raw[0][0].chars().count(), 250, "原始行不截断");
}

#[test]
fn inline_applies_only_to_single_line_sections() {
    let rl = fixture_reverse();
    let mut s = section("Unicode", "char", "${unicode}");
    s.inline = true;
    assert_eq!(
        render_new(&rl, &[s.clone()], &cand("好")),
        "Unicode: U+597D"
    );
    assert_eq!(
        render_new(&rl, &[s], &cand("好人")),
        "[Unicode]\nU+597D\nU+4EBA"
    );
}

#[test]
fn promote_is_stable_and_generic() {
    let rl = fixture_reverse();
    let mut s = section("", "han", "${char}${readings}");
    s.promote = "chaizi".into();
    // 有拆字的（好、人）按原文序在前，没有的（你、龘→无行）随后，组内顺序不变。
    assert_eq!(
        render_new(&rl, &[s], &cand("你好龘人")),
        "好hǎo/hào\n人rén\n你nǐ"
    );
}

#[test]
fn disabled_sections_and_unknown_each() {
    let rl = ReverseLookup::default();
    let mut off = section("关", "", "${word_code}");
    off.enabled = false;
    let odd = section("怪", "chars", "${word_code}");
    assert_eq!(render_new(&rl, &[off, odd], &cand("好")), "[怪]\nvbg");
}

#[test]
fn references_sees_label_template_and_promote() {
    let mut s = section("编码{(${code_source})}", "han", "${a|debug}");
    s.promote = "chaizi".into();
    let t = compile(&[s]);
    assert!(t.references("code_source"));
    assert!(t.references("debug"));
    assert!(t.references("chaizi"));
    assert!(!t.references("word_code"));
    let mut off = section("", "", "${debug}");
    off.enabled = false;
    assert!(!compile(&[off]).references("debug"), "关着的段不算");
}

// ───────────────────── 分段样式跟着字走（§8.2）─────────────────────

/// `(文字, 角色)` 逐段。
fn roles(t: &StyledText) -> Vec<(&str, Option<&str>)> {
    t.spans()
        .iter()
        .map(|s| (&t.as_str()[s.start as usize..s.end as usize], s.role))
        .collect()
}

/// 折行按字节区间切带样式的行：每条显示行的区间都落在自己那一截上。
#[test]
fn wrap_carries_styles_to_each_display_line() {
    let t = limited(0, 6, &[section("", "", "${debug}：${word_code}")]);
    let c = Cand {
        debug: "aaaa bbbb",
        word_code: Some("cc"),
        ..cand("好")
    };
    let r = render_limited(&t, &c);
    let lines: Vec<&StyledText> = r.doc.sections[0].lines.iter().map(|l| &l.text).collect();
    assert_eq!(
        lines.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
        vec!["aaaa", "bbbb：", "cc"]
    );
    assert_eq!(roles(lines[0]), vec![("aaaa", Some("debug"))]);
    assert_eq!(
        roles(lines[1]),
        vec![("bbbb", Some("debug")), ("：", Some("literal"))]
    );
    assert_eq!(roles(lines[2]), vec![("cc", Some("word_code"))]);
}

/// `${unicode_all:,}` 的分隔符带标记、角色仍是 unicode_all。
#[test]
fn unicode_all_marks_its_separator() {
    let t = limited(0, 0, &[section("", "", "${unicode_all:,}")]);
    let r = render_limited(&t, &cand("好𠀀"));
    let line = &r.doc.sections[0].lines[0].text;
    assert_eq!(line.as_str(), "U+597D,U+20000");
    let got: Vec<(&str, Option<&str>, bool)> = line
        .spans()
        .iter()
        .map(|s| {
            (
                &line.as_str()[s.start as usize..s.end as usize],
                s.role,
                s.separator,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("U+597D", Some("unicode_all"), false),
            (",", Some("unicode_all"), true),
            ("U+20000", Some("unicode_all"), false),
        ]
    );
}

/// 单行截断的 `…` 继承被截处前一簇的样式。
#[test]
fn truncation_mark_inherits_previous_grapheme_style() {
    let t = limited(3, 0, &[section("", "", "${debug}")]);
    let c = Cand {
        debug: "abcdef",
        ..cand("好")
    };
    let r = render_limited(&t, &c);
    let line = &r.doc.sections[0].lines[0].text;
    assert_eq!(line.as_str(), "abc…");
    assert_eq!(roles(line), vec![("abc…", Some("debug"))]);
}

/// 断行符归一逐簇映射，样式不丢；原始行保留原字符。
#[test]
fn break_normalization_keeps_styles() {
    let t = limited(0, 0, &[section("", "", "${debug}")]);
    let c = Cand {
        debug: "ab\u{2028}cd",
        ..cand("好")
    };
    let r = render_limited(&t, &c);
    let texts: Vec<&str> = r.doc.sections[0]
        .lines
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(texts, vec!["ab", "cd"]);
    assert_eq!(
        roles(&r.doc.sections[0].lines[1].text),
        vec![("cd", Some("debug"))]
    );
    assert_eq!(r.raw[0][0], "ab\u{2028}cd");
}

/// 段名：字面 title、变量 in_title；逐字段 `${char}` 角色 char。
#[test]
fn titles_and_per_char_rows_carry_roles() {
    let rl = fixture_reverse();
    let t = limited(
        0,
        0,
        &[section(
            "编码{(${code_source})}",
            "han",
            "${char}：${readings}",
        )],
    );
    let c = cand("好");
    let r = t.render(c.disp, c.full, &cand_eval(&c), &per_char(&rl));
    let sec = &r.doc.sections[0];
    let title = sec.title.as_ref().unwrap();
    assert_eq!(
        roles(title),
        vec![
            ("编码(", Some("title")),
            ("五笔", Some("code_source")),
            (")", Some("title"))
        ]
    );
    assert!(title.spans().iter().all(|s| s.in_title));
    assert_eq!(
        roles(&sec.lines[0].text),
        vec![
            ("好", Some("char")),
            ("：", Some("literal")),
            ("hǎo/hào", Some("readings"))
        ]
    );
    // 纯文本（复制 / 上屏）不含任何样式信息之外的东西。
    assert_eq!(r.doc.to_plain_text(), "[编码(五笔)]\n好：hǎo/hào");
}
