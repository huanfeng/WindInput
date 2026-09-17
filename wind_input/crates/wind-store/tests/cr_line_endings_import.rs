//! 导入端对行尾不挑食：同一份内容，无论行尾是 `\n` / `\r\n` / 孤立 `\r`，
//! 每个解析入口都必须给出完全相同的结果。
//!
//! **为什么专门立一组测试**：全仓按行解析走的是 `str::lines()` 语义（只认 `\n`、
//! 顺带剥掉紧邻其前的 `\r`），孤立 `\r` 因此不算换行，整份文件被当成一行。
//! 2026-09-17 实测的三种症状没有一种会提到行尾，最坏的一种还是**静默成功**：
//! TSV 提示「导入成功、只进 1 条」、`skipped` 仍是 0。修法见
//! `text_source::normalize_import_text`。
//!
//! 末尾的 `every_text_entry_point_normalizes` 是守卫：新增一个吃全文的 `pub fn`
//! 却忘了归一化，它会红。
//!
//! ⚠️ **样本一律在代码里构造，不要改用仓库里的 fixture 文件**：git 的 `core.autocrlf`
//! 会在 checkout 时按平台改写文件行尾，同一份 fixture 在 Windows 和 Linux 上行尾不同，
//! 这组测试就会变成「测 git 配置」而不是测解析器。

use wind_store::import_formats::{
    CodePolicy, DictFormat, detect_dict_format, parse_words_auto, parse_words_rime, parse_words_tsv,
};
use wind_store::phrase_text::parse_phrase_text;
use wind_store::wdict::{
    DictWdict, FreqIo, PhraseIo, ShadowActionIo, WordIo, export_dict_sections,
    export_phrases_wdict, parse_freq_wdict, parse_phrases_wdict, parse_shadow_wdict,
    parse_temp_words_wdict, parse_words_wdict, read_header_field, sections_present,
};

