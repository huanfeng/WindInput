//! 数据域 RPC 契约测试：真实 Coordinator + 临时 redb store，断言 web_data_rpc 输出形状
//! 与 WindInputSetting 的 mock.ts / models.ts 一致。
use super::*;
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

// 拼音准入契约的处置表测试随 `dispose_contract_rows` 搬到了
// `wind_coordinator::dict_transfer_api`（contract_disposition_table_covers_both_fill_choices）。

/// 导入导出的**文件路径通道**：`dict.export` 直接写盘、`dict.*Import` 直接读盘。
///
/// 走 `content` 时整库要穿过一帧 JSON-RPC，设置端读帧上限 16MB，几十万条必撞；传路径后
/// payload 是 O(1)。必须真做一次往返：`path` 被忽略、退回读 `content` 是最可能的回归，
/// 故 import 一侧只传 path 不传 content，让那种退化立刻现形。
#[test]
fn dict_io_round_trips_through_file_path() {
    let c = coord("dictpath");
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "wb", "code": "a", "text": "工", "weight": 100 }),
    )
    .unwrap();

    let out = std::env::temp_dir().join("wind_webdata_dictpath_export.wdict.yaml");
    let _ = std::fs::remove_file(&out);
    let path = out.to_string_lossy().to_string();

    // 导出：回的是落点与字节数，**不再回全文**
    let exp = c
        .web_data_rpc("dict.export", &json!({ "schemaId": "wb", "path": path }))
        .unwrap();
    assert!(
        exp.get("content").is_none(),
        "带 path 时不该再回全文：{exp}"
    );
    assert_eq!(
        exp.get("path").and_then(|v| v.as_str()),
        Some(path.as_str())
    );
    let written = std::fs::read_to_string(&out).expect("core 应已把文件写出来");
    assert!(
        written.contains("--- !words"),
        "写出来的得是 wdict：{written}"
    );
    assert_eq!(
        exp.get("bytes").and_then(|v| v.as_u64()),
        Some(written.len() as u64),
        "回报的字节数要与落盘一致"
    );

    // 预览 + 导入：只给路径
    let prev = c
        .web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "wb2", "path": path }),
        )
        .unwrap();
    assert_eq!(
        sec(&prev, "userWords")
            .get("willAdd")
            .and_then(|v| v.as_u64()),
        Some(1),
        "★ 预览必须从文件里读到内容，而不是把 path 当没看见"
    );
    let imported = c
        .web_data_rpc("dict.import", &json!({ "schemaId": "wb2", "path": path }))
        .unwrap();
    assert_eq!(
        sec(&imported, "userWords")
            .get("added")
            .and_then(|v| v.as_u64()),
        Some(1)
    );

    // 反向对照：路径不存在要**报错**，不能静默当成空文件导入 0 条。
    let bad = c.web_data_rpc(
        "dict.import",
        &json!({ "schemaId": "wb2", "path": "Z:\\no\\such\\file.yaml" }),
    );
    assert!(
        bad.is_err(),
        "读不到的路径必须报错，否则用户看到「导入 0 条」会以为文件坏了"
    );

    let _ = std::fs::remove_file(&out);
}

/// 造一个只带 `common_han` 类的临时数据目录。
///
/// 常用性判定归 `CharsetRegistry`：没有 `common_han` 类就一切兜底判常用，而本文件多条用例
/// 的前提是「域内的汉字默认判生僻」（让 `common: true` 真的落一条记录），由该类的
/// `scope: han` + `outside: rare` 给出。
///
/// 用 `ranges` 而不是列名单：多条用例假定初始是空表，而全表列的正是表态类的成员。给一段
/// 用不到的 PUA 码位，既让类有内容（不触发「名单为空就撤销表态」的安全阀），又不往全表里塞东西。
fn charsets_data_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_webdata_data_{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("charsets")).unwrap();
    std::fs::write(
        d.join("charsets").join("common_han.yaml"),
        "---
key: common_han
name: 常用汉字
order: 50
scope: han
ranges:
  - U+E000-U+E001
default: common
outside: rare
",
    )
    .unwrap();
    d
}

/// 构造一个带临时 store 的无头 Coordinator。
fn coord(tag: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_webdata_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let data = charsets_data_dir(tag);
    Coordinator::new_headless_with_store(Config::default(), Some(&data), store)
}

