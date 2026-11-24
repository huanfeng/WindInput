use super::*;
use wind_userdata::runtime::{
    USER_CODE_MAX_CHARS as USER_WORD_CODE_MAX_CHARS,
    USER_TEXT_MAX_CHARS as USER_WORD_TEXT_MAX_CHARS,
};

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// `purge_word`：所有编码下的用户词 / 临时词、词频、联想（上文或选中词）都清掉，别的词不动。
#[test]
fn purge_word_removes_every_code() {
    let path = tmp("wind_uw_purge.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("py", "shijian", "时间", 0, 0).unwrap();
    s.add_user_word("py", "sj", "时间", 0, 0).unwrap();
    s.add_user_word("py", "shijian", "实践", 0, 0).unwrap();
    s.learn_temp_word("py", "shijan", "时间", 1, 0).unwrap();
    s.record_freq("py", "shijian", "时间").unwrap();
    s.record_freq("py", "shijian", "实践").unwrap();
    s.record_assoc_pick("py", "时间", "到了").unwrap();
    s.record_assoc_pick("py", "没有", "时间").unwrap();
    s.record_assoc_pick("py", "没有", "办法").unwrap();
    assert_eq!(s.purge_word("py", "时间").unwrap(), 3);
    assert!(s.get_user_words("py", "sj").unwrap().is_empty());
    assert_eq!(
        s.get_user_words("py", "shijian").unwrap()[0].text,
        "实践",
        "同码别的词不动"
    );
    assert!(s.get_temp_words("py", "shijan").unwrap().is_empty());
    assert!(s.get_freq("py", "shijian", "时间").unwrap().is_none());
    assert!(s.get_freq("py", "shijian", "实践").unwrap().is_some());
    assert!(s.assoc_history("py", "时间", 0).unwrap().is_empty());
    let rest = s.assoc_history("py", "没有", 0).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].0, "办法");
    let _ = std::fs::remove_file(&path);
}

fn row(code: &str, text: &str) -> wdict::WordIo {
    wdict::WordIo {
        code: code.into(),
        text: text.into(),
        weight: 0,
        count: 0,
        boundary: None,
    }
}

fn io(code: &str, text: &str, weight: i32) -> wdict::WordIo {
    wdict::WordIo {
        weight,
        ..row(code, text)
    }
}

/// 用户词落进文本：门面写 → 折回 → 文件里有；重开后从文本读回。
#[test]
fn user_words_live_in_text() {
    let p = tmp("wind_uw_in_text.redb");
    {
        let s = Store::open(&p).unwrap();
        s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
            .unwrap();
        s.runtime()
            .data()
            .fold(wind_userdata::record::Target::User, "pinyin")
            .unwrap();
        let f = s.user_text_path("pinyin");
        assert!(
            std::fs::read_to_string(&f)
                .unwrap()
                .contains("ni hao\t你好\t9")
        );
    }
    let s = Store::open(&p).unwrap();
    let got = s.get_user_words("pinyin", "nihao").unwrap();
    assert_eq!(
        (got[0].text.as_str(), got[0].weight, got[0].boundary),
        ("你好", 9, 0b101)
    );
}

/// 未折回的新词列得出、数得对、导出排最后。
#[test]
fn unfolded_words_are_listed_counted_and_exported_last() {
    let s = Store::open(tmp("wind_uw_unfolded.redb")).unwrap();
    s.add_user_word("wb", "aa", "式", 5, 0).unwrap();
    s.runtime()
        .data()
        .fold(wind_userdata::record::Target::User, "wb")
        .unwrap();
    s.add_user_word("wb", "aa", "工", 5, 0).unwrap();
    let mut n = 0;
    s.for_each_user_word("wb", "", &mut |_| {
        n += 1;
        true
    })
    .unwrap();
    assert_eq!(n, 2);
    assert_eq!(s.count_user_words("wb").unwrap(), 2);
    let rows = s.collect_user_word_rows("wb").unwrap();
    assert_eq!(rows.last().unwrap().text, "工", "未折回的新词导出在末尾");
}

/// 预览分类与实际导入一致（含同批重复键、补边界）。
#[test]
fn preview_matches_import_counts() {
    let s = Store::open(tmp("wind_uw_preview2.redb")).unwrap();
    s.add_user_word("pinyin", "nihao", "你好", 100, 0).unwrap();
    let rows = vec![
        io("ni hao", "你好", 1),
        io("ta", "他", 3),
        io("ta", "他", 9),
    ];
    let (pc, _) = s.preview_import_user_words("pinyin", &rows).unwrap();
    let ic = s.import_user_words("pinyin", &rows).unwrap();
    assert_eq!(
        (pc.added, pc.updated, pc.unchanged),
        (ic.added, ic.updated, ic.unchanged)
    );
    assert_eq!((ic.added, ic.updated, ic.unchanged), (1, 2, 0));
}

