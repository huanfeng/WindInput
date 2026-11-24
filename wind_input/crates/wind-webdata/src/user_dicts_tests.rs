use crate::WebDataRpc;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::web_host::WebDataHost;
use wind_store::Store;

struct Fx {
    dir: PathBuf,
    ct: String,
    py: String,
    c: Arc<Coordinator>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some(cache) = Config::cache_dir() {
            let _ = std::fs::remove_dir_all(cache.join(&self.ct));
            let _ = std::fs::remove_dir_all(cache.join(&self.py));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 码表方案 `ct`（工 a / 式 aa）与拼音方案 `py`（你好 ni hao）。
fn fx(tag: &str) -> Fx {
    let pid = std::process::id();
    let (ct, py) = (
        format!("zz_wdudl_ct_{tag}_{pid}"),
        format!("zz_wdudl_py_{tag}_{pid}"),
    );
    let dir = std::env::temp_dir().join(format!("wind_wdudl_{tag}_{pid}"));
    let _ = std::fs::remove_dir_all(&dir);
    let s = dir.join("schemas");
    for (id, ty, dty, body) in [
        (
            &ct,
            "codetable",
            "rime_codetable",
            "工\ta\t100\n式\taa\t10\n",
        ),
        (
            &py,
            "pinyin",
            "rime_pinyin",
            "你好\tni hao\t100\n后\thou\t100\n",
        ),
    ] {
        std::fs::create_dir_all(s.join(id)).unwrap();
        std::fs::write(
            s.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"测\"\n[engine]\ntype = \"{ty}\"\n\
                     [engine.codetable]\nmax_code_length = 4\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{id}/main.dict.yaml\"\ntype = \"{dty}\"\ndefault = true\n"
            ),
        )
        .unwrap();
        std::fs::write(
            s.join(format!("{id}/main.dict.yaml")),
            format!("---\nname: main\ncolumns: [text, code, weight]\n...\n{body}"),
        )
        .unwrap();
    }
    let store = Arc::new(Store::open(dir.join("u.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec![ct.clone(), py.clone()];
    cfg.schema.active = ct.clone();
    let c =
        Coordinator::new_headless_with_store_override(cfg, Some(&dir), store, Some(dir.join("ov")));
    Fx { dir, ct, py, c }
}

impl Fx {
    fn rpc(&self, m: &str, p: Value) -> anyhow::Result<Value> {
        self.c.web_data_rpc(m, &p)
    }
    fn texts(&self, id: &str, input: &str) -> Vec<String> {
        self.c
            .engine_mgr()
            .convert_with(id, input, 20)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    }
    fn root(&self) -> PathBuf {
        self.c.user_store().unwrap().user_text_dir().to_path_buf()
    }
}

/// 端到端：建分库 → 外部写入词 → 查到（同权排在系统库前）→ 关掉查不到 → 改文件 + reload 后新词可查。
#[test]
fn create_write_query_toggle_reload() {
    let f = fx("e2e");
    let ct = f.ct.clone();
    let r = f
        .rpc("userDicts.create", json!({"schemaId": ct, "name": "我的"}))
        .unwrap();
    let file = f.root().join(r["file"].as_str().unwrap());
    assert!(file.is_file(), "{r}");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("a\t戈\t100\n");
    std::fs::write(&file, &text).unwrap();
    f.rpc("dict.reload", json!({"paths": [file.to_string_lossy()]}))
        .unwrap();
    assert_eq!(f.texts(&ct, "a")[..2], ["戈", "工"]);

    f.rpc(
        "userDicts.setEnabled",
        json!({"schemaId": ct, "name": "我的", "enabled": false}),
    )
    .unwrap();
    assert!(!f.texts(&ct, "a").contains(&"戈".to_string()));
    let l = f.rpc("userDicts.list", json!({"schemaId": ct})).unwrap();
    assert_eq!(l[0]["enabled"], false, "{l}");
    f.rpc(
        "schema.setDictEnabled",
        json!({"id": ct, "dictId": "user:我的", "enabled": true}),
    )
    .unwrap();
    assert_eq!(f.texts(&ct, "a")[0], "戈");
    let ov = f.dir.join("ov").join(format!("{ct}.toml"));
    assert!(
        !std::fs::read_to_string(&ov)
            .unwrap_or_default()
            .contains("user:"),
        "分库开关不进 schema_overrides"
    );

    text.push_str("b\t新词\t5\n");
    std::fs::write(&file, &text).unwrap();
    let r = f.rpc("dict.reload", json!({})).unwrap();
    assert!(
        r["reloaded"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("dicts:{ct}"))),
        "{r}"
    );
    assert_eq!(f.texts(&ct, "b"), ["新词"]);
}

#[test]
fn list_entries_remove_and_paging() {
    let f = fx("entries");
    let ct = f.ct.clone();
    let tsv = f.dir.join("in.txt");
    std::fs::write(&tsv, "jj\t甲\t3\nyy\t乙\t9\nbb\t丙\t1\n坏行\n").unwrap();
    let r = f
        .rpc(
            "userDicts.importAs",
            json!({"schemaId": ct, "name": "导入", "path": tsv.to_string_lossy()}),
        )
        .unwrap();
    assert_eq!(
        (r["imported"].clone(), r["format"].clone()),
        (json!(3), json!("tsv")),
        "{r}"
    );
    assert!(
        f.rpc(
            "userDicts.importAs",
            json!({"schemaId": ct, "name": "导入", "path": tsv.to_string_lossy()}),
        )
        .is_err(),
        "已存在"
    );
    assert_eq!(f.texts(&ct, "yy"), ["乙"], "导入后即可查");

    let l = f.rpc("userDicts.list", json!({"schemaId": ct})).unwrap();
    assert_eq!(
        l,
        json!([{"name": "导入", "label": "导入", "enabled": true, "entries": 3,
                "file": format!("{ct}/dicts/导入.dict.yaml")}])
    );
    let page = |p: Value| {
        let mut p = p;
        p["schemaId"] = json!(ct);
        p["name"] = json!("导入");
        f.rpc("userDicts.entries", p).unwrap()
    };
    let r = page(json!({"offset": 0, "limit": 2, "sortBy": "weight", "sortOrder": "desc"}));
    assert_eq!(r["total"], 3);
    assert_eq!(
        r["items"][0],
        json!({"code": "yy", "text": "乙", "weight": 9, "enabled": true})
    );
    assert_eq!(r["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        page(json!({"offset": 5, "limit": 2}))["items"],
        json!([]),
        "越界给空页"
    );
    assert_eq!(page(json!({"query": "甲"}))["items"][0]["text"], "甲");

    let r = f
        .rpc(
            "userDicts.removeEntry",
            json!({"schemaId": ct, "name": "导入", "code": "yy", "text": "乙"}),
        )
        .unwrap();
    assert_eq!(r["removed"], true);
    assert!(f.texts(&ct, "yy").is_empty(), "删词后失效重建");
    assert_eq!(page(json!({}))["total"], 2);
    assert!(
        f.rpc(
            "userDicts.entries",
            json!({"schemaId": ct, "name": "无", "offset": 0, "limit": 10}),
        )
        .is_err(),
        "不存在"
    );
}

/// 界面加词 / 清空：改文件、失效重建；外部编辑中拒绝。
#[test]
fn add_entry_and_clear() {
    let f = fx("add");
    let ct = f.ct.clone();
    f.rpc("userDicts.create", json!({"schemaId": ct, "name": "我的"}))
        .unwrap();
    let add = |p: Value| {
        let mut p = p;
        p["schemaId"] = json!(ct);
        p["name"] = json!("我的");
        f.rpc("userDicts.addEntry", p)
    };
    assert_eq!(
        add(json!({"code": "a", "text": "戈", "weight": 100})).unwrap(),
        json!({"ok": true})
    );
    assert_eq!(f.texts(&ct, "a")[..2], ["戈", "工"], "加词后失效重建");
    add(json!({"code": "a", "text": "戈", "weight": 1})).unwrap();
    add(json!({"code": "b", "text": "新"})).unwrap();
    let e = f
        .rpc(
            "userDicts.entries",
            json!({"schemaId": ct, "name": "我的", "offset": 0, "limit": 10, "sortBy": "code"}),
        )
        .unwrap();
    assert_eq!(e["total"], 2);
    assert_eq!(e["items"][0]["weight"], 1, "{e}");
    assert!(add(json!({"code": "a\tb", "text": "x"})).is_err());
    for w in [json!("5"), json!(1.5), json!(1u64 << 40)] {
        assert!(
            add(json!({"code": "z", "text": "x", "weight": w})).is_err(),
            "{w}"
        );
    }
    assert!(add(json!({"code": "", "text": "x"})).is_err());

    f.rpc(
        "data.beginEdit",
        json!({"kind": "user_dict", "schemaId": ct, "name": "我的"}),
    )
    .unwrap();
    let err = add(json!({"code": "c", "text": "丙"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("正在外部编辑"), "{err}");
    assert!(
        f.rpc("userDicts.clear", json!({"schemaId": ct, "name": "我的"}))
            .is_err()
    );
    f.rpc(
        "data.endEdit",
        json!({"kind": "user_dict", "schemaId": ct, "name": "我的"}),
    )
    .unwrap();

    let r = f
        .rpc("userDicts.clear", json!({"schemaId": ct, "name": "我的"}))
        .unwrap();
    assert_eq!(r, json!({"ok": true, "removed": 2}));
    assert_eq!(f.texts(&ct, "b"), Vec::<String>::new(), "清空后失效重建");
    assert_eq!(f.texts(&ct, "a")[0], "工");
}

/// 本线程在 `f` 里提交了几次文本文件。
fn commits_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
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

/// 分库条目全表（按码排序），比批量与逐条的结果用。
fn lib_rows(f: &Fx, schema: &str) -> Value {
    f.rpc(
        "userDicts.entries",
        json!({"schemaId": schema, "name": "我的", "offset": 0, "limit": 100, "sortBy": "code"}),
    )
    .unwrap()
}

/// 建分库并放两条（`a 戈 100`、`b 新` 无权重）。
fn seeded(tag: &str) -> Fx {
    let f = fx(tag);
    let ct = f.ct.clone();
    f.rpc("userDicts.create", json!({"schemaId": ct, "name": "我的"}))
        .unwrap();
    for p in [
        json!({"code": "a", "text": "戈", "weight": 100}),
        json!({"code": "b", "text": "新"}),
    ] {
        let mut p = p;
        p["schemaId"] = json!(ct);
        p["name"] = json!("我的");
        f.rpc("userDicts.addEntry", p).unwrap();
    }
    f
}

/// `items` 批量：与逐条结果一致、一次提交、有一条非法整批不写、空 items 不写。
#[test]
fn add_and_remove_entry_batch() {
    let items = json!([
        {"code": "a", "text": "戈", "weight": 7},
        {"code": "c", "text": "丙", "weight": 3},
        {"code": "d", "text": "丁"},
        {"code": "c", "text": "丙", "weight": 5},
        {"code": "b", "text": "新"},
    ]);
    let one = |f: &Fx, m: &str, it: &Value| {
        let mut p = it.clone();
        p["schemaId"] = json!(f.ct);
        p["name"] = json!("我的");
        f.rpc(m, p)
    };

    let seq = seeded("batch_seq");
    for it in items.as_array().unwrap() {
        one(&seq, "userDicts.addEntry", it).unwrap();
    }
    let bat = seeded("batch_bat");
    let (r, n) = commits_during(|| {
        bat.rpc(
            "userDicts.addEntry",
            json!({"schemaId": bat.ct, "name": "我的", "items": items}),
        )
    });
    assert_eq!(r.unwrap(), json!({"ok": true, "count": 5}));
    assert_eq!(n, 1, "整批应只提交一次分库文件");
    assert_eq!(lib_rows(&bat, &bat.ct), lib_rows(&seq, &seq.ct));
    assert_eq!(lib_rows(&bat, &bat.ct)["total"], 4);
    assert_eq!(bat.texts(&bat.ct, "c")[0], "丙", "批量写完失效重建");

    // 有一条非法（码里有制表符、权重不是整数）⇒ 整批不写
    let before = lib_rows(&bat, &bat.ct);
    for bad in [
        json!({"code": "e\tx", "text": "戊"}),
        json!({"code": "e", "text": "戊", "weight": "9"}),
        json!({"code": "e"}),
    ] {
        let r = bat.rpc(
            "userDicts.addEntry",
            json!({"schemaId": bat.ct, "name": "我的", "items": [
                {"code": "f", "text": "己"}, bad,
            ]}),
        );
        assert!(r.is_err(), "{r:?}");
        assert_eq!(lib_rows(&bat, &bat.ct), before);
    }

    // 删：与逐条一致、一次提交、不存在的跳过
    let del = json!([
        {"code": "a", "text": "戈"},
        {"code": "zz", "text": "无"},
        {"code": "c", "text": "丙"},
    ]);
    for it in del.as_array().unwrap() {
        one(&seq, "userDicts.removeEntry", it).unwrap();
    }
    let (r, n) = commits_during(|| {
        bat.rpc(
            "userDicts.removeEntry",
            json!({"schemaId": bat.ct, "name": "我的", "items": del}),
        )
    });
    assert_eq!(r.unwrap(), json!({"ok": true, "count": 3}));
    assert_eq!(n, 1);
    assert_eq!(lib_rows(&bat, &bat.ct), lib_rows(&seq, &seq.ct));
    assert_eq!(lib_rows(&bat, &bat.ct)["total"], 2);
    let before = lib_rows(&bat, &bat.ct);
    let r = bat.rpc(
        "userDicts.removeEntry",
        json!({"schemaId": bat.ct, "name": "我的", "items": [{"code": "b", "text": "新"}, {"code": "d"}]}),
    );
    assert!(r.is_err(), "{r:?}");
    assert_eq!(lib_rows(&bat, &bat.ct), before);

    for m in ["userDicts.addEntry", "userDicts.removeEntry"] {
        let (r, n) =
            commits_during(|| bat.rpc(m, json!({"schemaId": bat.ct, "name": "我的", "items": []})));
        assert_eq!(r.unwrap(), json!({"ok": true, "count": 0}), "{m}");
        assert_eq!(n, 0, "{m}");
    }
    assert_eq!(lib_rows(&bat, &bat.ct), before);
}

/// 拼音分库：批量加词同样补音节边界、拆空格码（与单条 `normalize_add_code` 同一条路）。
#[test]
fn add_entry_batch_normalizes_pinyin_codes() {
    let f = fx("batch_py");
    let py = f.py.clone();
    f.rpc("userDicts.create", json!({"schemaId": py, "name": "我的"}))
        .unwrap();
    f.rpc(
        "userDicts.addEntry",
        json!({"schemaId": py, "name": "我的", "items": [
            {"code": "nihao", "text": "你好"},
            {"code": "nv hai", "text": "女孩"},
        ]}),
    )
    .unwrap();
    let rows = lib_rows(&f, &py);
    let codes: Vec<&str> = rows["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["ni hao", "nv hai"], "{rows}");
}

/// 删除方案的全部用户数据：用户词、分库、方案覆盖都没了、引擎随之重建；拼音族共用数据拒绝。
#[test]
fn purge_schema_removes_user_data_and_refuses_shared() {
    let f = fx("purge");
    let ct = f.ct.clone();
    let store = f.c.user_store().cloned().unwrap();
    f.rpc("userDicts.create", json!({"schemaId": ct, "name": "我的"}))
        .unwrap();
    f.rpc(
        "userDicts.addEntry",
        json!({"schemaId": ct, "name": "我的", "code": "a", "text": "戈", "weight": 100}),
    )
    .unwrap();
    store.add_user_word(&ct, "aa", "域", 999, 0).unwrap();
    let ov = f.dir.join("ov").join(format!("{ct}.toml"));
    std::fs::create_dir_all(ov.parent().unwrap()).unwrap();
    std::fs::write(&ov, "[engine.codetable]\nmax_code_length = 4\n").unwrap();
    assert_eq!(f.texts(&ct, "a")[0], "戈");
    let r = f
        .rpc("userdata.purgeSchema", json!({"schemaId": ct}))
        .unwrap();
    let removed: Vec<String> = serde_json::from_value(r["removed"].clone()).unwrap();
    assert!(
        removed.contains(&format!("{ct}/dicts/我的.dict.yaml")),
        "{removed:?}"
    );
    assert!(
        removed.contains(&format!("schema_overrides/{ct}.toml")),
        "{removed:?}"
    );
    assert_eq!(r["ok"], true);
    assert!(!f.root().join(&ct).exists());
    assert!(!ov.exists());
    assert_eq!(f.texts(&ct, "a"), ["工", "式"], "{:?}", f.texts(&ct, "a"));

    // 第二个拼音方案与 py 共用 `pinyin` 数据
    let py2 = format!("{}_2", f.py);
    std::fs::write(
        f.dir.join("schemas").join(format!("{py2}.schema.toml")),
        format!("[schema]\nid = \"{py2}\"\nname = \"拼二\"\n[engine]\ntype = \"pinyin\"\n"),
    )
    .unwrap();
    let e = f
        .rpc("userdata.purgeSchema", json!({"schemaId": f.py}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("共用数据，不能单独删除"), "{e}");
    if let Some(cache) = Config::cache_dir() {
        let _ = std::fs::remove_dir_all(cache.join(&py2));
    }
}

/// 混输把码表数据写进主方案的数据方案、拼音数据写进 `pinyin`：两头都不能单删；混输自身也不能删
/// （数据不在它名下）。`data.locate {schemaId}` 的 purgeable / reason 与 purge 同一判据。
#[test]
fn purge_schema_guards_mixed_both_ways() {
    let f = fx("purge_mx");
    let (ct, py) = (f.ct.clone(), f.py.clone());
    let loc = |id: &str| f.rpc("data.locate", json!({"schemaId": id})).unwrap();
    assert_eq!(loc(&ct)["purgeable"], true, "{}", loc(&ct));
    assert!(loc(&ct).get("reason").is_none());

    let mx = format!("zz_wdudl_mx_purge_{}", std::process::id());
    std::fs::write(
        f.dir.join("schemas").join(format!("{mx}.schema.toml")),
        format!(
            "[schema]\nid = \"{mx}\"\nname = \"混\"\n[engine]\ntype = \"mixed\"\n\
                 [engine.mixed]\nprimary_schema = \"{ct}\"\nsecondary_schema = \"{py}\"\n"
        ),
    )
    .unwrap();
    let purge = |id: &str| {
        f.rpc("userdata.purgeSchema", json!({"schemaId": id}))
            .unwrap_err()
            .to_string()
    };
    for id in [&ct, &py] {
        let e = purge(id);
        assert!(e.contains("与「混」共用数据，不能单独删除"), "{id}: {e}");
        let l = loc(id);
        assert_eq!(l["purgeable"], false, "{l}");
        assert_eq!(l["reason"], e, "locate 与 purge 同一句");
    }
    let e = purge(&mx);
    assert_eq!(e, "混输方案的数据在「测」和拼音里，请在那里删除");
    assert_eq!(loc(&mx)["purgeable"], false);
    assert_eq!(loc(&mx)["reason"], e);
    if let Some(cache) = Config::cache_dir() {
        let _ = std::fs::remove_dir_all(cache.join(&mx));
    }
}

/// 保留目录（统计 / 字符分类 / .backup）与未安装的 id 不能拿来删。
#[test]
fn purge_schema_rejects_reserved_and_unknown() {
    let f = fx("purge_rsv");
    let stats = f.root().join("stats");
    std::fs::create_dir_all(&stats).unwrap();
    std::fs::write(stats.join("2026-10.toml"), "a = 1\n").unwrap();
    for id in ["stats", "Stats", "charsets", "CHARSETS", ".backup", "nope"] {
        assert!(
            f.rpc("userdata.purgeSchema", json!({"schemaId": id}))
                .is_err(),
            "{id}"
        );
        assert_eq!(
            f.rpc("data.locate", json!({"schemaId": id}))
                .map(|v| v["purgeable"].clone())
                .unwrap_or(json!(false)),
            false,
            "{id}"
        );
    }
    assert!(stats.join("2026-10.toml").exists());
}

#[test]
fn create_rename_delete_and_bad_names() {
    let f = fx("names");
    let ct = f.ct.clone();
    for bad in ["", "a/b", ".x", "a:b", "x."] {
        assert!(
            f.rpc("userDicts.create", json!({"schemaId": ct, "name": bad}))
                .is_err(),
            "{bad:?}"
        );
    }
    f.rpc("userDicts.create", json!({"schemaId": ct, "name": "a"}))
        .unwrap();
    f.rpc("userDicts.create", json!({"schemaId": ct, "name": "b"}))
        .unwrap();
    assert!(
        f.rpc("userDicts.create", json!({"schemaId": ct, "name": "a"}))
            .is_err(),
        "重名"
    );
    f.rpc(
        "userDicts.setEnabled",
        json!({"schemaId": ct, "name": "a", "enabled": false}),
    )
    .unwrap();
    assert!(
        f.rpc(
            "userDicts.rename",
            json!({"schemaId": ct, "name": "a", "newName": "b"}),
        )
        .is_err(),
        "改成已有的名字"
    );
    f.rpc(
        "userDicts.rename",
        json!({"schemaId": ct, "name": "a", "newName": "甲 乙"}),
    )
    .unwrap();
    let l = f.rpc("userDicts.list", json!({"schemaId": ct})).unwrap();
    let names: Vec<(&str, &str, bool)> = l
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["name"].as_str().unwrap(),
                v["label"].as_str().unwrap(),
                v["enabled"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [("b", "b", true), ("甲 乙", "甲 乙", false)],
        "开关随改名"
    );
    f.rpc("userDicts.delete", json!({"schemaId": ct, "name": "甲 乙"}))
        .unwrap();
    assert!(
        f.rpc("userDicts.delete", json!({"schemaId": ct, "name": "甲 乙"}))
            .is_err()
    );
    assert_eq!(
        f.rpc("userDicts.list", json!({"schemaId": ct}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.rpc("userDicts.list", json!({"schemaId": "不存在的方案"}))
            .is_err()
    );
}

/// 拼音族折叠到 `pinyin`；开关按方案 id 记。
#[test]
fn pinyin_libs_fold_and_switch_per_schema() {
    let f = fx("py");
    let py = f.py.clone();
    let tsv = "hou ji bo fa\t厚积薄发\t5\n";
    let r = f
        .rpc(
            "userDicts.importAs",
            json!({"schemaId": py, "name": "成语", "content": tsv}),
        )
        .unwrap();
    assert_eq!(r["imported"], 1, "{r}");
    assert!(f.root().join("pinyin/dicts/成语.dict.yaml").is_file());
    let l = f.rpc("userDicts.list", json!({"schemaId": py})).unwrap();
    assert_eq!(l[0]["file"], "pinyin/dicts/成语.dict.yaml");
    assert_eq!(f.texts(&py, "houjibofa")[0], "厚积薄发");
    let e = f
        .rpc(
            "userDicts.entries",
            json!({"schemaId": py, "name": "成语", "offset": 0, "limit": 10}),
        )
        .unwrap();
    assert_eq!(e["items"][0]["code"], "hou ji bo fa");
    f.rpc(
        "userDicts.setEnabled",
        json!({"schemaId": py, "name": "成语", "enabled": false}),
    )
    .unwrap();
    assert!(!f.texts(&py, "houjibofa").contains(&"厚积薄发".to_string()));
    let meta = std::fs::read_to_string(f.root().join("pinyin/dicts.toml")).unwrap();
    assert!(meta.contains(&format!("[schemas.{py}]")), "{meta}");
}

/// 删分库时文件已删、后面改开关文件失败：照样失效，引擎不能拿着已删文件的旧数据。
#[test]
fn delete_invalidates_even_when_switch_file_fails() {
    let f = fx("del_fail");
    let ct = f.ct.clone();
    f.rpc(
        "userDicts.importAs",
        json!({"schemaId": ct, "name": "删", "content": "zz\t自造\t1\n"}),
    )
    .unwrap();
    assert_eq!(f.texts(&ct, "zz"), ["自造"]);
    // 开关文件读不了（是个目录）
    std::fs::create_dir_all(f.root().join(&ct).join("dicts.toml")).unwrap();
    assert!(
        f.rpc("userDicts.delete", json!({"schemaId": ct, "name": "删"}))
            .is_err()
    );
    assert!(!f.root().join(&ct).join("dicts/删.dict.yaml").exists());
    assert!(f.texts(&ct, "zz").is_empty(), "{:?}", f.texts(&ct, "zz"));
}

/// 备份还原把分库写回后，用到它的方案失效重建（还原出的词立即可查）。
#[test]
fn restore_brings_libs_back_and_rebuilds() {
    let f = fx("restore");
    let ct = f.ct.clone();
    f.rpc(
        "userDicts.importAs",
        json!({"schemaId": ct, "name": "备", "content": "zz\t自造\t1\n"}),
    )
    .unwrap();
    let pkg = f.dir.join("b.zip");
    f.rpc("backup.create", json!({"path": pkg.to_string_lossy()}))
        .unwrap();
    f.rpc("userDicts.delete", json!({"schemaId": ct, "name": "备"}))
        .unwrap();
    assert!(f.texts(&ct, "zz").is_empty());
    f.rpc(
        "backup.restore",
        json!({"path": pkg.to_string_lossy(), "sections": ["dict"], "strategy": "merge"}),
    )
    .unwrap();
    assert_eq!(f.texts(&ct, "zz"), ["自造"]);
}
