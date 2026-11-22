//! 写类方法的批量形式（`items`）：批量结果与逐条调用一致；整批有一条不合法时什么都不改；
//! 空 `items` 不写。用户分库两个方法的用例在 `user_dicts.rs` 的测试里（要真实方案）。

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

/// 系统短语一条（`sy` → `系统`）+ 常用字类（U+E000–U+E001 默认常用，其余生僻）。
fn fresh(tag: &str) -> Arc<Coordinator> {
    let base = std::env::temp_dir().join(format!("wind_batch_rpc_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("charsets")).unwrap();
    std::fs::write(
        base.join("charsets").join("common_han.yaml"),
        "---\nkey: common_han\nname: 常用汉字\norder: 50\nscope: han\nranges:\n  - U+E000-U+E001\n\
         default: common\noutside: rare\n",
    )
    .unwrap();
    std::fs::write(
        base.join("system.phrases.toml"),
        "[[phrases]]\ncode = 'sy'\ntext = '系统'\nweight = 1000\nposition = 1\n",
    )
    .unwrap();
    let store = Arc::new(Store::open(base.join("s.redb")).unwrap());
    Coordinator::new_headless_with_store(Config::default(), Some(&base), store)
}

fn rpc(c: &Coordinator, m: &str, p: Value) -> anyhow::Result<Value> {
    c.web_data_rpc(m, &p)
}

/// 本线程在 `f` 里提交了几次文本文件（`commit_text`）。钩子是进程级的，按线程过滤。
fn commits_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let me = std::thread::current().id();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let _g = wind_userdata::text::fold::add_before_rename_hook(Box::new(move || {
        if std::thread::current().id() == me {
            n2.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let r = f();
    (r, n.load(Ordering::SeqCst))
}

/// `params` 加上 `items`。
fn with_items(params: &Value, items: Value) -> Value {
    let mut p = params.clone();
    p["items"] = items;
    p
}

/// 只留 `keys` 这几个字段（去掉时间戳之类两个装置之间必然不同的列）。
fn proj(list: &Value, keys: &[&str]) -> Vec<Value> {
    let arr = list
        .get("items")
        .unwrap_or(list)
        .as_array()
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .map(|row| {
            let mut m = serde_json::Map::new();
            for k in keys {
                m.insert(k.to_string(), row.get(*k).cloned().unwrap_or(Value::Null));
            }
            Value::Object(m)
        })
        .collect()
}

struct Case {
    tag: &'static str,
    method: &'static str,
    /// 各条共享的顶层参数（批量时与 `items` 并列，逐条时与每条合并）。
    top: Value,
    items: Vec<Value>,
    /// 混在合法条目之间的一条非法条目（单条调用会报错的那种）。
    bad: Value,
    seed: fn(&Coordinator),
    snap: fn(&Coordinator) -> Value,
}

fn merged(top: &Value, item: &Value) -> Value {
    let mut p = top.clone();
    for (k, v) in item.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

/// 三条契约：批量 == 逐条（且确实改了东西）；有一条非法整批不动；空 items 回 0 条且不写。
fn check(case: Case) {
    let a = fresh(&format!("{}_a", case.tag));
    (case.seed)(&a);
    let before = (case.snap)(&a);
    for it in &case.items {
        rpc(&a, case.method, merged(&case.top, it)).unwrap();
    }
    let seq = (case.snap)(&a);
    assert_ne!(
        seq, before,
        "{}：逐条调用没改到任何东西，用例无效",
        case.method
    );

    let b = fresh(&format!("{}_b", case.tag));
    (case.seed)(&b);
    let r = rpc(&b, case.method, with_items(&case.top, json!(case.items))).unwrap();
    assert_eq!(
        r,
        json!({ "ok": true, "count": case.items.len() }),
        "{}",
        case.method
    );
    assert_eq!(
        (case.snap)(&b),
        seq,
        "{}：批量结果与逐条不一致",
        case.method
    );

    let c = fresh(&format!("{}_c", case.tag));
    (case.seed)(&c);
    let mut bad_items = case.items.clone();
    bad_items.insert(bad_items.len() / 2 + 1, case.bad.clone());
    let err = rpc(&c, case.method, with_items(&case.top, json!(bad_items)));
    assert!(
        err.is_err(),
        "{}：有非法条目应整批报错：{err:?}",
        case.method
    );
    assert_eq!(
        (case.snap)(&c),
        before,
        "{}：报错的整批不该改动任何东西",
        case.method
    );

    let (r, n) = commits_during(|| rpc(&c, case.method, with_items(&case.top, json!([]))));
    assert_eq!(
        r.unwrap(),
        json!({ "ok": true, "count": 0 }),
        "{}",
        case.method
    );
    assert_eq!(n, 0, "{}：空 items 不该写文件", case.method);
    assert_eq!((case.snap)(&c), before, "{}", case.method);
}

fn phrase_list(c: &Coordinator) -> Value {
    json!(proj(
        &rpc(c, "phrase.list", json!({})).unwrap(),
        &["code", "text", "enabled", "isSystem"]
    ))
}

fn seed_phrases(c: &Coordinator) {
    for (code, text) in [("aa", "甲"), ("bb", r"乙\n丙"), ("cc", "丁")] {
        rpc(
            c,
            "phrase.add",
            json!({ "code": code, "text": text, "position": 0, "weight": 1 }),
        )
        .unwrap();
    }
}

#[test]
fn phrase_set_enabled_batch() {
    check(Case {
        tag: "ph_en",
        method: "phrase.setEnabled",
        top: json!({ "enabled": false }),
        // 系统条目写修改行、转义形态的 text 要还原、同批重复只写一行
        items: vec![
            json!({ "code": "aa", "text": "甲" }),
            json!({ "code": "bb", "text": r"乙\n丙" }),
            json!({ "code": "sy", "text": "系统" }),
            json!({ "code": "aa", "text": "甲" }),
        ],
        bad: json!({ "code": "cc" }),
        seed: seed_phrases,
        snap: phrase_list,
    });
}

/// 整批只提交一次短语文件（逐条是 N 次整文件重写）。
#[test]
fn phrase_set_enabled_batch_writes_once() {
    let c = fresh("ph_en_once");
    seed_phrases(&c);
    let items = json!([
        { "code": "aa", "text": "甲" },
        { "code": "bb", "text": r"乙\n丙" },
        { "code": "cc", "text": "丁" },
        { "code": "sy", "text": "系统" },
    ]);
    let (r, n) = commits_during(|| {
        rpc(
            &c,
            "phrase.setEnabled",
            json!({ "enabled": false, "items": items }),
        )
    });
    r.unwrap();
    assert_eq!(n, 1, "整批应只提交一次短语文件");
    let (_, n) = commits_during(|| {
        for it in [("aa", "甲"), ("cc", "丁")] {
            rpc(
                &c,
                "phrase.setEnabled",
                json!({ "enabled": true, "code": it.0, "text": it.1 }),
            )
            .unwrap();
        }
    });
    assert_eq!(n, 2, "对照：逐条每条一次");
}

#[test]
fn phrase_remove_batch() {
    check(Case {
        tag: "ph_rm",
        method: "phrase.remove",
        top: json!({}),
        items: vec![
            json!({ "code": "aa", "text": "甲" }),
            json!({ "code": "bb", "text": r"乙\n丙" }),
            json!({ "code": "zz", "text": "不存在" }),
        ],
        bad: json!({ "text": "丁" }),
        seed: seed_phrases,
        snap: phrase_list,
    });
}

/// 删掉遮蔽了系统条目的用户短语，批量也要补回那条系统短语（单条版的 restore 只做一次）。
#[test]
fn phrase_remove_batch_restores_shadowed_system_phrase() {
    let c = fresh("ph_rm_sys");
    rpc(
        &c,
        "phrase.add",
        json!({ "code": "sy", "text": "系统", "position": 0, "weight": 1 }),
    )
    .unwrap();
    rpc(
        &c,
        "phrase.remove",
        json!({ "items": [{ "code": "sy", "text": "系统" }] }),
    )
    .unwrap();
    let list = phrase_list(&c);
    assert_eq!(
        list,
        json!([{ "code": "sy", "text": "系统", "enabled": true, "isSystem": true }]),
        "{list}"
    );
}

fn dict_list(c: &Coordinator) -> Value {
    json!(proj(
        &rpc(
            c,
            "dict.listPaged",
            json!({ "schemaId": "wb", "offset": 0, "limit": 1000 })
        )
        .unwrap(),
        &["code", "text", "weight"]
    ))
}

fn seed_dict(c: &Coordinator) {
    for (code, text, w) in [("a", "工", 100), ("aa", "式", 10), ("b", "了", 5)] {
        rpc(
            c,
            "dict.add",
            json!({ "schemaId": "wb", "code": code, "text": text, "weight": w }),
        )
        .unwrap();
    }
}

#[test]
fn dict_add_batch() {
    check(Case {
        tag: "dict_add",
        method: "dict.add",
        top: json!({ "schemaId": "wb" }),
        // 已有词权重取 max、同批重复按逐条合并、转义还原、缺权重按 0
        items: vec![
            json!({ "code": "a", "text": "工", "weight": 300 }),
            json!({ "code": "c", "text": r"又\t双", "weight": 7 }),
            json!({ "code": "d", "text": "在", "weight": 9 }),
            json!({ "code": "d", "text": "在", "weight": 3 }),
            json!({ "code": "e", "text": "有" }),
        ],
        bad: json!({ "code": "f" }),
        seed: seed_dict,
        snap: dict_list,
    });
}

/// 存储层的校验（文本过长）同样整批拒绝，而不是写了前几条。
#[test]
fn dict_add_batch_rejects_whole_batch_on_store_validation() {
    let c = fresh("dict_add_long");
    seed_dict(&c);
    let before = dict_list(&c);
    let long = "长".repeat(wind_userdata::runtime::USER_TEXT_MAX_CHARS + 1);
    let r = rpc(
        &c,
        "dict.add",
        json!({ "schemaId": "wb", "items": [
            { "code": "x", "text": "先", "weight": 1 },
            { "code": "y", "text": long, "weight": 1 },
        ] }),
    );
    assert!(r.is_err(), "{r:?}");
    assert_eq!(dict_list(&c), before);
}

#[test]
fn dict_remove_batch() {
    check(Case {
        tag: "dict_rm",
        method: "dict.remove",
        top: json!({ "schemaId": "wb" }),
        items: vec![
            json!({ "code": "a", "text": "工" }),
            json!({ "code": "b", "text": "了" }),
            json!({ "code": "zz", "text": "无" }),
        ],
        bad: json!({ "code": "aa" }),
        seed: seed_dict,
        snap: dict_list,
    });
}

/// 拼音族：码带音节空格、方案折叠到 `pinyin`，批量与单条同样拆空格。
#[test]
fn dict_add_remove_batch_split_spaced_codes() {
    let c = fresh("dict_py");
    rpc(
        &c,
        "dict.add",
        json!({ "schemaId": "pinyin", "items": [
            { "code": "ni hao", "text": "你好", "weight": 5 },
            { "code": "zai jian", "text": "再见", "weight": 5 },
        ] }),
    )
    .unwrap();
    let list = rpc(
        &c,
        "dict.listPaged",
        json!({ "schemaId": "pinyin", "offset": 0, "limit": 10 }),
    )
    .unwrap();
    assert_eq!(list["total"], 2, "{list}");
    assert_eq!(list["items"][0]["code"], "ni hao", "{list}");
    rpc(
        &c,
        "dict.remove",
        json!({ "schemaId": "pinyin", "items": [
            { "code": "ni hao", "text": "你好" },
            { "code": "zai jian", "text": "再见" },
        ] }),
    )
    .unwrap();
    let list = rpc(
        &c,
        "dict.listPaged",
        json!({ "schemaId": "pinyin", "offset": 0, "limit": 10 }),
    )
    .unwrap();
    assert_eq!(list["total"], 0, "{list}");
}

fn temp_and_dict(c: &Coordinator) -> Value {
    json!({
        "temp": proj(
            &rpc(c, "temp.listPaged", json!({ "schemaId": "wb", "offset": 0, "limit": 100 })).unwrap(),
            &["code", "text"],
        ),
        "dict": dict_list(c),
    })
}

fn seed_temp(c: &Coordinator) {
    let s = c.user_store().unwrap();
    for (code, text) in [("gg", "王"), ("hh", "目"), ("jj", "日")] {
        s.learn_temp_word("wb", code, text, 100, 0).unwrap();
    }
    seed_dict(c);
}

#[test]
fn temp_remove_batch() {
    check(Case {
        tag: "temp_rm",
        method: "temp.remove",
        top: json!({ "schemaId": "wb" }),
        items: vec![
            json!({ "code": "gg", "text": "王" }),
            json!({ "code": "hh", "text": "目" }),
            json!({ "code": "kk", "text": "无" }),
        ],
        bad: json!({ "code": "jj" }),
        seed: seed_temp,
        snap: temp_and_dict,
    });
}

#[test]
fn temp_promote_batch() {
    check(Case {
        tag: "temp_pr",
        method: "temp.promote",
        top: json!({ "schemaId": "wb" }),
        items: vec![
            json!({ "code": "gg", "text": "王" }),
            json!({ "code": "jj", "text": "日" }),
            json!({ "code": "kk", "text": "无" }),
            json!({ "code": "gg", "text": "王" }),
        ],
        bad: json!({ "text": "目" }),
        seed: seed_temp,
        snap: temp_and_dict,
    });
}

/// 手动转正撤删除规则（单条版的 `clear_deleted_for_added`），批量对转正了的词整批撤一次。
#[test]
fn temp_promote_batch_clears_delete_rules() {
    let c = fresh("temp_pr_shadow");
    seed_temp(&c);
    rpc(
        &c,
        "shadow.delete",
        json!({ "schemaId": "wb", "code": "gg", "word": "王" }),
    )
    .unwrap();
    rpc(
        &c,
        "temp.promote",
        json!({ "schemaId": "wb", "items": [{ "code": "gg", "text": "王" }] }),
    )
    .unwrap();
    let rules = rpc(&c, "shadow.list", json!({ "schemaId": "wb" })).unwrap();
    assert_eq!(rules, json!([]), "{rules}");
}

fn freq_list(c: &Coordinator) -> Value {
    json!(proj(
        &rpc(
            c,
            "freq.listPaged",
            json!({ "schemaId": "pinyin", "offset": 0, "limit": 100 })
        )
        .unwrap(),
        &["code", "text", "count"]
    ))
}

fn seed_freq(c: &Coordinator) {
    let s = c.user_store().unwrap();
    for (code, text) in [("nihao", "你好"), ("haoya", "好呀"), ("wubian", "无边")] {
        s.record_freq("pinyin", code, text).unwrap();
    }
}

#[test]
fn freq_delete_batch() {
    check(Case {
        tag: "freq_del",
        method: "freq.delete",
        // 拼音族方案 id 折叠到 `pinyin`
        top: json!({ "schemaId": "pinyin" }),
        items: vec![
            json!({ "code": "nihao", "text": "你好" }),
            json!({ "code": "hao ya", "text": "好呀" }),
        ],
        bad: json!({ "code": "wubian" }),
        seed: seed_freq,
        snap: freq_list,
    });
}

fn shadow_list(c: &Coordinator) -> Value {
    rpc(c, "shadow.list", json!({ "schemaId": "wb" })).unwrap()
}

fn seed_shadow(c: &Coordinator) {
    let pin = |code: &str, word: &str, id: Option<&str>| {
        rpc(
            c,
            "shadow.pin",
            json!({ "schemaId": "wb", "code": code, "word": word, "candId": id, "position": 0 }),
        )
        .unwrap();
    };
    pin("aa", "甲", None);
    pin("aa", "今日", Some("phrase:date"));
    pin("bb", "丙", None);
    rpc(
        c,
        "shadow.delete",
        json!({ "schemaId": "wb", "code": "aa", "word": "乙" }),
    )
    .unwrap();
}

#[test]
fn shadow_remove_rule_batch() {
    check(Case {
        tag: "shadow_rm",
        method: "shadow.removeRule",
        top: json!({ "schemaId": "wb" }),
        // 同编码多条、candId 透传（word 已变也按 id 删得掉）
        items: vec![
            json!({ "code": "aa", "word": "甲" }),
            json!({ "code": "bb", "word": "丙" }),
            json!({ "code": "aa", "word": "昨日的文本", "candId": "phrase:date" }),
            json!({ "code": "aa", "word": "乙" }),
        ],
        bad: json!({ "code": "aa" }),
        seed: seed_shadow,
        snap: shadow_list,
    });
}

fn mail_list(c: &Coordinator) -> Value {
    json!(proj(
        &rpc(
            c,
            "mailSuffix.listPaged",
            json!({ "offset": 0, "limit": 100 })
        )
        .unwrap(),
        &["text", "count"]
    ))
}

fn seed_mail(c: &Coordinator) {
    let s = c.user_store().unwrap();
    for t in ["a.com", "b.com", "c.com"] {
        s.record_completion(CompletionKind::EmailSuffix, t).unwrap();
    }
    // 另一分区不受影响
    s.record_completion(CompletionKind::UrlHistory, "a.com")
        .unwrap();
}

#[test]
fn completion_delete_batch() {
    check(Case {
        tag: "mail_del",
        method: "mailSuffix.delete",
        top: json!({}),
        items: vec![json!({ "text": "a.com" }), json!({ "text": "c.com" })],
        bad: json!({ "txt": "b.com" }),
        seed: seed_mail,
        snap: |c| {
            json!({
                "mail": mail_list(c),
                "url": proj(
                    &rpc(c, "urlHistory.listPaged", json!({ "offset": 0, "limit": 100 })).unwrap(),
                    &["text"],
                ),
            })
        },
    });
}

fn quick_list(c: &Coordinator) -> Value {
    json!(proj(
        &rpc(c, "quick.list", json!({})).unwrap(),
        &["kind", "id", "enabled", "adjusted", "displayPos"]
    ))
}

fn seed_quick(c: &Coordinator) {
    for t in ["$Y/$M/$D", "$D-$M-$Y"] {
        rpc(c, "quick.add", json!({ "kind": "date", "text": t })).unwrap();
    }
    rpc(
        c,
        "quick.setEnabled",
        json!({ "kind": "number", "id": "number.amount", "enabled": false }),
    )
    .unwrap();
}

#[test]
fn quick_set_enabled_batch() {
    check(Case {
        tag: "quick_en",
        method: "quick.setEnabled",
        top: json!({ "enabled": false }),
        // 跨类别混在一批
        items: vec![
            json!({ "kind": "date", "id": "date.basic" }),
            json!({ "kind": "date", "id": "date.u1" }),
            json!({ "kind": "number", "id": "number.amount" }),
        ],
        bad: json!({ "kind": "nope", "id": "x" }),
        seed: seed_quick,
        snap: quick_list,
    });
}

#[test]
fn quick_reset_entry_batch() {
    check(Case {
        tag: "quick_reset",
        method: "quick.resetEntry",
        top: json!({}),
        items: vec![
            json!({ "kind": "number", "id": "number.amount" }),
            json!({ "kind": "date", "id": "date.cn" }),
        ],
        bad: json!({ "kind": "date" }),
        seed: seed_quick,
        snap: quick_list,
    });
}

#[test]
fn quick_delete_batch() {
    check(Case {
        tag: "quick_del",
        method: "quick.delete",
        top: json!({}),
        items: vec![
            json!({ "kind": "date", "id": "date.u1" }),
            json!({ "kind": "date", "id": "date.u2" }),
        ],
        // 出厂条目单条删会报错 ⇒ 整批不删
        bad: json!({ "kind": "date", "id": "date.cn" }),
        seed: seed_quick,
        snap: quick_list,
    });
}

fn common_list(c: &Coordinator) -> Value {
    json!(proj(
        &rpc(c, "commonChars.list", json!({ "onlyModified": true })).unwrap(),
        &["char", "common", "baseCommon"]
    ))
}

fn seed_common(c: &Coordinator) {
    for (ch, common) in [("\u{E000}", false), ("槮", true), ("ㄅ", true)] {
        rpc(
            c,
            "commonChars.set",
            json!({ "char": ch, "common": common }),
        )
        .unwrap();
    }
}

#[test]
fn common_chars_set_batch() {
    check(Case {
        tag: "cc_set",
        method: "commonChars.set",
        top: json!({ "common": true }),
        // `\u{E000}` 出厂常用：设回常用 = 删覆盖（不写同向记录）
        items: vec![
            json!({ "char": "\u{E000}" }),
            json!({ "char": "\u{E001}" }),
            json!({ "char": "穇" }),
            json!({ "char": "⚽️" }),
        ],
        bad: json!({ "char": "两字" }),
        seed: seed_common,
        snap: common_list,
    });
}

/// 设回出厂方向删覆盖、而不是写同向记录（`apply_common_target` 的语义），且整批只写一次。
#[test]
fn common_chars_set_batch_drops_redundant_and_writes_once() {
    let c = fresh("cc_once");
    seed_common(&c);
    let (r, n) = commits_during(|| {
        rpc(
            &c,
            "commonChars.set",
            json!({ "common": true, "items": [
                { "char": "\u{E000}" }, { "char": "穇" }, { "char": "丄" },
            ] }),
        )
    });
    r.unwrap();
    assert_eq!(n, 1, "整批应只提交一次常用字文件");
    let store = c.user_store().unwrap();
    assert_eq!(store.get_common_char_override("\u{E000}").unwrap(), None);
    assert_eq!(store.get_common_char_override("穇").unwrap(), Some(true));
    // 镜像已回灌：读端立即按新判定
    let q = rpc(&c, "commonChars.query", json!({ "char": "穇" })).unwrap();
    assert_eq!(q["effective"], json!(true), "{q}");
}

#[test]
fn common_chars_reset_batch() {
    check(Case {
        tag: "cc_reset",
        method: "commonChars.reset",
        top: json!({}),
        items: vec![json!({ "char": "\u{E000}" }), json!({ "char": "槮" })],
        bad: json!({ "char": "" }),
        seed: seed_common,
        snap: common_list,
    });
}

/// `items` 不是数组：报错、不退回单条路径。
#[test]
fn items_must_be_an_array() {
    let c = fresh("items_type");
    seed_phrases(&c);
    let before = phrase_list(&c);
    let r = rpc(
        &c,
        "phrase.setEnabled",
        json!({ "code": "aa", "text": "甲", "enabled": false, "items": { "code": "aa" } }),
    );
    assert!(r.is_err(), "{r:?}");
    assert_eq!(phrase_list(&c), before);
}