/// 超长文本（> `USER_WORD_TEXT_MAX_CHARS`）一律拒绝，覆盖显式给码/设置页手动加词这两条
/// 不经过 `check_derivable_word`（wind-coordinator）的路径。恰好等于上限的文本应正常写入；
/// 校验须在 `enc_key`/写事务之前，被拒绝的词不留部分写入痕迹。
#[test]
fn add_user_word_rejects_text_over_max_chars() {
    let p = tmp("wind_uw_text_max_len.redb");
    let s = Store::open(&p).unwrap();

    let ok_text: String = "汉".repeat(USER_WORD_TEXT_MAX_CHARS);
    s.add_user_word("pinyin", "abc", &ok_text, 100, 0)
        .expect("恰好等于上限应写入成功");

    let too_long: String = "汉".repeat(USER_WORD_TEXT_MAX_CHARS + 1);
    let err = s
        .add_user_word("pinyin", "abd", &too_long, 100, 0)
        .expect_err("超过上限应被拒绝");
    assert!(
        err.to_string().contains("过长"),
        "错误信息应提示过长: {err}"
    );
    assert!(
        s.get_user_words("pinyin", "abd").unwrap().is_empty(),
        "拒绝的词不得落库"
    );
}

/// 超长 code（> `USER_WORD_CODE_MAX_CHARS`）一律拒绝，语义与文本长度校验对称。
#[test]
fn add_user_word_rejects_code_over_max_chars() {
    let p = tmp("wind_uw_code_max_len.redb");
    let s = Store::open(&p).unwrap();

    let ok_code: String = "a".repeat(USER_WORD_CODE_MAX_CHARS);
    s.add_user_word("pinyin", &ok_code, "你好", 100, 0)
        .expect("恰好等于上限应写入成功");

    let too_long_code: String = "a".repeat(USER_WORD_CODE_MAX_CHARS + 1);
    let err = s
        .add_user_word("pinyin", &too_long_code, "你好", 100, 0)
        .expect_err("超过上限应被拒绝");
    assert!(
        err.to_string().contains("过长"),
        "错误信息应提示过长: {err}"
    );
    assert!(
        s.get_user_words("pinyin", &too_long_code)
            .unwrap()
            .is_empty(),
        "拒绝的词不得落库"
    );
}

/// 同码词条按导入文件的先后出。
///
/// 三条词按「甲 乙 丙」导入，text 字典序恰好相反（丙 < 乙 < 甲），「按 order」与「按 text」
/// 两种结果可区分。光断言顺序不够：order 全为 0 时稳定排序会原样吐回 search 的次序，
/// 故须同时断言 order 严格递增且 ≥ 1。删掉 `import_user_words` 里 `next_order` 的自增即红；
/// 只把它的初值改成 0 是无效变异（仍递增成 0,1,2，只红「≥ 1」那句）。
#[test]
fn import_assigns_order_following_row_sequence() {
    let p = tmp("wind_uw_order_import.redb");
    let s = Store::open(&p).unwrap();
    s.import_user_words(
        "pinyin",
        &[row("abc", "甲"), row("abc", "乙"), row("abc", "丙")],
    )
    .unwrap();

    let mut recs = s.get_user_words("pinyin", "abc").unwrap();
    assert_eq!(recs.len(), 3);
    recs.sort_by_key(|r| r.order);
    let texts: Vec<&str> = recs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["甲", "乙", "丙"], "同码词条应按导入行序排");

    let orders: Vec<u32> = recs.iter().map(|r| r.order).collect();
    assert!(
        orders[0] >= 1 && orders[0] < orders[1] && orders[1] < orders[2],
        "order 应自 1 起严格递增（0 会被稳定排序掩盖成假通过），实际: {orders:?}"
    );
}

/// 「导出 → 清空 → 再导入」一圈之后词序不变。
///
/// 迁词库 / 备份还原的真实用法。导出若按 key 字典序吐出，一次往返就把词序洗成字典序，
/// 且库里明明存着正确的 order，更隐蔽。守 `collect_user_word_rows` 里按 order 的排序。
#[test]
fn word_order_survives_export_clear_import() {
    let p = tmp("wind_uw_order_roundtrip.redb");
    let s = Store::open(&p).unwrap();
    s.import_user_words(
        "pinyin",
        &[row("abc", "甲"), row("abc", "乙"), row("abc", "丙")],
    )
    .unwrap();

    let text = s.export_user_words_wdict("pinyin", "2026-09-14").unwrap();
    s.clear_user_words("pinyin").unwrap();
    s.import_user_words_wdict("pinyin", &text).unwrap();

    let mut recs = s.get_user_words("pinyin", "abc").unwrap();
    recs.sort_by_key(|r| r.order);
    let texts: Vec<&str> = recs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["甲", "乙", "丙"], "一轮备份还原后词序应原样保留");
}