/// 在 `{sections:[{key,...}]}` 响应里按 key 取某段（无则 Null）。
fn sec(v: &Value, key: &str) -> Value {
    v.get("sections")
        .and_then(|s| s.as_array())
        .and_then(|a| {
            a.iter()
                .find(|x| x.get("key").and_then(|k| k.as_str()) == Some(key))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// 设置页显示带空格的音节码，存储 key 保持扁平：两个域在 RPC 边界上的往返契约。
///
/// 用户把看到的 `ni hao` 原样提交回来时，写入侧必须拆成扁平 key，否则 `niha` 前缀匹配不到；
/// remove/search 收到带空格的串也必须拆。用户打的空格被当作显式切分采信，优先于
/// `normalize_add_code` 的求解链。
#[test]
fn dict_spaced_code_display_flat_storage_roundtrip() {
    let c = coord("spaced_roundtrip");
    let p = |code: &str| {
        serde_json::json!({
            "schemaId": "pinyin", "code": code, "text": "你好", "weight": 500
        })
    };

    // 提交带空格的码（模拟用户从「出码」按钮拿到后直接保存）
    c.web_data_rpc("dict.add", &p("ni hao")).unwrap();

    // 存储侧：key 扁平、边界由空格得来（ni|hao → {0,2}）
    let store = c.user_store().expect("有 store");
    let recs = store.get_user_words("pinyin", "nihao").unwrap();
    assert_eq!(recs.len(), 1, "key 必须是扁平的 nihao，不能带空格");
    assert_eq!(recs[0].boundary, 0b101, "用户打的空格即显式切分，须被采信");

    // 显示侧：列表与搜索都回带空格的码
    let items = c
        .web_data_rpc(
            "dict.search",
            &serde_json::json!({ "schemaId": "pinyin", "query": "ni hao" }),
        )
        .unwrap();
    assert_eq!(
        items[0].get("code").and_then(|v| v.as_str()),
        Some("ni hao"),
        "列表回显须与出码同形；搜索词带空格也要能命中（查询侧同样拆）"
    );

    // 删除：带空格的码同样要能删掉
    c.web_data_rpc("dict.remove", &p("ni hao")).unwrap();
    assert!(
        store.get_user_words("pinyin", "nihao").unwrap().is_empty(),
        "remove 收到带空格的码须先拆再删"
    );
}

/// 设置页改词（`dict.update` 的 upsert）与单条手动转正（`temp.promote`）是显式加词：撤掉同码
/// 同文的删除规则，别的词的删除规则不动。全部转正（`temp.promoteAll`）是批量操作：不撤，报数。
#[test]
fn single_update_and_promote_clear_but_promote_all_only_counts() {
    let c = coord("readd_clear");
    let store = c.user_store().expect("有 store");
    for (code, text) in [("nihao", "你好"), ("haoya", "好呀"), ("shijie", "世界")] {
        store.delete_shadow("pinyin", code, text).unwrap();
    }
    store.delete_shadow("pinyin", "nihao", "拟好").unwrap();
    store
        .learn_temp_word("pinyin", "haoya", "好呀", 500, 0b1001)
        .unwrap();
    store
        .learn_temp_word("pinyin", "shijie", "世界", 500, 0b101)
        .unwrap();
    let deleted = |code: &str| {
        store
            .get_shadow_rules("pinyin", code)
            .unwrap()
            .map(|r| r.deleted)
            .unwrap_or_default()
    };

    c.web_data_rpc(
        "dict.update",
        &serde_json::json!({ "schemaId": "pinyin", "code": "ni hao", "text": "你好", "weight": 5 }),
    )
    .unwrap();
    assert_eq!(deleted("nihao"), vec!["拟好".to_string()], "只撤「你好」");

    c.web_data_rpc(
        "temp.promote",
        &serde_json::json!({ "schemaId": "pinyin", "code": "hao ya", "text": "好呀" }),
    )
    .unwrap();
    assert!(deleted("haoya").is_empty(), "手动转正应撤删除规则");

    // 全部转正是批量操作：不撤，只报数
    let r = c
        .web_data_rpc(
            "temp.promoteAll",
            &serde_json::json!({ "schemaId": "pinyin" }),
        )
        .unwrap();
    assert_eq!(
        r,
        serde_json::json!({ "promoted": 1, "hiddenByShadow": 1 }),
        "转正 1 条、其中 1 条被隐藏"
    );
    assert_eq!(
        deleted("shijie"),
        vec!["世界".to_string()],
        "全部转正不撤删除规则"
    );
}

/// 拼音「屏蔽这个词」行（`code = *`）在候选调整列表里照常列出、能按 `shadow.removeRule` 删；
/// 全部转正的计数按文字算上它。
#[test]
fn block_row_listed_removable_and_counted() {
    let c = coord("block_row");
    let store = c.user_store().expect("有 store");
    store.block_word("pinyin", "世界").unwrap();
    let list = c
        .web_data_rpc("shadow.list", &serde_json::json!({ "schemaId": "pinyin" }))
        .unwrap();
    let arr = list.as_array().expect("数组");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["code"], "*");
    assert_eq!(arr[0]["word"], "世界");
    assert_eq!(arr[0]["type"], "delete");

    store
        .learn_temp_word("pinyin", "shijie", "世界", 500, 0b101)
        .unwrap();
    let r = c
        .web_data_rpc(
            "temp.promoteAll",
            &serde_json::json!({ "schemaId": "pinyin" }),
        )
        .unwrap();
    assert_eq!(r, serde_json::json!({ "promoted": 1, "hiddenByShadow": 1 }));
    assert!(
        store.blocked_words("pinyin").unwrap().contains("世界"),
        "全部转正不撤屏蔽"
    );

    c.web_data_rpc(
        "shadow.removeRule",
        &serde_json::json!({ "schemaId": "pinyin", "code": "*", "word": "世界" }),
    )
    .unwrap();
    assert!(store.blocked_words("pinyin").unwrap().is_empty());
}

/// 临时词库与用户词库同款：列表显示带空格，remove / promote 收到后各自拆回扁平码。
/// **三处必须同改**——只改列表就成了「显示得了、删不掉、也晋升不了」。
#[test]
fn temp_word_spaced_code_roundtrip() {
    let c = coord("temp_spaced");
    let store = c.user_store().expect("有 store");
    // hao|ya → 起始字节位 {0,3}
    store
        .learn_temp_word("pinyin", "haoya", "好呀", 500, 0b1001)
        .unwrap();
    store
        .learn_temp_word("pinyin", "nihao", "你好", 500, 0b101)
        .unwrap();

    let items = c
        .web_data_rpc("temp.list", &serde_json::json!({ "schemaId": "pinyin" }))
        .unwrap();
    let codes: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("code").and_then(|c| c.as_str()))
        .collect();
    assert!(
        codes.contains(&"hao ya") && codes.contains(&"ni hao"),
        "临时词库列表须显示带空格的音节码，实际 {codes:?}"
    );

    // 晋升：带空格的 code 要能查到 temp 记录并写进用户词库
    c.web_data_rpc(
        "temp.promote",
        &serde_json::json!({ "schemaId": "pinyin", "code": "ni hao", "text": "你好" }),
    )
    .unwrap();
    assert!(
        !store.get_user_words("pinyin", "nihao").unwrap().is_empty(),
        "promote 收到带空格的码须先拆再晋升"
    );

    // 删除：同理
    c.web_data_rpc(
        "temp.remove",
        &serde_json::json!({ "schemaId": "pinyin", "code": "hao ya", "text": "好呀" }),
    )
    .unwrap();
    assert!(
        store
            .get_temp_words("pinyin", "haoya")
            .unwrap_or_default()
            .is_empty(),
        "remove 收到带空格的码须先拆再删"
    );
}

/// 两张回收名单里的每个方法名都必须真在分派表里：改名而忘了改名单，回收就静默失效。
/// 拿空参数逐个调一遍：分派表认得的方法会因缺参报错（或照常返回），只有不认得的才报
/// `unknown method`。名单里的方法都只读或缺参即返回，空参数调用不会动数据。
#[test]
fn rpc_name_tables_match_the_dispatch_table() {
    let c = coord("rpc_name_tables");
    for &m in FULL_SCAN_METHODS
        .iter()
        .chain(BULK_WRITE_METHODS)
        .chain(&["dict.search"])
    {
        if let Err(e) = c.web_data_rpc(m, &json!({})) {
            assert!(
                !e.to_string().contains("unknown method"),
                "{m} 不在分派表里（改名了？同步更新名单）"
            );
        }
    }
}

/// ime.get / set / toggle：中英切换幂等、取反回新值、未知目标报错，且 get 不带目标回全部。
#[test]
fn ime_state_rpcs_set_toggle_get() {
    let c = coord("ime_state_rpcs");
    let call = |m: &str, p: Value| c.web_data_rpc(m, &p);

    let all = call("ime.get", json!({})).unwrap();
    for t in ["cn-en", "fullshape", "s2t", "t2s", "toolbar"] {
        assert!(all.get(t).is_some_and(Value::is_boolean), "{t}: {all}");
    }

    let cur = call("ime.get", json!({"target": "cn-en"})).unwrap()["on"]
        .as_bool()
        .unwrap();
    // 设成当前值：不改动
    let r = call("ime.set", json!({"target": "cn-en", "on": cur})).unwrap();
    assert_eq!(r["changed"], json!(false), "{r}");
    // 设成另一值：改动，且 get 读到新值
    let r = call("ime.set", json!({"target": "cn-en", "on": !cur})).unwrap();
    assert_eq!(r["changed"], json!(true), "{r}");
    assert_eq!(
        call("ime.get", json!({"target": "cn-en"})).unwrap()["on"],
        json!(!cur)
    );
    // toggle 回到原值，并回报切换后的值
    let r = call("ime.toggle", json!({"target": "cn-en"})).unwrap();
    assert_eq!(r["on"], json!(cur), "{r}");

    assert!(call("ime.get", json!({"target": "layout"})).is_err());
    assert!(call("ime.toggle", json!({"target": "layout"})).is_err());
    assert!(
        call("ime.set", json!({"target": "cn-en"})).is_err(),
        "缺 on"
    );
}

/// dict.reload / dict.flush：分派在表里；reload 越界路径报错。
#[test]
fn dict_reload_and_flush_are_dispatched() {
    let c = coord("dict_reload_flush");
    let r = c.web_data_rpc("dict.reload", &json!({})).unwrap();
    assert!(r.get("reloaded").is_some_and(|v| v.is_array()), "{r}");
    let r = c.web_data_rpc("dict.flush", &json!({})).unwrap();
    assert_eq!(r, json!({"ok": true, "held": []}));
    assert!(
        c.web_data_rpc("dict.reload", &json!({"paths": ["/etc/passwd"]}))
            .is_err()
    );
}

/// 外部编辑（设计 user-dict-libraries-and-external-edit §4.2）：begin 返回路径与类别，编辑期间
/// 界面改同一张表被拒，end 载入手改并重建镜像，之后照常可改；越界与程序维护的文件拒绝。
#[test]
fn data_edit_session_rpcs() {
    let c = coord("data_edit");
    let root = c.user_store().unwrap().user_text_dir().to_path_buf();
    let r = c
        .web_data_rpc("data.beginEdit", &json!({"path": "phrases.dict.yaml"}))
        .unwrap();
    let abs = root.join("phrases.dict.yaml");
    assert_eq!(r["path"], abs.to_string_lossy().as_ref(), "{r}");
    assert_eq!(r["file"], "phrases.dict.yaml");
    assert_eq!(r["kind"], "phrases");
    let s = c.web_data_rpc("data.editSessions", &json!({})).unwrap();
    assert_eq!(s[0]["file"], "phrases.dict.yaml", "{s}");
    assert!(s[0]["since_secs"].is_u64(), "{s}");
    let e = c
        .web_data_rpc("phrase.add", &json!({"code": "dz", "text": "地址"}))
        .unwrap_err();
    assert!(e.to_string().contains("正在外部编辑"), "{e}");
    std::fs::write(
        &abs,
        std::fs::read_to_string(&abs).unwrap() + "yx\tme@x.com\t1\n",
    )
    .unwrap();
    let r = c
        .web_data_rpc("data.endEdit", &json!({"path": abs.to_string_lossy()}))
        .unwrap();
    assert_eq!(r, json!({"changed": true, "issues": []}));
    assert!(c.debug_phrase_texts("yx").contains(&"me@x.com".to_string()));
    c.web_data_rpc("phrase.add", &json!({"code": "dz", "text": "地址"}))
        .unwrap();
    assert_eq!(
        c.web_data_rpc("data.editSessions", &json!({})).unwrap(),
        json!([])
    );
    assert!(
        c.web_data_rpc("data.beginEdit", &json!({"path": "/etc/passwd"}))
            .is_err()
    );
    assert!(
        c.web_data_rpc("data.beginEdit", &json!({"path": "../user.dict.yaml"}))
            .is_err()
    );
    let e = c
        .web_data_rpc("data.beginEdit", &json!({"path": "pinyin/draft.dict.yaml"}))
        .unwrap_err();
    assert!(e.to_string().contains("自动维护"), "{e}");
}

/// 用户词会话：dict.flush 跳过暂停写回的文件并在返回里列出；endEdit 带回坏行。
#[test]
fn data_edit_words_holds_flush_and_reports_issues() {
    let c = coord("data_edit_words");
    let r = c
        .web_data_rpc("data.beginEdit", &json!({"path": "pinyin/user.dict.yaml"}))
        .unwrap();
    assert_eq!(r["kind"], "words");
    let r = c.web_data_rpc("dict.flush", &json!({})).unwrap();
    assert_eq!(r, json!({"ok": true, "held": ["pinyin/user.dict.yaml"]}));
    let f = std::path::PathBuf::from(
        c.web_data_rpc("data.locate", &json!({"path": "pinyin/user.dict.yaml"}))
            .unwrap()["path"]
            .as_str()
            .unwrap(),
    );
    std::fs::write(&f, std::fs::read_to_string(&f).unwrap() + "只有一列\n").unwrap();
    let r = c
        .web_data_rpc("data.endEdit", &json!({"path": "pinyin/user.dict.yaml"}))
        .unwrap();
    assert_eq!(r["issues"][0]["path"], "pinyin/user.dict.yaml", "{r}");
    assert_eq!(r["issues"][0]["kind"], "bad_lines", "{r}");
    assert_eq!(r["issues"][0]["lines"], 1, "{r}");
    let r = c.web_data_rpc("dict.flush", &json!({})).unwrap();
    assert_eq!(r, json!({"ok": true, "held": []}));
    wind_store::issues::clear(&f);
}

/// 租约到期按 end 处理：手改的短语载入，输入期短语层随之重建（协调器装的回调）。
#[test]
fn expired_edit_session_rebuilds_phrase_layer() {
    let c = coord("data_edit_lease");
    let store = c.user_store().cloned().unwrap();
    c.web_data_rpc("data.beginEdit", &json!({"path": "phrases.dict.yaml"}))
        .unwrap();
    let f = store.phrases_path();
    std::fs::write(
        &f,
        std::fs::read_to_string(&f).unwrap() + "yx\tme@x.com\t1\n",
    )
    .unwrap();
    assert!(
        !c.debug_phrase_texts("yx").contains(&"me@x.com".to_string()),
        "前置：镜像未变"
    );
    assert_eq!(store.expire_edit_sessions(std::time::Duration::ZERO), 1);
    assert!(c.debug_phrase_texts("yx").contains(&"me@x.com".to_string()));
    assert_eq!(
        c.web_data_rpc("data.editSessions", &json!({})).unwrap(),
        json!([])
    );
}

/// 编辑目标按类别给：用户词 / 候选调整带方案（拼音族折叠到 pinyin），小表只给类别；
/// begin 与 end 两种参数形式指向同一会话；locate 认方案。
#[test]
fn data_edit_accepts_kind_and_schema() {
    let tag = "data_edit_kind";
    let path = std::env::temp_dir().join(format!("wind_webdata_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let data = charsets_data_dir(tag);
    std::fs::create_dir_all(data.join("schemas")).unwrap();
    std::fs::write(
        data.join("schemas/double_pinyin.schema.toml"),
        "[engine]\ntype = \"pinyin\"\n",
    )
    .unwrap();
    let c = Coordinator::new_headless_with_store(Config::default(), Some(&data), store);
    let begin = |p: Value| c.web_data_rpc("data.beginEdit", &p);
    let r = begin(json!({"schemaId": "double_pinyin", "kind": "words"})).unwrap();
    assert_eq!(
        (&r["file"], &r["kind"]),
        (&json!("pinyin/user.dict.yaml"), &json!("words"))
    );
    let r = begin(json!({"schemaId": "wubi86", "kind": "shadow"})).unwrap();
    assert_eq!(
        (&r["file"], &r["kind"]),
        (&json!("wubi86/shadow.dict.yaml"), &json!("shadow"))
    );
    for (kind, file) in [
        ("phrases", "phrases.dict.yaml"),
        ("quick_format", "quick_format.toml"),
        ("common_chars", "common_chars.yaml"),
    ] {
        let r = begin(json!({"kind": kind})).unwrap();
        assert_eq!(
            (&r["file"], &r["kind"]),
            (&json!(file), &json!(kind)),
            "{r}"
        );
    }
    for (kind, file) in [
        ("temp", "pinyin/temp.dict.yaml"),
        ("freq", "pinyin/freq.table.yaml"),
    ] {
        let r = begin(json!({"schemaId": "double_pinyin", "kind": kind})).unwrap();
        assert_eq!(
            (&r["file"], &r["kind"]),
            (&json!(file), &json!(kind)),
            "{r}"
        );
    }
    assert!(begin(json!({"kind": "words"})).is_err(), "用户词要带方案");
    assert!(begin(json!({"kind": "temp"})).is_err(), "临时词要带方案");
    assert!(begin(json!({"kind": "draft", "schemaId": "wubi86"})).is_err());
    assert!(begin(json!({"schemaId": "../x", "kind": "words"})).is_err());
    assert_eq!(
        c.web_data_rpc("data.editSessions", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let r = c.web_data_rpc("dict.flush", &json!({})).unwrap();
    let mut held: Vec<String> = serde_json::from_value(r["held"].clone()).unwrap();
    held.sort();
    assert_eq!(
        held,
        [
            "pinyin/freq.table.yaml",
            "pinyin/temp.dict.yaml",
            "pinyin/user.dict.yaml"
        ]
    );
    let end = |p: Value| c.web_data_rpc("data.endEdit", &p).unwrap();
    end(json!({"schemaId": "double_pinyin", "kind": "words"}));
    end(json!({"schemaId": "double_pinyin", "kind": "temp"}));
    end(json!({"schemaId": "double_pinyin", "kind": "freq"}));
    end(json!({"schemaId": "wubi86", "kind": "shadow"}));
    for kind in ["phrases", "quick_format", "common_chars"] {
        end(json!({"kind": kind}));
    }
    assert_eq!(
        c.web_data_rpc("data.editSessions", &json!({})).unwrap(),
        json!([])
    );
    let root = c.user_store().unwrap().user_text_dir().to_path_buf();
    let at = |p: Value| c.web_data_rpc("data.locate", &p).unwrap()["path"].clone();
    assert_eq!(
        at(json!({"schemaId": "double_pinyin"})),
        root.join("pinyin").to_string_lossy().as_ref()
    );
    assert_eq!(
        at(json!({"schemaId": "wubi86"})),
        root.join("wubi86").to_string_lossy().as_ref()
    );
}

/// 字符分类按 key 编辑：没有个人调整就先建一份只带 key 的；不认识的 key 拒绝。结束时格式坏
/// 报 invalid、改出厂范围 / 整表替换被忽略并报 invalid，修好后问题消失。
#[test]
fn data_edit_charsets_by_key_and_reports_invalid() {
    let c = coord("data_edit_cs");
    let store = c.user_store().cloned().unwrap();
    let p = store.charset_path("common_han").unwrap();
    assert!(!p.exists());
    let begin = |k: &str| c.web_data_rpc("data.beginEdit", &json!({"kind": "charsets", "key": k}));
    let end = || {
        c.web_data_rpc(
            "data.endEdit",
            &json!({"kind": "charsets", "key": "common_han"}),
        )
        .unwrap()
    };
    let e = begin("nope").unwrap_err().to_string();
    assert!(e.contains("字符分类"), "{e}");
    let r = begin("common_han").unwrap();
    assert_eq!(
        (&r["file"], &r["kind"]),
        (&json!("charsets/common_han.yaml"), &json!("charsets"))
    );
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(
        text.starts_with(wind_config::charset_def::STORED_DOC_BANNER),
        "{text}"
    );
    assert!(text.contains("key: common_han"), "{text}");
    assert_eq!(end(), json!({"changed": false, "issues": []}));

    begin("common_han").unwrap();
    std::fs::write(&p, "---\nkey: common_han\nname: [\n").unwrap();
    let r = end();
    assert_eq!(r["issues"][0]["kind"], "invalid", "{r}");
    assert_eq!(r["issues"][0]["file"], "charsets/common_han.yaml", "{r}");
    assert!(
        r["issues"][0]["message"].as_str().unwrap().contains("格式"),
        "{r}"
    );

    begin("common_han").unwrap();
    std::fs::write(
        &p,
        "---\nkey: common_han\nname: 我的常用\nranges: [U+4E00-U+4E01]\nreplace: true\n",
    )
    .unwrap();
    let r = end();
    assert_eq!(r["issues"][0]["kind"], "invalid", "{r}");
    assert!(
        r["issues"][0]["message"]
            .as_str()
            .unwrap()
            .contains("出厂字符分类的范围不能改"),
        "{r}"
    );
    assert!(
        c.engine_mgr()
            .charsets()
            .class_by_key("common_han")
            .is_some()
    );

    begin("common_han").unwrap();
    std::fs::write(&p, "---\nkey: common_han\nname: 我的常用\n").unwrap();
    assert_eq!(end()["issues"], json!([]));
}

/// data.locate：数据目录、方案目录、数据文件；越界拒绝。
#[test]
fn data_locate_returns_paths_inside_data_dir() {
    let c = coord("data_locate");
    let root = c.user_store().unwrap().user_text_dir().to_path_buf();
    let at = |p: Value| c.web_data_rpc("data.locate", &p);
    assert_eq!(
        at(json!({})).unwrap()["path"],
        root.to_string_lossy().as_ref()
    );
    assert_eq!(
        at(json!({"path": "wubi86"})).unwrap()["path"],
        root.join("wubi86").to_string_lossy().as_ref()
    );
    assert!(at(json!({"path": "/etc"})).is_err());
    assert!(at(json!({"path": "../x"})).is_err());
    std::fs::create_dir_all(&root).unwrap();
    assert_eq!(at(json!({})).unwrap()["exists"], true);
    assert_eq!(at(json!({"path": "wubi86"})).unwrap()["exists"], false);
    assert_eq!(
        at(json!({"path": "wubi86/user.dict.yaml"})).unwrap()["exists"],
        false
    );
    assert_eq!(
        at(json!({"path": "pinyin_misread.yaml"})).unwrap()["path"],
        root.join("pinyin_misread.yaml").to_string_lossy().as_ref(),
        "设置端「误读表 → 打开文件」"
    );
}

/// `correction.conflictExample`：设置页自定义纠错规则行的冲突示例。
#[test]
fn correction_conflict_example_contract() {
    let c = coord("correction_conflict");
    let call = |rule: &str| {
        c.web_data_rpc("correction.conflictExample", &json!({ "rule": rule }))
            .unwrap()
    };
    let r = call("uen=un");
    assert_eq!(r["valid"], true, "{r}");
    assert!(r["example"].as_str().unwrap().ends_with("u+en"), "{r}");
    assert_eq!(call("ioa=iao"), json!({ "valid": true, "example": null }));
    let bad = call("ogn");
    assert_eq!(bad["valid"], false, "{bad}");
    assert!(
        bad["reason"].as_str().is_some_and(|s| !s.is_empty()),
        "{bad}"
    );
    assert!(
        c.web_data_rpc("correction.conflictExample", &json!({}))
            .is_err()
    );
}

/// 这些方法也扫整表 / 是批量写，名单里不能缺。
#[test]
fn rpc_name_tables_cover_reviewed_methods() {
    for m in ["dict.previewImport", "freq.clear", "shadow.clear"] {
        assert!(scans_whole_table(m, &json!({})), "{m} 应标记扫描待回收");
    }
    assert!(
        BULK_WRITE_METHODS.contains(&"schema.delete"),
        "schema.delete 级联清六张表，应与 dict.clear 一样立即回收"
    );
}

/// 协调器起来就迁完了全部旧数据、放下了 redb：设置页翻页（`dict.listPaged`）与窄查询照常，
/// 不再触发 redb 页缓存回收（无缓存可回收）。
#[test]
fn dict_list_paged_after_migration_has_no_redb_to_reclaim() {
    use std::time::Duration;
    const TICK: Duration = Duration::from_millis(20);
    const SCAN_IDLE: Duration = Duration::from_millis(300);
    const NEVER: Duration = Duration::from_secs(3600);
    let wait_drops = |store: &Store, n: u64, timeout: Duration| {
        let t0 = std::time::Instant::now();
        while t0.elapsed() < timeout {
            if store.page_cache_drops() >= n {
                return true;
            }
            std::thread::sleep(TICK / 2);
        }
        false
    };

    // 窄查询：只访问、不标记 ⇒ 长档一小时，短档不生效。
    let c = coord("scan_reclaim_narrow");
    let store = c.user_store().expect("有 store").clone();
    store.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    c.web_data_rpc(
        "dict.search",
        &json!({ "schemaId": "pinyin", "query": "ni", "limit": 50 }),
    )
    .unwrap();
    std::thread::sleep(SCAN_IDLE * 3);
    assert_eq!(store.page_cache_drops(), 0, "窄查询不触发回收");

    let c = coord("scan_reclaim");
    let store = c.user_store().expect("有 store").clone();
    store
        .add_user_word("pinyin", "nihao", "你好", 100, 0)
        .unwrap();
    store.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    let page = json!({ "schemaId": "pinyin", "offset": 0, "limit": 50 });
    let first = c.web_data_rpc("dict.listPaged", &page).unwrap();
    let second = c.web_data_rpc("dict.listPaged", &page).unwrap();
    assert!(store.is_paused(), "迁完即放下旧库");
    assert!(
        !wait_drops(&store, 1, SCAN_IDLE * 3),
        "没有 redb 页缓存可回收"
    );
    let after = c.web_data_rpc("dict.listPaged", &page).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, after);
    assert_eq!(after["total"], json!(1));
}

/// `dict.listPaged` 的三条路必须给出彼此自洽的结果。
///
/// 入口拆成三条分流（不搜不排序走流式、只物化一页；要排序走全量；搜索走一遍扫描分两组），
/// 最容易坏的是它们彼此不一致：分页拼起来少一条、搜索路径的 total 对不上、排序漏条目。
/// 逐条比对是唯一测得到这些的办法。
#[test]
fn dict_list_paged_stays_consistent_across_its_three_paths() {
    let c = coord("dict_paged");
    let store = c.user_store().expect("有 store");
    // `ni*` 三条供前缀命中；`haoya` 只能由「编码中段含 ya」命中；
    // 「你好呀」只能由「词条内容含 你」命中。
    for (code, text) in [
        ("ni hao", "你好"),
        ("ni men", "你们"),
        ("ni hao ya", "你好呀"),
        ("hao ya", "好呀"),
        ("wo men", "我们"),
    ] {
        store
            .add_user_word("pinyin", &code.replace(' ', ""), text, 100, 0)
            .unwrap();
    }
    let call = |p: Value| c.web_data_rpc("dict.listPaged", &p).unwrap();
    let texts = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["text"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // ── ① 流式路（不搜不排序）：分页拼起来必须等于一次取全部 ──────────────
    let all = call(json!({ "schemaId": "pinyin", "offset": 0, "limit": 100 }));
    assert_eq!(all["total"], json!(5));
    let whole = texts(&all);
    assert_eq!(whole.len(), 5);

    let mut stitched = Vec::new();
    for off in [0, 2, 4] {
        let page = call(json!({ "schemaId": "pinyin", "offset": off, "limit": 2 }));
        assert_eq!(page["total"], json!(5), "翻页不得改变总数");
        stitched.extend(texts(&page));
    }
    assert_eq!(stitched, whole, "分页拼接必须逐条等于一次取全部");

    // offset 越界给空页，但 total 照旧——分页器靠它算页数。
    let over = call(json!({ "schemaId": "pinyin", "offset": 99, "limit": 2 }));
    assert_eq!(over["total"], json!(5));
    assert!(texts(&over).is_empty());

    // ── ② 排序路：集合与 ① 相同，只是次序不同 ───────────────────────────
    let sorted = call(json!({
        "schemaId": "pinyin", "offset": 0, "limit": 100, "sortBy": "text", "sortDesc": false
    }));
    assert_eq!(sorted["total"], json!(5), "排序不得改变总数");
    let mut a = texts(&sorted);
    let mut b = whole.clone();
    a.sort();
    b.sort();
    assert_eq!(a, b, "排序路与流式路必须是同一批条目");

    // ── ③ 搜索路：前缀命中在前，其余命中在后，且不重复 ──────────────────
    let hit = call(json!({ "schemaId": "pinyin", "query": "ni", "offset": 0, "limit": 100 }));
    let got = texts(&hit);
    assert_eq!(
        hit["total"].as_u64().unwrap() as usize,
        got.len(),
        "total 必须等于实际条数"
    );
    let mut dedup = got.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), got.len(), "同一条不得同时进两组");
    // `ni*` 三条靠编码前缀命中，必须排在最前。
    assert_eq!(
        &got[..3],
        // key 序（扁平码字典序）：nihao < nihaoya < nimen。
        &["你好".to_string(), "你好呀".to_string(), "你们".to_string()],
        "编码前缀命中必须在前，实际 {got:?}"
    );

    // 只能由「编码中段」命中的那条：搜 `ya` 要捞到 `haoya`。
    let mid = texts(&call(
        json!({ "schemaId": "pinyin", "query": "ya", "offset": 0, "limit": 100 }),
    ));
    assert!(
        mid.contains(&"好呀".to_string()),
        "编码中段命中丢了：{mid:?}"
    );
    assert!(
        mid.contains(&"你好呀".to_string()),
        "编码中段命中丢了：{mid:?}"
    );

    // 只能由「词条内容」命中的：搜汉字。
    let by_text = texts(&call(
        json!({ "schemaId": "pinyin", "query": "我", "offset": 0, "limit": 100 }),
    ));
    assert_eq!(by_text, vec!["我们".to_string()], "按词条内容搜不到");
}

/// 临时词分页：切片、跨页 total、服务端搜索（含编码中段命中）、跨页排序。
/// 四件事都得由 core 兑现，缺一样的症状分别是：翻页翻不动 / 页数算错 / 搜索搜不全 /
/// 排序只排当页。
#[test]
fn temp_list_paged_slices_searches_and_sorts() {
    let c = coord("temp_paged");
    let store = c.user_store().expect("有 store");
    // hao|ya → 起始字节位 {0,3}；另两条用于凑够三条好切片。
    store
        .learn_temp_word("pinyin", "haoya", "好呀", 500, 0b1001)
        .unwrap();
    store
        .learn_temp_word("pinyin", "nihao", "你好", 500, 0b101)
        .unwrap();
    store
        .learn_temp_word("pinyin", "women", "我们", 500, 0)
        .unwrap();

    let call = |params: Value| c.web_data_rpc("temp.listPaged", &params).unwrap();
    let codes = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["code"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // ① 切片：total 是全集大小而非当页长度，否则设置端算出来的页数恒是 1。
    let page1 = call(json!({ "schemaId": "pinyin", "offset": 0, "limit": 2 }));
    assert_eq!(page1["total"], json!(3));
    assert_eq!(codes(&page1).len(), 2);
    let page2 = call(json!({ "schemaId": "pinyin", "offset": 2, "limit": 2 }));
    assert_eq!(page2["total"], json!(3), "翻页不改变总数");
    assert_eq!(codes(&page2).len(), 1, "末页只剩一条");

    // ② 编码仍带音节空格（与 temp.list 同形，remove/promote 那两个入口靠它往返）。
    let all = call(json!({ "schemaId": "pinyin", "offset": 0, "limit": 100 }));
    assert!(
        codes(&all).contains(&"hao ya".to_string()),
        "分页版也得显示带空格的音节码，实际 {:?}",
        codes(&all)
    );

    // ③ 搜索：编码**中段**命中。前缀扫描找不到 `haoya` 里的 `ya`，而用户并不知道
    //    搜索框只认前缀。
    let mid = call(json!({ "schemaId": "pinyin", "prefix": "ya", "offset": 0, "limit": 100 }));
    assert_eq!(mid["total"], json!(1), "中段命中要算进 total");
    assert_eq!(codes(&mid), vec!["hao ya".to_string()]);

    // ④ 搜索：词条内容命中（拿汉字搜）。
    let by_text =
        call(json!({ "schemaId": "pinyin", "prefix": "我们", "offset": 0, "limit": 100 }));
    assert_eq!(by_text["total"], json!(1));

    // ⑤ 排序在切片**之前**做，故第一页拿到的是全局最大者，不是当页最大者。
    let desc = call(json!({
        "schemaId": "pinyin", "offset": 0, "limit": 1,
        "sortBy": "code", "sortOrder": "desc"
    }));
    // `women` 那条 boundary 传的是 0（无切分信息），故显示码不插空格。
    assert_eq!(codes(&desc), vec!["women".to_string()], "全局倒序的头一条");
}

/// 临时词筛选：按次数上限 / 最近使用截止时刻过滤，按最近使用排序；
/// 过滤在切片前做，`total` 是过滤后的全集大小（设置端据此翻页、按筛选结果批量删）。
#[test]
fn temp_list_paged_filters_by_count_and_last_used() {
    let c = coord("temp_filter");
    let store = c.user_store().expect("有 store");
    store
        .learn_temp_word("pinyin", "haoya", "好呀", 500, 0)
        .unwrap();
    store
        .learn_temp_word("pinyin", "nihao", "你好", 500, 0)
        .unwrap();
    for _ in 0..2 {
        store
            .learn_temp_word("pinyin", "women", "我们", 500, 0)
            .unwrap();
    }
    let call = |params: Value| c.web_data_rpc("temp.listPaged", &params).unwrap();
    let texts = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["text"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // ① 次数上限：count ≤ 1 的只有两条（「我们」学了三次）。
    let low = call(json!({ "schemaId": "pinyin", "maxCount": 1, "offset": 0, "limit": 1 }));
    assert_eq!(low["total"], json!(2), "total 须是过滤后的全集而非全表");
    assert_eq!(texts(&low).len(), 1, "过滤后照样切片");

    // ② 每条都带 lastUsed（无词频记录时回落到加入时间，恒 > 0）。
    let all = call(json!({ "schemaId": "pinyin", "offset": 0, "limit": 100 }));
    for it in all["items"].as_array().unwrap() {
        assert!(
            it["lastUsed"].as_i64().unwrap_or(0) > 0,
            "缺 lastUsed：{it}"
        );
    }

    // ③ 最近使用截止：截止在未来 ⇒ 全中；截止在很久以前 ⇒ 一条不中。
    let now = chrono::Utc::now().timestamp();
    let future =
        call(json!({ "schemaId": "pinyin", "usedBefore": now + 3600, "offset": 0, "limit": 100 }));
    assert_eq!(future["total"], json!(3));
    let past =
        call(json!({ "schemaId": "pinyin", "usedBefore": now - 86400, "offset": 0, "limit": 100 }));
    assert_eq!(past["total"], json!(0));

    // ④ 词频记录里的最近使用优先于加入时间：选过一次的那条 lastUsed 取词频那份。
    //    时间戳写在加入时间之后很远：用当前时间常与加入时间同秒，`>=` 恒真、测不出东西。
    let freq_ts = now + 7 * 86400;
    store
        .import_freq_rows(
            "pinyin",
            &[wind_store::wdict::FreqIo {
                code: "nihao".into(),
                text: "你好".into(),
                count: 1,
                last_used: freq_ts,
            }],
        )
        .unwrap();
    let v = call(json!({ "schemaId": "pinyin", "offset": 0, "limit": 100,
        "sortBy": "lastUsed", "sortOrder": "desc" }));
    assert_eq!(texts(&v).len(), 3);
    let last = |t: &str| {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|it| it["text"] == json!(t))
            .unwrap()["lastUsed"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(last("你好"), freq_ts, "有词频记录时 lastUsed 须取词频那份");
    assert!(last("好呀") < freq_ts, "无词频记录的仍取加入时间");

    // ⑤ 两个过滤条件取交集。
    let both = call(
        json!({ "schemaId": "pinyin", "maxCount": 1, "usedBefore": now - 86400,
        "offset": 0, "limit": 100 }),
    );
    assert_eq!(both["total"], json!(0));
}

/// 影子规则分页：切片 + total，以及**按显示文本搜索/排序**。
///
/// 要害在显示文本：设置页「词」列显示 `candId` 里的模板原文，而 `word` 存的是最后一次调整时
/// 的候选文本（`date`/`time` 这类求值型短语次日即过期）。只按 `word` 匹配，照着屏幕搜会一条也搜不到。
#[test]
fn shadow_list_paged_searches_by_display_text() {
    let c = coord("shadow_paged");
    let store = c.user_store().expect("有 store");
    // 短语规则：word 是**过期的**候选文本，模板原文只存在于 candId 里。
    store
        .pin_shadow(
            "pinyin",
            "sj",
            "2026-07-30",
            Some("phrase:sj:{time(\"HH:mm:ss\")}"),
            0,
        )
        .unwrap();
    store.pin_shadow("pinyin", "nh", "你好", None, 1).unwrap();
    store.delete_shadow("pinyin", "wm", "我们").unwrap();
    // 这一条是排序断言的支点：它的 word（`zzz`）与显示文本（模板 `阿`）在排序里
    //   落在相反的两端。少了它，两种排法的头尾恰好重合，断言就成了摆设。
    store
        .pin_shadow("pinyin", "zz", "zzz", Some("phrase:zz:阿"), 0)
        .unwrap();

    let call = |params: Value| c.web_data_rpc("shadow.listPaged", &params).unwrap();
    let words = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["word"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // ① 切片与 total（pin + delete 展平后共四行）。
    let page1 = call(json!({ "schemaId": "pinyin", "offset": 0, "limit": 3 }));
    assert_eq!(page1["total"], json!(4), "total 是全集大小，不是当页长度");
    assert_eq!(words(&page1).len(), 3);
    let page2 = call(json!({ "schemaId": "pinyin", "offset": 3, "limit": 3 }));
    assert_eq!(page2["total"], json!(4), "翻页不改变总数");
    assert_eq!(words(&page2).len(), 1, "末页只剩一条");

    // ② 按模板原文搜（用户照着屏幕上看到的那串搜）。
    let by_tpl = call(json!({ "schemaId": "pinyin", "prefix": "HH:mm", "offset": 0, "limit": 9 }));
    assert_eq!(
        by_tpl["total"],
        json!(1),
        "模板原文必须可搜——它才是列表显示的东西"
    );
    assert_eq!(words(&by_tpl), vec!["2026-07-30".to_string()]);

    // ③ 按 word 搜仍然命中（两条路都留着）。
    let by_word = call(json!({ "schemaId": "pinyin", "prefix": "你好", "offset": 0, "limit": 9 }));
    assert_eq!(by_word["total"], json!(1));

    // ④ 按编码搜。
    let by_code = call(json!({ "schemaId": "pinyin", "prefix": "wm", "offset": 0, "limit": 9 }));
    assert_eq!(by_code["total"], json!(1));
    assert_eq!(by_code["items"][0]["type"], json!("delete"));

    // ⑤ 排序：`text` 是设置端「词」列的字段名，落到显示文本上。`zz` 那条 word 是 `zzz`、
    //    显示文本是 `阿`(U+963F)：按显示文本升序排最后，按 word 升序排在 `你好` 之前。
    let sorted = call(json!({
        "schemaId": "pinyin", "offset": 0, "limit": 9,
        "sortBy": "text", "sortOrder": "asc"
    }));
    assert_eq!(
        words(&sorted).last().map(String::as_str),
        Some("zzz"),
        "按显示文本排序：比的是 candId 里的模板，不是 word"
    );
}

/// 整表撤销。设置端分页后手上只有当前页，逐条 `remove_rule` 会静默只清一页，
/// 故 clear 必须是 core 侧的一次调用。
#[test]
fn shadow_clear_removes_every_rule() {
    let c = coord("shadow_clear");
    let store = c.user_store().expect("有 store");
    for i in 0..3 {
        store
            .pin_shadow("pinyin", &format!("c{i}"), "词", None, 0)
            .unwrap();
    }
    store.delete_shadow("pinyin", "d0", "词").unwrap();

    let n = c
        .web_data_rpc("shadow.clear", &json!({ "schemaId": "pinyin" }))
        .unwrap();
    assert!(n.as_u64().unwrap_or(0) > 0, "返回撤销条数");
    let left = c
        .web_data_rpc("shadow.listPaged", &json!({ "schemaId": "pinyin" }))
        .unwrap();
    assert_eq!(left["total"], json!(0), "一条不剩");
}

/// 词频列表的编码显示带音节空格 —— 边界靠**反查**，因为词频表自己不存 boundary。
///
/// 三处来源依次问：系统词典 → 用户词表 → 临时词表。这里用后两者（无真实词库时
/// 系统词典为空，正好把反查降级链走一遍）。查不到的记录原样保持扁平，不得报错。
///
/// 同时锁住 freq.delete：列表给的是带空格的 code，不拆就删不掉。
#[test]
fn freq_list_shows_spaced_code_via_boundary_lookup() {
    let c = coord("freq_spaced");
    let store = c.user_store().expect("有 store");
    // 用户词提供边界：ni|hao
    store
        .add_user_word("pinyin", "nihao", "你好", 500, 0b101)
        .unwrap();
    // 临时词提供边界：hao|ya
    store
        .learn_temp_word("pinyin", "haoya", "好呀", 500, 0b1001)
        .unwrap();
    // 无处可查 → 保持扁平（存量简拼记录/已删词条的遗留记录都是这种）
    store.record_freq("pinyin", "nihao", "你好").unwrap();
    store.record_freq("pinyin", "haoya", "好呀").unwrap();
    store.record_freq("pinyin", "wubian", "无边").unwrap();

    let r = c
        .web_data_rpc(
            "freq.listPaged",
            &serde_json::json!({ "schemaId": "pinyin" }),
        )
        .unwrap();
    let items = r.get("items").and_then(|v| v.as_array()).unwrap().clone();
    let code_of = |text: &str| -> String {
        items
            .iter()
            .find(|x| x.get("text").and_then(|t| t.as_str()) == Some(text))
            .and_then(|x| x.get("code").and_then(|c| c.as_str()))
            .unwrap_or_default()
            .to_string()
    };

    assert_eq!(code_of("你好"), "ni hao", "边界应从用户词表反查到");
    assert_eq!(code_of("好呀"), "hao ya", "边界应从临时词表反查到");
    assert_eq!(
        code_of("无边"),
        "wubian",
        "三处都查不到 → 保持扁平码，不得报错"
    );

    // 删除：列表给的是带空格的 code
    c.web_data_rpc(
        "freq.delete",
        &serde_json::json!({ "schemaId": "pinyin", "code": "ni hao", "text": "你好" }),
    )
    .unwrap();
    let r2 = c
        .web_data_rpc(
            "freq.listPaged",
            &serde_json::json!({ "schemaId": "pinyin" }),
        )
        .unwrap();
    let left = r2.get("total").and_then(|v| v.as_u64()).unwrap();
    assert_eq!(left, 2, "带空格的 code 须先拆再删，否则删不掉");
}

/// 词频列表的编码搜索同样要能命中中段（与用户词库同款，两处是各自独立的实现）。
#[test]
fn freq_search_matches_code_middle_segment() {
    let c = coord("freq_search_middle");
    let store = c.user_store().expect("有 store");
    store.record_freq("pinyin", "haoya", "好呀").unwrap();
    store.record_freq("pinyin", "nihao", "你好").unwrap();

    let hits = |q: &str| -> Vec<String> {
        let r = c
            .web_data_rpc(
                "freq.listPaged",
                &serde_json::json!({ "schemaId": "pinyin", "prefix": q }),
            )
            .unwrap();
        r.get("items")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };

    assert!(
        hits("hao").contains(&"好呀".to_string()),
        "前缀命中（原有行为）"
    );
    assert!(
        hits("ya").contains(&"好呀".to_string()),
        "中段命中：haoya 搜 ya 须能找到"
    );
    assert!(
        hits("hao").contains(&"你好".to_string()),
        "nihao 的中段 hao 同样要命中"
    );
    // 词频 key 是扁平码，但用户可能从用户词库列表复制带空格的串来搜
    assert!(
        hits("ni hao").contains(&"你好".to_string()),
        "带空格的搜索词须先拆再匹配"
    );
}

/// 编码搜索须能命中**中段**，不能只认前缀。
///
/// redb 前缀扫描只覆盖开头，`haoya` 搜 `ya` 一条也出不来——而搜索框并没有告诉用户
/// 它只认前缀。词条内容搜索本就在做全量扫描，编码子串搭同一趟车，不增加扫描次数。
#[test]
fn dict_search_matches_code_middle_segment() {
    let c = coord("search_middle");
    let add = |code: &str, text: &str| {
        c.web_data_rpc(
            "dict.add",
            &serde_json::json!({
                "schemaId": "pinyin", "code": code, "text": text, "weight": 500
            }),
        )
        .unwrap();
    };
    add("hao ya", "好呀");
    add("ni hao", "你好");

    let hits = |q: &str| -> Vec<String> {
        let r = c
            .web_data_rpc(
                "dict.listPaged",
                &serde_json::json!({ "schemaId": "pinyin", "prefix": q }),
            )
            .unwrap();
        r.get("items")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };

    assert!(
        hits("hao").contains(&"好呀".to_string()),
        "前缀命中（原有行为）"
    );
    assert!(
        hits("ya").contains(&"好呀".to_string()),
        "中段命中：haoya 搜 ya 须能找到"
    );
    assert!(
        hits("hao").contains(&"你好".to_string()),
        "nihao 的中段 hao 同样要命中"
    );
    // 带空格的搜索词照样走中段匹配（先拆再比）
    assert!(
        hits("hao ya").contains(&"好呀".to_string()),
        "带空格的搜索词须先拆再匹配"
    );
}

/// 逐行短语文本（`wind:p1`）导入契约：预览落点 → 缺省全部导入 → 重导跳过。
///
/// 命令短语不再受任何额外门控——它本就是短语的主要用途，且不会自行执行。
#[test]
fn phrase_text_import_contract() {
    let q = char::from_u32(34).unwrap();
    let nl = char::from_u32(10).unwrap();
    let c = coord("phrasetext");
    let text = format!(
        "wind:p1 我的直通车{nl}kx (＾▽＾){nl}\
             zd $CC({q}记事本{q}, proc.run({q}notepad.exe{q})){nl}\
             sh $CC({q}跑{q}, proc.shell({q}echo hi{q})){nl}\
             编码 缺少合法编码的一行{nl}"
    );

    let prev = c
        .web_data_rpc("phrase.previewImportText", &json!({ "content": text }))
        .unwrap();
    assert_eq!(
        prev.get("title").and_then(|v| v.as_str()),
        Some("我的直通车")
    );
    let entries = prev.get("entries").unwrap().as_array().unwrap();
    assert_eq!(entries.len(), 3, "非法行不进 entries");
    assert_eq!(entries[0].get("line").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(
        entries[0].get("status").and_then(|v| v.as_str()),
        Some("new")
    );

    // 能力分级已移除：预览不再携带这三个字段。
    for e in entries {
        assert!(e.get("capability").is_none(), "不应再有 capability");
        assert!(e.get("highRisk").is_none(), "不应再有 highRisk");
        assert!(e.get("effects").is_none(), "不应再有 effects");
    }

    assert_eq!(
        prev.get("problems").unwrap().as_array().unwrap().len(),
        1,
        "非法编码的行进 problems"
    );
    assert_eq!(
        prev.get("counts")
            .unwrap()
            .get("new")
            .and_then(|v| v.as_u64()),
        Some(3)
    );

    // 缺省全导：含 proc.shell 的那条也直接装进去。
    let out = c
        .web_data_rpc("phrase.importText", &json!({ "content": text }))
        .unwrap();
    assert_eq!(
        out.get("added").and_then(|v| v.as_u64()),
        Some(3),
        "不传 accept 就全部导入，命令短语不再需要额外确认"
    );

    // 再来一次：已存在的原样跳过。
    let again = c
        .web_data_rpc("phrase.importText", &json!({ "content": text }))
        .unwrap();
    assert_eq!(again.get("added").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(
        again.get("skippedExisting").and_then(|v| v.as_u64()),
        Some(3)
    );

    // accept 仍可用于选择性导入。
    let c2 = coord("phrasetextsel");
    let sel = c2
        .web_data_rpc(
            "phrase.importText",
            &json!({ "content": text, "accept": [2] }),
        )
        .unwrap();
    assert_eq!(sel.get("added").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(
        sel.get("skippedUnselected").and_then(|v| v.as_u64()),
        Some(2)
    );

    // position 依次追加。
    let list = c.web_data_rpc("phrase.listUser", &json!({})).unwrap();
    let mut got: Vec<(&str, i64)> = list
        .get("items")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x.get("code").and_then(|v| v.as_str()).unwrap(),
                x.get("position").and_then(|v| v.as_i64()).unwrap(),
            )
        })
        .collect();
    got.sort_by_key(|(_, p)| *p);
    let codes: Vec<&str> = got.iter().map(|(c, _)| *c).collect();
    assert_eq!(codes, vec!["kx", "zd", "sh"], "position 按导入顺序递增");
}

/// 语法坏掉的条目一律不装——无论有没有被 accept 选中。
#[test]
fn phrase_text_rejects_broken_syntax() {
    let c = coord("phrasetextbad");
    let text = "wind:p1\nbad $CC(\"未闭合\nok 好\n";
    let out = c
        .web_data_rpc(
            "phrase.importText",
            &json!({ "content": text, "accept": [2, 3] }),
        )
        .unwrap();
    assert_eq!(out.get("added").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(
        out.get("skippedInvalid").and_then(|v| v.as_u64()),
        Some(1),
        "显式 accept 也不能让语法错误的条目进库"
    );
}

/// 分发文本 / 设置页手动新增 / wdict 文件导入，三条路径对同一段文本必须落到同一形态。
/// 分发格式没有自己的一套反斜杠规则，用户在任一处学到的写法要能照搬。
#[test]
fn phrase_text_escape_domain_matches_manual_add() {
    let bs = char::from_u32(92).unwrap();
    let q = char::from_u32(34).unwrap();
    let tab = char::from_u32(9).unwrap();
    let nl = char::from_u32(10).unwrap();
    let c = coord("phraseesc");

    // 按约定写法：一个字面反斜杠写两个。目录名刻意取 `notes`——单写时反斜杠
    // 紧跟 n 会变成换行，是最典型的静默损坏。
    let plain = format!("D:{bs}{bs}notes");
    let cmd = format!("$CC({q}x{q}, proc.run({q}D:{bs}{bs}notes{bs}{bs}a.exe{q}))");

    c.web_data_rpc("phrase.add", &json!({ "code": "m1", "text": plain }))
        .unwrap();
    c.web_data_rpc("phrase.add", &json!({ "code": "m2", "text": cmd }))
        .unwrap();
    c.web_data_rpc(
        "phrase.importText",
        &json!({ "content": format!("wind:p1{nl}d1 {plain}{nl}d2 {cmd}{nl}") }),
    )
    .unwrap();
    let hdr = format!(
        "wind_dict:{nl}  version: 1{nl}  sections:{nl}    phrases:{nl}      columns: [code, text, weight, position, enabled]{nl}{nl}--- !phrases{nl}"
    );
    c.web_data_rpc(
        "phrase.import",
        &json!({ "content": format!("{hdr}w1{tab}{plain}{tab}1800{tab}9{tab}1{nl}w2{tab}{cmd}{tab}1800{tab}10{tab}1{nl}") }),
    )
    .unwrap();

    let list = c.web_data_rpc("phrase.listUser", &json!({})).unwrap();
    let text_of = |src: &Value, code: &str| -> String {
        src.get("items")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|it| it.get("code").and_then(|v| v.as_str()) == Some(code))
            .and_then(|it| it.get("text").and_then(|v| v.as_str()))
            .unwrap_or_default()
            .to_string()
    };

    assert_eq!(
        text_of(&list, "m1"),
        text_of(&list, "d1"),
        "普通短语：手动新增与分发导入须同形"
    );
    assert_eq!(
        text_of(&list, "m1"),
        text_of(&list, "w1"),
        "普通短语：手动新增与 wdict 导入须同形"
    );
    assert_eq!(
        text_of(&list, "m2"),
        text_of(&list, "d2"),
        "命令短语：手动新增与分发导入须同形"
    );
    assert_eq!(
        text_of(&list, "m2"),
        text_of(&list, "w2"),
        "命令短语：手动新增与 wdict 导入须同形"
    );

    // 双写确实得到**字面反斜杠**：回显是 escape 后的形态，字面反斜杠会再次双写。
    // 若存进去的是换行（单写的后果），回显只会有一个反斜杠。
    assert_eq!(
        text_of(&list, "d1"),
        plain,
        "双写往返后仍是双写 ⇒ 存的是字面反斜杠"
    );
    assert_eq!(
        text_of(&list, "d1").matches(bs).count(),
        2,
        "期望两个反斜杠，实得 {:?}",
        text_of(&list, "d1")
    );

    // 对照：单写一个反斜杠时它紧跟 n 会变成换行，回显与双写**不同**。
    // 这正是文档要求一律写两个的原因，也保证上面的断言不是恒真。
    c.web_data_rpc(
        "phrase.importText",
        &json!({ "content": format!("wind:p1{nl}s1 D:{bs}notes{nl}") }),
    )
    .unwrap();
    let list2 = c.web_data_rpc("phrase.listUser", &json!({})).unwrap();
    assert_ne!(
        text_of(&list2, "s1"),
        plain,
        "单写与双写必须落到不同内容，否则上面的断言证明不了什么"
    );
}

/// 路径里反斜杠写少了 ⇒ 预览里给一句提示，但**不阻止导入**（轻微提示）。
#[test]
fn phrase_text_hints_single_backslash_path() {
    let bs = char::from_u32(92).unwrap();
    let q = char::from_u32(34).unwrap();
    let nl = char::from_u32(10).unwrap();
    let c = coord("phrasehint");

    let bad = format!("$CC({q}x{q}, proc.run({q}D:{bs}notes{bs}a.exe{q}))");
    let good = format!("$CC({q}x{q}, proc.run({q}D:{bs}{bs}notes{bs}{bs}a.exe{q}))");
    let text = format!("wind:p1{nl}b1 {bad}{nl}g1 {good}{nl}");

    let prev = c
        .web_data_rpc("phrase.previewImportText", &json!({ "content": text }))
        .unwrap();
    let entries = prev.get("entries").unwrap().as_array().unwrap();
    let hints_of = |i: usize| entries[i].get("hints").unwrap().as_array().unwrap();

    assert_eq!(hints_of(0).len(), 1, "单写路径应有提示");
    assert_eq!(
        hints_of(0)[0].get("kind").and_then(|v| v.as_str()),
        Some("controlCharInPath")
    );
    assert!(hints_of(1).is_empty(), "双写路径不该有提示");

    // 轻微提示：不抦导入。
    let out = c
        .web_data_rpc(
            "phrase.importText",
            &json!({ "content": text, "accept": [2, 3] }),
        )
        .unwrap();
    assert_eq!(
        out.get("added").and_then(|v| v.as_u64()),
        Some(2),
        "带提示的条目照样可以导入"
    );
}

/// 没有格式标记 ⇒ 整段拒绝。侦测「不猜」，报错优于误装。
#[test]
fn phrase_text_requires_marker() {
    let c = coord("phrasetextmark");
    assert!(
        c.web_data_rpc("phrase.previewImportText", &json!({ "content": "kx 好\n" }))
            .is_err()
    );
}

#[test]
fn dict_export_import_preview_contract() {
    let c = coord("dictio");
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "wb", "code": "a", "text": "工", "weight": 100 }),
    )
    .unwrap();

    // export → {content} 且是 wdict words 文本
    let exp = c
        .web_data_rpc("dict.export", &json!({ "schemaId": "wb" }))
        .unwrap();
    let content = exp
        .get("content")
        .and_then(|v| v.as_str())
        .expect("content 字符串");
    assert!(content.contains("--- !words"));

    // preview 到空 schema:userWords 段全 willAdd
    let prev = c
        .web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "wb2", "content": content }),
        )
        .unwrap();
    assert_eq!(
        prev.get("format").and_then(|v| v.as_str()),
        Some("winddict")
    );
    let uw = sec(&prev, "userWords");
    assert_eq!(uw.get("willAdd").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(uw.get("willUpdate").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(uw.get("unchanged").and_then(|v| v.as_u64()), Some(0));
    assert!(uw.get("samples").and_then(|v| v.as_array()).is_some());

    // import(缺省 merge)→ sections[userWords]{added,skipped}
    let out = c
        .web_data_rpc(
            "dict.import",
            &json!({ "schemaId": "wb2", "content": content }),
        )
        .unwrap();
    let uw = sec(&out, "userWords");
    assert_eq!(uw.get("added").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(uw.get("skipped").and_then(|v| v.as_u64()), Some(0));

    // 同内容再 import:权重相等 ⇒ 全 unchanged,added=updated=0
    let out2 = c
        .web_data_rpc(
            "dict.import",
            &json!({ "schemaId": "wb2", "content": content }),
        )
        .unwrap();
    let uw2 = sec(&out2, "userWords");
    assert_eq!(uw2.get("added").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(uw2.get("updated").and_then(|v| v.as_u64()), Some(0));
    // preview 同内容 ⇒ unchanged=1,与落盘一致
    let prev2 = c
        .web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "wb2", "content": content }),
        )
        .unwrap();
    assert_eq!(
        sec(&prev2, "userWords")
            .get("unchanged")
            .and_then(|v| v.as_u64()),
        Some(1)
    );

    // replace:先加一条杂词,replace 导入后只剩导入内容
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "wb2", "code": "x", "text": "另", "weight": 1 }),
    )
    .unwrap();
    let out3 = c
        .web_data_rpc(
            "dict.import",
            &json!({ "schemaId": "wb2", "content": content, "strategy": "replace" }),
        )
        .unwrap();
    assert_eq!(
        sec(&out3, "userWords")
            .get("added")
            .and_then(|v| v.as_u64()),
        Some(1),
        "清空后全部计 added"
    );
    let listed = c
        .web_data_rpc("dict.listPaged", &json!({ "schemaId": "wb2", "limit": 10 }))
        .unwrap();
    assert_eq!(
        listed.get("total").and_then(|v| v.as_u64()),
        Some(1),
        "replace 应清掉 x"
    );
}

