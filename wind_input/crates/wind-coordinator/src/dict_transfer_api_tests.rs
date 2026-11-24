use super::*;

/// 拼音词条准入契约的处置表：六种裁决 × 两种 fill 选择，各落到哪一档。
///
/// 这条测试之所以直接喂 `dispose_contract_rows` 而不走 `import`：没有数据目录的
/// 夹具引擎加载不起来 ⇒ `target_is_pinyin` 恒 false ⇒ 契约整段空转。走导入口写出来的
/// 用例会永远绿着却一个分支都没进，是本仓最典型的假绿形态。
#[test]
fn contract_disposition_table_covers_both_fill_choices() {
    use wind_engine::BoundaryResolution as B;
    let row = |code: &str, text: &str| WordIo {
        code: code.into(),
        text: text.into(),
        weight: 100,
        count: 0,
        boundary: None,
    };
    let rows = vec![
        row("ni hao", "你好"),   // 层 1：自带空格
        row("nihao", "你好"),    // Derived
        row("angan", "安甘"),    // Ambiguous
        row("wgkq", "工"),       // Unresolvable
        row("verylong", "超长"), // NoInfo（码超 64 字节等）
        row("zuo", "←"),         // NoReading（issue #97：拼音码 → 符号候选）
    ];
    let verdicts = [
        None,
        Some(B::Derived(0b101)),
        Some(B::Ambiguous(0b101)),
        Some(B::Unresolvable),
        Some(B::NoInfo),
        Some(B::NoReading(0b1)),
    ];

    // ① 补充（默认）
    let (kept, st) = dispose_contract_rows(rows.clone(), &verdicts, true);
    assert_eq!(kept.len(), 5, "只有 Unresolvable 不入库");
    assert_eq!(st.rejected, 1);
    assert_eq!(st.filled, 3, "Derived / Ambiguous / NoReading 都算补齐");
    assert_eq!(st.ambiguous, 1, "Ambiguous 是 filled 的子集");
    assert_eq!(
        st.no_reading, 1,
        "★ NoReading 单列一档：它入库，但读音表验证不了，用户该知道有多少条"
    );
    assert_eq!(st.no_boundary, 1);
    assert_eq!(st.unfilled_skipped, 0);
    assert_eq!(kept[0].boundary, None, "层 1 不该被改写");
    assert_eq!(
        kept[1].boundary,
        Some(0b101),
        "求解结果必须落到 boundary 字段"
    );
    assert_eq!(
        kept[4].boundary,
        Some(0b1),
        "★ #97：符号词条要带着解出的单音节边界入库，落 0 会让简拼索引静默失效"
    );
    assert_eq!(
        st.samples,
        vec!["wgkq 工".to_string()],
        "拒收要留样例；★ NoReading 不该混进来——它没被拒"
    );

    // ② 不补充：可补的那三条跳过，而不是原样入库
    let (kept, st) = dispose_contract_rows(rows.clone(), &verdicts, false);
    assert_eq!(kept.len(), 2, "层 1 与 NoInfo 仍入库，可补的三条被跳过");
    assert_eq!(st.unfilled_skipped, 3);
    assert_eq!(st.filled, 0);
    assert_eq!(st.rejected, 1, "拒收与用户的选择无关");
    assert!(
        kept.iter().all(|r| r.text != "安甘"),
        "★ 不补充 ≠ 照原样导入：boundary=0 的拼音词条正是契约要消灭的东西"
    );
}

/// 拼音的默认导出段必须含候选调整，与设置页子标签同增同减。
///
/// 这条跨仓契约没有编译期约束：设置页给了拼音「候选调整」tab、而导出默认不含该段时，
/// 用户在设置页看得见规则，导出的文件里却没有，直到还原时才发现丢了数据。
#[test]
fn pinyin_default_export_sections_include_shadow() {
    let py = default_dict_sections("pinyin");
    assert!(
        py.contains(&DictSection::Shadow),
        "拼音默认导出段须含候选调整，实际: {py:?}"
    );
    // 反向对照：混输只有候选调整这一段，证明本函数确实在按引擎类型分流，
    // 而不是被改成了「一律返回全部段」。
    assert_eq!(
        default_dict_sections("mixed"),
        vec![DictSection::Shadow],
        "混输仍只导出候选调整"
    );
}