/// 再次导入同一批词**不得**把它们挪到队尾。
///
/// 重复导入是常态；若更新路径也发新号，每导一次全库词序就被翻搅一次。
/// 只有本测试覆盖这个维度（更新分支 `enc_val_ordered` 的 `o` 换成 `next_order` 即红）。
#[test]
fn reimport_keeps_existing_order() {
    let p = tmp("wind_uw_order_reimport.redb");
    let s = Store::open(&p).unwrap();
    let rows = [row("abc", "甲"), row("abc", "乙")];
    s.import_user_words("pinyin", &rows).unwrap();
    let before: Vec<u32> = {
        let mut r = s.get_user_words("pinyin", "abc").unwrap();
        r.sort_by_key(|x| x.text.clone());
        r.iter().map(|x| x.order).collect()
    };

    // 第二次导入：权重抬高以确保真的走了写盘分支（否则 unchanged 不写，测不到东西）。
    let bumped: Vec<wdict::WordIo> = rows
        .iter()
        .map(|r| wdict::WordIo {
            weight: 900,
            ..r.clone()
        })
        .collect();
    s.import_user_words("pinyin", &bumped).unwrap();

    let after: Vec<u32> = {
        let mut r = s.get_user_words("pinyin", "abc").unwrap();
        r.sort_by_key(|x| x.text.clone());
        assert!(
            r.iter().all(|x| x.weight == 900),
            "前提：第二次导入应已写盘"
        );
        r.iter().map(|x| x.order).collect()
    };
    assert_eq!(before, after, "重复导入不得改变既有词条的入库序号");
}

/// 晋升临时词**不得**抹掉该词条在用户词表里的入库序号。
///
/// `promote_temp_word` 住在 `temp_words.rs` 里，但写的是 `USER_WORDS`，按文件名去数用户词
/// 写入点必漏这一处。若用 24B 的 `enc_val` 写回，order=10 的导入词晋升后变 0，升序下跳到
/// 该 code 所有词之前，且要用过一阵子才显形。
#[test]
fn promote_temp_word_keeps_user_word_order() {
    let p = tmp("wind_uw_order_promote.redb");
    let s = Store::open(&p).unwrap();
    s.import_user_words("pinyin", &[row("abc", "甲"), row("abc", "乙")])
        .unwrap();
    let pick = |st: &Store| -> UserWordRecord {
        st.get_user_words("pinyin", "abc")
            .unwrap()
            .into_iter()
            .find(|r| r.text == "乙")
            .expect("「乙」应在用户词表里")
    };
    let before = pick(&s).order;
    assert!(before >= 1, "前提：导入应已发号，实际 {before}");

    // 用户词表里已有「乙」→ 晋升走 existing 分支（真实场景：导入的词后来被用熟）。
    s.learn_temp_word("pinyin", "abc", "乙", 800, 0).unwrap();
    assert!(
        s.promote_temp_word("pinyin", "abc", "乙").unwrap(),
        "前提：应晋升成功，否则测的是空路径"
    );

    assert_eq!(
        pick(&s).order,
        before,
        "晋升不得改动既有入库序号（抹成 0 会把该词顶到所有导入词之前）"
    );
}

/// 简拼召回的记录必须带**真实** order，否则表现为「全码对、简拼不对」。
///
/// 缩写索引自己不定序，但它交出的记录经 `StoreUserLayer::search_abbrev` → `record_to_candidate`
/// → `sort_trunc`（用 `better()`，含 `natural_order`）排序。`abbrev_index::search` 若硬编码
/// `order: 0`，同一批词走全码顺序正确、走简拼退回 text 字典序。
#[test]
fn abbrev_search_carries_real_order() {
    let p = tmp("wind_uw_order_abbrev.redb");
    let s = Store::open(&p).unwrap();
    // 按「拟好 你好」导入；text 字典序恰好相反（你 U+4F60 < 拟 U+62DF），两者可区分。
    s.import_user_words("pinyin", &[row("ni hao", "拟好"), row("ni hao", "你好")])
        .unwrap();

    let mut recs = s.search_user_words_by_abbrev("pinyin", "nh", 0).unwrap();
    assert_eq!(recs.len(), 2, "简拼 nh 应召回两条，实际 {recs:?}");
    recs.sort_by_key(|r| r.order);
    let texts: Vec<&str> = recs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["拟好", "你好"], "简拼召回应保住导入词序");

    let orders: Vec<u32> = recs.iter().map(|r| r.order).collect();
    assert!(
        orders[0] >= 1 && orders[0] < orders[1],
        "order 须为真值而非硬编码 0（全 0 时稳定排序会掩盖成假通过），实际 {orders:?}"
    );
}

/// 向后兼容契约：v2 的 24B 记录（无 order）读出 order=0，且不 panic。
///
/// 与 `dec_val_reads_v1_16byte_records` 同理，长度守卫必须分三档，直接切 `b[24..28]` 会在
/// v1/v2 记录上越界。`dec_val` 是 `dec_val_ordered` 的薄壳，两条用例共用同一条解码路径。
#[test]
fn dec_val_ordered_reads_v1_and_v2_records() {
    let mut v1 = [0u8; 16];
    v1[0..4].copy_from_slice(&123i32.to_le_bytes());
    assert_eq!(
        dec_val_ordered(&v1).map(|t| (t.3, t.4)),
        Some((0, 0)),
        "v1（16B）：boundary 与 order 均取 0"
    );

    let v2 = enc_val(123, 7, 1_700_000_000, 0b101);
    assert_eq!(v2.len(), 24);
    assert_eq!(
        dec_val_ordered(&v2),
        Some((123, 7, 1_700_000_000, 0b101, 0)),
        "v2（24B）：boundary 读回、order 取 0"
    );

    let v3 = enc_val_ordered(123, 7, 1_700_000_000, 0b101, 42);
    assert_eq!(v3.len(), 28);
    assert_eq!(
        dec_val_ordered(&v3),
        Some((123, 7, 1_700_000_000, 0b101, 42))
    );

    // 薄壳与全量版对同一条记录必须给出一致的前四项。
    assert_eq!(dec_val(&v3), Some((123, 7, 1_700_000_000, 0b101)));
    assert_eq!(dec_val_ordered(&[0u8; 15]), None);
}

