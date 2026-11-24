use super::*;

// ------------------------------------------------------------------
// 单表基础用例
// ------------------------------------------------------------------

/// 单字单码：基本构建 + 查询
#[test]
fn single_code_per_char_basic() {
    let t = AuxCodeTable::from_rows(vec![('李', "mz"), ('杨', "ms"), ('林', "mm")]);
    assert_eq!(t.char_count(), 3);
    assert_eq!(t.code_count(), 3);
    assert_eq!(t.first_code('李'), Some("mz"));
    assert_eq!(t.codes_of('李').collect::<Vec<_>>(), vec!["mz"]);
    assert_eq!(t.first_code('王'), None, "未收录字返回空");
}

/// 同字多码：严格按行序保留，不去重不覆盖
#[test]
fn multiple_codes_preserve_order() {
    // 对应 rime 例子：厑 有 ib、ii 两码
    let t = AuxCodeTable::from_rows(vec![('厑', "ib"), ('厑', "ii"), ('阿', "ek")]);
    assert_eq!(t.char_count(), 2, "厑 + 阿 = 2 字");
    assert_eq!(t.code_count(), 3, "2 + 1 = 3 码");

    let e_codes: Vec<_> = t.codes_of('厑').collect();
    assert_eq!(e_codes, vec!["ib", "ii"], "多码按行序排列，前行优先");
    assert_eq!(t.first_code('厑'), Some("ib"), "首选码 = 出现最早的那条");

    assert_eq!(t.codes_of('阿').collect::<Vec<_>>(), vec!["ek"]);
}

/// 同字完全重复的码：first-seen 去重，只留首次出现位置
#[test]
fn duplicate_codes_are_deduplicated() {
    let t = AuxCodeTable::from_rows(vec![('厑', "ib"), ('厑', "ib"), ('厑', "ii")]);
    assert_eq!(t.char_count(), 1);
    assert_eq!(t.code_count(), 2, "重复的 ib 只入列一次：ib + ii = 2");
    let codes: Vec<_> = t.codes_of('厑').collect();
    assert_eq!(codes, vec!["ib", "ii"], "重复 ib 被去重后只留首次出现的");
    // 匹配判断无感知：第一个 ib 命中就 early-return，去重不影响功能
    assert!(t.any_code_starts_with('厑', "ib"));
    // 乱序重复也能去重：ib,ii,ib → first-seen 保序去重后仍是 ib,ii
    let t2 = AuxCodeTable::from_rows(vec![('厑', "ib"), ('厑', "ii"), ('厑', "ib")]);
    let c2: Vec<_> = t2.codes_of('厑').collect();
    assert_eq!(c2, vec!["ib", "ii"]);
}

/// 空码行跳过
#[test]
fn empty_code_is_skipped() {
    let t = AuxCodeTable::from_rows(vec![('李', ""), ('杨', "ms"), ('杨', "")]);
    assert_eq!(t.char_count(), 1, "李被跳过，只留杨");
    assert_eq!(t.code_count(), 1);
    assert_eq!(t.first_code('李'), None);
    assert_eq!(t.first_code('杨'), Some("ms"));
}

/// 乱序输入构建后正确排序并二分
#[test]
fn unsorted_input_builds_sorted_entries() {
    let t = AuxCodeTable::from_rows(vec![('林', "mm"), ('李', "mz"), ('杨', "ms")]);
    assert_eq!(t.first_code('李'), Some("mz"));
    assert_eq!(t.first_code('杨'), Some("ms"));
    assert_eq!(t.first_code('林'), Some("mm"));
}

/// any_code_starts_with：匹配到任一码即 true，early-return
#[test]
fn any_code_starts_with_prefix() {
    let t = AuxCodeTable::from_rows(vec![('厑', "ib"), ('厑', "ii"), ('李', "mz")]);
    assert!(t.any_code_starts_with('厑', "i"));
    assert!(t.any_code_starts_with('厑', "ib"));
    assert!(t.any_code_starts_with('厑', "ii"));
    assert!(!t.any_code_starts_with('厑', "x"));
    assert!(!t.any_code_starts_with('王', "m"));
    assert!(t.any_code_starts_with('李', "mz"));
}