/// 把 LF 文本改写成 CRLF 版与孤立 CR 版。
///
/// 字段内的换行在 wdict 里是字面的 `\` + `n` 两个字符（`escape_text_field` 转过），
/// 不是真换行，所以这里的整体替换只会动到行尾。
fn variants(lf: &str) -> [(&'static str, String); 3] {
    [
        ("LF", lf.to_string()),
        ("CRLF", lf.replace('\n', "\r\n")),
        ("CR", lf.replace('\n', "\r")),
    ]
}

/// 三种行尾下 `f` 的结果必须与 LF 版逐字节一致。
fn same_across_endings<T: std::fmt::Debug>(what: &str, lf: &str, f: impl Fn(&str) -> T) {
    let [(_, a), (crlf_tag, crlf), (cr_tag, cr)] = variants(lf);
    let want = format!("{:?}", f(&a));
    assert_eq!(
        format!("{:?}", f(&crlf)),
        want,
        "{what}: {crlf_tag} 结果与 LF 不同"
    );
    assert_eq!(
        format!("{:?}", f(&cr)),
        want,
        "{what}: {cr_tag} 结果与 LF 不同"
    );
}

fn word(code: &str, text: &str, weight: i32) -> WordIo {
    WordIo {
        code: code.into(),
        text: text.into(),
        weight,
        count: 0,
        boundary: None,
    }
}

const TSV: &str = "# 注释\nni\t你\t100\nhao\t好\t50\nnihao\t你好\t30\n";
const RIME: &str =
    "# Rime\n---\nname: t\nversion: \"1\"\n...\n\n你好\tni hao\t100\n世界\tshi jie\t50\n";
const PHRASE: &str = "wind:p1 我的直通车\nkx (＾▽＾)\nzw 早上好呀\n";

fn dict_sample() -> String {
    let d = DictWdict {
        words: Some(vec![word("ni", "你", 100), word("hao", "好", 50)]),
        temp_words: Some(vec![word("ts", "临时", 10)]),
        freq: Some(vec![FreqIo {
            code: "ni".into(),
            text: "你".into(),
            count: 7,
            last_used: 1_700_000_000,
        }]),
        shadow: Some(vec![ShadowActionIo {
            action: "pin".into(),
            code: "ni".into(),
            word: "你".into(),
            position: 1,
            cand_id: None,
        }]),
    };
    export_dict_sections(&d, "2026-09-17", "pinyin", "pinyin")
}

// ---- import_formats 的 4 个入口 ----

#[test]
fn detect_dict_format_ignores_line_endings() {
    same_across_endings("detect(TSV)", TSV, detect_dict_format);
    same_across_endings("detect(Rime)", RIME, detect_dict_format);
    same_across_endings("detect(WindDict)", &dict_sample(), detect_dict_format);
}

#[test]
fn parse_words_auto_ignores_line_endings() {
    for (what, s) in [("TSV", TSV), ("Rime", RIME)] {
        same_across_endings(what, s, |t| parse_words_auto(t, CodePolicy::default()));
    }
    same_across_endings("WindDict", &dict_sample(), |t| {
        parse_words_auto(t, CodePolicy::default())
    });
}

#[test]
fn parse_words_rime_ignores_line_endings() {
    same_across_endings("rime", RIME, |t| parse_words_rime(t, CodePolicy::default()));
}

#[test]
fn parse_words_tsv_ignores_line_endings() {
    same_across_endings("tsv", TSV, |t| parse_words_tsv(t, CodePolicy::default()));
}

// ---- wdict 的 7 个入口 ----

#[test]
fn wdict_entry_points_ignore_line_endings() {
    let s = dict_sample();
    same_across_endings("words", &s, parse_words_wdict);
    same_across_endings("temp_words", &s, parse_temp_words_wdict);
    same_across_endings("freq", &s, parse_freq_wdict);
    same_across_endings("shadow", &s, parse_shadow_wdict);
    same_across_endings("sections_present", &s, sections_present);
    same_across_endings("read_header_field", &s, |t| {
        read_header_field(t, "schema_id")
    });

    let p = export_phrases_wdict(
        &[PhraseIo {
            code: "kx".into(),
            text: "(＾▽＾)".into(),
            weight: 1,
            position: 0,
            enabled: true,
        }],
        "2026-09-17",
    );
    same_across_endings("phrases", &p, parse_phrases_wdict);
}

// ---- phrase_text ----

#[test]
fn parse_phrase_text_ignores_line_endings() {
    same_across_endings("phrase_text", PHRASE, parse_phrase_text);
}

// ---- 症状回归：这些是修之前 CR 文件的真实表现，不许回去 ----

#[test]
fn cr_tsv_no_longer_silently_imports_one_row() {
    let cr = TSV.replace('\n', "\r");
    let (fmt, rows, skipped) = parse_words_auto(&cr, CodePolicy::default()).expect("应能解析");
    assert_eq!(fmt, DictFormat::Tsv);
    // 修之前：rows=1、skipped=0、不报错 —— 界面显示「导入成功」，实际只进一条
    assert_eq!(rows.len(), 3, "CR 行尾的 TSV 应完整导入 3 条");
    assert_eq!(skipped, 0);
    assert_eq!(rows[2].text, "你好");
    assert_eq!(rows[0].weight, 100, "权重列不该被吞掉");
}

#[test]
fn cr_rime_is_no_longer_misdetected_as_tsv() {
    let cr = RIME.replace('\n', "\r");
    // 修之前：全文一行必含 TAB → has_tsv_line 命中 → 误判 Tsv → 0 条
    assert_eq!(detect_dict_format(&cr), DictFormat::Rime);
    let (_, rows, _) = parse_words_auto(&cr, CodePolicy::default()).expect("应能解析");
    assert_eq!(rows.len(), 2);
}

#[test]
fn cr_wdict_no_longer_blames_the_version() {
    let cr = dict_sample().replace('\n', "\r");
    // 修之前：报「不支持的 WindDict 版本（需 version: 1）」，把人引向去改 version
    let (rows, _) = parse_words_wdict(&cr).expect("不该再报版本错");
    assert_eq!(rows.len(), 2);
}

// ---- 守卫 ----

/// 每个吃「全文」的 `pub fn`（首参 `text: &str`）都必须先归一化。
///
/// 靠人记住给新入口加一行是靠不住的——本条改动一次就接了 13 个入口，而其中
/// `is_phrase_text` 正是这条守卫第一次跑就替我抓出来的。
///
/// 扫的是整个 `src/`（而不是写死几个文件），这样新开的模块里的入口也跑不掉。
#[test]
fn every_text_entry_point_normalizes() {
    /// 首参叫 `text` 但并不按行解析的，归一化对它们没有意义（甚至会改结果）。
    const NOT_LINE_BASED: &[&str] = &[
        // 字符计数：归一化会把 \r\n 折成一个字符，统计值就变了
        "classify_chars",
        "classify_chars_full",
        // 归一化函数自己
        "normalize_import_text",
    ];

    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<_> = std::fs::read_dir(&src_dir)
        .expect("读 src/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "src/ 下没扫到 .rs，守卫会假通过");

    let mut checked = Vec::new();
    let mut missing = Vec::new();
    for path in &files {
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(path).expect("读源文件");
        // 逐个 `pub fn`，签名可能跨行，所以取到函数体的第一个 `{` 为止
        for (pos, _) in src.match_indices("\npub fn ") {
            let rest = &src[pos + 1..];
            let Some(brace) = rest.find(" {\n") else {
                continue;
            };
            let sig = &rest[..brace];
            if !sig.contains("text: &str") {
                continue;
            }
            let name = sig
                .trim_start_matches("pub fn ")
                .split(['(', '<'])
                .next()
                .unwrap_or(sig)
                .to_string();
            if NOT_LINE_BASED.contains(&name.as_str()) {
                continue;
            }
            checked.push(format!("{file}::{name}"));
            // 函数体开头附近应当出现归一化调用
            let body_head = &rest[brace..rest.len().min(brace + 220)];
            if !body_head.contains("normalize_import_text") {
                missing.push(format!("{file}::{name}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "这些吃全文的入口没有归一化行尾，孤立 \\r 会让整份文件算成一行：{missing:#?}\n\
         修法：函数体第一行加\n  \
         let normalized = normalize_import_text(text);\n  \
         let text = normalized.as_ref();\n\
         确实不按行解析的，加进 NOT_LINE_BASED 并写明理由。"
    );
    assert_eq!(
        checked.len(),
        13,
        "入口数量变了（原 13 个）。新增的已经被上面验过归一化，这里只是让\
         「入口集合变了」这件事本身被看见——确认无误后更新数字。当前：{checked:#?}"
    );
}