/// 备份还原不得丢音节边界。
///
/// 还原路径是 `clear_user_words` + `import_user_words_wdict`，清空后全是新键，边界只能靠 wdict
/// 的 code 列（带空格的音节码）携带。`collect_user_word_rows` 的 `join_code_by_boundary`
/// 若换回 `r.code`，一轮备份还原会把 boundary 全部清零。
#[test]
fn boundary_survives_export_clear_import() {
    let p = tmp("wind_uw_boundary_backup.redb");
    let s = Store::open(&p).unwrap();
    s.add_user_word("pinyin", "nihao", "你好", 500, 0b101)
        .unwrap();
    s.add_user_word("pinyin", "xianning", "西安宁", 800, 0b10101)
        .unwrap();

    let text = s.export_user_words_wdict("pinyin", "2026-07-29").unwrap();
    assert!(
        text.contains("ni hao") && text.contains("xi an ning"),
        "导出文本的 code 列须为带空格的音节码，实际:\n{text}"
    );

    // ── 模拟备份还原 ──
    s.clear_user_words("pinyin").unwrap();
    s.import_user_words_wdict("pinyin", &text).unwrap();

    let recs = s.search_user_words_prefix("pinyin", "", 0).unwrap();
    let mut got: Vec<(String, u64)> = recs.iter().map(|r| (r.code.clone(), r.boundary)).collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("nihao".to_string(), 0b101),
            ("xianning".to_string(), 0b10101)
        ],
        "还原后 key 须仍是扁平码、且边界原样存活"
    );
    let _ = std::fs::remove_file(&p);
}

/// 旧版 wdict（code 列为扁平码，无空格）导入后 boundary=0，不报错、不误判为单音节。
/// 用户手上已有的备份文件都是这个形态，必须平滑降级。
#[test]
fn legacy_flat_wdict_imports_as_unknown_boundary() {
    let p = tmp("wind_uw_legacy_wdict.redb");
    let s = Store::open(&p).unwrap();
    let legacy = "# WindInput 用户数据文件\nwind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight, count]\n\n--- !words\nnihao\t你好\t500\t0\nabcd\t工作\t100\t0\n";
    let (imported, skipped) = s.import_user_words_wdict("pinyin", legacy).unwrap();
    assert_eq!((imported, skipped), (2, 0));
    let recs = s.search_user_words_prefix("pinyin", "", 0).unwrap();
    assert!(
        recs.iter().all(|r| r.boundary == 0),
        "无空格的码一律按「无边界信息」处理"
    );
    assert!(recs.iter().any(|r| r.code == "nihao"));
    let _ = std::fs::remove_file(&p);
}

/// 导入行带边界、库中旧记录 boundary=0（v1 遗留或历史扁平导入）→ 补齐而非忽略，
/// 且该行计入 updated；`preview_import_user_words` 的分类必须给出同样答案。
#[test]
fn import_fills_missing_boundary_and_preview_agrees() {
    let p = tmp("wind_uw_fill_boundary.redb");
    let s = Store::open(&p).unwrap();
    s.add_user_word("pinyin", "nihao", "你好", 500, 0).unwrap(); // 旧记录无边界

    let rows = vec![wdict::WordIo {
        code: "ni hao".into(),
        text: "你好".into(),
        weight: 500, // weight/count 均不变，只有 boundary 可补
        count: 0,
        boundary: None,
    }];
    let (pc, _) = s.preview_import_user_words("pinyin", &rows).unwrap();
    let ic = s.import_user_words("pinyin", &rows).unwrap();
    assert_eq!(
        (pc.added, pc.updated, pc.unchanged),
        (0, 1, 0),
        "dry-run 须把「仅补边界」也算作会落盘的 updated"
    );
    assert_eq!(
        (ic.added, ic.updated, ic.unchanged),
        (0, 1, 0),
        "实际导入的分类须与 dry-run 完全一致"
    );
    assert_eq!(
        s.get_user_words("pinyin", "nihao").unwrap()[0].boundary,
        0b101
    );
    let _ = std::fs::remove_file(&p);
}

