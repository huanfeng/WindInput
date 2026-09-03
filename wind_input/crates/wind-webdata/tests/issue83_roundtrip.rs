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

    // ★★★ emoji 归**一类**：⚽️ 在「杂项符号」块、👨‍👩‍👧 在「表情符号」块，类型列上却是
    // 同一个「emoji」。用户点「整类设为生僻」时心里想的正是这个范围，而按块走只会处理掉
    // 其中一个块，另一个原样留在候选里——界面却报告已经处理完了。
    for e in [ball, family] {
        assert_eq!(row(e)["block"], json!("emoji"), "{e} 的类型");
        assert_eq!(
            row(e)["blockRange"],
            json!(""),
            "emoji 跨二十个块，给不出连续码位段"
        );
        assert!(row(e)["blockBulkEditable"].as_bool().unwrap());
    }

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

/// 边界：旧格式文件、空表、重复条目、以及「只登记了区域指示符的一半」。
///
/// 最后一条是导出分段最刁钻的输入——`🇨` 与 `🇳` 各自单码位，若都塞进文本串会合成
/// 国旗，读回来条数就对不上了。
#[test]
fn import_export_edge_cases() {
    // ① 旧格式（只有 common/rare 字符串段）必须照旧读得回来。
    let c = coord("old");
    let legacy = "wind_common_chars = 1\ncommon = \"槮鬱\"\nrare = \"⿰ㄅ\"\n";
    let o = rpc(&c, "commonChars.import", json!({ "content": legacy }));
    assert_eq!(o["imported"], json!(4), "旧格式四条: {o:?}");

    // ② 空表导出后再导入：不该报错，也不该凭空多出条目。
    let empty = coord("empty");
    let text = rpc(&empty, "commonChars.export", json!({}))["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!text.contains("_seq"), "空表不该有数组段: {text}");
    let o2 = rpc(&empty, "commonChars.import", json!({ "content": text }));
    assert_eq!(o2["imported"], json!(0));

    // ③ 区域指示符各登记一半：导出后必须仍是两条，不能合成一个国旗。
    let c3 = coord("ri");
    for t in ["\u{1F1E8}", "\u{1F1F3}"] {
        rpc(
            &c3,
            "commonChars.set",
            json!({ "char": t, "common": false }),
        );
    }
    let t3 = rpc(&c3, "commonChars.export", json!({}))["content"]
        .as_str()
        .unwrap()
        .to_string();
    let c4 = coord("ri2");
    let o3 = rpc(&c4, "commonChars.import", json!({ "content": t3 }));
    assert_eq!(
        o3["imported"],
        json!(2),
        "两个区域指示符必须各自留存: {o3:?}"
    );
    for t in ["\u{1F1E8}", "\u{1F1F3}"] {
        assert_eq!(
            rpc(&c4, "commonChars.query", json!({ "char": t }))["effective"],
            json!(false),
            "{t} 应仍是生僻"
        );
    }
    // 而完整国旗（两个拼一起）是**另一个**键，没被登记过，照旧放行。
    assert_eq!(
        rpc(
            &c4,
            "commonChars.query",
            json!({ "char": "\u{1F1E8}\u{1F1F3}" })
        )["effective"],
        json!(true),
        "整面国旗是另一个键，不该被两个半边牵连"
    );
}