#[test]
fn dict_import_rime_and_tsv_auto_detect() {
    let c = coord("dictio_fmt");

    // Rime:默认列 [text, code, weight],拼音码去空格;preview 回报 format + userWords 段
    let rime = "# Rime dictionary\n---\nname: demo\nversion: \"1.0\"\n...\n你好\tni hao\t100\n世界\tshi jie\t50\n";
    let prev = c
        .web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "pinyin", "content": rime }),
        )
        .unwrap();
    assert_eq!(prev.get("format").and_then(|v| v.as_str()), Some("rime"));
    let uw = sec(&prev, "userWords");
    assert_eq!(uw.get("willAdd").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(uw.get("skipped").and_then(|v| v.as_u64()), Some(0));
    let out = c
        .web_data_rpc(
            "dict.import",
            &json!({ "schemaId": "pinyin", "content": rime }),
        )
        .unwrap();
    assert_eq!(
        sec(&out, "userWords").get("added").and_then(|v| v.as_u64()),
        Some(2)
    );
    let listed = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "pinyin", "prefix": "nihao", "limit": 10 }),
        )
        .unwrap();
    assert_eq!(
        listed.get("total").and_then(|v| v.as_u64()),
        Some(1),
        "拼音码应去空格入库(ni hao→nihao)"
    );

    // TSV:code\ttext\t[weight];坏行计入 skipped
    let tsv = "a\t工\t10\nbadline\nab\t好\n";
    let prev = c
        .web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "wb", "content": tsv }),
        )
        .unwrap();
    assert_eq!(prev.get("format").and_then(|v| v.as_str()), Some("tsv"));
    let uw = sec(&prev, "userWords");
    assert_eq!(uw.get("willAdd").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(uw.get("skipped").and_then(|v| v.as_u64()), Some(1));
    let out = c
        .web_data_rpc("dict.import", &json!({ "schemaId": "wb", "content": tsv }))
        .unwrap();
    let uw = sec(&out, "userWords");
    assert_eq!(uw.get("added").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(uw.get("skipped").and_then(|v| v.as_u64()), Some(1));

    // 不可识别内容 → 错误
    assert!(
        c.web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "wb", "content": "没有制表符的纯文本\n" }),
        )
        .is_err(),
        "未知格式应报错"
    );
}

#[test]
fn dict_import_rejects_engine_type_mismatch() {
    let c = coord("dict_engine");
    // 手工构造「拼音」来源的 wdict；导入到默认解析为码表的方案 → 应拒绝（防编码错乱）。
    let content = "# x\nwind_dict:\n  version: 1\n  engine_type: pinyin\n  sections:\n    words:\n      columns: [code, text, weight, count]\n\n--- !words\nnihao\t你好\t0\t0\n";
    let r = c.web_data_rpc(
        "dict.import",
        &json!({ "schemaId": "wb", "content": content }),
    );
    assert!(r.is_err(), "拼音来源导入码表方案应被拒绝");
    // previewImport 回报 compatible=false + 来源引擎，供 UI 提前拦。
    let prev = c
        .web_data_rpc(
            "dict.previewImport",
            &json!({ "schemaId": "wb", "content": content }),
        )
        .unwrap();
    assert_eq!(
        prev.get("compatible").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        prev.get("sourceEngine").and_then(|v| v.as_str()),
        Some("pinyin")
    );
}

#[test]
fn shadow_roundtrip_shape() {
    let c = coord("shadow");
    // pin + delete 两条规则
    c.web_data_rpc(
        "shadow.pin",
        &json!({ "schemaId": "wb", "code": "aaaa", "word": "恭恭敬敬", "candId": "c1", "position": 0 }),
    )
    .unwrap();
    c.web_data_rpc(
        "shadow.delete",
        &json!({ "schemaId": "wb", "code": "bbbb", "word": "某词" }),
    )
    .unwrap();

    let list = c
        .web_data_rpc("shadow.list", &json!({ "schemaId": "wb" }))
        .unwrap();
    let arr = list.as_array().expect("shadow.list 应为数组");
    assert_eq!(arr.len(), 2, "应有 pin/delete 两条");
    // 每条形状对齐 ShadowRuleItem {code, word, candId, type, position?}
    for it in arr {
        assert!(it.get("code").is_some());
        assert!(it.get("word").is_some());
        assert!(it.get("candId").is_some());
        let ty = it["type"].as_str().unwrap();
        assert!(ty == "pin" || ty == "delete");
    }
    let pin = arr.iter().find(|i| i["type"] == "pin").unwrap();
    assert_eq!(pin["candId"], "c1");
    assert_eq!(pin["position"], 0);

    // removeRule 后清空
    c.web_data_rpc(
        "shadow.removeRule",
        &json!({ "schemaId": "wb", "code": "aaaa", "word": "恭恭敬敬" }),
    )
    .unwrap();
    c.web_data_rpc(
        "shadow.removeRule",
        &json!({ "schemaId": "wb", "code": "bbbb", "word": "某词" }),
    )
    .unwrap();
    let list2 = c
        .web_data_rpc("shadow.list", &json!({ "schemaId": "wb" }))
        .unwrap();
    assert_eq!(list2.as_array().unwrap().len(), 0, "removeRule 后应清空");
}

#[test]
fn shadow_add_rule_routes_pin_and_hide() {
    let c = coord("shadow_add_rule");
    // pin：带 position
    c.web_data_rpc(
        "shadow.addRule",
        &json!({ "schemaId": "wb", "code": "aaaa", "word": "恭恭敬敬", "type": "pin", "position": 2 }),
    )
    .unwrap();
    let list = c
        .web_data_rpc("shadow.list", &json!({ "schemaId": "wb" }))
        .unwrap();
    let arr = list.as_array().unwrap();
    // aaaa 应有一条 pin，position=2
    assert!(arr.iter().any(|e| e["code"] == "aaaa"));
    // hide：转为 delete
    c.web_data_rpc(
        "shadow.addRule",
        &json!({ "schemaId": "wb", "code": "bbbb", "word": "某词", "type": "hide" }),
    )
    .unwrap();
    let list2 = c
        .web_data_rpc("shadow.list", &json!({ "schemaId": "wb" }))
        .unwrap();
    assert!(
        list2
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "bbbb")
    );
}

#[test]
fn rpc_device_param() {
    let c = coord("stats_dev");
    let store = c.user_store().unwrap().clone();
    store
        .put_daily_stat(
            "2020-01-01",
            &wind_store::stats::DailyStats {
                chinese: 10,
                ..Default::default()
            },
        )
        .unwrap();
    let d = store.stats_dir().join("devices/phone");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("2020-01.toml"),
        "version = 1\n[days.2020-01-02]\nchinese = 5\n",
    )
    .unwrap();
    std::fs::create_dir_all(store.stats_dir().join("devices/bad name")).unwrap();

    let devs = c.web_data_rpc("stats.devices", &json!({})).unwrap();
    assert_eq!(devs, json!(["phone"]));
    let q = |dev: Option<&str>| {
        let mut p = json!({ "from": "2020-01-01", "to": "2020-01-02" });
        if let Some(d) = dev {
            p["device"] = json!(d);
        }
        c.web_data_rpc("stats.daily", &p).unwrap()
    };
    let tc = |v: &Value| (v[0]["tc"].as_u64().unwrap(), v[1]["tc"].as_u64().unwrap());
    assert_eq!(tc(&q(None)), (10, 5));
    assert_eq!(tc(&q(Some("all"))), (10, 5));
    assert_eq!(tc(&q(Some("self"))), (10, 0));
    assert_eq!(tc(&q(Some("phone"))), (0, 5));
    let sum = |dev: &str| {
        c.web_data_rpc("stats.summary", &json!({ "device": dev }))
            .unwrap()["total_chars"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(sum("phone"), 5);
    assert_eq!(
        sum("all"),
        5 + c.stat_collector().unwrap().get_meta().total_chars
    );
    let err = c.web_data_rpc("stats.summary", &json!({ "device": "nope" }));
    assert!(err.is_err());
    assert!(
        c.web_data_rpc(
            "stats.daily",
            &json!({ "from": "2020-01-01", "to": "2020-01-02", "device": "bad name" })
        )
        .is_err()
    );
}

#[test]
fn stats_summary_daily_shape() {
    let c = coord("stats");
    let today = wind_store::stats_report::today_str();
    // 采集器记录今日：中文 2（码长 4，首选）。
    c.debug_record_commit("你好", 4, 0, wind_store::stats::CommitSource::Candidate);

    // stats.summary 形状对齐富 StatsSummary（17 字段）。
    let sum = c.web_data_rpc("stats.summary", &json!({})).unwrap();
    for k in [
        "today_chars",
        "today_chinese",
        "today_english",
        "total_chars",
        "active_days",
        "daily_avg",
        "streak_current",
        "streak_max",
        "week_chars",
        "month_chars",
        "max_day_chars",
        "avg_code_len",
        "first_select_rate",
        "today_speed",
        "overall_speed",
        "max_speed",
    ] {
        assert!(sum.get(k).is_some(), "summary 缺 {k}");
    }
    assert_eq!(sum["today_chars"], 2);

    // flush 落库后 stats.daily 形状对齐 DailyStatItem。
    c.stat_collector().unwrap().flush();
    let daily = c
        .web_data_rpc("stats.daily", &json!({ "from": &today, "to": &today }))
        .unwrap();
    let arr = daily.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["d"], json!(today));
    assert_eq!(arr[0]["tc"], 2);
    assert_eq!(arr[0]["cc"], 2);

    // pruneBefore(days) 返回 {pruned}。
    let pr = c
        .web_data_rpc("stats.pruneBefore", &json!({ "days": 0 }))
        .unwrap();
    assert!(pr.get("pruned").and_then(|v| v.as_u64()).is_some());

    // clear 后 summary 归零（含采集器内存）。
    c.web_data_rpc("stats.clear", &json!({})).unwrap();
    let sum2 = c.web_data_rpc("stats.summary", &json!({})).unwrap();
    assert_eq!(sum2["today_chars"], 0);
    assert_eq!(sum2["total_chars"], 0);

    // replaceMonths：整体换成给定月表，累计随之重算；格式不对的拒绝
    let months =
        json!({ "months": { "2020-01": "version = 1\n[days.\"2020-01-05\"]\nchinese = 7\n" } });
    let r = c.web_data_rpc("stats.replaceMonths", &months).unwrap();
    assert_eq!(r["written"], 1);
    let sum3 = c.web_data_rpc("stats.summary", &json!({})).unwrap();
    assert_eq!(sum3["total_chars"], 7);
    assert!(
        c.web_data_rpc(
            "stats.replaceMonths",
            &json!({ "months": { "2020-1": "" } })
        )
        .is_err()
    );
    assert!(c.web_data_rpc("stats.replaceMonths", &json!({})).is_err());
}

#[test]
fn freq_list_delete_clear_shape() {
    let c = coord("freq");
    let store = c.user_store().unwrap();
    store.record_freq("py", "de", "的").unwrap();
    store.record_freq("py", "shi", "是").unwrap();

    // freq.listPaged 形状对齐 PagedResult<FreqItem{code,text,count,lastUsed}>
    let r = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "limit": 50, "offset": 0 }),
        )
        .unwrap();
    assert_eq!(r["total"], 2);
    let it = &r["items"][0];
    for k in ["code", "text", "count", "lastUsed"] {
        assert!(it.get(k).is_some(), "FreqItem 缺 {k}");
    }
    // delete
    c.web_data_rpc(
        "freq.delete",
        &json!({ "schemaId": "py", "code": "de", "text": "的" }),
    )
    .unwrap();
    let r2 = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "limit": 50, "offset": 0 }),
        )
        .unwrap();
    assert_eq!(r2["total"], 1);
    // clear 返回删除数（number）
    let cleared = c
        .web_data_rpc("freq.clear", &json!({ "schemaId": "py" }))
        .unwrap();
    assert_eq!(cleared, json!(1));
}

/// 设置端正文框是原文，出门前过 `escape_text_field`；core 还原后候选须逐字等于原文。
/// 字符画里的 `\\`、行尾 `\` 连换行不得被当成转义符吃掉。
#[test]
fn phrase_raw_backslash_text_roundtrips_to_candidate() {
    let c = coord("phrase_art");
    let art = "  /  \\\\|||  :  |||//  \\\n /  _||||| -:- |||||-  \\\n |   | \\\\\\  -  /// |";
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": "cott", "text": wind_store::wdict::escape_text_field(art) }),
    )
    .unwrap();
    c.rebuild_phrases();
    assert_eq!(c.debug_phrase_texts("cott"), [art]);
}

/// 转义形态只活在设置页边界上：库里存真实文本，RPC 出口投影成 `\n`、入口还原回真实文本。
///
/// 出入口必须成对：某个入口漏了还原，就会拿转义形态去匹配真实文本的 key，静默失败
/// （表现为「删了没反应」）。本测试逐个入口用「列表回什么就拿什么去操作」走一遍。
#[test]
fn phrase_ui_escape_boundary_roundtrips() {
    let c = coord("phrase_esc");
    // 设置页提交转义形态：`\n` 表示换行（用户在输入框里看到并编辑的就是它）
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": "duo", "text": r"甲\n乙", "position": 0, "weight": 1 }),
    )
    .unwrap();

    // 存储域：真实文本（含真换行），转义形态不入库
    let store = c.user_store().unwrap();
    assert!(
        store
            .list_phrases()
            .unwrap()
            .iter()
            .any(|p| p.code == "duo" && p.text == "甲\n乙"),
        "库里应是真换行；若这里失败说明入口没还原、把字面 \\n 存进去了"
    );

    // 出口：列表回转义形态，用户可继续编辑
    let list = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert_eq!(
        list[0]["text"],
        json!(r"甲\n乙"),
        "列表应回转义形态而非真换行——真换行在输入框里没法编辑"
    );

    // 候选侧：短语走 PhraseLayer（比用户词多一层 rebuild + cmdbar/模板分派），
    // 拿到的必须是真实文本。用户词与短语两条路径不同，须各自锁住。
    c.rebuild_phrases();
    let hits = c.debug_phrase_texts("duo");
    assert_eq!(hits.len(), 1, "短语候选应命中一条：{hits:?}");
    assert_eq!(
        hits[0], "甲\n乙",
        "短语候选须是真实文本（含真换行），不是转义形态"
    );

    // 入口 setEnabled：拿列表回的形态去操作，必须命中
    c.web_data_rpc(
        "phrase.setEnabled",
        &json!({ "code": "duo", "text": r"甲\n乙", "enabled": false }),
    )
    .unwrap();
    let list2 = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert_eq!(
        list2[0]["enabled"],
        json!(false),
        "setEnabled 未命中 → 该入口漏了 store_text"
    );

    // 入口 remove：同理
    c.web_data_rpc(
        "phrase.remove",
        &json!({ "code": "duo", "text": r"甲\n乙" }),
    )
    .unwrap();
    let after = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert!(
        after.as_array().unwrap().is_empty(),
        "remove 未命中 → 该入口漏了 store_text"
    );
}