/// 空表行为
#[test]
fn empty_table() {
    let t = AuxCodeTable::new();
    assert!(t.is_empty());
    assert_eq!(t.char_count(), 0);
    assert_eq!(t.code_count(), 0);
    assert_eq!(t.first_code('李'), None);
    assert!(!t.any_code_starts_with('李', "m"));
}

/// codes_of 对未收录字返回空迭代（不 panic）
#[test]
fn codes_of_missing_char_is_empty() {
    let t = AuxCodeTable::from_rows(vec![('李', "mz")]);
    assert_eq!(t.codes_of('王').count(), 0);
}

/// 码长任意：笔画/拆字 1 码、2 码、4 码混合在同一张表里也能正确切片
#[test]
fn mixed_code_lengths_work() {
    // 对应文档示例的长短码混合：u (1B) + ab (2B) + wxyz (4B)
    let t = AuxCodeTable::from_rows(vec![
        ('厑', "u"),
        ('厑', "ab"),
        ('厑', "wxyz"),
        ('李', "abcdef"), // 6 码（笔画方案可能出现）
        ('李', "m"),      // 1 码的简码
    ]);

    // 1) 厑：三码长度各不相同，正确按序切片
    let e: Vec<_> = t.codes_of('厑').collect();
    assert_eq!(e, vec!["u", "ab", "wxyz"]);
    assert_eq!(t.first_code('厑'), Some("u"));
    assert_eq!(t.code_count(), 5, "厑 3 码 + 李 2 码 = 5 码");

    // 2) 前缀匹配正确作用在变长码上
    assert!(t.any_code_starts_with('厑', "u")); // 1 码整码命中
    assert!(t.any_code_starts_with('厑', "a")); // ab 前缀命中
    assert!(t.any_code_starts_with('厑', "ab"));
    assert!(t.any_code_starts_with('厑', "wx")); // wxyz 前 2 位命中
    assert!(t.any_code_starts_with('厑', "wxy"));
    assert!(t.any_code_starts_with('厑', "wxyz"));
    assert!(!t.any_code_starts_with('厑', "wxyza"), "超过码长不匹配");

    // 3) 李：6 码 + 1 码混排
    let li: Vec<_> = t.codes_of('李').collect();
    assert_eq!(li, vec!["abcdef", "m"]);
    assert_eq!(t.first_code('李'), Some("abcdef"), "行序优先，6 码在前");
    assert!(t.any_code_starts_with('李', "abcd"));
    assert!(t.any_code_starts_with('李', "m"));
}

