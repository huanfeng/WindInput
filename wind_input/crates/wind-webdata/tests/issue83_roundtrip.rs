//! issue #83 的完整链路走查：从「候选里冒出看不懂的符号」到「关掉它们并带到另一台机器」。
//!
//! 单元测试各自钉住一环，这条把环连起来跑一遍——环与环之间的接缝（键的形态、默认判定的
//! 方向、导出格式的往返）正是本 issue 反复出问题的地方。
use serde_json::{Value, json};
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;
use wind_webdata::WebDataRpc;

fn coord(tag: &str) -> Arc<Coordinator> {
    let p = std::env::temp_dir().join(format!("wind_issue83_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    Coordinator::new_headless_with_store(
        Config::default(),
        None,
        Arc::new(Store::open(&p).unwrap()),
    )
}

fn rpc(c: &Coordinator, m: &str, p: Value) -> Value {
    c.web_data_rpc(m, &p)
        .unwrap_or_else(|e| panic!("{m} 失败: {e}"))
}

/// 用户在候选里看到的那几类东西，逐一登记为生僻，再导出、导入到另一台机器。
#[test]
fn hide_symbols_then_carry_to_another_machine() {
    let c = coord("a");

    // issue #83 里用户点名的几类，外加一个 emoji 序列（后续追问补上的）。
    let ball = "\u{26BD}\u{FE0F}"; // ⚽️ 变体选择符
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"; // 👨‍👩‍👧 ZWJ
    let targets = ["⿰", "ㄅ", "ㆠ", "あ", ball, family];

    for t in targets {
        // 每一条都要先能被识别为「一个字符」，再能写进去、且真的生效。
        let q = rpc(&c, "commonChars.query", json!({ "char": t }));
        assert_eq!(q["effective"], json!(true), "{t} 默认应放行");
        rpc(&c, "commonChars.set", json!({ "char": t, "common": false }));
        let q2 = rpc(&c, "commonChars.query", json!({ "char": t }));
        assert_eq!(q2["effective"], json!(false), "{t} 设了必须生效");
    }

    // 列表里认得出类型，且汉字块之外的才给整类操作。
    let items = rpc(&c, "commonChars.list", json!({}))["items"].clone();
    let row = |t: &str| {
        items
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["char"] == json!(t))
            .unwrap_or_else(|| panic!("{t} 不在列表里"))
            .clone()
    };
    assert_eq!(row("ㄅ")["block"], json!("注音符号"));
    assert_eq!(row("⿰")["block"], json!("表意文字描述符"));
    assert!(row("ㄅ")["blockBulkEditable"].as_bool().unwrap());

    // 导出：单字符走文本段，两条 emoji 序列走数组段。
    let text = rpc(&c, "commonChars.export", json!({}))["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("rare = "), "{text}");
    assert!(text.contains("rare_seq = ["), "多码位要单列: {text}");

    // 导到另一台机器：六条一条不少，且都真的生效。
    let c2 = coord("b");
    let o = rpc(&c2, "commonChars.import", json!({ "content": text }));
    assert_eq!(o["imported"], json!(targets.len()), "{o:?}");
    assert!(
        o["skipped"].as_array().unwrap().is_empty(),
        "{:?}",
        o["skipped"]
    );
    for t in targets {
        assert_eq!(
            rpc(&c2, "commonChars.query", json!({ "char": t }))["effective"],
            json!(false),
            "{t} 在新机器上也该判生僻"
        );
    }

    // ZWJ 组合的成员不受牵连——关的是那个组合，不是所有 👨。
    assert_eq!(
        rpc(&c2, "commonChars.query", json!({ "char": "\u{1F468}" }))["effective"],
        json!(true),
        "只登记了组合，单个成员不该跟着被判非常用"
    );
}
