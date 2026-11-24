use super::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// 交换只认邻居自己的规则：带 id 的规则不按文本回退；邻居有规则时改写到 `here`。
#[test]
fn move_shadow_swapping_matches_like_reader() {
    let path = tmp("wind_sh_swap.redb");
    let s = Store::open(&path).unwrap();
    s.pin_shadow("ct", "aa", "乙", Some("phrase:aa:x"), 7)
        .unwrap();
    s.move_shadow_swapping("ct", "aa", "甲", None, 1, Some(("乙", None)), 0)
        .unwrap();
    let rec = s.get_shadow_rules("ct", "aa").unwrap().unwrap();
    let pos = |w: &str, id: Option<&str>| {
        rec.pinned
            .iter()
            .find(|p| p.word == w && p.cand_id.as_deref() == id)
            .map(|p| p.position)
    };
    assert_eq!(pos("乙", Some("phrase:aa:x")), Some(7), "id 规则原样");
    assert_eq!(pos("甲", None), Some(1));
    assert_eq!(rec.pinned.len(), 2);

    s.pin_shadow("ct", "aa", "丙", None, 1).unwrap();
    s.move_shadow_swapping("ct", "aa", "丁", None, 1, Some(("丙", None)), 2)
        .unwrap();
    let rec = s.get_shadow_rules("ct", "aa").unwrap().unwrap();
    let at = |w: &str| rec.pinned.iter().find(|p| p.word == w).map(|p| p.position);
    assert_eq!(
        (at("丁"), at("丙")),
        (Some(1), Some(2)),
        "邻居规则改写到原位"
    );
    let _ = std::fs::remove_file(&path);
}

/// 只撤同 (码, 文) 的删除规则：同码别的词、同文别的码、置顶都不动；键经 `key_norm` 归一后比。
#[test]
fn clear_deleted_for_words_only_touches_matching_deletes() {
    let path = tmp("wind_sh_clear_del.redb");
    let s = Store::open(&path).unwrap();
    s.delete_shadow("py", "nihao", "你好").unwrap();
    s.delete_shadow("py", "nihao", "拟好").unwrap();
    s.delete_shadow("py", "ni'hao", "你好").unwrap();
    s.delete_shadow("py", "nh", "你好").unwrap();
    s.pin_shadow("py", "nihao", "你号", None, 0).unwrap();
    let strip = |k: &str| vec![k.replace('\'', "")];

    assert_eq!(
        s.clear_deleted_for_words("py", &[("nihao", "你好")], &strip)
            .unwrap(),
        2,
        "`nihao` 与 `ni'hao` 两条同文删除都该撤"
    );
    let nihao = s.get_shadow_rules("py", "nihao").unwrap().unwrap();
    assert_eq!(nihao.deleted, vec!["拟好".to_string()]);
    assert_eq!(nihao.pinned.len(), 1, "置顶不动");
    assert!(s.get_shadow_rules("py", "ni'hao").unwrap().is_none());
    assert_eq!(
        s.get_shadow_rules("py", "nh").unwrap().unwrap().deleted,
        vec!["你好".to_string()],
        "别的编码（简拼）上的删除不动"
    );

    // 只读计数：按词条去重（`nh` 上的规则不算 `nihao` 的），不改文件
    s.delete_shadow("py", "nihao", "你好").unwrap();
    s.delete_shadow("py", "ni'hao", "你好").unwrap();
    let hidden = s
        .words_hidden_by_shadow(
            "py",
            &[("nihao", "你好"), ("nihao", "拟好"), ("hao", "好")],
            &strip,
        )
        .unwrap();
    assert_eq!(
        hidden.len(),
        2,
        "「你好」两条规则只算一个词，加「拟好」，实际 {hidden:?}"
    );
    assert!(
        s.get_shadow_rules("py", "ni'hao").unwrap().is_some(),
        "计数不撤规则"
    );

    // 恒等归一：`ni'hao` 不再等于 `nihao`
    s.delete_shadow("wb", "a'b", "工").unwrap();
    assert_eq!(
        s.clear_deleted_for_words("wb", &[("ab", "工")], &|k: &str| vec![k.to_string()])
            .unwrap(),
        0
    );
    let _ = std::fs::remove_file(&path);
}