/// 向后兼容契约（数据安全）：value 从 v1 的 16B 扩到 24B（追加 boundary u64）。
/// 旧库里全是 16B 记录，不可直接切 `b[16..24]`，必须按长度分支，这是惰性升级免 migration 的前提。
#[test]
fn dec_val_reads_v1_16byte_records() {
    // 手工拼一条 v1（16B）记录，模拟旧库数据。
    let mut v1 = [0u8; 16];
    v1[0..4].copy_from_slice(&123i32.to_le_bytes());
    v1[4..8].copy_from_slice(&7u32.to_le_bytes());
    v1[8..16].copy_from_slice(&1_700_000_000i64.to_le_bytes());
    assert_eq!(
        dec_val(&v1),
        Some((123, 7, 1_700_000_000, 0)),
        "v1 记录须能读出，boundary 取 0（无信息 → 消费方降级回 DAG）"
    );

    // v2（24B）：boundary 原样读回。
    let v2 = enc_val(123, 7, 1_700_000_000, 0b101);
    assert_eq!(v2.len(), 24);
    assert_eq!(dec_val(&v2), Some((123, 7, 1_700_000_000, 0b101)));

    // 短于 16B 视为损坏 → None（而非 panic）。
    assert_eq!(dec_val(&[0u8; 15]), None);
    assert_eq!(dec_val(&[]), None);
}