/// 命令栏语法条目在这条边界上原样穿过：它的反斜杠归 cmdbar lexer 独占。
///
/// 本层若也吃一层，`open("D:\\notes")` 落库剩 `D:\notes`，lexer 再把 `\n` 解成换行，路径静默损坏。
/// 判据是用户在输入框里写的与库里的逐字相同，出口回显同样不得改写。
#[test]
fn cmdbar_phrase_backslash_survives_ui_boundary() {
    let c = coord("phrase_cmd_esc");
    let src = r#"$CC("[打开临时目录]", open("D:\\notes\\temp"))"#;
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": "cotmp", "text": src, "position": 0, "weight": 1 }),
    )
    .unwrap();

    let store = c.user_store().unwrap();
    assert!(
        store
            .list_phrases()
            .unwrap()
            .iter()
            .any(|p| p.code == "cotmp" && p.text == src),
        "库里应与用户所写逐字相同；失败即本层又吃掉了一个反斜杠"
    );

    let list = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert_eq!(list[0]["text"], json!(src), "出口回显不得再加反斜杠");

    // 入口 remove 拿列表回的形态操作，仍须命中（出入口成对）
    c.web_data_rpc("phrase.remove", &json!({ "code": "cotmp", "text": src }))
        .unwrap();
    assert!(
        c.web_data_rpc("phrase.list", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty(),
        "remove 未命中 → 出入口分流判据不一致"
    );
}

/// 用户词库侧的同一契约（见 [`phrase_ui_escape_boundary_roundtrips`]）。
#[test]
fn user_word_ui_escape_boundary_roundtrips() {
    let c = coord("word_esc");
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "wb", "code": "a", "text": r"甲\n乙", "weight": 100 }),
    )
    .unwrap();

    // 存储域是真实文本
    let store = c.user_store().unwrap();
    let recs = store.get_user_words("wb", "a").unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].text, "甲\n乙", "库里应是真换行");

    // 出口投影
    let list = c
        .web_data_rpc("dict.listPaged", &json!({ "schemaId": "wb" }))
        .unwrap();
    let item = &list["items"][0];
    assert_eq!(item["text"], json!(r"甲\n乙"), "列表应回转义形态");

    // 入口：拿列表回的形态删除，必须命中
    c.web_data_rpc(
        "dict.remove",
        &json!({ "schemaId": "wb", "code": "a", "text": r"甲\n乙" }),
    )
    .unwrap();
    assert!(
        store.get_user_words("wb", "a").unwrap().is_empty(),
        "remove 未命中 → 该入口漏了 store_text"
    );
}

/// 字面反斜杠必须能表达：用户写 `\\n` 表示"反斜杠加字母 n"，不是换行。
/// 没有这条，`C:\note` 这类内容就会被存成 `C:` + 换行 + `ote`。
#[test]
fn literal_backslash_survives_ui_boundary() {
    let c = coord("backslash_esc");
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": "p", "text": r"C:\\note", "position": 0, "weight": 1 }),
    )
    .unwrap();
    let store = c.user_store().unwrap();
    assert!(
        store
            .list_phrases()
            .unwrap()
            .iter()
            .any(|p| p.code == "p" && p.text == r"C:\note"),
        "`\\\\n` 应还原为字面反斜杠加 n，而非换行"
    );
    let list = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert_eq!(list[0]["text"], json!(r"C:\\note"), "出口须重新转义反斜杠");
}

#[test]
fn phrase_crud_shape() {
    let c = coord("phrase");
    // add → list 形状对齐 PhraseItem{code,text,position,weight,enabled}
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": "rq", "text": "2026-06-20", "position": 0, "weight": 1 }),
    )
    .unwrap();
    let list = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    for k in ["code", "text", "position", "weight", "enabled"] {
        assert!(arr[0].get(k).is_some(), "PhraseItem 缺 {k}");
    }
    assert_eq!(arr[0]["enabled"], json!(true));

    // setEnabled
    c.web_data_rpc(
        "phrase.setEnabled",
        &json!({ "code": "rq", "text": "2026-06-20", "enabled": false }),
    )
    .unwrap();
    let list2 = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert_eq!(list2[0]["enabled"], json!(false));

    // update 改 code（键迁移）
    c.web_data_rpc(
        "phrase.update",
        &json!({ "code": "rq", "text": "2026-06-20", "newCode": "date", "weight": 5 }),
    )
    .unwrap();
    let list3 = c.web_data_rpc("phrase.list", &json!({})).unwrap();
    assert_eq!(list3[0]["code"], json!("date"));

    // remove + resetDefault
    c.web_data_rpc(
        "phrase.remove",
        &json!({ "code": "date", "text": "2026-06-20" }),
    )
    .unwrap();
    assert_eq!(
        c.web_data_rpc("phrase.list", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    c.web_data_rpc("phrase.resetDefault", &json!({})).unwrap();
}

/// 回归：phrase.resetDefault 只删用户短语，系统短语必须保留。
#[test]
fn phrase_reset_default_keeps_system() {
    use wind_store::phrases::SystemPhrase;

    let path = std::env::temp_dir().join("wind_webdata_phrase_reset_keeps_system.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());

    // 先同步一条系统短语（is_system=true）
    store
        .sync_system_phrases(&[SystemPhrase {
            code: "rq".into(),
            text: "$date".into(),
            weight: 1000,
            position: 0,
            category: String::new(),
            enabled: true,
        }])
        .unwrap();

    // 构造 coordinator（共享同一个 Arc<Store>）
    let c = Coordinator::new_headless_with_store(Config::default(), None, Arc::clone(&store));

    // 加一条用户短语
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": "me", "text": "自定义", "position": 0, "weight": 1 }),
    )
    .unwrap();

    // 执行用户"清空"操作
    c.web_data_rpc("phrase.resetDefault", &json!({})).unwrap();

    // 系统短语应保留
    let sys = c.web_data_rpc("phrase.listSystem", &json!({})).unwrap();
    let sys_arr = sys.as_array().expect("listSystem 应返回数组");
    assert_eq!(sys_arr.len(), 1, "系统短语应保留，不应被 resetDefault 删除");
    assert_eq!(sys_arr[0]["code"], json!("rq"));

    // 用户短语应为 0
    let user = c.web_data_rpc("phrase.listUser", &json!({})).unwrap();
    assert_eq!(user["total"], json!(0), "用户短语应被 resetDefault 清空");
}

// ───────── quick.*（快捷输入格式表）─────────

/// headless coordinator + 独立 redb（redb 是单写者，共用文件会让并发测试互相阻塞）。
/// 格式表走 `FormatTable::load(None)` → 内置出厂表。
fn quick_coord(tag: &str) -> (Arc<Coordinator>, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("wind_webdata_quick_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    (
        Coordinator::new_headless_with_store(Config::default(), None, store),
        path,
    )
}

/// `commonChars.list` 是**分页**类别：返回 `{items,total}`，不是裸数组。
/// 形状由 spec 的 `paged: true` 决定走哪个解析分支，两边对不上会解析出空表且不报错。
fn common_char_rows_rpc(c: &Coordinator, params: Value) -> (Vec<Value>, usize) {
    let v = c.web_data_rpc("commonChars.list", &params).unwrap();
    let items = v
        .get("items")
        .and_then(|x| x.as_array())
        .expect("commonChars.list 应返回 {items,total}")
        .clone();
    let total = v.get("total").and_then(|x| x.as_u64()).unwrap() as usize;
    (items, total)
}

/// `commonChars.*` 五个方法的往返契约。
///
/// 本装置没有 data_dir ⇒ 默认字表为空 ⇒ 全表里只剩用户加的字，故只测 `common:true` 方向：
/// 设成 `false` 与默认同向，按设计删覆盖而不写记录（见 `Coordinator::apply_common_target`）。
/// 「全表按字表原序 + 追加」在 `wind-candidate` 的 `list_all` 用例里测。
#[test]
fn common_chars_rpc_roundtrip() {
    let c = coord("commonchars");
    assert_eq!(common_char_rows_rpc(&c, json!({})).1, 0, "初始应为空表");

    c.web_data_rpc("commonChars.set", &json!({ "char": "槮", "common": true }))
        .unwrap();
    let (rows, total) = common_char_rows_rpc(&c, json!({}));
    assert_eq!((rows.len(), total), (1, 1));
    assert_eq!(rows[0]["char"], json!("槮"));
    assert_eq!(rows[0]["common"], json!(true));
    assert!(
        rows[0].get("baseCommon").is_some(),
        "默认判定必须一起给：界面靠它显示「默认 → 现在」的对照"
    );
    assert_eq!(
        rows[0]["adjusted"],
        json!(true),
        "改过的行要标出来——设置页靠它决定「恢复默认」灰不灰"
    );

    let q = c
        .web_data_rpc("commonChars.query", &json!({ "char": "槮" }))
        .unwrap();
    assert_eq!(q["governed"], json!(true));
    assert_eq!(q["override"], json!(true));
    assert_eq!(q["effective"], json!(true));

    // 默认字表管不着的字符：`governed` 如实报 false（纯提示），但 set **必须放行**
    // ——issue #83 起词库管理全范围放开，用户要能把字根、注音、假名这些关掉。
    let q2 = c
        .web_data_rpc("commonChars.query", &json!({ "char": "、" }))
        .unwrap();
    assert_eq!(q2["governed"], json!(false), "默认字表确实管不着它");
    assert_eq!(
        q2["effective"],
        json!(true),
        "没覆盖时域外字符照常放行，故当前判定是「常用」"
    );
    c.web_data_rpc("commonChars.set", &json!({ "char": "、", "common": false }))
        .expect("域外字符必须可登记");
    let q3 = c
        .web_data_rpc("commonChars.query", &json!({ "char": "、" }))
        .unwrap();
    assert_eq!(
        q3["effective"],
        json!(false),
        "设了就得生效——写得进去却不生效，就是一条静默的死记录"
    );
    c.web_data_rpc("commonChars.reset", &json!({ "char": "、" }))
        .unwrap();

    // 多字符一律拒绝，不取首字符：悄悄截取会让用户以为整个词都标记了。
    assert!(
        c.web_data_rpc("commonChars.query", &json!({ "char": "我们" }))
            .is_err()
    );

    c.web_data_rpc("commonChars.reset", &json!({ "char": "槮" }))
        .unwrap();
    assert_eq!(
        common_char_rows_rpc(&c, json!({})).1,
        0,
        "恢复默认后该行回落默认判定；本装置默认表为空，故整行消失"
    );

    for ch in ["槮", "鬱"] {
        c.web_data_rpc("commonChars.set", &json!({ "char": ch, "common": true }))
            .unwrap();
    }
    assert_eq!(common_char_rows_rpc(&c, json!({})).1, 2);

    // 搜索：只留出现在查询串里的字。用户既可以只打一个字，也可以粘一整句。
    let (rows, total) = common_char_rows_rpc(&c, json!({ "prefix": "槮" }));
    assert_eq!((rows.len(), total), (1, 1), "搜一个字应只剩它");
    assert_eq!(rows[0]["char"], json!("槮"));
    assert_eq!(
        common_char_rows_rpc(&c, json!({ "prefix": "槮鬱" })).1,
        2,
        "整句搜索列出句中所有命中的字"
    );
    assert_eq!(common_char_rows_rpc(&c, json!({ "prefix": "的" })).1, 0);

    // 分页：total 是**过滤后的总数**，不是当页条数——设置页靠它算总页数。
    let (page, total) = common_char_rows_rpc(&c, json!({ "offset": 0, "limit": 1 }));
    assert_eq!((page.len(), total), (1, 2), "当页 1 条、总数仍是 2");
    let (page2, _) = common_char_rows_rpc(&c, json!({ "offset": 1, "limit": 1 }));
    assert_ne!(page2[0]["char"], page[0]["char"], "第二页要换一条");

    // 「只看已修改」的独立作用在这个装置下测不出来（默认表为空，两个口径等价），真正的用例在
    // `wind-coordinator/tests/common_chars_override.rs`。这里只确认参数被认得、不报错。
    assert_eq!(
        common_char_rows_rpc(&c, json!({ "onlyModified": true })).1,
        2
    );

    c.web_data_rpc("commonChars.clear", &json!({})).unwrap();
    assert_eq!(common_char_rows_rpc(&c, json!({})).1, 0, "整表恢复默认");
    assert_eq!(
        common_char_rows_rpc(&c, json!({ "onlyModified": true })).1,
        0
    );
}

/// 列表每行要带类型（Unicode 块名）与「这一类能不能整类批量」。
///
/// 类型列的价值在于光看字形分不清哪些字符会显示、哪些不会。
#[test]
fn list_rows_carry_block_type_and_bulk_flag() {
    let c = coord("commonchars_block");
    // 一个汉字、一个注音——两类的 bulk 标志必须相反。
    //
    // 两者方向刻意相反，都为让记录真的落库：本装置默认字表为空 ⇒ 域内的「我」默认判生僻，
    // 域外的「ㄅ」默认判常用；各设成与默认相反的那一边，才不会被「同向即删覆盖」吃掉。
    c.web_data_rpc("commonChars.set", &json!({ "char": "我", "common": true }))
        .unwrap();
    c.web_data_rpc("commonChars.set", &json!({ "char": "ㄅ", "common": false }))
        .unwrap();
    let (rows, _) = common_char_rows_rpc(&c, json!({}));
    let find = |ch: &str| {
        rows.iter()
            .find(|r| r["char"] == json!(ch))
            .unwrap_or_else(|| panic!("{ch} 不在列表里"))
    };
    let han = find("我");
    assert_eq!(han["block"], json!("基本汉字"));
    assert_eq!(han["blockRange"], json!("4E00-9FFF"));
    assert_eq!(
        han["blockBulkEditable"],
        json!(false),
        "汉字块必须禁止整类操作——放行就是一个一键作废整张默认字表的按钮"
    );
    let bopo = find("ㄅ");
    assert_eq!(bopo["block"], json!("注音符号"));
    assert_eq!(bopo["blockRange"], json!("3100-312F"));
    assert_eq!(bopo["blockBulkEditable"], json!(true));
}

/// 整类批量：汉字块拒绝、域外块放行，且预览不写库。
///
/// 本装置没有 data_dir ⇒ 扫不到任何词库 ⇒ 命中恒 0。这里锁的是闸门与形态，
/// 真实扫描结果要靠带真实词库的装置。
#[test]
fn bulk_by_block_refuses_han_blocks() {
    let c = coord("commonchars_bulk");

    // ⛔ 汉字块：一次误点就是七千多条覆盖，必须在 core 就拒绝，不能只靠界面灰显。
    let err = c
        .web_data_rpc(
            "commonChars.bulkByBlock",
            &json!({ "char": "我", "common": false, "apply": true }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("基本汉字"), "报错要说清是哪一类: {err}");
    assert!(err.contains("逐字"), "要给出替代做法: {err}");

    // 域外块：放行，返回预览形态。
    let o = c
        .web_data_rpc(
            "commonChars.bulkByBlock",
            &json!({ "char": "ㄅ", "common": false }),
        )
        .unwrap();
    assert_eq!(o["block"], json!("注音符号"));
    assert!(
        o.get("entries").is_some(),
        "命中条目数必须给——`，` 只有 1 个字符却在 326 条词条里"
    );
    assert_eq!(o["written"], json!(0), "apply 缺省为 false，预览不得写库");
    assert_eq!(
        common_char_rows_rpc(&c, json!({})).1,
        0,
        "预览之后库里必须还是空的"
    );
}

/// 一份 TOML 关掉整块表意文字描述符（IDC，U+2FF0–U+2FFF）。
///
/// 码表可能给这 16 个记号整块编码（`⿰`=rgs…），它们描述间架结构、不是字，不在默认字表的
/// 管辖域内。这条把「16 个字符一次导入并真的生效」端到端钉住（此前只有 parse 层覆盖）。
#[test]
fn ideographic_description_chars_can_be_hidden_via_import() {
    let c = coord("commonchars_idc");
    // 整块 16 个，与用户贴出的清单逐字对应。
    let idc = "⿰⿱⿲⿳⿴⿵⿶⿷⿸⿹⿺⿻⿼⿽⿾⿿";
    assert_eq!(idc.chars().count(), 16);

    let o = c
        .web_data_rpc(
            "commonChars.import",
            &json!({ "content": format!("wind_common_chars = 1\nrare = \"{idc}\"\n") }),
        )
        .unwrap();
    assert_eq!(o["imported"], json!(16), "16 个应全部落库：{o:?}");
    assert_eq!(
        o["sameAsDefault"],
        json!(0),
        "默认放行 ⇒ 设为生僻是反向 ⇒ 一条都不该被当成「与默认同向」而丢掉"
    );
    assert!(
        o["skipped"].as_array().unwrap().is_empty(),
        "{:?}",
        o["skipped"]
    );

    // 逐个查：登记后当前判定必须真的翻过来，否则就是一批静默的死记录。
    for ch in idc.chars() {
        let q = c
            .web_data_rpc("commonChars.query", &json!({ "char": ch.to_string() }))
            .unwrap();
        assert_eq!(q["governed"], json!(false), "{ch} 本就不受默认字表管辖");
        assert_eq!(q["baseCommon"], json!(true), "{ch} 默认是放行的");
        assert_eq!(q["effective"], json!(false), "{ch} 设了就得生效");
    }
}

/// 导出格式：单字符走文本段，多码位走数组段，**数组段为空时整段不出现**。
///
/// 从不配置多码位的人，不该在文件里看到自己用不上的格式。
#[test]
fn export_omits_array_sections_when_no_multi_code_point_entries() {
    let c = coord("commonchars_fmt");
    // 方向各取「与本装置默认相反」的那一边，记录才留得下：本装置无 data_dir ⇒
    // 域内的「槮」默认判生僻、域外的「ㄅ」默认判常用（见 `is_base_common`）。
    c.web_data_rpc("commonChars.set", &json!({ "char": "槮", "common": true }))
        .unwrap();
    c.web_data_rpc("commonChars.set", &json!({ "char": "ㄅ", "common": false }))
        .unwrap();
    let text = c
        .web_data_rpc("commonChars.export", &json!({}))
        .unwrap()
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert!(text.contains("common = "), "单字符要走文本段: {text}");
    assert!(
        !text.contains("_seq"),
        "没有多码位条目时不该出现数组段: {text}"
    );

    // 加一条 emoji 序列后，数组段才出现，且文本段仍在。
    let ball = "\u{26BD}\u{FE0F}";
    c.web_data_rpc("commonChars.set", &json!({ "char": ball, "common": false }))
        .unwrap();
    let text2 = c
        .web_data_rpc("commonChars.export", &json!({}))
        .unwrap()
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert!(
        text2.contains("rare_seq = ["),
        "多码位条目要走数组段: {text2}"
    );
    assert!(text2.contains(ball), "{text2}");

    // 导到另一台机器：四个段都要读得回来。
    let c2 = coord("commonchars_fmt2");
    let o = c2
        .web_data_rpc("commonChars.import", &json!({ "content": text2 }))
        .unwrap();
    assert_eq!(o["imported"], json!(3), "两个单字符 + 一条序列: {o:?}");
    assert!(
        o["skipped"].as_array().unwrap().is_empty(),
        "{:?}",
        o["skipped"]
    );
}

/// `commonChars.export` / `previewImport` / `import` 的往返契约。
///
/// 本装置没有 data_dir ⇒ 域内（汉字/PUA）的字默认判「生僻」，域外字符默认判「常用」
/// （见 `is_base_common`）。导入的两条分支因此都可见：与默认相反的落库；与默认同向的按设计
/// 删覆盖而不写记录（`sameAsDefault`），若误实现成照单全收，`total` 会多出行来。
#[test]
fn common_chars_import_export_roundtrip() {
    let c = coord("commonchars_io");
    for ch in ["槮", "鬱"] {
        c.web_data_rpc("commonChars.set", &json!({ "char": ch, "common": true }))
            .unwrap();
    }

    let text = c
        .web_data_rpc("commonChars.export", &json!({}))
        .unwrap()
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert!(
        text.contains("wind_common_chars = 1"),
        "导出必须带格式标记，导入端靠它认出选错了文件：{text}"
    );
    assert!(text.contains('槮') && text.contains('鬱'), "{text}");

    // 导到第二台「机器」（另一份 store）。
    let c2 = coord("commonchars_io2");
    let p = c2
        .web_data_rpc("commonChars.previewImport", &json!({ "content": text }))
        .unwrap();
    assert_eq!(p["common"], json!(2));
    assert_eq!(p["rare"], json!(0));
    assert_eq!(
        common_char_rows_rpc(&c2, json!({})).1,
        0,
        "预览不能写库——用户还没点确认"
    );

    let o = c2
        .web_data_rpc("commonChars.import", &json!({ "content": text }))
        .unwrap();
    assert_eq!(o["imported"], json!(2));
    assert_eq!(common_char_rows_rpc(&c2, json!({})).1, 2);

    // 手写文件：与默认同向的不写记录；非汉字照收。同为 rare 段，「畢」与默认同向、不留记录，
    // 「、」与默认相反、必须留下，否则用户想隐藏顿号会得到「导入成功但毫无变化」。
    let hand = "wind_common_chars = 1\ncommon = \"玥\"\nrare = \"畢、\"\n";
    let o2 = c2
        .web_data_rpc("commonChars.import", &json!({ "content": hand }))
        .unwrap();
    assert_eq!(o2["imported"], json!(2), "「玥」与「、」都与默认相反");
    assert_eq!(
        o2["sameAsDefault"],
        json!(1),
        "与默认同向的字不该留记录——照单全收会把它钉死在当前判定上"
    );
    assert!(
        o2["skipped"].as_array().unwrap().is_empty(),
        "非汉字不再被跳过：{:?}",
        o2["skipped"]
    );
    assert_eq!(common_char_rows_rpc(&c2, json!({})).1, 4);

    // JSONL（备份包 `userdata/common_chars.jsonl` 那一段的原始形态）也认。
    let c3 = coord("commonchars_io3");
    let jsonl = "{\"ch\":\"槮\",\"common\":true}\n不是 json\n";
    let o3 = c3
        .web_data_rpc("commonChars.import", &json!({ "content": jsonl }))
        .unwrap();
    assert_eq!(o3["imported"], json!(1));
    assert_eq!(o3["skipped"].as_array().unwrap().len(), 1, "坏行如实上报");

    // replace：用文件里的状态覆盖现状，而不是「文件里的 + 我原有的」。
    let only_yue = "wind_common_chars = 1\ncommon = \"玥\"\n";
    c2.web_data_rpc(
        "commonChars.import",
        &json!({ "content": only_yue, "strategy": "replace" }),
    )
    .unwrap();
    assert_eq!(common_char_rows_rpc(&c2, json!({})).1, 1);

    // 选错文件：缺标记直接拒绝，而不是「已导入 0 条」那种让人以为文件坏了的回话。
    assert!(
        c2.web_data_rpc(
            "commonChars.import",
            &json!({ "content": "[quick]\nfoo = 1\n" })
        )
        .is_err()
    );
}

/// 列头排序：三列都要认，且**降序不能破坏同值组内的顺序**。
///
/// 用纯函数直接验、不经 RPC：webdata 装置的默认字表为空，凑不出「同值多行」的样本。
#[test]
fn common_chars_sorting_is_stable_and_accepts_both_char_keys() {
    use wind_coordinator::handle_common_chars::CommonCharRow;
    let row = |ch: char, base: bool, now: bool| {
        let blk = wind_candidate::block_of(ch);
        CommonCharRow {
            text: ch.to_string(),
            common: now,
            base_common: base,
            overridden: base != now,
            block: blk.name,
            // 本条测的是排序稳定性，与字符类无关。
            class: None,
            block_bulk_editable: wind_candidate::block_allows_bulk_edit(&blk),
        }
    };
    // 入参顺序 = 字表原序，刻意不按码位排。
    let seed = vec![
        row('一', true, true),
        row('乙', true, false),
        row('二', true, true),
        row('槮', false, true),
    ];

    // 不给 sortBy：原序不动。
    let mut r = seed.clone();
    sort_common_char_rows(&mut r, "", false);
    assert_eq!(chars(&r), "一乙二槮", "无排序时须保持字表原序");

    // 「字」那一列发来的是 `text`（复用 WordField::Text），也要认 `char`。
    for key in ["text", "char"] {
        let mut r = seed.clone();
        sort_common_char_rows(&mut r, key, false);
        let asc = chars(&r);
        let mut expect: Vec<String> = seed.iter().map(|x| x.text.clone()).collect();
        expect.sort_unstable();
        assert_eq!(asc, expect.concat(), "sortBy={key} 应按码位升序");
    }

    // 按「当前」升序：生僻(false) 在前；同值组内保持字表原序。
    let mut r = seed.clone();
    sort_common_char_rows(&mut r, "common", false);
    assert_eq!(chars(&r), "乙一二槮", "生僻的「乙」提前，其余保持原序");

    // 降序：整体翻面，但**同值组内仍是字表原序**（这正是不能用 reverse() 的原因）。
    let mut r = seed.clone();
    sort_common_char_rows(&mut r, "common", true);
    assert_eq!(
        chars(&r),
        "一二槮乙",
        "常用的那组内部仍按字表原序，不能连同值一起倒过来"
    );

    // 未知字段一律不动，别把列表搅乱。
    let mut r = seed.clone();
    sort_common_char_rows(&mut r, "weight", false);
    assert_eq!(chars(&r), "一乙二槮");
}

fn chars(rows: &[wind_coordinator::handle_common_chars::CommonCharRow]) -> String {
    rows.iter().map(|r| r.text.as_str()).collect()
}

fn quick_rows(c: &Coordinator) -> Vec<Value> {
    c.web_data_rpc("quick.list", &json!({}))
        .unwrap()
        .as_array()
        .expect("quick.list 应返回数组")
        .clone()
}

fn quick_row(rows: &[Value], id: &str) -> Value {
    rows.iter()
        .find(|r| r["id"] == json!(id))
        .unwrap_or_else(|| panic!("列表里找不到 {id}"))
        .clone()
}

/// 某类**启用**条目的 id 序（= 候选顺序）。
fn quick_enabled_ids(c: &Coordinator, kind: &str) -> Vec<String> {
    quick_rows(c)
        .iter()
        .filter(|r| r["kind"] == json!(kind) && r["enabled"] == json!(true))
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn quick_list_covers_every_entry_with_samples() {
    let (c, p) = quick_coord("list");
    let rows = quick_rows(&c);
    // 出厂表全部条目都在（含各类别）。条数对照内置表取、不写死：headless 无 data 目录时
    // 格式表回落 `FormatTable::builtin()`，验的是 RPC 转换没吞掉也没重复任何条目。
    let builtin_count = wind_quick_input::FormatTable::builtin().entries().len();
    assert_eq!(
        rows.len(),
        builtin_count,
        "quick.list 应逐条转换出厂表，实际 {} 条对 {} 条",
        rows.len(),
        builtin_count
    );
    for kind in ["date", "month_day", "year_month", "number", "calc"] {
        assert!(
            rows.iter().any(|r| r["kind"] == json!(kind)),
            "缺类别 {kind}"
        );
    }
    // 示例是「用示例输入跑真实候选生成」得到的，故出厂条目必须都有示例——
    // 空示例说明那条渲染不出来，对出厂表而言就是缺陷。
    let cn = quick_row(&rows, "date.cn");
    let sample = cn["sample"].as_str().unwrap();
    assert!(!sample.is_empty(), "date.cn 应有示例");
    assert!(sample.ends_with('日'), "示例应是完整日期: {sample}");
    // 金额示例用 1234.5 渲染，必然带「元」。
    let amt = quick_row(&rows, "number.amount");
    assert!(
        amt["sample"].as_str().unwrap().contains('元'),
        "金额示例: {}",
        amt["sample"]
    );
    // 未调整时的初始状态
    assert_eq!(cn["enabled"], json!(true));
    assert_eq!(cn["adjusted"], json!(false));
    assert_eq!(cn["moveIndex"], json!(0), "date.cn 是 date 组首条");
    assert_eq!(cn["displayPos"], json!(1));
    let _ = std::fs::remove_file(&p);
}

/// 停用后条目**留在列表里**（否则用户再也开不回来），且 `moveIndex` 为 null
/// ——它不在候选里，移动没有意义。这正是设置页要补的那个缺口。
#[test]
fn quick_disabled_entry_stays_listed_and_unmovable() {
    let (c, p) = quick_coord("disable");
    let before = quick_rows(&c).len();
    c.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "date", "id": "date.basic", "enabled": false }),
    )
    .unwrap();
    let rows = quick_rows(&c);
    assert_eq!(rows.len(), before, "管理列表条数不减");
    let row = quick_row(&rows, "date.basic");
    assert_eq!(row["enabled"], json!(false));
    assert_eq!(row["adjusted"], json!(true));
    assert_eq!(row["moveIndex"], Value::Null, "停用项不可移动");
    // 候选那边确实少了一条
    assert!(!quick_enabled_ids(&c, "date").contains(&"date.basic".to_string()));

    // 双向：再开回来，且恢复原位（记录清空 → 回到基表位置）
    c.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "date", "id": "date.basic", "enabled": true }),
    )
    .unwrap();
    let row = quick_row(&quick_rows(&c), "date.basic");
    assert_eq!(row["enabled"], json!(true));
    assert_eq!(row["adjusted"], json!(false), "开回来即无残留规则");
    let _ = std::fs::remove_file(&p);
}