/// 屏蔽行（`code = *`）：进屏蔽集合、不当输入码匹配；单字不收；加词按文字撤、批量计数按文字算。
#[test]
fn block_word_rows() {
    let path = tmp("wind_sh_block.redb");
    let s = Store::open(&path).unwrap();
    s.block_word("pinyin", "时间").unwrap();
    assert!(s.block_word("pinyin", "是").is_err(), "单字不能按文字屏蔽");
    s.delete_shadow("pinyin", "shijian", "实践").unwrap();
    let set = s.blocked_words("pinyin").unwrap();
    assert_eq!(set.len(), 1);
    assert!(set.contains("时间"));
    assert!(
        std::sync::Arc::ptr_eq(&set, &s.blocked_words("pinyin").unwrap()),
        "表没变时复用缓存"
    );
    assert!(
        s.get_shadow_rules("pinyin", BLOCK_ALL_CODES)
            .unwrap()
            .is_none(),
        "屏蔽组不是输入码"
    );
    assert!(
        s.list_shadow_rules("pinyin")
            .unwrap()
            .iter()
            .any(|(c, r)| c == BLOCK_ALL_CODES && r.deleted == vec!["时间".to_string()]),
        "规则列表照常列出屏蔽组"
    );

    let same = |k: &str| vec![k.to_string()];
    let hidden = s
        .words_hidden_by_shadow("pinyin", &[("sj", "时间"), ("shijian", "时间")], &same)
        .unwrap();
    assert_eq!(hidden.len(), 2, "按文字算，任何编码都命中：{hidden:?}");
    assert_eq!(
        s.clear_deleted_for_words("pinyin", &[("shijian", "时间")], &same)
            .unwrap(),
        1
    );
    assert!(
        s.blocked_words("pinyin").unwrap().is_empty(),
        "加词撤销屏蔽"
    );
    assert_eq!(
        s.get_shadow_rules("pinyin", "shijian")
            .unwrap()
            .unwrap()
            .deleted,
        vec!["实践".to_string()],
        "按编码的删除不动"
    );

    // 手改出的单字屏蔽行不生效
    s.delete_shadow("pinyin", BLOCK_ALL_CODES, "是").unwrap();
    assert!(s.blocked_words("pinyin").unwrap().is_empty());
    s.block_word("pinyin", "时间").unwrap();
    assert!(
        s.remove_shadow_rule("pinyin", BLOCK_ALL_CODES, "时间", None)
            .is_ok()
    );
    assert!(
        s.blocked_words("pinyin").unwrap().is_empty(),
        "规则列表能删屏蔽行"
    );
    let _ = std::fs::remove_file(&path);
}

/// 字数按用户看到的字算：带变体选择符 / 肤色 / 组合附加符 / 零宽连接的单个符号、国旗都是一个字。
#[test]
fn blockable_word_counts_visible_chars() {
    for one in [
        "好",
        "❤\u{FE0F}",
        "👍\u{1F3FD}",
        "👨\u{200D}👩\u{200D}👧",
        "🇨🇳",
        "e\u{301}",
        "1\u{FE0F}\u{20E3}",
        "",
    ] {
        assert!(!is_blockable_word(one), "{one:?} 是一个字");
    }
    for two in ["你好", "❤\u{FE0F}❤\u{FE0F}", "🇨🇳🇯🇵", "e\u{301}好"] {
        assert!(is_blockable_word(two), "{two:?} 至少两个字");
    }
    assert!(!is_blockable_word("好\u{200D}"), "尾随连接符不成字");
}