/// 无边界词（手输码/旧版扁平导入）仍须被简拼查询捞出来交给引擎现判，
/// 且**只在首字符对得上时**才捞——这正是分组的意义。
#[test]
fn no_boundary_words_are_recalled_by_first_char_only() {
    let p = tmp("wind_abbrev_noboundary.redb");
    let s = Store::open(&p).unwrap();
    s.add_user_word("py", "xianning", "西安宁", 100, 0).unwrap();
    s.add_user_word("py", "nihao", "你好", 100, 0).unwrap();

    let texts = |ab: &str| -> Vec<String> {
        s.search_user_words_by_abbrev("py", ab, 0)
            .unwrap()
            .into_iter()
            .map(|r| r.text)
            .collect()
    };
    // 首字符 x → 只捞出 x 开头的那条，n 开头的那条被分组挡掉
    assert_eq!(texts("xan"), vec!["西安宁"]);
    assert_eq!(texts("nh"), vec!["你好"]);
    // 首字符对不上 → 一条不返回（改动前是整库返回）
    assert!(texts("zg").is_empty());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn test_add_get_user_word() {
    let path = tmp("wind_uw_addget.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    s.add_user_word("wb", "a", "戈", 50, 0).unwrap();
    let mut got = s.get_user_words("wb", "a").unwrap();
    got.sort_by_key(|r| r.text.clone());
    assert_eq!(got.len(), 2);
    assert!(got.iter().any(|r| r.text == "工" && r.weight == 100));
    // add 同词更高权重 → 取 max
    s.add_user_word("wb", "a", "工", 200, 0).unwrap();
    let g = s.get_user_words("wb", "a").unwrap();
    assert_eq!(g.iter().find(|r| r.text == "工").unwrap().weight, 200);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_prefix_remove_update() {
    let path = tmp("wind_uw_prefix.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "ab", "阿", 10, 0).unwrap();
    s.add_user_word("wb", "abc", "啊", 20, 0).unwrap();
    s.add_user_word("wb", "x", "西", 30, 0).unwrap();
    // 前缀 "ab" 命中 ab/abc，不含 x
    let pre = s.search_user_words_prefix("wb", "ab", 0).unwrap();
    assert_eq!(pre.len(), 2);
    assert!(pre.iter().all(|r| r.code.starts_with("ab")));
    // 更新权重
    assert!(s.update_user_word_weight("wb", "ab", "阿", 99).unwrap());
    assert!(!s.update_user_word_weight("wb", "ab", "缺", 1).unwrap());
    assert_eq!(s.get_user_words("wb", "ab").unwrap()[0].weight, 99);
    // 删除
    s.remove_user_word("wb", "ab", "阿").unwrap();
    assert!(s.get_user_words("wb", "ab").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_import_user_words_roundtrip() {
    let path = tmp("wind_uw_io.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    s.add_user_word("wb", "ml", "多行\n带\t制表", 5, 0).unwrap();
    let text = s
        .export_user_words_wdict("wb", "2026-07-11T00:00:00+08:00")
        .unwrap();
    assert!(text.contains("--- !words"));

    // 导入到新库应还原
    let path2 = tmp("wind_uw_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    let (imported, skipped) = s2.import_user_words_wdict("wb", &text).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(imported, 2);
    let got = s2.get_user_words("wb", "a").unwrap();
    assert_eq!(got[0].text, "工");
    assert_eq!(got[0].weight, 100);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

/// 显式 `WordIo::boundary` 优先于 code 里的空格，且单音节边界必须能存活。
///
/// 空格载体表达不了单音节（`join_code_by_boundary("xian", 0b1)` 读回来是 0），导入闸口
/// 求解出的单字词边界若走 code 传递会静默退化为 0。用户词落文本后，单个汉字的单音节由
/// 文本推断，不给显式边界也是 0b1；非汉字单字符（emoji）退化为 0，是对照组。
#[test]
fn explicit_boundary_survives_where_spaces_cannot() {
    let path = tmp("wind_uw_explicit_b.redb");
    let s = Store::open(&path).unwrap();
    let row = |code: &str, text: &str, b: Option<u64>| crate::wdict::WordIo {
        code: code.into(),
        text: text.into(),
        weight: 100,
        count: 0,
        boundary: b,
    };
    s.import_user_words(
        "pinyin",
        &[
            // 单音节：显式 0b1
            row("xian", "先", Some(0b1)),
            // 不给显式边界的单个汉字：由文本推断
            row("gong", "工", None),
            // 对照：非汉字单字符，显式单音节在文本里表达不了
            row("xiao", "😊", Some(0b1)),
            // 多音节：显式边界与 code 空格都能表达，显式优先
            row("nihao", "你好", Some(0b101)),
        ],
    )
    .unwrap();
    let b = |code: &str, text: &str| {
        s.get_user_words("pinyin", code)
            .unwrap()
            .into_iter()
            .find(|r| r.text == text)
            .unwrap()
            .boundary
    };
    assert_eq!(b("xian", "先"), 0b1, "单音节边界必须存活");
    assert_eq!(b("gong", "工"), 0b1, "单个汉字由文本推断为单音节");
    assert_eq!(b("xiao", "😊"), 0, "非汉字单字符的单音节退化为 0");
    assert_eq!(b("nihao", "你好"), 0b101);
    let _ = std::fs::remove_file(&path);
}

/// 显式边界缺省时仍按 code 里的空格推断——旧路径逐字节不变。
#[test]
fn spaced_code_still_carries_boundary_when_no_explicit() {
    let path = tmp("wind_uw_spaced_b.redb");
    let s = Store::open(&path).unwrap();
    s.import_user_words(
        "pinyin",
        &[crate::wdict::WordIo {
            code: "ni hao".into(),
            text: "你好".into(),
            weight: 100,
            count: 0,
            boundary: None,
        }],
    )
    .unwrap();
    let got = s.get_user_words("pinyin", "nihao").unwrap();
    assert_eq!(got[0].boundary, 0b101, "key 拆成扁平、边界从空格来");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn import_user_words_merges_max_weight() {
    let path = tmp("wind_uw_merge.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    // 导入同词更低权重 → 保持 max(100)
    let text = crate::wdict::export_words_wdict(
        &[crate::wdict::WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 30,
            count: 0,
            boundary: None,
        }],
        "2026-07-11T00:00:00+08:00",
    );
    let (imported, _) = s.import_user_words_wdict("wb", &text).unwrap();
    assert_eq!(imported, 1);
    assert_eq!(
        s.get_user_words("wb", "a").unwrap()[0].weight,
        100,
        "Merge 取 max"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn import_user_words_classifies_added_updated_unchanged() {
    let path = tmp("wind_uw_batch.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    let rows = vec![
        // 已有且权重更低 → unchanged(P2 约束 1:不落盘)
        crate::wdict::WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 30,
            count: 0,
            boundary: None,
        },
        // 新键 → added
        crate::wdict::WordIo {
            code: "b".into(),
            text: "了".into(),
            weight: 5,
            count: 0,
            boundary: None,
        },
    ];
    let c = s.import_user_words("wb", &rows).unwrap();
    assert_eq!((c.added, c.updated, c.unchanged), (1, 0, 1));
    assert_eq!(
        s.get_user_words("wb", "a").unwrap()[0].weight,
        100,
        "unchanged 不改权重"
    );

    // 权重严格更大 → updated,取导入值
    let rows2 = vec![crate::wdict::WordIo {
        code: "a".into(),
        text: "工".into(),
        weight: 200,
        count: 0,
        boundary: None,
    }];
    let c2 = s.import_user_words("wb", &rows2).unwrap();
    assert_eq!((c2.added, c2.updated, c2.unchanged), (0, 1, 0));
    assert_eq!(s.get_user_words("wb", "a").unwrap()[0].weight, 200);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn preview_import_is_readonly_and_matches_import() {
    let path = tmp("wind_uw_preview.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    let rows = vec![
        crate::wdict::WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 30,
            count: 0,
            boundary: None,
        },
        crate::wdict::WordIo {
            code: "b".into(),
            text: "了".into(),
            weight: 5,
            count: 0,
            boundary: None,
        },
        crate::wdict::WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 300,
            count: 0,
            boundary: None,
        },
    ];
    let (c, samples) = s.preview_import_user_words("wb", &rows).unwrap();
    assert_eq!((c.added, c.updated, c.unchanged), (1, 1, 1));
    assert_eq!(samples.len(), 2, "samples 只含会落盘的行(added+updated)");
    assert!(samples.iter().any(|x| x.contains("了")));
    // 只读:预览后库里仍只有原 1 条、权重未动
    assert_eq!(s.get_user_words("wb", "a").unwrap()[0].weight, 100);
    assert!(s.get_user_words("wb", "b").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn clear_user_words_only_target_schema() {
    let path = tmp("wind_uw_clear.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 1, 0).unwrap();
    s.add_user_word("wb", "b", "了", 1, 0).unwrap();
    s.add_user_word("py", "ni", "你", 1, 0).unwrap();
    let n = s.clear_user_words("wb").unwrap();
    assert_eq!(n, 2);
    assert!(s.search_user_words_prefix("wb", "", 0).unwrap().is_empty());
    assert_eq!(
        s.search_user_words_prefix("py", "", 0).unwrap().len(),
        1,
        "其它 schema 不受影响"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_dict_wdict_roundtrips_words_and_shadow() {
    let path = tmp("wind_uw_dict_io.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    // shadow：pin + del
    s.pin_shadow("wb", "aaaa", "恭", None, 0).unwrap();
    s.delete_shadow("wb", "bbbb", "见").unwrap();

    let text = s
        .export_dict_wdict("wb", "2026-07-14T00:00:00+08:00")
        .unwrap();
    assert!(text.contains("--- !words"), "含 words 段");
    assert!(text.contains("--- !shadow"), "含 shadow 段");

    // 导入到新库：words + shadow 均还原
    let path2 = tmp("wind_uw_dict_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    let (imported, skipped) = s2.import_user_words_wdict("wb", &text).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(imported, 1);
    let got = s2.get_user_words("wb", "a").unwrap();
    assert_eq!(got[0].weight, 100);

    let (actions, sk) = crate::wdict::parse_shadow_wdict(&text).unwrap();
    assert_eq!(sk, 0);
    let n = s2.import_shadow_actions("wb", &actions).unwrap();
    assert!(n >= 2, "至少重放 pin + del 两条");
    assert!(
        s2.get_shadow_rules("wb", "aaaa").unwrap().is_some(),
        "pin 规则还原"
    );
    assert_eq!(
        s2.get_shadow_rules("wb", "bbbb").unwrap().unwrap().deleted,
        vec!["见".to_string()],
        "del 规则还原"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

/// 手改文本 → sync → 新词可见、删掉的行消失、代次推进；未被手改的未折回操作仍在。
#[test]
fn reload_picks_up_hand_edit_and_bumps_generation() {
    let s = Store::open(tmp("wind_uw_handedit.redb")).unwrap();
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    s.add_user_word("pinyin", "ta", "他", 1, 0b1).unwrap();
    s.runtime()
        .data()
        .fold(wind_userdata::record::Target::User, "pinyin")
        .unwrap();
    s.add_user_word("pinyin", "wo", "我", 2, 0b1).unwrap(); // 未折回
    let f = s.user_text_path("pinyin");
    let edited = std::fs::read_to_string(&f)
        .unwrap()
        .replace("ta\t他\t1\n", "")
        + "shou gai\t手改\t3\n";
    std::fs::write(&f, edited).unwrap();
    let g0 = s.words_generation_of("pinyin");
    assert_eq!(s.sync_user_texts(), vec!["pinyin".to_string()]);
    assert_ne!(s.words_generation_of("pinyin"), g0);
    assert_eq!(
        s.get_user_words("pinyin", "shougai").unwrap()[0].text,
        "手改"
    );
    assert!(s.get_user_words("pinyin", "ta").unwrap().is_empty());
    assert_eq!(s.get_user_words("pinyin", "wo").unwrap()[0].text, "我");
    assert!(s.sync_user_texts().is_empty(), "没变就不重建");
}

/// dict.reload 的路径只认 `<text_dir>/<schema>/user.dict.yaml`。
#[test]
fn reload_rejects_paths_outside_user_dir() {
    let s = Store::open(tmp("wind_uw_reloadpath.redb")).unwrap();
    s.add_user_word("wb", "a", "工", 1, 0).unwrap();
    s.runtime()
        .data()
        .fold(wind_userdata::record::Target::User, "wb")
        .unwrap();
    let ok = s.user_text_path("wb").to_string_lossy().into_owned();
    assert!(s.reload_user_text_paths(&[ok]).is_ok());
    let base = s.user_text_dir().to_string_lossy().into_owned();
    for bad in [
        format!("{base}/../x/user.dict.yaml"),
        format!("{base}/wb/other.dict.yaml"),
        "/etc/passwd".to_string(),
        format!("{base}/wb/../../user.dict.yaml"),
        format!("{base}/user.dict.yaml"),
        "user.dict.yaml".to_string(),
    ] {
        assert!(
            s.reload_user_text_paths(std::slice::from_ref(&bad))
                .is_err(),
            "{bad}"
        );
    }
}

/// Windows 路径不分大小写：盘符、目录、文件名换了大小写 / 斜杠方向，仍认得是哪个文件；
/// 方案名取磁盘上的实际目录名。区分大小写的平台上照旧拒绝。
#[test]
fn text_path_matching_folds_case_when_asked() {
    let d = std::env::temp_dir().join(format!("wind_uw_ci-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let base = d.join("User");
    std::fs::create_dir_all(base.join("WubiEx")).unwrap();
    let upper = base.to_string_lossy().to_uppercase();
    let p = format!("{upper}/WUBIEX/User.Dict.YAML");
    match classify_text_path(&base, &p, true).unwrap() {
        TextPath::Words(s) => assert_eq!(s, "WubiEx"),
        _ => panic!("应认作用户词文件"),
    }
    assert!(matches!(
        classify_text_path(&base, &format!("{upper}/Phrases.Dict.Yaml"), true).unwrap(),
        TextPath::Phrases
    ));
    assert!(classify_text_path(&base, &p, false).is_err());
    assert!(classify_text_path(&base, &format!("{upper}/../X/user.dict.yaml"), true).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// P5：学到的数据与补全历史也能经 dict.reload 指名重载。
#[test]
fn reload_accepts_learned_and_completion_paths() {
    use wind_userdata::record::Target;
    let base = std::path::Path::new("/u/data");
    let p = |rel: &str| base.join(rel).to_string_lossy().into_owned();
    assert!(matches!(
        classify_text_path(base, &p("pinyin/freq.table.yaml"), false).unwrap(),
        TextPath::Learned(Target::Freq, s) if s == "pinyin"
    ));
    assert!(matches!(
        classify_text_path(base, &p("pinyin/temp.dict.yaml"), false).unwrap(),
        TextPath::Learned(Target::Temp, _)
    ));
    assert!(matches!(
        classify_text_path(base, &p("pinyin/draft.dict.yaml"), false).unwrap(),
        TextPath::Learned(Target::Draft, _)
    ));
    assert!(matches!(
        classify_text_path(base, &p("pinyin/assoc.table.yaml"), false).unwrap(),
        TextPath::Learned(Target::Assoc, _)
    ));
    assert!(matches!(
        classify_text_path(base, &p("completion.table.yaml"), false).unwrap(),
        TextPath::Completion
    ));
    assert!(classify_text_path(base, &p("pinyin/other.yaml"), false).is_err());
    assert!(
        matches!(
            classify_text_path(base, &p("stats/2026-10.toml"), false).unwrap(),
            TextPath::Stats
        ),
        "统计按需读文件，重载即生效"
    );
}

#[test]
fn reload_of_hand_edited_freq_text_takes_effect() {
    let s = Store::open(tmp("wind_uw_reload_freq.redb")).unwrap();
    s.record_freq("pinyin", "a", "啊").unwrap();
    s.flush_user_data().unwrap();
    let f = s.user_text_dir().join("pinyin/freq.table.yaml");
    let t = std::fs::read_to_string(&f)
        .unwrap()
        .replace("a\t啊\t1\t", "a\t啊\t9\t");
    std::fs::write(&f, t).unwrap();
    let changed = s
        .reload_user_text_paths(&[f.to_string_lossy().into_owned()])
        .unwrap();
    assert_eq!(changed, vec!["pinyin".to_string()]);
    assert_eq!(
        s.get_freq("pinyin", "a", "啊").unwrap().map(|r| r.count),
        Some(9)
    );
}

/// 真实 Windows：dict.reload 收到小写 + 正斜杠写法的路径也能重载。
#[cfg(windows)]
#[test]
fn reload_accepts_case_and_slash_variants_on_windows() {
    let s = Store::open(tmp("wind_uw_reload_ci.redb")).unwrap();
    s.add_user_word("wb", "a", "工", 1, 0).unwrap();
    s.flush_user_data().unwrap();
    let f = s.user_text_path("wb");
    std::fs::write(&f, std::fs::read_to_string(&f).unwrap() + "b\t式\t1\n").unwrap();
    let p = f.to_string_lossy().to_lowercase().replace('\\', "/");
    assert_eq!(
        s.reload_user_text_paths(&[p]).unwrap(),
        vec!["wb".to_string()]
    );
    assert_eq!(s.get_user_words("wb", "b").unwrap()[0].text, "式");
}

/// dict.flush：全部待处理操作折回，文本里有。
#[test]
fn flush_folds_everything() {
    let s = Store::open(tmp("wind_uw_flush.redb")).unwrap();
    s.add_user_word("wb", "a", "工", 1, 0).unwrap();
    s.flush_user_data().unwrap();
    assert!(
        std::fs::read_to_string(s.user_text_path("wb"))
            .unwrap()
            .contains("a\t工\t1")
    );
}

/// 手改短语与 Shadow 文件：sync_user_texts 一并载入，返回值含 "phrases" 与方案名；dict.reload
/// 也认这两种文件。
#[test]
fn sync_picks_up_phrase_and_shadow_hand_edits() {
    let s = Store::open(tmp("wind_uw_sync_ph_sh.redb")).unwrap();
    s.add_phrase("yx", "me@x.com", 0, 1800).unwrap();
    s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
    let pf = s.phrases_path();
    std::fs::write(
        &pf,
        std::fs::read_to_string(&pf).unwrap() + "dz\t地址\t1800\n",
    )
    .unwrap();
    let sf = s.shadow_path("wb");
    std::fs::write(
        &sf,
        std::fs::read_to_string(&sf).unwrap() + "bb\t八\tdelete\n",
    )
    .unwrap();
    let mut changed = s.sync_user_texts();
    changed.sort();
    assert_eq!(changed, vec!["phrases".to_string(), "wb".to_string()]);
    assert!(
        s.enabled_phrases_for_input()
            .unwrap()
            .iter()
            .any(|p| p.code == "dz")
    );
    assert!(s.get_shadow_rules("wb", "bb").unwrap().is_some());
    std::fs::write(&pf, std::fs::read_to_string(&pf).unwrap() + "xx\t叉\t1\n").unwrap();
    let p = pf.to_string_lossy().into_owned();
    assert_eq!(
        s.reload_user_text_paths(&[p]).unwrap(),
        vec!["phrases".to_string()]
    );
    let p = sf.to_string_lossy().into_owned();
    assert!(s.reload_user_text_paths(&[p]).is_ok());
}