/// 移动落到候选顺序上，并被 `moveIndex` 如实反映（UI 的上/下移就是 ±1 这个值）。
#[test]
fn quick_move_changes_candidate_order() {
    let (c, p) = quick_coord("move");
    assert_ne!(quick_enabled_ids(&c, "date")[0], "date.lunar");
    c.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.lunar", "index": 0 }),
    )
    .unwrap();
    assert_eq!(quick_enabled_ids(&c, "date")[0], "date.lunar");
    assert_eq!(
        quick_row(&quick_rows(&c), "date.lunar")["moveIndex"],
        json!(0)
    );
    let _ = std::fs::remove_file(&p);
}

/// 单条恢复只清这一条（store 的 `reset_quick_format_entry` 此前无调用点，
/// 注释写着「单条恢复要等设置页」——就是这个 RPC）。
#[test]
fn quick_reset_entry_only_clears_that_entry() {
    let (c, p) = quick_coord("reset_entry");
    c.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.lunar", "index": 0 }),
    )
    .unwrap();
    c.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "date", "id": "date.basic", "enabled": false }),
    )
    .unwrap();
    c.web_data_rpc(
        "quick.resetEntry",
        &json!({ "kind": "date", "id": "date.lunar" }),
    )
    .unwrap();
    let rows = quick_rows(&c);
    assert_eq!(quick_row(&rows, "date.lunar")["adjusted"], json!(false));
    assert_eq!(
        quick_row(&rows, "date.basic")["enabled"],
        json!(false),
        "不该连累别的条目"
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn quick_reset_kind_clears_whole_kind_only() {
    let (c, p) = quick_coord("reset_kind");
    c.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.lunar", "index": 0 }),
    )
    .unwrap();
    c.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "number", "id": "number.digits", "enabled": false }),
    )
    .unwrap();
    c.web_data_rpc("quick.resetKind", &json!({ "kind": "date" }))
        .unwrap();
    let rows = quick_rows(&c);
    assert!(
        rows.iter()
            .filter(|r| r["kind"] == json!("date"))
            .all(|r| r["adjusted"] == json!(false)),
        "date 全类恢复"
    );
    assert_eq!(
        quick_row(&rows, "number.digits")["enabled"],
        json!(false),
        "别的类别不动"
    );
    let _ = std::fs::remove_file(&p);
}

/// 端到端往返：导出 → 到一台干净的机器上导入 → 状态必须一致。部分字段没写出去时
/// 最容易在这里静默失守。
#[test]
fn quick_export_import_roundtrip_restores_state() {
    let (a, pa) = quick_coord("export_src");
    // 造一组有代表性的改动：移动两条（含 LIFO 关系）+ 停用一条 + 跨类别
    a.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.iso", "index": 1 }),
    )
    .unwrap();
    a.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.lunar", "index": 0 }),
    )
    .unwrap();
    a.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "number", "id": "number.digits", "enabled": false }),
    )
    .unwrap();
    let content = a.web_data_rpc("quick.export", &json!({})).unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    let expect_date = quick_enabled_ids(&a, "date");

    let (b, pb) = quick_coord("export_dst");
    let out = b
        .web_data_rpc("quick.import", &json!({ "content": content }))
        .unwrap();
    assert_eq!(out["moved"], json!(2));
    assert_eq!(out["disabled"], json!(1));
    assert_eq!(out["skipped"].as_array().unwrap().len(), 0);

    assert_eq!(
        quick_enabled_ids(&b, "date"),
        expect_date,
        "★ 导入后 date 顺序必须与导出前一致（LIFO 顺序不能在往返中反转）"
    );
    assert_eq!(
        quick_row(&quick_rows(&b), "number.digits")["enabled"],
        json!(false),
        "停用状态也要带过来"
    );
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
}

/// `strategy = "replace"` 先清空既有调整；缺省（合并）则保留未被文件提到的。
#[test]
fn quick_import_replace_clears_existing() {
    let (c, p) = quick_coord("import_replace");
    c.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "calc", "id": "calc.equation", "enabled": false }),
    )
    .unwrap();
    let file = "[[adjust]]\nkind = 'date'\ndisabled = ['date.basic']\n";

    // 合并：两处改动并存
    c.web_data_rpc("quick.import", &json!({ "content": file }))
        .unwrap();
    let rows = quick_rows(&c);
    assert_eq!(quick_row(&rows, "calc.equation")["enabled"], json!(false));
    assert_eq!(quick_row(&rows, "date.basic")["enabled"], json!(false));

    // 替换：文件没提到的 calc 改动被清掉
    c.web_data_rpc(
        "quick.import",
        &json!({ "content": file, "strategy": "replace" }),
    )
    .unwrap();
    let rows = quick_rows(&c);
    assert_eq!(
        quick_row(&rows, "calc.equation")["enabled"],
        json!(true),
        "replace 应先清空"
    );
    assert_eq!(quick_row(&rows, "date.basic")["enabled"], json!(false));
    let _ = std::fs::remove_file(&p);
}

/// 坏条目**如实报告**，不静默丢弃：用户看到「导入成功」却少了规则，
/// 无从判断是文件坏了还是程序吃了。
#[test]
fn quick_preview_import_reports_skipped() {
    let (c, p) = quick_coord("preview");
    let file = "\
[[formats]]
id = 'my.one'
kind = 'date'
text = '$YY.$MM'

[[adjust]]
kind = 'weather'
disabled = ['x']

[[adjust]]
kind = 'date'
moved = [{ id = 'date.lunar', position = 0 }]
";
    let pv = c
        .web_data_rpc("quick.previewImport", &json!({ "content": file }))
        .unwrap();
    assert_eq!(pv["moved"], json!(1));
    assert_eq!(pv["formats"], json!(1), "自定义条目如实计数");
    assert_eq!(
        pv["skipped"].as_array().unwrap().len(),
        1,
        "未知类别要报出来"
    );
    assert_eq!(pv["kinds"], json!(["date"]));
    // 预览不得写库
    assert!(
        quick_rows(&c).iter().all(|r| r["adjusted"] == json!(false)),
        "previewImport 必须是只读的"
    );
    let _ = std::fs::remove_file(&p);
}

/// 未知类别在 RPC 边界就被挡住，且错误信息带上那个类别名。
#[test]
fn quick_edit_rejects_unknown_kind() {
    let (c, p) = quick_coord("bad_kind");
    let e = c
        .web_data_rpc(
            "quick.setEnabled",
            &json!({ "kind": "weather", "id": "x", "enabled": false }),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("weather"), "错误应指出是哪个类别: {e}");
    let _ = std::fs::remove_file(&p);
}

// ───────── quick.* 自定义条目（P2）─────────

/// 变量提示覆盖全部五类，且 year_month **不含**农历变量。
///
/// 后一条是真实的分工（农历月与公历月不一一对应）：提示里放了农历，用户写 `$LMD` 会在保存时才被拒。
#[test]
fn quick_vars_covers_all_kinds_without_lunar_in_year_month() {
    let (c, p) = quick_coord("vars");
    let v = c.web_data_rpc("quick.vars", &json!({})).unwrap();
    let arr = v.as_array().unwrap();
    let kinds: Vec<&str> = arr.iter().map(|k| k["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        vec!["date", "month_day", "year_month", "number", "calc"],
        "五类齐全且按 ALL 序"
    );
    let names_of = |kind: &str| -> Vec<String> {
        arr.iter().find(|k| k["kind"] == json!(kind)).unwrap()["vars"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(names_of("date").contains(&"LMD".to_string()));
    assert!(
        !names_of("year_month").contains(&"LMD".to_string()),
        "年月不该提供农历变量"
    );
    assert!(names_of("calc").contains(&"RESULT".to_string()));
    // 每条都得有说明，否则界面上是一列空白
    for k in arr {
        for var in k["vars"].as_array().unwrap() {
            assert!(
                var["desc"].as_str().is_some_and(|d| !d.is_empty()),
                "kind={} 的 {} 缺说明",
                k["kind"],
                var["name"]
            );
        }
    }
    let _ = std::fs::remove_file(&p);
}

/// 新增的条目立刻是一个**完整的**列表行：能列出、有示例、可调序、标着 user。
///
/// 示例列尤其要验：它由「只清 moved/disabled、保留 added」的那份调整渲染，用空调整会让
/// 用户条目的示例恒为空。
#[test]
fn quick_add_yields_a_complete_row() {
    let (c, p) = quick_coord("add");
    let r = c
        .web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    assert_eq!(r["id"], json!("date.u1"), "首条用户条目的 id");

    let rows = quick_rows(&c);
    let row = quick_row(&rows, "date.u1");
    assert_eq!(row["user"], json!(true), "标记为用户条目");
    assert_eq!(row["enabled"], json!(true));
    assert!(row["moveIndex"].is_number(), "启用的条目可调序");
    assert!(
        row["sample"].as_str().is_some_and(|s| s.contains('/')),
        "★ 示例必须渲染出来，实际: {:?}",
        row["sample"]
    );
    // 出厂条目不该被误标
    assert_eq!(quick_row(&rows, "date.cn")["user"], json!(false));

    // 第二条接着排号，且落在本类末尾
    let r2 = c
        .web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$D-$M-$Y" }))
        .unwrap();
    assert_eq!(r2["id"], json!("date.u2"));
    let rows = quick_rows(&c);
    let date_ids: Vec<&str> = rows
        .iter()
        .filter(|r| r["kind"] == json!("date"))
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        &date_ids[date_ids.len() - 2..],
        &["date.u1", "date.u2"],
        "用户条目排在出厂条目之后"
    );
    let _ = std::fs::remove_file(&p);
}

/// 校验在**保存前**拒绝并说明原因（与文件加载「剔除 + warn」相反：那边用户看不到日志）。
#[test]
fn quick_add_rejects_invalid_template() {
    let (c, p) = quick_coord("add_bad");
    // year_month 没有 $D 系列
    let e = c
        .web_data_rpc(
            "quick.add",
            &json!({ "kind": "year_month", "text": "$Y-$M-$D" }),
        )
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("$D") || e.contains('D'),
        "错误要指出是哪个变量: {e}"
    );

    let e = c
        .web_data_rpc("quick.add", &json!({ "kind": "date", "text": "   " }))
        .unwrap_err()
        .to_string();
    assert!(e.contains("为空"), "纯空白模板要拒绝: {e}");

    assert!(
        quick_rows(&c).iter().all(|r| r["user"] == json!(false)),
        "被拒绝的条目不得落库"
    );
    let _ = std::fs::remove_file(&p);
}

/// 模板逐字相同就拒绝，并**指出撞的是哪一条**——两行长得一模一样时用户看不到 id，
/// 只说「重复了」他还得自己一行行找。
#[test]
fn quick_add_rejects_duplicate_template() {
    let (c, p) = quick_coord("add_dup");
    let e = c
        .web_data_rpc(
            "quick.add",
            &json!({ "kind": "date", "text": "$Y年$M月$D日" }),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("date.cn"), "要指出撞的是哪条: {e}");

    c.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    let e = c
        .web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap_err()
        .to_string();
    assert!(e.contains("date.u1"), "也要认出撞的是用户条目: {e}");

    // 同一串模板在别的类别里不算撞车（各类各一张表）
    c.web_data_rpc(
        "quick.add",
        &json!({ "kind": "month_day", "text": "$Y/$M/$D" }),
    )
    .unwrap();
    let _ = std::fs::remove_file(&p);
}

/// 出厂条目的模板不可改、不可删——这是 P2 的核心设计决策，由 store 的数据结构
/// 兜住（模板只存在 `added` 里）。这条测试是那个决策的守门。
#[test]
fn quick_factory_entries_refuse_edit_and_delete() {
    let (c, p) = quick_coord("factory_ro");
    let e = c
        .web_data_rpc(
            "quick.setText",
            &json!({ "kind": "date", "id": "date.cn", "text": "$Y.$M.$D" }),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("不可修改"), "要说清为什么: {e}");

    let e = c
        .web_data_rpc("quick.delete", &json!({ "kind": "date", "id": "date.cn" }))
        .unwrap_err()
        .to_string();
    assert!(e.contains("只能停用"), "要指出替代做法: {e}");

    // 模板没被动过
    assert_eq!(
        quick_row(&quick_rows(&c), "date.cn")["text"],
        json!("$Y年$M月$D日")
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn quick_set_text_updates_user_entry_and_its_sample() {
    let (c, p) = quick_coord("set_text");
    // 模板不能与出厂条目逐字相同（`$CNL` 就是出厂的 number.cn_lower），否则被查重拦下
    c.web_data_rpc("quick.add", &json!({ "kind": "number", "text": "约$CNL" }))
        .unwrap();
    c.web_data_rpc(
        "quick.setText",
        &json!({ "kind": "number", "id": "number.u1", "text": "共$THOU元" }),
    )
    .unwrap();
    let row = quick_row(&quick_rows(&c), "number.u1");
    assert_eq!(row["text"], json!("共$THOU元"));
    assert!(
        row["sample"].as_str().is_some_and(|s| s.starts_with('共')),
        "示例要跟着变: {:?}",
        row["sample"]
    );
    let _ = std::fs::remove_file(&p);
}

/// 改写时把自己排除掉：只改了个错别字、模板本身没动，不该被判成「与自己重复」。
#[test]
fn quick_set_text_to_the_same_value_is_allowed() {
    let (c, p) = quick_coord("set_same");
    c.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    c.web_data_rpc(
        "quick.setText",
        &json!({ "kind": "date", "id": "date.u1", "text": "$Y/$M/$D" }),
    )
    .expect("模板没变也该允许保存");
    let _ = std::fs::remove_file(&p);
}

/// 删除连带清掉它的调序规则，否则重加一条同 id 的条目会被旧规则挪到意外的位置。
#[test]
fn quick_delete_also_drops_its_move_rule() {
    let (c, p) = quick_coord("del");
    c.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    c.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.u1", "index": 0 }),
    )
    .unwrap();
    assert_eq!(quick_rows(&c)[0]["id"], json!("date.u1"));

    c.web_data_rpc("quick.delete", &json!({ "kind": "date", "id": "date.u1" }))
        .unwrap();
    let rows = quick_rows(&c);
    assert!(
        !rows.iter().any(|r| r["id"] == json!("date.u1")),
        "已从列表消失"
    );
    // 重新加一条：不该继承已删条目的位置，而要落回本类末尾。
    // 不断言 id：删掉唯一一条后复用 `date.u1` 是安全的（规则已连带清掉）；「删中间一条后撞上
    // 仍存在的 id」由 `next_user_id_survives_a_deletion_in_the_middle` 覆盖。
    let r = c
        .web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    let fresh = r["id"].as_str().unwrap().to_string();
    let rows = quick_rows(&c);
    assert_ne!(rows[0]["id"], json!(fresh), "★ 不得继承已删条目的首位");
    let date_ids: Vec<&str> = rows
        .iter()
        .filter(|r| r["kind"] == json!("date"))
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(*date_ids.last().unwrap(), fresh, "落回本类末尾");
    let _ = std::fs::remove_file(&p);
}

/// 「恢复默认」清调序与停用，保留用户条目。store 层已有同名测试，这里钉 RPC 全链路：
/// 右键菜单的 `resetKind` 与设置页共用这条路径，整类 `remove(kind)` 会删穿用户条目。
#[test]
fn quick_reset_kind_keeps_user_entries() {
    let (c, p) = quick_coord("reset_keep");
    c.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    c.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.u1", "index": 0 }),
    )
    .unwrap();
    c.web_data_rpc(
        "quick.setEnabled",
        &json!({ "kind": "date", "id": "date.cn", "enabled": false }),
    )
    .unwrap();

    c.web_data_rpc("quick.resetKind", &json!({ "kind": "date" }))
        .unwrap();

    let rows = quick_rows(&c);
    let row = quick_row(&rows, "date.u1");
    assert_eq!(row["user"], json!(true), "★ 用户条目必须还在");
    assert_eq!(row["adjusted"], json!(false), "它的调序规则被清掉了");
    assert_eq!(quick_row(&rows, "date.cn")["enabled"], json!(true));
    let date_ids: Vec<&str> = rows
        .iter()
        .filter(|r| r["kind"] == json!("date"))
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        *date_ids.last().unwrap(),
        "date.u1",
        "回到「用户条目在末尾」的初始位置"
    );
    let _ = std::fs::remove_file(&p);
}

/// 导出/导入往返带上用户条目本身（不只是调整）。
#[test]
fn quick_roundtrip_carries_user_entries() {
    let (a, pa) = quick_coord("rt_user_a");
    a.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    a.web_data_rpc(
        "quick.add",
        &json!({ "kind": "calc", "text": "结果是$RESULT" }),
    )
    .unwrap();
    a.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.u1", "index": 0 }),
    )
    .unwrap();
    let content = a.web_data_rpc("quick.export", &json!({})).unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();

    let (b, pb) = quick_coord("rt_user_b");
    let o = b
        .web_data_rpc("quick.import", &json!({ "content": content }))
        .unwrap();
    assert_eq!(o["formats"], json!(2), "两条自定义条目都导入了");
    assert!(
        o["skipped"].as_array().unwrap().is_empty(),
        "不该有跳过项: {:?}",
        o["skipped"]
    );

    let rows = quick_rows(&b);
    assert_eq!(rows[0]["id"], json!("date.u1"), "调序规则也带过来了");
    assert_eq!(rows[0]["text"], json!("$Y/$M/$D"));
    assert_eq!(quick_row(&rows, "calc.u1")["text"], json!("结果是$RESULT"));
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
}

/// 导入时 id 冲突：换新 id 之后，引用它的 `moved` 规则必须一起改写。
///
/// 两台机器各有一条内容不同的 `date.u1`，若只改 `[[formats]]` 侧，移动规则会指向本机原有的
/// 条目，症状只是「导入后顺序不对」。与 `.rev()` 同属导入重放的引用完整性。
#[test]
fn quick_import_remaps_conflicting_id_and_its_move_rule() {
    // A 机：date.u1 = $Y/$M/$D，并把它移到首位
    let (a, pa) = quick_coord("remap_a");
    a.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    a.web_data_rpc(
        "quick.move",
        &json!({ "kind": "date", "id": "date.u1", "index": 0 }),
    )
    .unwrap();
    let content = a.web_data_rpc("quick.export", &json!({})).unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();

    // B 机：已有一条**内容不同**的 date.u1
    let (b, pb) = quick_coord("remap_b");
    b.web_data_rpc(
        "quick.add",
        &json!({ "kind": "date", "text": "本机原有$Y" }),
    )
    .unwrap();

    let o = b
        .web_data_rpc("quick.import", &json!({ "content": content }))
        .unwrap();
    assert_eq!(o["formats"], json!(1));

    let rows = quick_rows(&b);
    // 本机原有那条内容不变
    assert_eq!(quick_row(&rows, "date.u1")["text"], json!("本机原有$Y"));
    // 导入的那条换了 id
    let fresh = quick_row(&rows, "date.u2");
    assert_eq!(fresh["text"], json!("$Y/$M/$D"), "导入的条目改用 u2");
    // 移动规则跟着改写：排首位的是导入的那条，不是本机原有的
    assert_eq!(
        rows[0]["id"],
        json!("date.u2"),
        "★ moved 必须一起改写，否则规则会指向本机原有的 date.u1"
    );
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
}

/// 重复导入同一份文件是幂等的：模板相同的条目按「撞车」跳过并如实报告，
/// 而不是每导一次就多出一条一模一样的候选。
#[test]
fn quick_import_twice_does_not_duplicate_entries() {
    let (a, pa) = quick_coord("idem_a");
    a.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "$Y/$M/$D" }))
        .unwrap();
    let content = a.web_data_rpc("quick.export", &json!({})).unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    let (b, pb) = quick_coord("idem_b");
    b.web_data_rpc("quick.import", &json!({ "content": &content }))
        .unwrap();
    let o = b
        .web_data_rpc("quick.import", &json!({ "content": &content }))
        .unwrap();
    assert_eq!(o["formats"], json!(0), "第二次一条都不该写入");
    assert_eq!(
        o["skipped"].as_array().unwrap().len(),
        1,
        "跳过要如实报告: {:?}",
        o["skipped"]
    );
    assert_eq!(
        quick_rows(&b)
            .iter()
            .filter(|r| r["user"] == json!(true))
            .count(),
        1,
        "只有一条用户条目"
    );
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
}

/// `strategy = "replace"` 连用户条目一起清（语义是「用文件覆盖现状」），
/// 与面向用户的「恢复默认」刻意相反。
#[test]
fn quick_import_replace_clears_user_entries_too() {
    let (a, pa) = quick_coord("repl_user_a");
    a.web_data_rpc("quick.add", &json!({ "kind": "calc", "text": "得$RESULT" }))
        .unwrap();
    let content = a.web_data_rpc("quick.export", &json!({})).unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();

    let (b, pb) = quick_coord("repl_user_b");
    b.web_data_rpc("quick.add", &json!({ "kind": "date", "text": "本机的$Y" }))
        .unwrap();
    b.web_data_rpc(
        "quick.import",
        &json!({ "content": content, "strategy": "replace" }),
    )
    .unwrap();

    let rows = quick_rows(&b);
    assert!(
        !rows.iter().any(|r| r["text"] == json!("本机的$Y")),
        "replace 应清掉本机原有的用户条目"
    );
    assert_eq!(quick_row(&rows, "calc.u1")["text"], json!("得$RESULT"));
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
}

