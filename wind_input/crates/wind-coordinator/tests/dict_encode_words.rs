//! `dict.encodeWords` RPC 契约测试（纯词列表导入的出码通道）。
//!
//! 设置端按批（约 1000 词）调用本 RPC，拿到码后拼成 TSV 走既有导入通道。因此
//! **codes 与 texts 同序等长**是这条链路的地基：调用方靠下标把码配回词，少一个元素
//! 就会让其后所有词错位配到别人的码上，静默写进用户词库。
//!
//! 另一条要守的是 `dict.encode` 与 `dict.encodeWords` 出码口径一致——两者共用
//! `encode_texts`，若哪天各自漂移，单条加词与批量导入会给同一个词两种码。
//!
//! 词典缺失时自动跳过（无数据 CI 环境）。判据是耗时 0.00s。

use serde_json::{Value, json};
use std::path::PathBuf;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_webdata::WebDataRpc;

fn data_dir() -> PathBuf {
    // 三级：crates/wind-coordinator → crates → wind_input → 仓库根（build_dev 在仓库根）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schema(id: &str) -> bool {
    let d = data_dir();
    d.join(format!("schemas/{id}.schema.toml")).exists()
        || d.join(format!("schemas/{id}.schema.yaml")).exists()
}

fn coord(schema: &str) -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.to_string()];
    cfg.schema.active = schema.to_string();
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

fn codes_of(c: &Coordinator, schema: &str, texts: &[&str]) -> Vec<String> {
    let v = c
        .web_data_rpc(
            "dict.encodeWords",
            &json!({ "schemaId": schema, "texts": texts }),
        )
        .expect("dict.encodeWords 应已注册");
    v.get("codes")
        .and_then(Value::as_array)
        .expect("返回必须含 codes 数组")
        .iter()
        .map(|x| x.as_str().unwrap_or("").to_string())
        .collect()
}

fn code_of(c: &Coordinator, schema: &str, text: &str) -> String {
    c.web_data_rpc("dict.encode", &json!({ "schemaId": schema, "text": text }))
        .expect("dict.encode")
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// 批量出码与单条 `dict.encode` 必须逐位一致——两条口径漂移会让同一个词在
/// 「加词对话框」和「纯词列表导入」下得到两种码。
#[test]
fn encode_words_agrees_with_encode_for_codetable() {
    if !has_schema("wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let c = coord("wubi86");
    let texts = ["中国", "计算机", "输入法", "深度学习"];

    let batch = codes_of(&c, "wubi86", &texts);
    assert_eq!(batch.len(), texts.len(), "codes 必须与 texts 同序等长");
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(batch[i], code_of(&c, "wubi86", t), "「{t}」两条口径不一致");
    }
    assert!(
        batch.iter().any(|s| !s.is_empty()),
        "真实 wubi86 词库下不该整批空码（说明 fixture 或取码链路坏了）"
    );
}

/// 拼音方案走的是另一条分派（词级消歧 + 反查表回退），同样要与单条一致。
#[test]
fn encode_words_agrees_with_encode_for_pinyin() {
    if !has_schema("pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let c = coord("pinyin");
    let texts = ["中国", "银行", "重复"];

    let batch = codes_of(&c, "pinyin", &texts);
    assert_eq!(batch.len(), texts.len());
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(batch[i], code_of(&c, "pinyin", t), "「{t}」两条口径不一致");
    }
    // 拼音回的是带空格的音节码（与 word_item 同形），落库侧再拆成扁平 key。
    assert!(
        batch.iter().any(|s| s.contains(' ')),
        "多音节词应回带空格的音节码，实际: {batch:?}"
    );
}

/// 出不了码的位置回**空串占位**而非跳过；非字符串元素同样占位。
/// 长度一旦缩水，调用方按下标配对就会全线错位。
#[test]
fn encode_words_keeps_length_for_every_input_shape() {
    if !has_schema("wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let c = coord("wubi86");
    // 中间两个分别是「码表取不到码的拉丁串」与「非字符串元素」。
    let v = c
        .web_data_rpc(
            "dict.encodeWords",
            &json!({ "schemaId": "wubi86", "texts": ["中国", "abc", 42, "输入法"] }),
        )
        .unwrap();
    let codes: Vec<&str> = v["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap_or(""))
        .collect();

    assert_eq!(codes.len(), 4, "失败项与非法元素都必须占位");
    assert!(codes[1].is_empty(), "取不到码应为空串");
    assert!(codes[2].is_empty(), "非字符串元素应为空串");
    assert_eq!(
        codes[3],
        code_of(&c, "wubi86", "输入法"),
        "失败项之后不能错位"
    );
}

/// 边界形状：空数组与缺 texts 都回空 codes，不报错、不 panic。
#[test]
fn encode_words_handles_empty_and_missing_texts() {
    if !has_schema("wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let c = coord("wubi86");
    for params in [
        json!({ "schemaId": "wubi86", "texts": [] }),
        json!({ "schemaId": "wubi86" }),
    ] {
        let v = c.web_data_rpc("dict.encodeWords", &params).unwrap();
        assert_eq!(
            v["codes"].as_array().map(Vec::len),
            Some(0),
            "参数: {params}"
        );
    }
    // schemaId 缺失是调用方错误，应报错而非静默出空码。
    assert!(c.web_data_rpc("dict.encodeWords", &json!({})).is_err());
}

/// 单字出码：加词界面最常见的一类输入（给某个字补一条编码），两类引擎都必须出得来。
///
/// 码表这一侧曾整体失效：`dict.encode` 对非拼音方案走 `encode_words` → `calc_word_code`，
/// 而后者是**词组**取码（按方案 `[[encoder.rules]]` 的公式从各字全码组装），开头就有
/// `if chars.len() < 2 { return Err(TooShort) }`——单字压根不进公式。可单字全码本就在
/// `single_char_full_codes` 里躺着，`encode_words` 上一行才刚取过。
#[test]
fn single_char_encodes_for_both_engine_kinds() {
    // 逐条断言会在第一个引擎上就停住，看不到另一条的实况；先收全再一起报。
    let mut empty = Vec::new();
    let mut mismatch = Vec::new();
    for (schema, ch) in [("wubi86", "工"), ("pinyin", "你")] {
        if !has_schema(schema) {
            continue;
        }
        let c = coord(schema);
        let code = code_of(&c, schema, ch);
        if code.is_empty() {
            empty.push(format!("{schema}/{ch}"));
        }
        // 与批量通道同口径（同 `encode_texts`），两条路不得对同一个字给两种答案。
        let batch = codes_of(&c, schema, &[ch]);
        if batch != vec![code.clone()] {
            mismatch.push(format!("{schema}/{ch}: 单条={code:?} 批量={batch:?}"));
        }
    }
    assert!(empty.is_empty(), "这些单字出不了码: {empty:?}");
    assert!(mismatch.is_empty(), "单条与批量出码不一致: {mismatch:?}");
}