/// 英文桶同样认屏蔽行：键是词库原文、不归一大小写（`ok` 与 `OK` 是两个词），单字母不收。
#[test]
fn english_block_rows_are_case_sensitive() {
    let path = tmp("wind_sh_block_en.redb");
    let s = Store::open(&path).unwrap();
    s.block_word("english", "OK").unwrap();
    assert!(
        s.block_word("english", "x").is_err(),
        "单字母不能按文字屏蔽"
    );
    let set = s.blocked_words("english").unwrap();
    assert!(set.contains("OK") && !set.contains("ok"));
    assert!(
        s.get_shadow_rules("english", BLOCK_ALL_CODES)
            .unwrap()
            .is_none()
    );
    let same = |k: &str| vec![k.to_string()];
    assert!(
        s.words_hidden_by_shadow("english", &[("ok", "ok")], &same)
            .unwrap()
            .is_empty(),
        "小写的 ok 不算被屏蔽"
    );
    assert_eq!(
        s.clear_deleted_for_words("english", &[("ok", "OK")], &same)
            .unwrap(),
        1
    );
    assert!(s.blocked_words("english").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

/// `*` 的「屏蔽这个词」语义只在拼音 / 英文数据方案：码表（通配键 `*` 浏览态）里它就是普通编码。
#[test]
fn star_is_plain_code_outside_pinyin() {
    let path = tmp("wind_sh_star_ct.redb");
    let s = Store::open(&path).unwrap();
    s.delete_shadow("wb", BLOCK_ALL_CODES, "工作").unwrap();
    assert_eq!(
        s.get_shadow_rules("wb", BLOCK_ALL_CODES)
            .unwrap()
            .map(|r| r.deleted),
        Some(vec!["工作".to_string()]),
        "码表的 `*` 是输入码，规则照常读出"
    );
    assert!(s.blocked_words("wb").unwrap().is_empty());
    assert!(
        s.block_word("wb", "工作").is_err(),
        "只有拼音与英文能按文字屏蔽"
    );
    let same = |k: &str| vec![k.to_string()];
    assert!(
        s.words_hidden_by_shadow("wb", &[("aaaa", "工作")], &same)
            .unwrap()
            .is_empty(),
        "别的编码不按文字命中"
    );
    assert_eq!(
        s.clear_deleted_for_words("wb", &[("aaaa", "工作")], &same)
            .unwrap(),
        0
    );
    assert_eq!(
        s.clear_deleted_for_words("wb", &[(BLOCK_ALL_CODES, "工作")], &same)
            .unwrap(),
        1,
        "同码（`*`）照常按编码撤"
    );
    let _ = std::fs::remove_file(&path);
}

/// 命中按文件现状判：表已载入后文件被外部改过（这里拿另一份库生成的规则文件覆盖），
/// 不先 sync 就看不见新写进去的删除规则。
#[test]
fn clear_deleted_for_words_sees_hand_edited_file() {
    let path = tmp("wind_sh_clear_sync.redb");
    let s = Store::open(&path).unwrap();
    assert!(s.get_shadow_rules("py", "nihao").unwrap().is_none()); // 表载入内存
    let path2 = tmp("wind_sh_clear_sync2.redb");
    let s2 = Store::open(&path2).unwrap();
    s2.delete_shadow("py", "nihao", "你好").unwrap();
    let dst = s.shadow_path("py");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(s2.shadow_path("py"), &dst).unwrap();

    let n = s
        .clear_deleted_for_words("py", &[("nihao", "你好")], &|k: &str| vec![k.to_string()])
        .unwrap();
    assert_eq!(n, 1);
    assert!(s.get_shadow_rules("py", "nihao").unwrap().is_none());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

#[test]
fn shadow_jsonl_roundtrip_and_clear() {
    let path = tmp("wind_sh_io.redb");
    let s = Store::open(&path).unwrap();
    s.pin_shadow("wb", "aaaa", "恭", Some("c1"), 0).unwrap();
    s.pin_shadow("wb", "aaaa", "敬", None, 1).unwrap(); // 后置顶 → 存储序 index 0
    s.delete_shadow("wb", "bbbb", "删词").unwrap();
    let text = s.export_shadow_jsonl("wb").unwrap();
    assert_eq!(text.lines().count(), 2);

    let path2 = tmp("wind_sh_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    let (imported, skipped) = s2.import_shadow_jsonl("wb", &text).unwrap();
    assert_eq!(skipped, 0);
    assert!(imported >= 3);
    let rules = s2.list_shadow_rules("wb").unwrap();
    assert_eq!(rules.len(), 2);
    let pinned = rules.iter().find(|(c, _)| c == "aaaa").unwrap();
    // round-trip 保序：LIFO 存储序（最新在前）导入后不反转
    assert_eq!(pinned.1.pinned.len(), 2);
    assert_eq!(pinned.1.pinned[0].word, "敬");
    assert_eq!(pinned.1.pinned[0].position, 1);
    assert_eq!(pinned.1.pinned[1].word, "恭");
    assert_eq!(pinned.1.pinned[1].position, 0);

    assert_eq!(s.clear_shadow("wb").unwrap(), 2);
    assert!(s.list_shadow_rules("wb").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

#[test]
fn test_redb_shadow_ops() {
    let path = std::env::temp_dir().join("wind_shadow_redb.redb");
    let _ = std::fs::remove_file(&path);
    let s = Store::open(&path).unwrap();

    // pin + delete 不同码
    s.pin_shadow("wb", "aaaa", "恭恭敬敬", None, 0).unwrap();
    s.delete_shadow("wb", "bbbb", "某词").unwrap();
    let r = s.get_shadow_rules("wb", "aaaa").unwrap().unwrap();
    assert_eq!(r.pinned.len(), 1);
    assert_eq!(r.pinned[0].position, 0);
    assert!(s.get_shadow_rules("wb", "bbbb").unwrap().unwrap().deleted == vec!["某词".to_string()]);

    // pin 后 delete 同词 → pin 被移除、转为 deleted
    s.delete_shadow("wb", "aaaa", "恭恭敬敬").unwrap();
    let r = s.get_shadow_rules("wb", "aaaa").unwrap().unwrap();
    assert!(r.pinned.is_empty());
    assert_eq!(r.deleted, vec!["恭恭敬敬".to_string()]);

    // remove → 规则清空后该键删除
    s.remove_shadow_rule("wb", "aaaa", "恭恭敬敬", None)
        .unwrap();
    assert!(s.get_shadow_rules("wb", "aaaa").unwrap().is_none());

    // cand_id 动态短语：按 id 去重
    s.pin_shadow("wb", "zz", "日期", Some("phrase:zz:date"), 0)
        .unwrap();
    s.pin_shadow("wb", "zz", "日期改", Some("phrase:zz:date"), 1)
        .unwrap();
    let r = s.get_shadow_rules("wb", "zz").unwrap().unwrap();
    assert_eq!(r.pinned.len(), 1, "同 cand_id 应去重为 1 条");
    assert_eq!(r.pinned[0].word, "日期改");

    let _ = std::fs::remove_file(&path);
}

/// 动态短语（`date`）的完整生命周期：规则里的 word 是写入当天的求值文本，次日候选文本已变。
/// 去重 / 恢复默认 / 菜单灰显三处都必须按 cand_id 定位。必须用两个不同的 word（昨天/今天），
/// 否则按 word 匹配也会全绿。
#[test]
fn dynamic_phrase_rule_located_by_cand_id_across_days() {
    let path = tmp("wind_shadow_dyn_phrase.redb");
    let s = Store::open(&path).unwrap();
    let id = "phrase:date:$Y-$MM-$DD";

    // 昨天：把 date 的 `$Y-$MM-$DD` 那条置顶，word 记的是昨天的求值结果。
    s.pin_shadow("wb", "date", "2026-07-28", Some(id), 0)
        .unwrap();

    // 今天：候选文本已变成 2026-07-29，而规则里存的仍是昨天的 2026-07-28。
    // 判别力全在这两行——按 id 查得到，只按今天的文本查不到。
    let rec = s.get_shadow_rules("wb", "date").unwrap().unwrap();
    assert!(rec.has_target("2026-07-29", Some(id)), "按 id 应跨日命中");
    assert!(
        !rec.has_target("2026-07-29", None),
        "只按当日文本必失配——这正是「昨天调好今天被还原」的成因"
    );

    // 今天：同一条短语再次置顶到别的位置 → 应更新原规则而非新增一条。
    s.pin_shadow("wb", "date", "2026-07-29", Some(id), 2)
        .unwrap();
    let rec = s.get_shadow_rules("wb", "date").unwrap().unwrap();
    assert_eq!(rec.pinned.len(), 1, "同 id 跨日应去重，而不是逐日堆积");
    assert_eq!(rec.pinned[0].position, 2);
    assert_eq!(rec.pinned[0].word, "2026-07-29", "word 刷新为当次文本");

    // 明天：文本又变了，按 id 仍能删掉。
    s.remove_shadow_rule("wb", "date", "2026-07-30", Some(id))
        .unwrap();
    assert!(
        s.get_shadow_rules("wb", "date").unwrap().is_none(),
        "按 cand_id 恢复默认应删得掉跨日规则"
    );
    let _ = std::fs::remove_file(&path);
}

/// 反向闸门：静态候选（无 id）的规则仍按 word 定位，不受上面的改动影响。
#[test]
fn static_candidate_rule_still_located_by_word() {
    let path = tmp("wind_shadow_static_word.redb");
    let s = Store::open(&path).unwrap();
    s.pin_shadow("wb", "aaaa", "工", None, 0).unwrap();
    let rec = s.get_shadow_rules("wb", "aaaa").unwrap().unwrap();
    assert!(rec.has_target("工", None));
    assert!(!rec.has_target("恭", None));
    // 传了 id 但规则侧无 id → 落回 word 比较，仍命中。
    assert!(rec.has_target("工", Some("phrase:x:y")));
    s.remove_shadow_rule("wb", "aaaa", "工", None).unwrap();
    assert!(s.get_shadow_rules("wb", "aaaa").unwrap().is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_list_shadow_rules() {
    let path = std::env::temp_dir().join("wind_shadow_list.redb");
    let _ = std::fs::remove_file(&path);
    let s = Store::open(&path).unwrap();
    s.pin_shadow("wb", "aaaa", "恭恭敬敬", None, 0).unwrap();
    s.delete_shadow("wb", "bbbb", "某词").unwrap();
    s.pin_shadow("py", "nihao", "你好", None, 0).unwrap();

    let mut wb = s.list_shadow_rules("wb").unwrap();
    wb.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(wb.len(), 2, "wb 应有 aaaa/bbbb 两个 code");
    assert_eq!(wb[0].0, "aaaa");
    assert_eq!(wb[0].1.pinned.len(), 1);
    assert_eq!(wb[1].0, "bbbb");
    assert_eq!(wb[1].1.deleted, vec!["某词".to_string()]);
    // 跨方案隔离
    assert_eq!(s.list_shadow_rules("py").unwrap().len(), 1);
    let _ = std::fs::remove_file(&path);
}

/// 规则落文本：pin / delete 按行写进 shadow.dict.yaml，重开读回；newest-first 保持。
#[test]
fn shadow_rules_live_in_text() {
    let p = tmp("wind_shadow_text.redb");
    {
        let s = Store::open(&p).unwrap();
        s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
        s.pin_shadow("wb", "aa", "工", None, 1).unwrap();
        s.delete_shadow("wb", "aa", "式").unwrap();
        let f = std::fs::read_to_string(s.shadow_path("wb")).unwrap();
        assert!(
            f.contains("aa\t工\tpin\t1\t\naa\t恭\tpin\t0\t\naa\t式\tdelete\t\t\n"),
            "{f}"
        );
    }
    let s = Store::open(&p).unwrap();
    let r = s.get_shadow_rules("wb", "aa").unwrap().unwrap();
    assert_eq!(
        r.pinned.iter().map(|x| x.word.as_str()).collect::<Vec<_>>(),
        vec!["工", "恭"]
    );
    assert_eq!(r.deleted, vec!["式".to_string()]);
}

/// 空编码桶（特殊模式）往返不丢。
#[test]
fn empty_code_rules_roundtrip() {
    let p = tmp("wind_shadow_empty_code.redb");
    {
        let s = Store::open(&p).unwrap();
        s.pin_shadow("wb", "", "日期", Some("phrase:rq:{date}"), 2)
            .unwrap();
    }
    let s = Store::open(&p).unwrap();
    let r = s.get_shadow_rules("wb", "").unwrap().unwrap();
    assert_eq!(r.pinned[0].cand_id.as_deref(), Some("phrase:rq:{date}"));
    assert_eq!(r.pinned[0].position, 2);
}

/// 按键路径查 Shadow 不碰 redb、不读文件（加载后删掉文件、暂停 redb 照样查得到）；手改文件
/// sync 后生效。
#[test]
fn shadow_lookup_does_not_touch_redb_or_file() {
    let s = Store::open(tmp("wind_shadow_hot.redb")).unwrap();
    s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
    let f = s.shadow_path("wb");
    let saved = std::fs::read(&f).unwrap();
    std::fs::remove_file(&f).unwrap();
    s.pause().unwrap();
    assert!(s.get_shadow_rules("wb", "aa").unwrap().is_some());
    s.resume().unwrap();
    std::fs::write(
        &f,
        [saved.as_slice(), "bb\t八\tdelete\t\t\n".as_bytes()].concat(),
    )
    .unwrap();
    assert!(
        s.get_shadow_rules("wb", "bb").unwrap().is_none(),
        "未 sync 不生效"
    );
    assert_eq!(s.sync_shadows(), vec!["wb".to_string()]);
    assert_eq!(
        s.get_shadow_rules("wb", "bb").unwrap().unwrap().deleted,
        vec!["八".to_string()]
    );
}

/// 导入一批规则只写一次文件（逐条提交会把备份还原拖成每条一次 fsync）。
#[test]
fn import_writes_once() {
    let s = Store::open(tmp("wind_shadow_import_once.redb")).unwrap();
    let actions: Vec<crate::wdict::ShadowActionIo> = (0..200)
        .map(|i| crate::wdict::ShadowActionIo {
            action: "del".into(),
            code: format!("c{i}"),
            word: format!("词{i}"),
            position: 0,
            cand_id: None,
        })
        .collect();
    let t = std::time::Instant::now();
    assert_eq!(s.import_shadow_actions("wb", &actions).unwrap(), 200);
    assert!(
        t.elapsed() < std::time::Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(s.list_shadow_rules("wb").unwrap().len(), 200);
}