/// 用一份 system.phrases.toml 起一个带 data_dir 的 headless coordinator。
fn coord_with_phrase_toml(tag: &str, toml: &str) -> (Arc<Coordinator>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("wind_phrase_reread_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("system.phrases.toml"), toml).unwrap();
    let store = Arc::new(Store::open(dir.join("store.redb")).unwrap());
    let c = Coordinator::new_headless_with_store(Config::default(), Some(&dir), store);
    (c, dir)
}

fn system_codes(c: &Coordinator) -> Vec<(String, String)> {
    c.web_data_rpc("phrase.listSystem", &json!({}))
        .unwrap()
        .as_array()
        .expect("listSystem 应返回数组")
        .iter()
        .map(|v| {
            (
                v["code"].as_str().unwrap().to_string(),
                v["text"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// phrase.resetSystem 应重读 TOML：手工编辑后无需重启服务即可生效。
#[test]
fn phrase_reset_system_rereads_toml() {
    let (c, dir) = coord_with_phrase_toml("ok", "[[phrases]]\ncode = 'rq'\ntext = '$date'\n");

    assert_eq!(system_codes(&c), vec![("rq".into(), "$date".into())]);

    // 手工编辑：改文本 + 增一条
    std::fs::write(
        dir.join("system.phrases.toml"),
        "[[phrases]]\ncode = 'rq'\ntext = '$datetime'\n\n[[phrases]]\ncode = 'xx'\ntext = '新增'\n",
    )
    .unwrap();

    c.web_data_rpc("phrase.resetSystem", &json!({})).unwrap();

    let mut got = system_codes(&c);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("rq".to_string(), "$datetime".to_string()),
            ("xx".to_string(), "新增".to_string()),
        ],
        "重读后应取到编辑后的文本并含新增条目"
    );
}

/// TOML 语法错误时必须回退到启动缓存，绝不能把库里系统短语清空。
#[test]
fn phrase_reset_system_falls_back_on_broken_toml() {
    let (c, dir) = coord_with_phrase_toml("broken", "[[phrases]]\ncode = 'rq'\ntext = '$date'\n");

    // 写坏 TOML（未闭合引号）
    std::fs::write(
        dir.join("system.phrases.toml"),
        "[[phrases]]\ncode = 'rq\ntext = ",
    )
    .unwrap();

    c.web_data_rpc("phrase.resetSystem", &json!({})).unwrap();

    assert_eq!(
        system_codes(&c),
        vec![("rq".to_string(), "$date".to_string())],
        "解析失败应沿用启动缓存，不得清空系统短语"
    );
}

#[test]
fn json_diff_sparse() {
    let base = json!({ "a": 1, "t": { "x": 1, "y": 2 }, "same": "v" });
    let cfg = json!({ "a": 9, "t": { "x": 1, "y": 20 }, "same": "v" });
    let d = json_diff(&base, &cfg).unwrap();
    // 仅含变化项：a + t.y
    assert_eq!(d, json!({ "a": 9, "t": { "y": 20 } }));
    // 完全相同 → None
    assert!(json_diff(&base, &base).is_none());
}

/// `custom_mappings` 必须整表进 override，不能只写变化的那一行。
///
/// 读侧 `merge_toml` 对它是整体替换（否则用户删不掉作者写的行）；写侧若逐键下钻，改一行 ⇒
/// override 只有那一行 ⇒ 整体替换后作者写的其余行全没了。断言落在「没被改的那一行在不在
/// diff 里」，只断言改的那行在，逐键下钻也能通过。
#[test]
fn json_diff_keeps_custom_mappings_atomic() {
    let base = json!({
        "punct": { "mode": "follow", "custom_mappings": { ".": ["。"], ",": ["，"] } }
    });
    // 用户只改了 `.` 那一行，`,` 原样不动。
    let cfg = json!({
        "punct": { "mode": "follow", "custom_mappings": { ".": ["·"], ",": ["，"] } }
    });
    let d = json_diff(&base, &cfg).unwrap();
    assert_eq!(
        d.pointer("/punct/custom_mappings"),
        Some(&json!({ ".": ["·"], ",": ["，"] })),
        "★ 整张表都要进 override——逐键 diff 只会写 `.` 那一行，读侧整体替换后 `,` 丢失"
    );
    // 表没变时不该无谓地写进 override（否则每次打开设置页保存都在冻结一份快照）。
    assert!(json_diff(&base, &base).is_none(), "未改动时不得产生 diff");
    // 同层的其余键仍走稀疏 diff——原子只对 `custom_mappings` 一个键成立。
    let mode_only = json!({
        "punct": { "mode": "english", "custom_mappings": { ".": ["。"], ",": ["，"] } }
    });
    assert_eq!(
        json_diff(&base, &mode_only).unwrap(),
        json!({ "punct": { "mode": "english" } }),
        "只改 mode 时不该把整表也写进 override"
    );
}

/// 只读旁路字段不得随 saveConfig 落进 override。
///
/// 否则 override 里多一段 `[effectiveCodetable]`，该方案的码表行为被冻结在打开设置页那一刻，
/// 之后改全局配置不再生效。
#[test]
fn save_config_strips_readonly_sidecar() {
    let cfg = json!({
        "schema": { "id": "wubi86" },
        "engine": { "type": "codetable", "codetable": { "punct_commit": true } },
        "effectiveCodetable": { "punct_commit": true, "z_key_repeat": false },
    });
    let stripped = strip_readonly_fields(&cfg);
    assert!(
        stripped.get("effectiveCodetable").is_none(),
        "旁路字段应被剥掉，实际 {stripped}"
    );
    // 其余内容原样保留——剥错了会静默丢配置。
    assert_eq!(
        stripped.pointer("/engine/codetable/punct_commit"),
        Some(&json!(true))
    );
    assert_eq!(stripped.pointer("/schema/id"), Some(&json!("wubi86")));

    // 没有旁路字段时原样返回（不因为多一次 clone 就改变结构）。
    let plain = json!({ "engine": { "type": "codetable" } });
    assert_eq!(strip_readonly_fields(&plain), plain);

    // 与 diff 串起来看：剥之后，方案文件没写过的旁路键不会被判成「新增」。
    let base = json!({ "schema": { "id": "wubi86" }, "engine": { "type": "codetable" } });
    let d = json_diff(&base, &stripped).unwrap_or(json!({}));
    assert!(
        d.get("effectiveCodetable").is_none(),
        "diff 里不该出现旁路字段，实际 {d}"
    );

    // 逐个覆盖 READONLY_SIDECAR_FIELDS，而不是只测其中一个：新增旁路字段却忘了
    // 登记，正是这个坑的复发形态，只测 effectiveCodetable 的话照样全绿。
    for f in READONLY_SIDECAR_FIELDS {
        let mut one = json!({ "schema": { "id": "wubi86" } });
        one.as_object_mut()
            .unwrap()
            .insert((*f).to_string(), json!("任意值"));
        let s = strip_readonly_fields(&one);
        assert!(s.get(*f).is_none(), "旁路字段 {f} 未被剥掉，实际 {s}");
        assert_eq!(
            s.pointer("/schema/id"),
            Some(&json!("wubi86")),
            "剥 {f} 时误伤了真实配置"
        );
    }
}

/// `[overlay]` 段经 getConfig → 改 → saveConfig 往返，并**立即体现在 overlay 注册表里**。
///
/// 锁的是「设置页把普通方案变成 overlay 方案（快符）」的整条通路：`[overlay]` 住在方案文件里，
/// 读（getConfig 三层合并）、写（saveConfig 稀疏 diff 落 override）、生效（read_schema 的
/// merge_toml）三条通路都是现成的。
#[test]
fn schema_overlay_section_roundtrips_through_save_config() {
    let dir = std::env::temp_dir().join("wind_webdata_overlay_rt");
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    // 出厂是个**普通**码表方案：没有 [overlay] 段。
    std::fs::write(
        schemas.join("zz_rt.schema.toml"),
        "[schema]\nid = \"zz_rt\"\nname = \"往返\"\n\
             [engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\n",
    )
    .unwrap();
    let ov_dir = dir.join("overrides");
    std::fs::create_dir_all(&ov_dir).unwrap();

    let store_path = std::env::temp_dir().join("wind_webdata_overlay_rt.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let c = Coordinator::new_headless_with_store_override(
        wind_config::Config::default(),
        Some(&dir),
        store,
        Some(ov_dir.clone()),
    );

    // 起点：不是 overlay 方案。
    assert!(
        c.engine_mgr().overlay_index_of("zz_rt").is_none(),
        "出厂无 [overlay] 段，不该在注册表里"
    );
    let mut cfg = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_rt" }))
        .unwrap();
    assert!(
        cfg.get("overlay").is_none_or(|v| v.is_null()),
        "未配置时 overlay 应缺省/为 null，实际 {cfg}"
    );

    // 设置页动作：加上 [overlay] 段并保存。
    cfg.as_object_mut().unwrap().insert(
        "overlay".to_string(),
        json!({ "kind": "special", "show_all_on_enter": true, "candidate_layout": "vertical" }),
    );
    c.web_data_rpc("schema.saveConfig", &json!({ "id": "zz_rt", "cfg": cfg }))
        .unwrap();

    // 落盘的是**稀疏 diff**：只有改动项进 override，方案文件的其余定义仍可透传。
    let written = std::fs::read_to_string(ov_dir.join("zz_rt.toml")).unwrap();
    assert!(
        written.contains("[overlay]"),
        "override 未写入 [overlay]：{written}"
    );

    // 生效：注册表整表随 invalidate 重建，该方案成为 overlay 方案。
    c.engine_mgr().invalidate_schema("zz_rt");
    let idx = c
        .engine_mgr()
        .overlay_index_of("zz_rt")
        .expect("保存后应进 overlay 注册表");
    let e = c.engine_mgr().overlay_modes()[idx as usize].clone();
    assert!(e.spec.show_all_on_enter);
    assert_eq!(e.spec.candidate_layout, wind_config::LayoutIntent::Vertical);
    assert_eq!(e.name, "往返", "override 未提及的字段仍来自方案文件");

    let _ = std::fs::remove_file(&store_path);
}

/// 设置页区分「跟随方案」与「用户选的来源」要看方案文件自己的 files——合并值答不了。
#[test]
fn get_config_exposes_aux_code_base_files_and_strips_it_on_save() {
    let dir = std::env::temp_dir().join(format!("wind_webdata_aux_base_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join("zz_py.schema.toml"),
        "[schema]\nid = \"zz_py\"\nname = \"拼\"\n[engine]\ntype = \"pinyin\"\n\
             [engine.aux_code]\nfiles = [\"aux_code/stroke.txt\"]\n",
    )
    .unwrap();
    let ov = dir.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("zz_py.toml"),
        "[engine.aux_code]\nfiles = [\"schema:wubi86\"]\n",
    )
    .unwrap();
    let store = std::sync::Arc::new(wind_store::Store::open(dir.join("s.redb")).unwrap());
    let c = Coordinator::new_headless_with_store_override(
        wind_config::Config::default(),
        Some(&dir),
        store,
        Some(ov.clone()),
    );
    let v = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_py" }))
        .unwrap();
    assert_eq!(
        v["auxCodeBaseFiles"],
        json!(["aux_code/stroke.txt"]),
        "方案文件自己的 files"
    );
    assert_eq!(
        v["engine"]["aux_code"]["files"],
        json!(["schema:wubi86"]),
        "合并值是 override 的"
    );
    // 原样回传不能把旁路字段写进 override。
    c.web_data_rpc("schema.saveConfig", &json!({ "id": "zz_py", "cfg": v }))
        .unwrap();
    let saved = std::fs::read_to_string(ov.join("zz_py.toml")).unwrap();
    assert!(
        !saved.contains("auxCodeBaseFiles"),
        "旁路字段必须剥掉：{saved}"
    );
    assert!(saved.contains("schema:wubi86"), "用户的选择保留：{saved}");
}

/// 连续辅助码的内部项（`continuous`、`preedit_separator`）不进设置页，手写在 override 里；
/// 设置页改别的项（这里关 `direct`）走 getConfig → saveConfig 时不能把它们抹掉。
#[test]
fn save_config_keeps_internal_aux_keys() {
    let dir =
        std::env::temp_dir().join(format!("wind_webdata_aux_internal_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join("zz_sp.schema.toml"),
        "[schema]\nid = \"zz_sp\"\nname = \"双\"\n[engine]\ntype = \"pinyin\"\n\
             [engine.pinyin]\nscheme = \"shuangpin\"\n\
             [engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\n",
    )
    .unwrap();
    let ov = dir.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("zz_sp.toml"),
        "[engine.aux_code]\nenabled = true\ndirect = true\n\
             continuous = false\npreedit_separator = \"\"\n",
    )
    .unwrap();
    let store = std::sync::Arc::new(wind_store::Store::open(dir.join("s.redb")).unwrap());
    let c = Coordinator::new_headless_with_store_override(
        wind_config::Config::default(),
        Some(&dir),
        store,
        Some(ov.clone()),
    );
    let mut v = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_sp" }))
        .unwrap();
    assert_eq!(v["engine"]["aux_code"]["continuous"], json!(false));
    assert_eq!(v["engine"]["aux_code"]["preedit_separator"], json!(""));
    v["engine"]["aux_code"]["direct"] = json!(false);
    c.web_data_rpc("schema.saveConfig", &json!({ "id": "zz_sp", "cfg": v }))
        .unwrap();
    let saved: toml::Value =
        toml::from_str(&std::fs::read_to_string(ov.join("zz_sp.toml")).unwrap()).unwrap();
    let aux = &saved["engine"]["aux_code"];
    assert_eq!(aux.get("direct").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(aux.get("continuous").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        aux.get("preedit_separator").and_then(|v| v.as_str()),
        Some(""),
        "{saved}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 三态控件的读侧契约：`getConfig` 必须让设置页分得清「作者写的」与「用户改的」。
///
/// 某方案在自己的文件里声明了 `short_code_yield_level`（作者基线），用户又在设置页改成别的值
/// （override 层）。三份数据各答一个问题：
///
/// | 字段 | 回答的问题 | 本例取值 |
/// | --- | --- | --- |
/// | `engine.codetable`（合并值） | 现在按什么跑 | 2 |
/// | `followedCodetable` | 取消覆盖后变成什么 | 3（作者值） |
/// | `codetableOverride` | 这一项是不是用户改的 | 有 ⇒ 是 |
///
/// 缺了 `followedCodetable`，「取消覆盖」在界面上什么都不变；缺了 `codetableOverride`，
/// 方案自带的那一项会被显示成「用户已改」。
///
/// 用自造的 `zz_ct` 而不是 wubi86：内置方案一项都不声明（见 `data/schemas/wubi86.schema.toml`），
/// 这个形态只出现在第三方码表上。
#[test]
fn schema_get_config_separates_author_baseline_from_user_override() {
    let dir = std::env::temp_dir().join("wind_webdata_ct_tristate");
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    // 方案作者基线：出简让全 3；顶码那一项作者没写 ⇒ 该跟随全局。
    std::fs::write(
        schemas.join("zz_ct.schema.toml"),
        "[schema]
id = \"zz_ct\"
name = \"三态\"
             [engine]
type = \"codetable\"
             [engine.codetable]
max_code_length = 4
short_code_yield_level = 3
",
    )
    .unwrap();
    // 用户覆盖层：把它改成 2。
    let ov_dir = dir.join("overrides");
    std::fs::create_dir_all(&ov_dir).unwrap();
    std::fs::write(
        ov_dir.join("zz_ct.toml"),
        "[engine.codetable]
short_code_yield_level = 2
",
    )
    .unwrap();

    let mut cfg = wind_config::Config::default();
    // 全局基线给顶码一个与结构体零值不同的取值，好让「跟随全局」这一路可被断言。
    cfg.schema.codetable.top_code_commit = true;
    let c = Coordinator::new_headless_with_override(cfg, Some(&dir), Some(ov_dir.clone()));
    let got = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_ct" }))
        .unwrap();

    assert_eq!(
        got.pointer("/engine/codetable/short_code_yield_level"),
        Some(&json!(2)),
        "合并值应是用户改后的那个，实际 {got}"
    );
    assert_eq!(
        got.pointer("/effectiveCodetable/short_code_yield_level"),
        Some(&json!(2)),
        "当前生效值同上"
    );
    assert_eq!(
        got.pointer("/followedCodetable/short_code_yield_level"),
        Some(&json!(3)),
        "回落值必须是**作者基线**，不含 override 层"
    );
    assert_eq!(
        got.pointer("/codetableOverride/short_code_yield_level"),
        Some(&json!(2)),
        "override 层原文要原样给出（判某项是不是用户改的）"
    );

    // 作者没写、用户也没改的那一项：回落值来自全局，且不在 override 层里。
    assert_eq!(
        got.pointer("/followedCodetable/top_code_commit"),
        Some(&json!(true)),
        "作者没写时回落值取全局基线"
    );
    assert!(
        got.pointer("/codetableOverride/top_code_commit").is_none(),
        "没改过的项不该出现在 override 层，实际 {}",
        got.pointer("/codetableOverride").unwrap()
    );

    // 反向对照：没有 override 文件时，回落值与生效值一致（否则上面的差异可能来自
    // 别的原因，而不是「剥掉了 override 层」）。
    std::fs::remove_file(ov_dir.join("zz_ct.toml")).unwrap();
    c.engine_mgr().invalidate_schema("zz_ct");
    let got2 = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_ct" }))
        .unwrap();
    assert_eq!(
        got2.pointer("/effectiveCodetable/short_code_yield_level"),
        Some(&json!(3)),
        "删掉 override 后生效值回到作者值"
    );
    assert_eq!(
        got2.pointer("/followedCodetable/short_code_yield_level"),
        Some(&json!(3))
    );
    assert_eq!(
        got2.pointer("/codetableOverride"),
        Some(&json!({})),
        "无 override 文件时给空对象而不是缺字段——UI 少一次 null 分支"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 写侧：回传 `null` 就是「取消覆盖」——该键从 override 里消失，该项回到方案作者的值。
///
/// `json_to_toml` 跳过 null，而 `saveConfig` 拿方案文件基线（不含 override）做 diff，两条既有
/// 性质合起来正好是三态要的语义；改任一侧都会静默破坏它，故端到端钉住。
/// 同时验稀疏性：同一次保存里另一项照常写入。
#[test]
fn save_config_null_cancels_one_override_and_keeps_the_others() {
    let dir = std::env::temp_dir().join("wind_webdata_ct_cancel");
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join("zz_cx.schema.toml"),
        "[schema]\nid = \"zz_cx\"\nname = \"取消\"\n\
             [engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\nshort_code_yield_level = 3\n",
    )
    .unwrap();
    let ov_dir = dir.join("overrides");
    std::fs::create_dir_all(&ov_dir).unwrap();
    std::fs::write(
        ov_dir.join("zz_cx.toml"),
        "[engine.codetable]\nshort_code_yield_level = 2\npunct_commit = false\n",
    )
    .unwrap();

    let c = Coordinator::new_headless_with_override(
        wind_config::Config::default(),
        Some(&dir),
        Some(ov_dir.clone()),
    );
    let mut cfg = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_cx" }))
        .unwrap();

    // 设置页动作：取消「出简让全」的覆盖（写 null），另一项照旧勾着（写实值）。
    let ct = cfg
        .pointer_mut("/engine/codetable")
        .and_then(|v| v.as_object_mut())
        .expect("码表段应在");
    ct.insert("short_code_yield_level".to_string(), Value::Null);
    ct.insert("punct_commit".to_string(), json!(false));
    c.web_data_rpc("schema.saveConfig", &json!({ "id": "zz_cx", "cfg": cfg }))
        .unwrap();

    let written = std::fs::read_to_string(ov_dir.join("zz_cx.toml")).unwrap();
    assert!(
        !written.contains("short_code_yield_level"),
        "取消覆盖的键必须从 override 里消失，实际:\n{written}"
    );
    assert!(
        written.contains("punct_commit"),
        "同一次保存里仍勾着的项不该被牵连，实际:\n{written}"
    );

    // 生效面：该项回到**方案作者**的 3（不是全局的默认值），另一项仍是用户的 false。
    c.engine_mgr().invalidate_schema("zz_cx");
    let got = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_cx" }))
        .unwrap();
    assert_eq!(
        got.pointer("/effectiveCodetable/short_code_yield_level"),
        Some(&json!(3)),
        "取消覆盖后应回到方案作者写的值"
    );
    assert!(
        got.pointer("/codetableOverride/short_code_yield_level")
            .is_none(),
        "override 层不该再有它（否则设置页下次打开仍显示为已勾选）"
    );
    assert_eq!(
        got.pointer("/codetableOverride/punct_commit"),
        Some(&json!(false)),
        "另一项的覆盖要原样留着"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schema_get_config_graceful_without_data_dir() {
    // data_dir=None（coord helper）→ 无方案文件 → getConfig 返回 {}，saveConfig 报错（无基础）。
    let c = coord("schema");
    let r = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "pinyin" }))
        .unwrap();
    assert!(r.is_object() && r.as_object().unwrap().is_empty());
    assert!(
        c.web_data_rpc("schema.saveConfig", &json!({ "id": "pinyin", "cfg": {} }))
            .is_err()
    );
}

#[test]
fn stats_summary_rich_fields() {
    use wind_store::stats::CommitSource;
    let c = coord("stats_summary_rich");
    // 今日：中文 2(码长4,首选) + 英文 2(临英,次选)
    c.debug_record_commit("你好", 4, 0, CommitSource::Candidate);
    c.debug_record_commit("ab", 0, 1, CommitSource::TempEnglish);
    let r = c.web_data_rpc("stats.summary", &json!({})).unwrap();
    assert_eq!(r["today_chinese"], 2);
    assert_eq!(r["today_english"], 2);
    assert_eq!(r["today_chars"], 4);
    assert_eq!(r["total_chars"], 4);
    assert_eq!(r["active_days"], 1);
    assert!((r["avg_code_len"].as_f64().unwrap() - 4.0).abs() < 1e-9);
    assert!(
        (r["first_select_rate"].as_f64().unwrap() - 0.5).abs() < 1e-9,
        "首选率=首选1/总选2=0.5"
    );
}

#[test]
fn stats_daily_rich_shape() {
    use wind_store::stats::CommitSource;
    let c = coord("stats_daily_rich");
    c.debug_record_commit("你好", 4, 0, CommitSource::Candidate);
    c.stat_collector().unwrap().flush(); // 落库才能被 daily 区间读到
    let today = wind_store::stats_report::today_str();
    let r = c
        .web_data_rpc("stats.daily", &json!({ "from": today, "to": today }))
        .unwrap();
    let arr = r.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let d = &arr[0];
    assert_eq!(d["d"], today);
    assert_eq!(d["tc"], 2);
    assert_eq!(d["cc"], 2);
    assert_eq!(d["cls"], 4);
    assert_eq!(d["h"].as_array().unwrap().len(), 24);
    assert_eq!(d["cpd"][0], 1);
}

/// Task 2：拼音类方案（pinyin_simp / double_pinyin）写入共享 "pinyin" 存储，
/// 跨方案 id 互读能取到同一份用户词。
#[test]
fn pinyin_and_shuangpin_share_userdict() {
    use std::io::Write;
    // 写两个拼音类方案 schema.toml，让 data_schema_id 折叠到 "pinyin"
    let base_dir = std::env::temp_dir().join("wind_coord_share_userdict_test");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    for name in ["pinyin_simp", "double_pinyin"] {
        let mut f = std::fs::File::create(schemas.join(format!("{name}.schema.toml"))).unwrap();
        write!(f, "[engine]\ntype = \"pinyin\"\n").unwrap();
    }

    let db_path = std::env::temp_dir().join("wind_coord_share_userdict.redb");
    let _ = std::fs::remove_file(&db_path);
    let store = Arc::new(Store::open(&db_path).unwrap());
    let c = Coordinator::new_headless_with_store(
        Config::default(),
        Some(base_dir.as_path()),
        Arc::clone(&store),
    );

    // 用拼音方案加词
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "pinyin_simp", "code": "nihao", "text": "你好", "weight": 5 }),
    )
    .unwrap();

    // 用双拼方案读，应读到同一条（共享 "pinyin" 存储键）
    let list = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "double_pinyin", "offset": 0, "limit": 100 }),
        )
        .unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(
        items.iter().any(|it| it["text"] == "你好"),
        "双拼应读到拼音下加的词（data_schema_id 共享）"
    );
}

/// `dict.stats` 的三个计数（userWords / tempWords / shadowRules）都必须走 `data_schema_id`
/// 折叠，与 `dict.listPaged` / `dict.search` / `shadow.list` 同源。`write_data_schema_id`
/// 只在混输按来源分桶，非混输方案直接折叠，直传原始 id 会让双拼计数恒为 0。
///
/// 两个方向缺一不可：
/// - 正向：拼音族折叠 ⇒ 全拼与双拼报同一个数（共享同一份词）。
/// - 反向：码表方案 `wubi_like` 报自己的数，不等于拼音族那个数；否则把 `data_schema_id(id)`
///   误写成常量 `"pinyin"` 也能全绿。
#[test]
fn dict_stats_counts_all_follow_data_schema_folding() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_coord_stats_fold_all");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    for name in ["pinyin_simp", "double_pinyin"] {
        let mut f = std::fs::File::create(schemas.join(format!("{name}.schema.toml"))).unwrap();
        write!(f, "[engine]\ntype = \"pinyin\"\n").unwrap();
    }
    // 反向对照方案：非拼音族 ⇒ `data_schema_id` 返回自身 id，不参与折叠。
    {
        let mut f = std::fs::File::create(schemas.join("wubi_like.schema.toml")).unwrap();
        write!(f, "[engine]\ntype = \"codetable\"\n").unwrap();
    }

    let db_path = std::env::temp_dir().join("wind_coord_stats_fold_all.redb");
    let _ = std::fs::remove_file(&db_path);
    let store = Arc::new(Store::open(&db_path).unwrap());
    // `dict.stats` 遍历的是**已启用方案**（`config.schema.available`），不是目录扫描结果
    // ——只写 schema.toml 不够，方案都得在这份清单里才会出现在统计中。
    let mut cfg = Config::default();
    cfg.schema.available = vec![
        "pinyin_simp".into(),
        "double_pinyin".into(),
        "wubi_like".into(),
    ];
    cfg.schema.active = "pinyin_simp".into();
    let c = Coordinator::new_headless_with_store(cfg, Some(base_dir.as_path()), Arc::clone(&store));

    // 全拼下写一条用户词 + 置顶一条（写端都折叠 → 落在 "pinyin"）。
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "pinyin_simp", "code": "nihao", "text": "你好", "weight": 5 }),
    )
    .unwrap();
    c.web_data_rpc(
        "shadow.pin",
        &json!({ "schemaId": "pinyin_simp", "code": "hao", "word": "好", "position": 0 }),
    )
    .unwrap();
    // 码表方案下写两条，数量**刻意与拼音族不同**——相等的话反向断言就退化成恒真。
    for (code, text) in [("aaaa", "工"), ("bbbb", "王")] {
        c.web_data_rpc(
            "dict.add",
            &json!({ "schemaId": "wubi_like", "code": code, "text": text, "weight": 5 }),
        )
        .unwrap();
    }

    // 临时词由引擎写端落库，走 `write_data_schema_id`，拼音族已折叠成 "pinyin"，故直接写折叠后的桶。
    // 条数与用户词刻意错开（拼音族 2 条、码表 3 条），免得 tempWords 碰巧与 userWords 同值放过串线。
    for (code, text) in [("haoya", "好呀"), ("nihao", "你好")] {
        store.learn_temp_word("pinyin", code, text, 500, 0).unwrap();
    }
    for (code, text) in [("cccc", "又"), ("dddd", "大"), ("eeee", "月")] {
        store
            .learn_temp_word("wubi_like", code, text, 500, 0)
            .unwrap();
    }

    let stats = c.web_data_rpc("dict.stats", &json!({})).unwrap();
    let rows = stats.as_array().expect("stats 是数组");
    let row_of = |id: &str| -> Value {
        rows.iter()
            .find(|r| r["schemaId"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    };

    // tempWords：只把 `count_temp_words(&data_schema)` 退回 `count_temp_words(id)` 时，
    // 只有这三条会红——userWords 与 shadowRules 的断言对它全绿。
    assert_eq!(
        row_of("pinyin_simp")["tempWords"],
        2,
        "全拼须报出拼音桶里的临时词"
    );
    assert_eq!(
        row_of("double_pinyin")["tempWords"],
        2,
        "双拼与全拼共享临时词，须报同一个数"
    );
    assert_eq!(
        row_of("wubi_like")["tempWords"],
        3,
        "码表方案报自己桶里的临时词"
    );

    // 正向：拼音族两行报同一个数。
    assert_eq!(
        row_of("pinyin_simp")["userWords"],
        1,
        "全拼须报出自己加的那条词"
    );
    assert_eq!(
        row_of("double_pinyin")["userWords"],
        1,
        "双拼与全拼共享同一份用户词（data_schema_id 折叠到 \"pinyin\"），须报同一个数"
    );
    assert_eq!(row_of("pinyin_simp")["shadowRules"], 1, "全拼报 1 条规则");
    assert_eq!(
        row_of("double_pinyin")["shadowRules"],
        1,
        "双拼须报出折叠后的规则数（与全拼共享同一条）"
    );

    // 反向：码表方案不参与折叠，报自己的 2 条。
    assert_eq!(
        row_of("wubi_like")["userWords"],
        2,
        "码表方案须报自己桶里的条数，不受拼音族折叠影响"
    );
}

// 默认导出段的测试随 `default_dict_sections` 搬到了 `wind_coordinator::dict_transfer_api`
// （pinyin_default_export_sections_include_shadow）。

/// 在设置里关掉扩展词库，保存后它不得仍在出词。
///
/// 设置端一次保存先 `setDictEnabled(x, false)`，再 `saveConfig(打开对话框时的快照)`；快照里的
/// `dictionaries` 只要与基线不同，`json_diff` 就会把旧数组写进 override、冲掉 `enabled = false`。
/// 词库开关的唯一写入口是 `setDictEnabled`，`saveConfig` 不得经由快照改它。
#[test]
fn save_config_snapshot_does_not_clobber_dict_toggle() {
    let dir = std::env::temp_dir().join(format!("wind_webdata_dict_toggle_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(
        schemas.join("zz_en.schema.toml"),
        "[schema]\nid = \"zz_en\"\nname = \"英\"\n[engine]\ntype = \"english\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"en/main.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"en_it\"\npath = \"en/it.dict.yaml\"\ndefault_enabled = true\n\
             [[dictionaries]]\nid = \"en_x\"\npath = \"en/x.dict.yaml\"\ndefault_enabled = true\n",
    )
    .unwrap();
    let ov = dir.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    let store = std::sync::Arc::new(wind_store::Store::open(dir.join("s.redb")).unwrap());
    let c = Coordinator::new_headless_with_store_override(
        wind_config::Config::default(),
        Some(&dir),
        store,
        Some(ov.clone()),
    );
    let toggle = |dict: &str, on: bool| {
        c.web_data_rpc(
            "schema.setDictEnabled",
            &json!({ "id": "zz_en", "dictId": dict, "enabled": on }),
        )
        .unwrap();
    };
    // 早先翻过另一个扩展库 ⇒ 合并值的 dictionaries 已与方案文件基线不同。
    toggle("en_x", false);
    let snapshot = c
        .web_data_rpc("schema.getConfig", &json!({ "id": "zz_en" }))
        .unwrap();

    // 同一次保存：先关 en_it，再回传打开对话框时的旧快照（顺带改一个普通字段）。
    toggle("en_it", false);
    let mut cfg = snapshot.clone();
    cfg["schema"]["name"] = json!("英文改名");
    c.web_data_rpc("schema.saveConfig", &json!({ "id": "zz_en", "cfg": cfg }))
        .unwrap();

    let merged = c.engine_mgr().schema_merged("zz_en").expect("方案在");
    let enabled_of = |id: &str| {
        merged
            .dictionaries
            .iter()
            .find(|d| d.id == id)
            .and_then(|d| d.enabled)
    };
    let saved = std::fs::read_to_string(ov.join("zz_en.toml")).unwrap();
    assert_eq!(
        enabled_of("en_it"),
        Some(false),
        "刚关掉的 en_it 被快照冲掉了：{saved}"
    );
    assert_eq!(
        enabled_of("en_x"),
        Some(false),
        "早先关掉的 en_x 仍关：{saved}"
    );
    assert_eq!(merged.schema.name, "英文改名", "普通字段照常保存");
    assert!(
        !saved.contains("path"),
        "override 的 dictionaries 只许 {{id, enabled}} 稀疏项，不得冻结整份定义：{saved}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Task 3：schema.list 每个拼音方案应携带 scheme 字段（full/shuangpin），非拼音方案为空串。
#[test]
fn schema_list_exposes_scheme() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_coord_schema_list_scheme_test");
    let schemas = base_dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    // 创建全拼方案
    {
        let mut f = std::fs::File::create(schemas.join("pinyin_full.schema.toml")).unwrap();
        write!(
            f,
            "[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"full\"\n"
        )
        .unwrap();
    }
    // 创建双拼方案
    {
        let mut f = std::fs::File::create(schemas.join("double_pinyin_sp.schema.toml")).unwrap();
        write!(
            f,
            "[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"shuangpin\"\n"
        )
        .unwrap();
    }
    let db_path = std::env::temp_dir().join("wind_webdata_schema_list_scheme.redb");
    let _ = std::fs::remove_file(&db_path);
    let store = Arc::new(Store::open(&db_path).unwrap());
    let c = Coordinator::new_headless_with_store(
        Config::default(),
        Some(base_dir.as_path()),
        Arc::clone(&store),
    );

    let list = c.web_data_rpc("schema.list", &json!({})).unwrap();
    let arr = list.as_array().unwrap();

    // 必须有方案
    assert!(!arr.is_empty(), "schema.list 应返回非空数组");
    // 每项都应有 scheme 键
    for item in arr.iter() {
        assert!(
            item.get("scheme").is_some(),
            "每个方案项应有 scheme 字段，缺失于: {item}"
        );
    }
    // 全拼方案 scheme="full"
    let full = arr.iter().find(|s| s["id"] == "pinyin_full");
    assert!(full.is_some(), "应有 pinyin_full 方案");
    assert_eq!(full.unwrap()["scheme"], "full", "全拼方案 scheme 应为 full");
    // 双拼方案 scheme="shuangpin"
    let sp = arr.iter().find(|s| s["id"] == "double_pinyin_sp");
    assert!(sp.is_some(), "应有 double_pinyin_sp 方案");
    assert_eq!(
        sp.unwrap()["scheme"],
        "shuangpin",
        "双拼方案 scheme 应为 shuangpin"
    );
}

/// 英文方案是可切换方案：出现在 schema.list 里，且 engineType 自成一档。
///
/// `is_pinyin()` 对 `type = "english"` 会一路落到 `"codetable"`，设置页类型徽章就会把英文
/// 标成「码表」。故 `resolve_engine_type` 必须显式分流。
#[test]
fn english_schema_listed_with_its_own_engine_type() {
    use std::io::Write;
    let base_dir = std::env::temp_dir().join("wind_coord_schema_list_english_test");
    let schemas = base_dir.join("schemas");
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&schemas).unwrap();
    {
        let mut f = std::fs::File::create(schemas.join("en_test.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"en_test\"\n[engine]\ntype = \"english\"\n"
        )
        .unwrap();
    }
    // 反向对照：显式 hidden 的方案必须仍被挡在列表外。少了这条，上面的断言在
    // 「hidden 过滤整个失效」时也会通过——那才是去掉 english 的 hidden 时最该防的回归。
    {
        let mut f = std::fs::File::create(schemas.join("en_hidden.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"en_hidden\"\nhidden = true\n[engine]\ntype = \"english\"\n"
        )
        .unwrap();
    }
    let db_path = std::env::temp_dir().join("wind_webdata_schema_list_english.redb");
    let _ = std::fs::remove_file(&db_path);
    let store = Arc::new(Store::open(&db_path).unwrap());
    let c = Coordinator::new_headless_with_store(
        Config::default(),
        Some(base_dir.as_path()),
        Arc::clone(&store),
    );

    let list = c.web_data_rpc("schema.list", &json!({})).unwrap();
    let arr = list.as_array().unwrap();

    let en = arr.iter().find(|s| s["id"] == "en_test");
    assert!(en.is_some(), "英文方案应出现在 schema.list 中：{arr:?}");
    assert_eq!(
        en.unwrap()["engineType"],
        "english",
        "英文方案的 engineType 应为 english，落成 codetable 会让类型徽章显示为「码表」"
    );
    assert!(
        !arr.iter().any(|s| s["id"] == "en_hidden"),
        "hidden = true 的方案默认不得出现在列表中：{arr:?}"
    );

    // includeHidden = true：隐藏方案出现，且带 hidden 标志供设置页区分该行能配什么。
    let list2 = c
        .web_data_rpc("schema.list", &json!({ "includeHidden": true }))
        .unwrap();
    let arr2 = list2.as_array().unwrap();
    let hid = arr2
        .iter()
        .find(|s| s["id"] == "en_hidden")
        .expect("includeHidden 时隐藏方案应出现");
    assert_eq!(hid["hidden"], true, "隐藏方案应带 hidden = true");
    let vis = arr2.iter().find(|s| s["id"] == "en_test").unwrap();
    assert_eq!(vis["hidden"], false, "非隐藏方案的 hidden 应为 false");
}

/// 设置页「辅助码来源」的可选项：码表方案（不含请求方自己、不含非码表方案）+ aux_code/*.txt。
#[test]
fn schema_aux_code_sources_lists_codetable_schemas_and_tables() {
    let dir = std::env::temp_dir().join(format!("wind_webdata_aux_src_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::write(
        schemas.join("pinyin.schema.toml"),
        "[schema]\nid = \"pinyin\"\nname = \"拼\"\n[engine]\ntype = \"pinyin\"\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("zz_wb.schema.toml"),
        "[schema]\nid = \"zz_wb\"\nname = \"测五\"\nhidden = true\n[engine]\ntype = \"codetable\"\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("aux_code/zz_named.txt"),
        "# name: 小鹤\n李=mz\n",
    )
    .unwrap();
    std::fs::write(schemas.join("aux_code/zz_plain.txt"), "李=mz\n").unwrap();
    let store_path = dir.join("s.redb");
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    let c = Coordinator::new_headless_with_store_override(
        wind_config::Config::default(),
        Some(&dir),
        store,
        None,
    );
    let v = c
        .web_data_rpc("schema.auxCodeSources", &json!({ "id": "pinyin" }))
        .unwrap();
    let schemas_v = v["schemas"].as_array().unwrap();
    assert!(
        schemas_v
            .iter()
            .any(|s| s["id"] == "zz_wb" && s["name"] == "测五"),
        "隐藏的码表方案也列出"
    );
    assert!(
        !schemas_v.iter().any(|s| s["id"] == "pinyin"),
        "请求方自己 / 非码表不列"
    );
    let files = v["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|f| f["path"] == "aux_code/zz_named.txt" && f["label"] == "小鹤")
    );
    assert!(
        files
            .iter()
            .any(|f| f["path"] == "aux_code/zz_plain.txt" && f["label"] == "zz_plain"),
        "无头时退回文件名"
    );
}

#[test]
fn dict_list_paged_sort() {
    let c = coord("dict_sort");
    for (code, text, weight) in [("ab", "B词", 10i32), ("aa", "A词", 30), ("ac", "C词", 5)] {
        c.web_data_rpc(
            "dict.add",
            &json!({ "schemaId": "wb", "code": code, "text": text, "weight": weight }),
        )
        .unwrap();
    }
    // weight asc：5 → 10 → 30
    let r = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "wb", "offset": 0, "limit": 10,
                      "sortBy": "weight", "sortOrder": "asc" }),
        )
        .unwrap();
    let items = r["items"].as_array().unwrap();
    assert_eq!(items[0]["weight"], 5, "asc 首项应为最小权重");
    assert_eq!(items[2]["weight"], 30, "asc 末项应为最大权重");
    // weight desc：30 → 10 → 5
    let r2 = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "wb", "offset": 0, "limit": 10,
                      "sortBy": "weight", "sortOrder": "desc" }),
        )
        .unwrap();
    assert_eq!(r2["items"][0]["weight"], 30, "desc 首项应为最大权重");
    // 跨页切片：asc offset=1 limit=1 取排序后第 2 条（weight=10）
    let r3 = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "wb", "offset": 1, "limit": 1,
                      "sortBy": "weight", "sortOrder": "asc" }),
        )
        .unwrap();
    assert_eq!(r3["total"], 3, "跨页切片 total 不变");
    assert_eq!(r3["items"][0]["weight"], 10, "offset=1 asc 应取 weight=10");
    // 不传 sortBy 行为不变（total 正确）
    let r4 = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "wb", "offset": 0, "limit": 10 }),
        )
        .unwrap();
    assert_eq!(r4["total"], 3, "不传 sortBy 应保持原有行为");
}

