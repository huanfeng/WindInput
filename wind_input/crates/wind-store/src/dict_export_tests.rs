use super::*;

/// 恒等闸口：本模块的用例测的是多段编排（段的存在性、replace 清空、计数），
/// 准入判据要引擎、属 `wind-webdata` 的测试范围。
fn pass_through(_sec: DictSection, rows: Vec<WordIo>) -> Vec<WordIo> {
    rows
}

/// 准入闸口作用于 **words 与 temp_words 两段**，被丢掉的行如实进 `skipped`、
/// 不进 `imported`。
///
/// ★ 为什么这条测试有必要：闸口是**注入**的，接错段或漏接一段都不会有任何编译错误
/// 或运行时报错——表现是「导入成功，但那批词悄悄进去了」。临时词那段尤其容易漏：
/// 它会随 `promote_temp_word` 晋升成用户词，是绕过不变量的一条现成后门。
#[test]
fn contract_gates_both_word_sections_and_is_counted_as_skipped() {
    let path = tmp("wind_dict_contract_gate.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "好", 100, 0).unwrap();
    s.add_user_word("wb", "b", "坏", 100, 0).unwrap();
    s.learn_temp_word("wb", "c", "好", 50, 0).unwrap();
    s.learn_temp_word("wb", "d", "坏", 50, 0).unwrap();

    let all = [DictSection::UserWords, DictSection::TempWords];
    let text = s
        .export_dict_sections_wdict("wb", &all, "2026-08-22T00:00:00+08:00", "codetable")
        .unwrap();

    let path2 = tmp("wind_dict_contract_gate2.redb");
    let s2 = Store::open(&path2).unwrap();
    let mut seen: Vec<&'static str> = Vec::new();
    let rep = s2
        .import_dict_sections_wdict("wb", &text, &all, false, &mut |sec, rows| {
            seen.push(sec.key());
            rows.into_iter().filter(|r| r.text != "坏").collect()
        })
        .unwrap();

    assert_eq!(
        seen,
        vec!["userWords", "tempWords"],
        "两段都必须过闸，且闸口要知道自己在处理哪一段"
    );
    let uw = rep.sections.iter().find(|s| s.key == "userWords").unwrap();
    assert_eq!(uw.imported, 1, "imported 取闸口之后的条数");
    assert_eq!(uw.skipped, 1, "被闸口丢掉的行必须进 skipped");
    let tw = rep.sections.iter().find(|s| s.key == "tempWords").unwrap();
    assert_eq!(tw.imported, 1);
    assert_eq!(tw.skipped, 1);

    // 落库端的真凭据：被丢的词一条都不能在库里。
    assert!(
        s2.get_user_words("wb", "b").unwrap().is_empty(),
        "坏 不该入库"
    );
    assert!(s2.get_temp_words("wb", "d").unwrap().is_empty());
    assert_eq!(
        s2.get_user_words("wb", "a").unwrap().len(),
        1,
        "放行的照常入库"
    );
}

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// **freq 段的 code 列也要带音节空格**，与 words / temp_words 两段同形；
/// 且导入端必须拆回扁平 key。
///
/// 词频表自己不存 boundary，导出时只能反查——store 层查得到用户词与临时词，
/// 查不到系统词（那需要 engine），故系统词的词频记录保持扁平，属正常降级。
///
/// ⚠️ **导入端拆空格比导出更要紧**：不拆就会写进一条永不匹配任何候选的死键
/// （查询侧拿的是候选的扁平 code），用户只会看到「调频不生效」，且毫无痕迹。
#[test]
fn freq_section_uses_spaced_code_and_imports_flat() {
    let path = tmp("wind_dict_freq_spaced.redb");
    let s = Store::open(&path).unwrap();
    // 用户词提供边界：ni|hao
    s.add_user_word("pinyin", "nihao", "你好", 500, 0b101)
        .unwrap();
    s.record_freq("pinyin", "nihao", "你好").unwrap();
    // 无处可查边界 → 保持扁平
    s.record_freq("pinyin", "wubian", "无边").unwrap();

    let text = s
        .export_dict_sections_wdict("pinyin", &[DictSection::Freq], "t", "pinyin")
        .unwrap();
    assert!(
        text.contains("ni hao\t你好"),
        "freq 段的 code 应带音节空格，实际:\n{text}"
    );
    assert!(
        text.contains("wubian\t无边"),
        "查不到边界的记录保持扁平，实际:\n{text}"
    );

    // 导入到新库：key 必须是扁平的，否则这条记录永远匹配不到候选
    let p2 = tmp("wind_dict_freq_spaced2.redb");
    let s2 = Store::open(&p2).unwrap();
    s2.import_dict_sections_wdict(
        "pinyin",
        &text,
        &[DictSection::Freq],
        false,
        &mut pass_through,
    )
    .unwrap();
    let (rows, _) = s2.list_freq_paged("pinyin", "", 0, 0).unwrap();
    let mut codes: Vec<String> = rows.into_iter().map(|(c, _, _)| c).collect();
    codes.sort();
    assert_eq!(
        codes,
        vec!["nihao".to_string(), "wubian".to_string()],
        "导入端须把带空格的 code 拆回扁平 key"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&p2);
}

#[test]
fn all_sections_roundtrip() {
    let path = tmp("wind_dict_sections_io.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    s.learn_temp_word("wb", "ab", "临时", 50, 0).unwrap();
    s.learn_temp_word("wb", "ab", "临时", 50, 0).unwrap(); // temp count -> 2
    s.record_freq("wb", "a", "工").unwrap();
    s.record_freq("wb", "a", "工").unwrap(); // freq count -> 2
    s.pin_shadow("wb", "aaaa", "恭", None, 0).unwrap();
    s.delete_shadow("wb", "bbbb", "见").unwrap();

    let all = [
        DictSection::UserWords,
        DictSection::TempWords,
        DictSection::Freq,
        DictSection::Shadow,
    ];
    let text = s
        .export_dict_sections_wdict("wb", &all, "2026-07-15T00:00:00+08:00", "codetable")
        .unwrap();
    for tag in ["--- !words", "--- !temp_words", "--- !freq", "--- !shadow"] {
        assert!(text.contains(tag), "缺段 {tag}");
    }

    // 导入新库，全段还原
    let path2 = tmp("wind_dict_sections_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    let rep = s2
        .import_dict_sections_wdict("wb", &text, &all, false, &mut pass_through)
        .unwrap();
    assert_eq!(rep.sections.len(), 4);

    let uw = s2.get_user_words("wb", "a").unwrap();
    assert_eq!(uw[0].weight, 100);
    assert_eq!(uw[0].count, 0, "用户词 P2 起不保留 count");
    let tw = s2.get_temp_words("wb", "ab").unwrap();
    assert_eq!(tw[0].count, 2, "临时词 count 保真");
    assert_eq!(
        s2.get_freq("wb", "a", "工").unwrap().unwrap().count,
        2,
        "词频 count 流转"
    );
    assert!(
        s2.get_shadow_rules("wb", "aaaa").unwrap().is_some(),
        "pin 还原"
    );
    assert_eq!(
        s2.get_shadow_rules("wb", "bbbb").unwrap().unwrap().deleted,
        vec!["见".to_string()],
        "del 还原"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

/// .wdict 不带联想历史：Freq 段 replace 导入时一并清空该方案的联想历史，merge 不动。
#[test]
fn freq_replace_clears_assoc_history() {
    let path = tmp("wind_dict_freq_assoc.redb");
    let s = Store::open(&path).unwrap();
    s.record_freq("wb", "a", "工").unwrap();
    let text = s
        .export_dict_sections_wdict(
            "wb",
            &[DictSection::Freq],
            "2026-07-15T00:00:00+08:00",
            "codetable",
        )
        .unwrap();
    s.record_assoc_pick("wb", "荷", "荷花").unwrap();
    s.record_assoc_pick("py", "荷", "荷花").unwrap();
    s.import_dict_sections_wdict("wb", &text, &[DictSection::Freq], false, &mut pass_through)
        .unwrap();
    assert_eq!(
        s.assoc_history("wb", "荷", 0).unwrap().len(),
        1,
        "merge 不清"
    );
    s.import_dict_sections_wdict("wb", &text, &[DictSection::Freq], true, &mut pass_through)
        .unwrap();
    assert!(
        s.assoc_history("wb", "荷", 0).unwrap().is_empty(),
        "replace 清本方案"
    );
    assert_eq!(
        s.assoc_history("py", "荷", 0).unwrap().len(),
        1,
        "别的方案不动"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn selective_export_and_present_guard() {
    let path = tmp("wind_dict_sel.redb");
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    s.record_freq("wb", "a", "工").unwrap();

    // 只导词频
    let text = s
        .export_dict_sections_wdict("wb", &[DictSection::Freq], "t", "codetable")
        .unwrap();
    assert!(text.contains("--- !freq"));
    assert!(!text.contains("--- !words"), "未选用户词库不应写出");

    // 目标库已有用户词；即便选了 UserWords，但文件无 words 段 → replace 也不清空
    let path2 = tmp("wind_dict_sel2.redb");
    let s2 = Store::open(&path2).unwrap();
    s2.add_user_word("wb", "z", "旧", 9, 0).unwrap();
    let all = [DictSection::UserWords, DictSection::Freq];
    let rep = s2
        .import_dict_sections_wdict("wb", &text, &all, true, &mut pass_through)
        .unwrap();
    assert_eq!(rep.sections.len(), 1, "只处理文件存在的 freq 段");
    assert_eq!(rep.sections[0].key, "freq");
    assert!(
        !s2.get_user_words("wb", "z").unwrap().is_empty(),
        "文件无 words 段，replace 不应清空既有用户词"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}