/// load_from_file + merge 组合（协调器懒加载路径）：多文件按顺序合并、
/// 跨文件同码去重、异码共存
#[test]
fn load_and_merge_multiple_files() {
    let dir = std::env::temp_dir().join(format!("wind-aux-code-merge-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hi = dir.join("high.txt");
    let lo = dir.join("low.txt");
    std::fs::write(&hi, "李=mz\n河=sk\n").unwrap();
    std::fs::write(&lo, "李=mz\n河=dk\n樱=mn\n").unwrap();
    let t = AuxCodeTable::merge(
        [&hi, &lo]
            .into_iter()
            .map(|p| crate::loader::load_from_file(p)),
    );
    for f in [&hi, &lo] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir(&dir);
    // 高优表在前：跨文件同码去重 → 李只剩高优的 mz；河两文件码不同 → sk + dk 并存
    assert_eq!(t.codes_of('李').collect::<Vec<_>>(), vec!["mz"]);
    assert_eq!(t.codes_of('河').collect::<Vec<_>>(), vec!["sk", "dk"]);
    assert_eq!(t.codes_of('樱').collect::<Vec<_>>(), vec!["mn"]);
}

// ------------------------------------------------------------------
// 合并表 / 多表挂载用例
// ------------------------------------------------------------------

/// 构造合并双表：拆分（高优）+ 小鹤（低优）
fn two_tables_merged() -> AuxCodeTable {
    let chaifen = AuxCodeTable::from_rows(vec![
        ('李', "mz"), // 木+子
        ('河', "sk"), // 氵+可
        ('樱', "my"), // 木+婴
        ('厑', "ii"), // rime 示例
    ]);
    let xiaohe = AuxCodeTable::from_rows(vec![
        ('李', "mz"), // 木子
        ('河', "dk"), // 氵口
        ('樱', "mn"), // 木女
        ('厑', "ib"), // rime 示例第二码
    ]);
    // merge(高, 低) 等同于 append_table(高).append_table(低)
    let mut s = AuxCodeTable::new();
    s.append(chaifen);
    s.append(xiaohe);
    s
}

/// 合并后顺序：高优先级表的所有码在前
#[test]
fn merge_preserves_table_priority_order() {
    let s = two_tables_merged();
    // 樱：拆分表 my（高），小鹤表 mn（低）→ 合并后顺序反映挂载顺序
    let ying: Vec<_> = s.codes_of('樱').collect();
    assert_eq!(ying, vec!["my", "mn"]);
    let yi: Vec<_> = s.codes_of('厑').collect();
    assert_eq!(yi, vec!["ii", "ib"]);
    // 李：两表都是 mz → 跨表同码 first-seen 去重，只输出一个 mz
    let li: Vec<_> = s.codes_of('李').collect();
    assert_eq!(
        li,
        vec!["mz"],
        "跨表同码去重：高优表 mz 首次出现即保留，低优表 mz 被丢弃"
    );
}

/// first_code：取最高优先级表的首选码
#[test]
fn merge_first_code_takes_highest_priority() {
    let s = two_tables_merged();
    assert_eq!(s.first_code('樱'), Some("my"));
}

/// merge：名称取首个非空（与「先出现 = 高优」的码优先级同语义）
#[test]
fn merge_name_takes_first_non_empty() {
    let a = AuxCodeTable::from_rows(vec![('李', "mz")]).with_name("拆分".to_string());
    let b = AuxCodeTable::from_rows(vec![('林', "mm")]).with_name("小鹤".to_string());
    let m = AuxCodeTable::merge(vec![a, b]);
    assert_eq!(m.name, "拆分", "高优表名称优先");
    let empty_first = AuxCodeTable::from_rows(vec![('李', "mz")]);
    let named_second = AuxCodeTable::from_rows(vec![('林', "mm")]).with_name("笔画".to_string());
    let m2 = AuxCodeTable::merge(vec![empty_first, named_second]);
    assert_eq!(m2.name, "笔画", "首表无名称时取后续首个非空");
}

/// 高优表未收录时，回落低优表
#[test]
fn merge_first_code_falls_back_to_lower_table() {
    let a = AuxCodeTable::from_rows(vec![('李', "mz")]);
    let b = AuxCodeTable::from_rows(vec![('林', "mm")]);
    let s = AuxCodeTable::merge(vec![a, b]);
    assert_eq!(s.first_code('林'), Some("mm"));
    assert_eq!(s.first_code('王'), None);
}

/// 跨表前缀匹配：高优/低优/未命中的分支都覆盖
#[test]
fn merge_any_code_starts_with_cross_tables() {
    let s = two_tables_merged();
    assert!(s.any_code_starts_with('李', "m"));
    assert!(s.any_code_starts_with('河', "s")); // 拆分表 sk 直接命中
    assert!(s.any_code_starts_with('河', "d")); // 拆分 sk 不匹配，小鹤 dk 命中
    assert!(!s.any_code_starts_with('樱', "y"));
    assert!(s.any_code_starts_with('樱', "my")); // 第一个码 my
    assert!(s.any_code_starts_with('樱', "mn")); // 第二个码 mn
    assert!(!s.any_code_starts_with('李', "x"));
    assert!(!s.any_code_starts_with('王', "m"));
}

/// 空表 append 被跳过（is_empty 仍为 true）
#[test]
fn merge_empty_table_append_is_skipped() {
    let mut s = AuxCodeTable::new();
    s.append(AuxCodeTable::new());
    assert!(s.is_empty());
}

/// merge 是整体替换：老表内容被清掉
#[test]
fn merge_replaces_all_previous() {
    let mut s = two_tables_merged();
    assert!(!s.is_empty());
    // 只用一张新表重新 merge，完全替换
    let new_only = AuxCodeTable::from_rows(vec![('杨', "ms")]);
    s = AuxCodeTable::merge(vec![new_only]);
    assert_eq!(s.first_code('杨'), Some("ms"));
    assert_eq!(s.first_code('李'), None, "老表的李必须被清掉");
}