/// `freq.listPaged` 的三条路必须彼此自洽（与 `dict.listPaged` 同款判据：流式分页 / 全量排序 /
/// 一遍扫描分两组，最容易坏的是三条彼此不一致）。
#[test]
fn freq_list_paged_stays_consistent_across_its_three_paths() {
    let c = coord("freq_paths");
    let store = c.user_store().unwrap();
    for (code, text) in [
        ("nihao", "你好"),
        ("nimen", "你们"),
        ("haoya", "好呀"),
        ("women", "我们"),
    ] {
        store.record_freq("py", code, text).unwrap();
    }
    let call = |p: Value| c.web_data_rpc("freq.listPaged", &p).unwrap();
    let texts = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["text"].as_str().unwrap_or("").to_string())
            .collect()
    };

    // ① 流式路：分页拼接 == 一次取全部。
    let all = call(json!({ "schemaId": "py", "offset": 0, "limit": 100 }));
    assert_eq!(all["total"], json!(4));
    let whole = texts(&all);
    let mut stitched = Vec::new();
    for off in [0, 2] {
        let page = call(json!({ "schemaId": "py", "offset": off, "limit": 2 }));
        assert_eq!(page["total"], json!(4), "翻页不得改变总数");
        stitched.extend(texts(&page));
    }
    assert_eq!(stitched, whole, "分页拼接必须逐条等于一次取全部");

    // ② 排序路：集合与 ① 相同。
    let sorted = call(json!({
        "schemaId": "py", "offset": 0, "limit": 100, "sortBy": "text", "sortDesc": false
    }));
    assert_eq!(sorted["total"], json!(4), "排序不得改变总数");
    let (mut a, mut b) = (texts(&sorted), whole.clone());
    a.sort();
    b.sort();
    assert_eq!(a, b, "排序路与流式路必须是同一批条目");

    // ③ 搜索路：前缀命中在前、不重复；编码中段与词条内容也要能捞到。
    let hit = call(json!({ "schemaId": "py", "prefix": "ni", "offset": 0, "limit": 100 }));
    let got = texts(&hit);
    assert_eq!(
        hit["total"].as_u64().unwrap() as usize,
        got.len(),
        "total 必须等于实际条数"
    );
    let mut dedup = got.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), got.len(), "同一条不得同时进两组");
    assert_eq!(
        &got[..2],
        &["你好".to_string(), "你们".to_string()],
        "编码前缀命中必须在前，实际 {got:?}"
    );
    // 只能由编码中段命中的那条。
    let mid = texts(&call(
        json!({ "schemaId": "py", "prefix": "ya", "offset": 0, "limit": 100 }),
    ));
    assert!(
        mid.contains(&"好呀".to_string()),
        "编码中段命中丢了：{mid:?}"
    );
    // 只能由词条内容命中的那条。
    let by_text = texts(&call(
        json!({ "schemaId": "py", "prefix": "我", "offset": 0, "limit": 100 }),
    ));
    assert_eq!(by_text, vec!["我们".to_string()], "按词条内容搜不到");
}

#[test]
fn freq_list_paged_sort() {
    let c = coord("freq_sort");
    let store = c.user_store().unwrap();
    // de=1次, ta=2次, shi=3次
    store.record_freq("py", "de", "的").unwrap();
    store.record_freq("py", "ta", "他").unwrap();
    store.record_freq("py", "ta", "他").unwrap();
    store.record_freq("py", "shi", "是").unwrap();
    store.record_freq("py", "shi", "是").unwrap();
    store.record_freq("py", "shi", "是").unwrap();
    // count asc：1 → 2 → 3
    let r = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "offset": 0, "limit": 10,
                      "sortBy": "count", "sortOrder": "asc" }),
        )
        .unwrap();
    let items = r["items"].as_array().unwrap();
    assert_eq!(items[0]["count"], 1, "asc 首项应为 count=1");
    assert_eq!(items[2]["count"], 3, "asc 末项应为 count=3");
    // count desc：3 → 2 → 1
    let r2 = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "offset": 0, "limit": 10,
                      "sortBy": "count", "sortOrder": "desc" }),
        )
        .unwrap();
    assert_eq!(r2["items"][0]["count"], 3, "desc 首项应为 count=3");
    // 跨页切片：asc offset=1 limit=1 取第 2 条（count=2）
    let r3 = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "offset": 1, "limit": 1,
                      "sortBy": "count", "sortOrder": "asc" }),
        )
        .unwrap();
    assert_eq!(r3["total"], 3, "跨页切片 total 不变");
    assert_eq!(r3["items"][0]["count"], 2, "offset=1 asc 应取 count=2");
    // 不传 sortBy 行为不变
    let r4 = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "offset": 0, "limit": 10 }),
        )
        .unwrap();
    assert_eq!(r4["total"], 3, "不传 sortBy 应保持原有行为");
}

#[test]
fn dict_list_paged_text_query() {
    let c = coord("dict_text_query");
    for (code, text, weight) in [
        ("wghg", "程序", 3i32),
        ("ggkg", "王中", 5),
        ("aaaa", "工", 0),
    ] {
        c.web_data_rpc(
            "dict.add",
            &json!({ "schemaId": "wb", "code": code, "text": text, "weight": weight }),
        )
        .unwrap();
    }
    // 按词条内容搜索：编码 "wghg" 不以 "程" 开头，命中应来自 text 包含匹配。
    let r = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "wb", "prefix": "程", "offset": 0, "limit": 10 }),
        )
        .unwrap();
    assert_eq!(r["total"], 1, "词条内容搜索应命中 1 条");
    assert_eq!(r["items"][0]["text"], "程序", "应按 text 内容命中");
    // 按编码前缀搜索仍生效。
    let r2 = c
        .web_data_rpc(
            "dict.listPaged",
            &json!({ "schemaId": "wb", "prefix": "wg", "offset": 0, "limit": 10 }),
        )
        .unwrap();
    assert_eq!(r2["total"], 1, "编码前缀搜索应命中 1 条");
    assert_eq!(r2["items"][0]["code"], "wghg", "应按 code 前缀命中");
}

#[test]
fn freq_list_paged_text_query() {
    let c = coord("freq_text_query");
    let store = c.user_store().unwrap();
    store.record_freq("py", "nihao", "你好").unwrap();
    store.record_freq("py", "women", "我们").unwrap();
    // 按词条内容搜索：编码 "nihao" 不以 "你" 开头，命中应来自 text 包含匹配。
    let r = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "prefix": "你", "offset": 0, "limit": 10 }),
        )
        .unwrap();
    assert_eq!(r["total"], 1, "词条内容搜索应命中 1 条");
    assert_eq!(r["items"][0]["text"], "你好", "应按 text 内容命中");
    // 按编码前缀搜索仍生效。
    let r2 = c
        .web_data_rpc(
            "freq.listPaged",
            &json!({ "schemaId": "py", "prefix": "women", "offset": 0, "limit": 10 }),
        )
        .unwrap();
    assert_eq!(r2["total"], 1, "编码前缀搜索应命中 1 条");
    assert_eq!(r2["items"][0]["text"], "我们", "应按 code 前缀命中对应词条");
}

#[test]
fn phrase_list_user_sort() {
    let c = coord("phrase_sort");
    for (code, text, weight) in [("b", "乙", 20i32), ("a", "甲", 50), ("c", "丙", 5)] {
        c.web_data_rpc(
            "phrase.add",
            &json!({ "code": code, "text": text, "position": 0, "weight": weight }),
        )
        .unwrap();
    }
    // weight asc：5 → 20 → 50
    let r = c
        .web_data_rpc(
            "phrase.listUser",
            &json!({ "offset": 0, "limit": 10,
                      "sortBy": "weight", "sortOrder": "asc" }),
        )
        .unwrap();
    let items = r["items"].as_array().unwrap();
    assert_eq!(items[0]["weight"], 5, "asc 首项应为 weight=5");
    assert_eq!(items[2]["weight"], 50, "asc 末项应为 weight=50");
    // weight desc：50 → 20 → 5
    let r2 = c
        .web_data_rpc(
            "phrase.listUser",
            &json!({ "offset": 0, "limit": 10,
                      "sortBy": "weight", "sortOrder": "desc" }),
        )
        .unwrap();
    assert_eq!(r2["items"][0]["weight"], 50, "desc 首项应为 weight=50");
    // 跨页切片：asc offset=1 limit=1 取第 2 条（weight=20）
    let r3 = c
        .web_data_rpc(
            "phrase.listUser",
            &json!({ "offset": 1, "limit": 1,
                      "sortBy": "weight", "sortOrder": "asc" }),
        )
        .unwrap();
    assert_eq!(r3["total"], 3, "跨页切片 total 不变");
    assert_eq!(r3["items"][0]["weight"], 20, "offset=1 asc 应取 weight=20");
    // 不传 sortBy 行为不变
    let r4 = c
        .web_data_rpc("phrase.listUser", &json!({ "offset": 0, "limit": 10 }))
        .unwrap();
    assert_eq!(r4["total"], 3, "不传 sortBy 应保持原有行为");
}

#[test]
fn scheme_package_rpc_contract() {
    let c = coord("schemepkg");
    // exportPackage:不存在的方案 id → 错误
    assert!(
        c.web_data_rpc(
            "scheme.exportPackage",
            &json!({ "id": "zz_no_such_schema", "path": std::env::temp_dir().join("zz_no.zip").to_string_lossy() }),
        )
        .is_err(),
        "不存在的方案应报错"
    );
    // previewImport:不存在的包路径 → 错误
    assert!(
        c.web_data_rpc(
            "scheme.previewImport",
            &json!({ "path": std::env::temp_dir().join("zz_no_such_pkg.zip").to_string_lossy() }),
        )
        .is_err()
    );
    // previewImport:真实构造的包 → 只读预览成功,形状正确
    let t = std::env::temp_dir().join("wind_schemepkg_test");
    let _ = std::fs::remove_dir_all(&t);
    let (user, system) = (t.join("u"), t.join("s"));
    std::fs::create_dir_all(user.join("my")).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(
        user.join("my.schema.toml"),
        "[schema]\nid=\"my\"\n[[dictionaries]]\npath=\"my/d.yaml\"\n",
    )
    .unwrap();
    std::fs::write(user.join("my/d.yaml"), "d").unwrap();
    let pkg = t.join("my.zip");
    wind_transfer::scheme::export_package(
        "my",
        &user,
        std::slice::from_ref(&system),
        None,
        &pkg,
        "1.0.0",
        "windows",
        "t",
    )
    .unwrap();
    let prev = c
        .web_data_rpc(
            "scheme.previewImport",
            &json!({ "path": pkg.to_string_lossy() }),
        )
        .unwrap();
    assert_eq!(
        prev.get("package")
            .and_then(|p| p.get("schema"))
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str()),
        Some("my"),
        "v2 预览返回 package 元信息"
    );
    assert!(prev.get("willAdd").and_then(|v| v.as_array()).is_some());
    assert!(prev.get("conflicts").and_then(|v| v.as_array()).is_some());
    let _ = std::fs::remove_dir_all(&t);
    // importPackage:不存在的包路径 → 错误
    assert!(
        c.web_data_rpc(
            "scheme.importPackage",
            &json!({ "path": std::env::temp_dir().join("zz_no_such_pkg.zip").to_string_lossy() }),
        )
        .is_err()
    );
}

/// 主题包三个 RPC 的边界契约:导出→预览的闭环,以及各自的拒绝路径。
///
/// 只测不写用户目录的那两条：`theme.importPackage` 落盘到真实的用户主题目录，在测试进程里跑
/// 会改开发机上的配置；导入的落盘/回滚语义由 `wind-transfer::theme` 的单测覆盖。
///
/// 导出取 data 层的主题，整条「按 slug 找目录 → 打包 → 读回」在临时目录里闭合。但搜索链首是
/// 真实的用户主题目录（优先级高于 data 层），夹具 slug 必须取开发机上撞不上的名字。
#[test]
fn theme_package_rpc_contract() {
    // 目录名带 pid:多 worktree / 多会话并行跑测试时固定名会互删夹具
    // (同 `tests/custom_layer_hide_theme_import.rs` 的纪律)。
    let tag = format!("themepkg{}", std::process::id());
    let c = coord(&tag);
    let data = std::env::temp_dir().join(format!("wind_webdata_data_{tag}"));
    let out_dir = std::env::temp_dir().join(format!("wind_themepkg_out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();
    let slug = "wind_test_themepkg_mo";

    // exportPackage：穿越形态的 slug 在碰盘之前按 slug 拒掉。断错误文案而不是 `is_err()`：
    // 这些路径本就不存在，只断 `is_err()` 的话删掉守卫也照样绿。
    for bad in ["../evil", "a/b", r"a\b", ""] {
        let e = c
            .web_data_rpc(
                "theme.exportPackage",
                &json!({ "slug": bad, "path": out_dir.join("x.wtheme").to_string_lossy() }),
            )
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("非法主题 slug"),
            "应在碰盘前按 slug 拒掉 {bad:?}，实际报的是: {e}"
        );
    }
    // 反过来:合法但不存在的 slug 走的是**另一条**路(找不到),两条合起来才证明
    // 守卫与「找不到」没有混成一条。
    let e = c
        .web_data_rpc(
            "theme.exportPackage",
            &json!({ "slug": "zz_no_such_theme", "path": out_dir.join("x.wtheme").to_string_lossy() }),
        )
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("主题不存在") && !e.contains("非法主题 slug"),
        "不存在的主题应报「主题不存在」，实际: {e}"
    );

    // 在 data 层造一个主题:theme.toml + 两个资源 + 预览图 + 一个无关文件。
    let src = data.join("themes").join(slug);
    std::fs::create_dir_all(src.join("assets/sub")).unwrap();
    std::fs::write(
        src.join("theme.toml"),
        "[meta]\nname = \"水墨\"\nauthor = \"某人\"\nversion = \"1.2\"\n\n[window]\nbackground = { image = { ref = \"assets/bg.png\" } }\n",
    )
    .unwrap();
    std::fs::write(src.join("assets/bg.png"), b"bg").unwrap();
    std::fs::write(src.join("assets/sub/x.png"), b"x").unwrap();
    std::fs::write(src.join("preview.png"), b"fake-png").unwrap();
    std::fs::write(src.join("Thumbs.db"), b"junk").unwrap();

    let pkg = out_dir.join("mo.wtheme");
    let r = c
        .web_data_rpc(
            "theme.exportPackage",
            &json!({ "slug": slug, "path": pkg.to_string_lossy() }),
        )
        .unwrap();
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["display_name"], json!("水墨"), "展示名取自主题 meta");
    // 无关文件不计入,预览图不算资源(它由 has_preview 单独表达)。
    assert_eq!(
        r["asset_count"],
        json!(2),
        "只有 assets/** 下的两个文件算资源"
    );
    assert_eq!(
        r["has_preview"],
        json!(true),
        "主题目录里的 preview.png 要带进包——这一步是 RPC 层独有的,crate 层的 export_package 不读它"
    );
    assert!(pkg.is_file(), "包要真的落在给定路径上");

    // previewPackage:刚导出的包读得回来,且与导出回执逐字对得上(导出与导入两套
    // 条目判据走岔时,这里当场炸)。
    let prev = c
        .web_data_rpc(
            "theme.previewPackage",
            &json!({ "path": pkg.to_string_lossy() }),
        )
        .unwrap();
    assert_eq!(prev["display_name"], r["display_name"]);
    assert_eq!(prev["asset_count"], r["asset_count"]);
    assert_eq!(prev["has_preview"], r["has_preview"]);
    assert_eq!(prev["author"], json!("某人"));
    assert_eq!(prev["version"], json!("1.2"));

    // 没有 preview.png 的主题:导出照样成,has_preview 如实为 false。
    std::fs::remove_file(src.join("preview.png")).unwrap();
    let bare = out_dir.join("mo-bare.wtheme");
    let r2 = c
        .web_data_rpc(
            "theme.exportPackage",
            &json!({ "slug": slug, "path": bare.to_string_lossy() }),
        )
        .unwrap();
    assert_eq!(r2["has_preview"], json!(false));

    // 导出失败不留残骸,更不毁掉落点上原有的包:拿一个必然过不了自检的主题
    // (条目数越过包格式上限)去打到一个已有好包的路径上。
    let many = data.join("themes").join("wind_test_themepkg_many");
    std::fs::create_dir_all(many.join("assets")).unwrap();
    std::fs::write(many.join("theme.toml"), "[meta]\nname = \"太多\"\n").unwrap();
    for i in 0..=wind_transfer::theme::MAX_THEME_ENTRIES {
        std::fs::write(many.join("assets").join(format!("{i}.png")), b"x").unwrap();
    }
    let good_before = std::fs::read(&bare).unwrap();
    let e = c
        .web_data_rpc(
            "theme.exportPackage",
            &json!({ "slug": "wind_test_themepkg_many", "path": bare.to_string_lossy() }),
        )
        .unwrap_err();
    // `{:#}` 与 RPC 出口同口径:`dispatch` 就是按这个格式把错误发给设置端与 CLI 的
    // （出口本身不许退回 `to_string()`,那条由 `dispatch` 自己的用例钉着）。
    let e = format!("{e:#}");
    // 断文案:否则哪天这个夹具因为别的原因(比如主题本身没通过 validate)提前失败,
    // 下面那两条「旧包还在」就会在一个根本没走到打包的路径上假绿。
    assert!(
        e.contains("主题资源过多"),
        "应因条目数越过上限而失败（导出前就按导入口径拦下），实际: {e}"
    );
    assert!(
        e.contains("导出主题"),
        "错误链要带上「导出主题」这层主语，实际: {e}"
    );
    assert_eq!(
        std::fs::read(&bare).unwrap(),
        good_before,
        "失败的导出必须原封不动地留下落点上那份旧包"
    );
    assert!(
        !bare.with_extension("wtheme.tmp").exists(),
        "失败后不留 .tmp 残骸"
    );

    // previewPackage / importPackage:不存在的包路径 → 错误
    let missing = out_dir.join("zz_no_such.wtheme");
    for m in ["theme.previewPackage", "theme.importPackage"] {
        assert!(
            c.web_data_rpc(m, &json!({ "path": missing.to_string_lossy() }))
                .is_err(),
            "{m} 对不存在的包应报错"
        );
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}

/// 分发包的说明元信息：包级 title/description 提到响应顶层，片段自带的说明进
/// `configPatch.info`。两者取不同的值，取错了源也照绿是同名字段最容易漏的假绿。
#[test]
fn scheme_preview_import_separates_package_and_patch_info() {
    let c = coord("schemepkginfo");
    let t = std::env::temp_dir().join("wind_schemepkginfo_test");
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(&t).unwrap();
    let pkg = t.join("info.wpkg");
    {
        let f = std::fs::File::create(&pkg).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            (
                "package.toml",
                "[package]\nformat_version = 2\ntitle = \"包级标题\"\ndescription = \"包级说明\"\n",
            ),
            (
                "zz_info_probe.schema.toml",
                "[schema]\nid=\"zz_info_probe\"\n",
            ),
            (
                "config_patch.toml",
                "[package]\ntitle = \"片段级标题\"\ndescription = \"片段级说明\"\n\
                     [ui.candidate]\nper_page = 9\n",
            ),
        ] {
            use std::io::Write;
            w.start_file(name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }
    let prev = c
        .web_data_rpc(
            "scheme.previewImport",
            &json!({ "path": pkg.to_string_lossy() }),
        )
        .unwrap();
    assert_eq!(prev["title"], json!("包级标题"), "包级说明提到顶层");
    assert_eq!(prev["description"], json!("包级说明"));
    let cp = prev.get("configPatch").expect("含 config_patch 应附 diff");
    assert_eq!(
        cp["info"]["title"],
        json!("片段级标题"),
        "片段自带的说明进 configPatch.info,不与包级混淆"
    );
    assert_eq!(cp["info"]["description"], json!("片段级说明"));
    // 保留段不产出配置条目:entries 只有真配置键那一条。
    let entries = cp["entries"].as_array().expect("entries 应为数组");
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["key"], json!("ui.candidate.per_page"));
    let _ = std::fs::remove_dir_all(&t);
}

/// 信封路径的两形:写了说明 → 顶层带出(只写 title 时 description 不出现);
/// 没写 → 顶层与 configPatch 都没有说明字段。
#[test]
fn scheme_preview_import_text_info_present_and_absent() {
    let c = coord("schemetextinfo");
    let with_info = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                         title = \"快符方案\"\n\
                         [[files]]\npath = \"zz_kf_info.schema.toml\"\ncontent = \"[schema]\\nid = 'kf'\\n\"\n";
    let prev = c
        .web_data_rpc("scheme.previewImportText", &json!({ "text": with_info }))
        .unwrap();
    assert_eq!(prev["title"], json!("快符方案"));
    assert!(
        prev.get("description").is_none(),
        "没写的字段不输出: {prev}"
    );

    let without_info = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                            [[files]]\npath = \"zz_kf_info.schema.toml\"\ncontent = \"[schema]\\nid = 'kf'\\n\"\n\
                            [[files]]\npath = \"config_patch.toml\"\ncontent = \"ui.candidate.per_page = 9\\n\"\n";
    let prev = c
        .web_data_rpc("scheme.previewImportText", &json!({ "text": without_info }))
        .unwrap();
    assert!(prev.get("title").is_none(), "无说明不输出 title: {prev}");
    assert!(prev.get("description").is_none());
    let cp = prev.get("configPatch").expect("含 config_patch 应附 diff");
    assert!(cp.get("info").is_none(), "片段没写说明就没有 info: {cp}");
}

/// 文本信封 RPC 契约(只测只读与错误路径:importText 会真写用户 schemas 目录,
/// 落盘语义已在 wind-transfer::envelope 层覆盖——与 importPackage 同一取舍)。
#[test]
fn scheme_text_envelope_rpc_contract() {
    let c = coord("schemetext");
    let envelope = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                        [schema]\nid = \"kf\"\nversion = \"1.00.0\"\n\
                        [[files]]\npath = \"zz_kf_probe.schema.toml\"\ncontent = \"[schema]\\nid = 'kf'\\n\"\n\
                        [[files]]\npath = \"config_patch.toml\"\ncontent = \"ui.candidate.per_page = 9\\n\"\n";
    let prev = c
        .web_data_rpc("scheme.previewImportText", &json!({ "text": envelope }))
        .unwrap();
    // 形状与 path 版一致
    assert_eq!(
        prev.get("package")
            .and_then(|p| p.get("schema"))
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str()),
        Some("kf")
    );
    assert!(prev.get("willAdd").and_then(|v| v.as_array()).is_some());
    assert!(prev.get("conflicts").and_then(|v| v.as_array()).is_some());
    // 配置片段随预览附带逐键 diff(不落盘、不应用)
    let cp = prev.get("configPatch").expect("含 config_patch 应附 diff");
    assert_eq!(cp["ok"], json!(true));
    let entries = cp["entries"].as_array().expect("entries 应为数组");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["key"], json!("ui.candidate.per_page"));
    assert_eq!(entries[0]["next"], json!(9));
    assert!(
        prev.get("willAdd")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().all(|v| v != "config_patch.toml"))
            .unwrap_or(false),
        "config_patch 不进文件清单"
    );

    // 非信封文本 → 错误带 not_schema_text: 前缀(设置端据此回落片段管线),两个方法一致。
    for method in ["scheme.previewImportText", "scheme.importText"] {
        let err = c
            .web_data_rpc(method, &json!({ "text": "ui.candidate.per_page = 9\n" }))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(wind_transfer::envelope::NOT_SCHEMA_TEXT),
            "{method} 对非信封文本须带回落前缀: {err}"
        );
    }
    // 缺 text 参数 → 错误
    assert!(
        c.web_data_rpc("scheme.previewImportText", &json!({}))
            .is_err()
    );
}

/// P4a Review Focus 4：Replace 还原统计后，收集器下一次写盘不覆盖还原值。
#[test]
fn restore_stats_resets_collector() {
    use wind_store::stat_collector::StatEvent;
    let c = coord("restore_stats");
    let store = c.user_store().cloned().unwrap();
    let col = c.stat_collector().expect("采集器");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    store
        .put_daily_stat(
            &today,
            &wind_store::stats::DailyStats {
                chinese: 5,
                ..Default::default()
            },
        )
        .unwrap();
    col.resume();
    let out = std::env::temp_dir().join(format!("wind_restore_stats_{}.zip", std::process::id()));
    let _ = std::fs::remove_file(&out);
    c.web_data_rpc(
        "backup.create",
        &json!({ "path": out.to_string_lossy(), "includeStats": true }),
    )
    .unwrap();
    col.record(StatEvent {
        timestamp: chrono::Local::now(),
        chinese: 100,
        english: 0,
        punct: 0,
        other: 0,
        code_len: 2,
        candidate_pos: 0,
        keystrokes: 2,
        schema_id: "wb".into(),
        source: Default::default(),
    });
    c.web_data_rpc(
        "backup.restore",
        &json!({ "path": out.to_string_lossy(), "sections": ["stats"], "strategy": "replace" }),
    )
    .unwrap();
    col.flush();
    assert_eq!(
        store.get_daily_stat(&today).unwrap().chinese,
        5,
        "收集器不得覆盖还原值"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn backup_rpc_contract() {
    let c = coord("backuprpc");
    // 种一条数据,create 到临时路径(coord 的 store 是临时 redb;文件域目录真实但只读不写:
    // create 只读取 config/schemas/themes,不写入它们)
    c.web_data_rpc(
        "dict.add",
        &json!({ "schemaId": "wb", "code": "a", "text": "工", "weight": 100 }),
    )
    .unwrap();
    let out = std::env::temp_dir().join("wind_backup_rpc_test.zip");
    let _ = std::fs::remove_file(&out);
    let r = c
        .web_data_rpc(
            "backup.create",
            &json!({ "path": out.to_string_lossy(), "includeStats": false }),
        )
        .unwrap();
    assert!(r.get("manifest").is_some());
    // inspect
    let ins = c
        .web_data_rpc("backup.inspect", &json!({ "path": out.to_string_lossy() }))
        .unwrap();
    assert_eq!(
        ins.get("manifest")
            .and_then(|m| m.get("kind"))
            .and_then(|v| v.as_str()),
        Some("backup")
    );
    // inspect 不存在的包 → 错误
    assert!(
        c.web_data_rpc(
            "backup.inspect",
            &json!({ "path": std::env::temp_dir().join("zz_no.zip").to_string_lossy() }),
        )
        .is_err()
    );
    // restore 仅数据域 sections(dict):写临时 store,不碰真实用户文件
    c.web_data_rpc("dict.clear", &json!({ "schemaId": "wb" }))
        .unwrap();
    let rr = c
        .web_data_rpc(
            "backup.restore",
            &json!({ "path": out.to_string_lossy(), "sections": ["dict"] }),
        )
        .unwrap();
    assert!(
        rr.get("restored")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    );
    let listed = c
        .web_data_rpc("dict.listPaged", &json!({ "schemaId": "wb", "limit": 10 }))
        .unwrap();
    assert_eq!(listed.get("total").and_then(|v| v.as_u64()), Some(1));
    let _ = std::fs::remove_file(&out);
}

/// 方案层 `[key_actions]` 的组合键没有消费通路：列出但标 `ineffective: true`，且不遮盖
/// 同名全局条目（全局那条照常列出、`ineffective: false`）。
///
/// 对照：方案层的单键 / 修饰键条目照旧遮盖全局（`semicolon`）；会话表不受影响。
#[test]
fn keys_overview_marks_schema_combo_keys_ineffective_without_shadowing_global() {
    let schema_cfg = json!({
        "key_actions": {
            "ctrl+alt+shift+j": "command:open(\"https://example.com\")",
            "ctrl+shift+r": "toggle_mode",
            "semicolon": "special:emoji",
        },
        "session_actions": { "comma": "page_prev" },
    });
    let mut cfg = wind_config::Config::default();
    cfg.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), "toggle_punct".into());
    cfg.keys
        .key_actions
        .insert("semicolon".into(), "mix:quick_mix".into());

    let (rows, _) = keys_overview_of(&cfg, &schema_cfg);
    let pick = |key: &str| -> Vec<&Value> {
        rows.iter()
            .filter(|r| r["table"] == json!("lead") && r["key"] == json!(key))
            .collect()
    };

    // 同名组合键：全局那条照常列出且生效，方案那条列出但不生效。
    let j = pick("ctrl+alt+shift+j");
    assert_eq!(j.len(), 2, "全局与方案两条都应列出：{j:?}");
    assert!(
        j.iter().any(|r| r["from"] == json!("global")
            && r["action"] == json!("toggle_punct")
            && r["ineffective"] == json!(false)),
        "全局那条不得被遮盖：{j:?}"
    );
    assert!(
        j.iter()
            .any(|r| r["from"] == json!("schema") && r["ineffective"] == json!(true)),
        "方案那条应标不生效：{j:?}"
    );

    // 只有方案层有的组合键：同样标不生效。
    let r = pick("ctrl+shift+r");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["ineffective"], json!(true));

    // 对照：方案层单键照旧遮盖全局，且生效。
    let semi = pick("semicolon");
    assert_eq!(semi.len(), 1, "单键方案条目照旧遮盖全局：{semi:?}");
    assert_eq!(semi[0]["from"], json!("schema"));
    assert_eq!(semi[0]["ineffective"], json!(false));

    // 字段恒在：每一条都带 `ineffective`（布尔）。
    assert!(
        rows.iter().all(|r| r["ineffective"].is_boolean()),
        "ineffective 须恒在：{rows:?}"
    );
    // 会话表不受影响。
    assert!(
        rows.iter()
            .filter(|r| r["table"] == json!("session"))
            .all(|r| r["ineffective"] == json!(false))
    );
}

/// 按键总览的降级标记：`keys.key_actions` 段回落出厂值时，那张表的全局层不列出，
/// 并经 `keysOverviewDegraded` 说明缺了什么，否则会把出厂默认当成用户的真实绑定展示。
///
/// 三条判据互相兜底：① 坏表的全局层消失；② 好表的全局层一条不少；③ 方案层照常列出
/// （来自方案文件，不受 `Config` 降级影响）。只测 ① 的话，「两张表全清」也照样绿。
#[test]
fn keys_overview_marks_degraded_table_and_spares_the_healthy_one() {
    let schema_cfg = json!({
        "key_actions": { "semicolon": "special:emoji" },
        "session_actions": { "comma": "page_prev" },
    });

    let mut cfg = wind_config::Config::default();
    cfg.keys
        .key_actions
        .insert("F7".into(), "special:number".into());
    cfg.keys
        .session_actions
        .insert("apostrophe".into(), "none".into());

    // 前置：无降级时两层都在，且**全局层确实非空**——否则下面「消失了」的断言
    // 会因为它本来就是空的而恒绿。
    let (clean, marker) = keys_overview_of(&cfg, &schema_cfg);
    assert_eq!(marker, Value::Null, "无降级时标记须为显式 null");
    let global_of = |rows: &[Value], table: &str| -> Vec<String> {
        rows.iter()
            .filter(|r| r["table"] == json!(table) && r["from"] == json!("global"))
            .map(|r| r["key"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    assert!(
        global_of(&clean, "lead").contains(&"F7".to_string()),
        "前置：无降级时全局层的 F7 须在表里\n{clean:?}"
    );
    assert!(
        global_of(&clean, "session").contains(&"apostrophe".to_string()),
        "前置：无降级时全局层的 apostrophe 须在表里\n{clean:?}"
    );

    // 只有 `keys.key_actions` 降级（同段的 `session_actions` 完好）。
    cfg.degradation = wind_config::config::ConfigDegradation {
        sections: vec!["keys.key_actions".into()],
        total_fallback: false,
        unparsable: Vec::new(),
    };
    let (rows, marker) = keys_overview_of(&cfg, &schema_cfg);

    assert!(
        global_of(&rows, "lead").is_empty(),
        "★ 不可信的全局层必须不列，而不是把出厂表当用户绑定展示\n{rows:?}"
    );
    assert!(
        global_of(&rows, "session").contains(&"apostrophe".to_string()),
        "★ 完好的那张表不该被牵连\n{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r["table"] == json!("lead") && r["from"] == json!("schema")),
        "方案层来自方案文件，不受 Config 降级影响，须照常列出\n{rows:?}"
    );

    assert_eq!(
        marker,
        json!({
            "tables": ["lead"],
            "sections": ["keys.key_actions"],
            "totalFallback": false,
        }),
        "★ 调用方须看得出「哪张表不完整、因为哪些段降级」；点分路径原样传出"
    );

    // 整份回落：两张表的全局层都不可信。
    cfg.degradation = wind_config::config::ConfigDegradation {
        sections: Vec::new(),
        total_fallback: true,
        unparsable: Vec::new(),
    };
    let (rows, marker) = keys_overview_of(&cfg, &schema_cfg);
    assert!(
        global_of(&rows, "lead").is_empty() && global_of(&rows, "session").is_empty(),
        "整份回落时两张表的全局层都不该列\n{rows:?}"
    );
    assert_eq!(marker["tables"], json!(["lead", "session"]), "{marker}");
    assert_eq!(marker["totalFallback"], json!(true), "{marker}");
}

/// session 表的判据必须覆盖折算来源，不只是同名的 `keys.session_actions`。
///
/// `effective_session_actions()` 把 `page_keys` / `select_char_keys` / `highlight_keys` /
/// `select_key_groups` 折算成具体绑定，段级降级能单独定位它们（如 `[keys] page_keys = "brackets"`
/// ⇒ `sections = ["keys.page_keys"]`）；只问 `keys.session_actions` 会让总览照常列出折算自
/// 出厂组名的翻页键、标记却是 `null`。四个来源逐个断言，只测一条的话另外三条删掉照样绿。
#[test]
fn session_table_judgement_covers_every_folded_source() {
    let schema_cfg = json!({ "session_actions": { "comma": "page_prev" } });
    let mut cfg = wind_config::Config::default();
    cfg.keys
        .key_actions
        .insert("F7".into(), "special:number".into());

    let global_of = |rows: &[Value], table: &str| -> Vec<String> {
        rows.iter()
            .filter(|r| r["table"] == json!(table) && r["from"] == json!("global"))
            .map(|r| r["key"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    // 前置：出厂 `page_keys` 等折算出的全局绑定确实非空，否则「消失了」恒真。
    let (clean, _) = keys_overview_of(&cfg, &schema_cfg);
    assert!(
        !global_of(&clean, "session").is_empty(),
        "前置：折算来的全局 session 绑定须非空\n{clean:?}"
    );

    for src in [
        "keys.session_actions",
        "keys.page_keys",
        "keys.select_char_keys",
        "keys.highlight_keys",
        "keys.select_key_groups",
    ] {
        cfg.degradation = wind_config::config::ConfigDegradation {
            sections: vec![src.into()],
            total_fallback: false,
            unparsable: Vec::new(),
        };
        let (rows, marker) = keys_overview_of(&cfg, &schema_cfg);
        assert!(
            global_of(&rows, "session").is_empty(),
            "★ {src} 降级时 session 表的全局层必须不列——它是这张表的折算来源\n{rows:?}"
        );
        assert_eq!(marker["tables"], json!(["session"]), "{src}: {marker}");
        assert_eq!(marker["sections"], json!([src]), "{src}: {marker}");
        // lead 表与这些来源无关，一条都不该被牵连。
        assert!(
            global_of(&rows, "lead").contains(&"F7".to_string()),
            "★ {src} 与 lead 表无关，不该牵连它\n{rows:?}"
        );
    }
}

/// `keysOverviewDegraded.sections` 只列**真正导致这些表不可信**的降级段。
///
/// 传全量的话，`ui.font` 坏了也会出现在按键总览的「缺失原因」里，成了一条自信的错误解释。
#[test]
fn degraded_marker_lists_only_the_sections_that_caused_it() {
    let schema_cfg = json!({});
    let mut cfg = wind_config::Config::default();
    cfg.keys
        .key_actions
        .insert("F7".into(), "special:number".into());
    cfg.degradation = wind_config::config::ConfigDegradation {
        // 一条相关（导致 lead 表不可信）、两条无关。
        sections: vec!["keys.key_actions".into(), "ui.font".into(), "schema".into()],
        total_fallback: false,
        unparsable: Vec::new(),
    };

    let (_, marker) = keys_overview_of(&cfg, &schema_cfg);
    assert_eq!(marker["tables"], json!(["lead"]), "{marker}");
    assert_eq!(
        marker["sections"],
        json!(["keys.key_actions"]),
        "★ 与按键表无关的降级段不得混进「缺失原因」\n{marker}"
    );
}

/// 标记字段必须登记进 [`READONLY_SIDECAR_FIELDS`]，否则它会随 saveConfig 落进
/// override，方案文件从此带着一段某次启动的降级快照，没人读也没人删。
/// （剥离行为本身由 `strip_readonly_fields` 那条用例逐字段覆盖。）
#[test]
fn keys_overview_degraded_is_registered_as_sidecar() {
    assert!(
        READONLY_SIDECAR_FIELDS.contains(&"keysOverviewDegraded"),
        "旁路字段漏登记：{READONLY_SIDECAR_FIELDS:?}"
    );
}
