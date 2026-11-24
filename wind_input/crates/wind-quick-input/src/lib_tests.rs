use super::*;

// ── 成员 id ──

#[test]
fn test_member_ids() {
    assert_eq!(
        QuickSource::from_member(MEMBER_DATE),
        Some(QuickSource::Date)
    );
    assert_eq!(
        QuickSource::from_member(MEMBER_CALC),
        Some(QuickSource::Calc)
    );
    assert_eq!(
        QuickSource::from_member(MEMBER_NUMBER),
        Some(QuickSource::Number)
    );
    // repeat 由协调器实现，旧值应先展开
    assert_eq!(QuickSource::from_member(MEMBER_REPEAT), None);
    assert_eq!(QuickSource::from_member(MEMBER_LEGACY), None);
    assert_eq!(QuickSource::from_member("pinyin"), None);
    // 家族判定覆盖 repeat 与旧值，不误伤真实方案
    assert!(is_quick_member(MEMBER_LEGACY));
    assert!(is_quick_member(MEMBER_REPEAT));
    assert!(is_quick_member(MEMBER_DATE));
    assert!(!is_quick_member("pinyin"));
    assert!(!is_quick_member("english"));
}

// ── 计算 ──

#[test]
fn test_calc_result_is_first_candidate() {
    // 结果首候选，等式次之（使用算式形态的是少数）
    let c = generate_calc_candidates("1+2*3", 6);
    assert_eq!(c[0], "7");
    assert_eq!(c[1], "1+2*3=7");
}

#[test]
fn test_calc_parentheses() {
    assert_eq!(evaluate_expression("(1+2)*3"), Some(9.0));
    assert_eq!(evaluate_expression("2*(3+4)-1"), Some(13.0));
}

#[test]
fn test_calc_power_precedence_and_associativity() {
    // 幂高于乘除
    assert_eq!(evaluate_expression("2*3^2"), Some(18.0));
    assert_eq!(evaluate_expression("3^2+1"), Some(10.0));
    // 右结合：2^(3^2) = 512，而非 (2^3)^2 = 64
    assert_eq!(evaluate_expression("2^3^2"), Some(512.0));
    // 括号仍可改写结合
    assert_eq!(evaluate_expression("(2^3)^2"), Some(64.0));
    let c = generate_calc_candidates("5^2", 6);
    assert_eq!(c[0], "25");
    assert_eq!(c[1], "5^2=25");
}

#[test]
fn test_calc_unary_sign() {
    // 一元号低于幂：-2^2 = -(2^2)
    assert_eq!(evaluate_expression("-2^2"), Some(-4.0));
    // 指数侧接受一元号
    assert_eq!(evaluate_expression("2^-1"), Some(0.5));
    assert_eq!(evaluate_expression("-5+3"), Some(-2.0));
    // 首负号的算式产出候选
    let c = generate_calc_candidates("-5+3", 6);
    assert_eq!(c[0], "-2");
    // 纯负数不是算式（无二元运算符），交给数字来源
    assert!(generate_calc_candidates("-5", 6).is_empty());
}

#[test]
fn test_calc_division_and_trailing_op() {
    // 尾部运算符应被裁剪
    let c = generate_calc_candidates("10/4+", 6);
    assert_eq!(c[0], "2.5");
    assert_eq!(c[1], "10/4=2.5");
}

#[test]
fn test_calc_division_by_zero_no_candidates() {
    assert!(generate_calc_candidates("1/0", 6).is_empty());
    // 0 的负幂 = inf，同样无候选
    assert!(generate_calc_candidates("0^-1", 6).is_empty());
}

#[test]
fn test_calc_rejects_non_expression() {
    assert!(generate_calc_candidates("123", 6).is_empty()); // 无运算符
    assert!(generate_calc_candidates("abc", 6).is_empty());
    assert!(generate_calc_candidates("2025.12.25", 6).is_empty()); // 日期不是算式
}

#[test]
fn test_calc_keeps_result_through_equals() {
    // 用户按 = 写出完整等式：候选维持不清空。
    assert_eq!(generate_calc_candidates("123+100", 6)[1], "123+100=223");
    assert_eq!(generate_calc_candidates("123+100=", 6)[1], "123+100=223");
    // 续打答案也维持（取 = 前的表达式求值）。
    assert_eq!(generate_calc_candidates("123+100=223", 6)[1], "123+100=223");
}

#[test]
fn test_trailing_operator_matches_prefix() {
    // "123+" 的候选与 "123" 一致（不中断）。
    assert_eq!(
        generate_quick_input_candidates("123+", 6),
        generate_quick_input_candidates("123", 6)
    );
    // "1+2*" 的候选与 "1+2" 一致。
    assert_eq!(
        generate_quick_input_candidates("1+2*", 6),
        generate_quick_input_candidates("1+2", 6)
    );
}

// ── 日期 ──

#[test]
fn test_date_full_formats() {
    let c = generate_date_candidates("2025.12.25");
    // 公历五条的内容与次序（农历追加在其后，由 tests/factory_table.rs 把关）
    assert_eq!(
        c[..5],
        [
            "2025年12月25日",
            "二〇二五年十二月二十五日",
            "2025-12-25",
            "20251225",
            "2025/12/25"
        ],
        "中文优先，全汉字次之，且不含补零的中文写法"
    );
    assert_eq!(c.len(), 7, "外加农历两条");
}

#[test]
fn test_small_int_to_chinese() {
    // 短语层 ${MC}/${DC} 迁来的用例：口径必须与迁移前逐字一致
    assert_eq!(small_int_to_chinese(6), "六");
    assert_eq!(small_int_to_chinese(10), "十");
    assert_eq!(small_int_to_chinese(12), "十二");
    assert_eq!(small_int_to_chinese(20), "二十");
    assert_eq!(small_int_to_chinese(25), "二十五");
    assert_eq!(small_int_to_chinese(31), "三十一");
    assert_eq!(small_int_to_chinese(0), "〇");
    // 越界不 panic，回退逐位
    assert_eq!(small_int_to_chinese(100), "一〇〇");
    assert_eq!(year_to_chinese(2026), "二〇二六");
}

#[test]
fn test_date_all_chinese_form() {
    // 年份逐位（0 → 〇），月日位值读法且十位不写「一」
    let c = generate_date_candidates("2005.10.1");
    assert!(
        c.contains(&"二〇〇五年十月一日".to_string()),
        "实际: {:?}",
        c
    );
    assert!(!c.iter().any(|s| s.contains("二零")), "年份不得用「零」");
    assert!(
        !c.iter().any(|s| s.contains("一十")),
        "月日十位不写「一」，实际: {:?}",
        c
    );
}

#[test]
fn test_date_no_padded_chinese_form() {
    // 中文日期不加前导零（GB/T 15835），补零写法不再产出
    let c = generate_date_candidates("2025.3.5");
    assert!(c.contains(&"2025年3月5日".to_string()));
    assert!(!c.contains(&"2025年03月05日".to_string()));
    // 数字格式仍补零（ISO 8601）
    assert!(c.contains(&"2025-03-05".to_string()));
}

#[test]
fn test_date_month_day_uses_current_year() {
    let c = generate_date_candidates("12.25.");
    let year = chrono::Local::now().year();
    assert!(c.iter().any(|s| s == &format!("{}年12月25日", year)));
}

#[test]
fn test_date_invalid() {
    // 带第二个点，确保拦下它的是「月/日越界」而不是归属判据——写 `13.40`
    // 的话这条会被闸门提前挡掉，测的就不再是解析。
    assert!(generate_date_candidates("13.40.").is_empty());
    assert!(generate_date_candidates("abc").is_empty());
}

/// 归属判据：一个点归数字、两个点归日期，两组互斥。正反两向各要一例，
/// 只断言「一个点不出日期」的话 `render_date` 返回空也能全绿。
#[test]
fn test_date_requires_second_dot() {
    // 一个点：日期整组让开
    assert!(
        generate_date_candidates("2026.2").is_empty(),
        "实际: {:?}",
        generate_date_candidates("2026.2")
    );
    assert!(generate_date_candidates("12.25").is_empty());
    // 两个点：日期照常，且与旧的「一个点」形态逐条等价
    assert_eq!(
        generate_date_candidates("2026.2."),
        vec!["2026年2月", "二〇二六年二月", "2026-02", "2026/02"]
    );
    assert_eq!(generate_date_candidates("12.25.")[0], "12月25日");
    // 三段完整日期天然带两个点，用户无需多做任何事
    assert_eq!(generate_date_candidates("2025.12.25")[0], "2025年12月25日");
    // 判据看的是裁剪前的串：尾点若先被 `trim_pending_tail` 吃掉，
    // `2026.2.` 会退回一个点而被自己挡住。
    assert!(!generate_date_candidates("2026.2.").is_empty());
}

#[test]
fn test_year_month() {
    let c = generate_year_month_candidates("2025.6");
    assert_eq!(c, vec!["2025年6月", "二〇二五年六月", "2025-06", "2025/06"]);
}

#[test]
fn test_year_month_survives_trailing_dot() {
    // 「2026.3.」输入到一半：第三段为空不得让候选全空，仍应给出年月。
    // 尾点在这里担着两个身份——归属判据靠它认出「我要日期」，
    // `trim_pending_tail` 又要把它当「小数位还没打」裁掉。两者次序错了这条就红。
    let c = generate_date_candidates("2026.3.");
    assert_eq!(c, vec!["2026年3月", "二〇二六年三月", "2026-03", "2026/03"]);
    // 第三段打出来后候选不变（尾点只是过渡态）
    assert_eq!(generate_date_candidates("2026.3.")[0], "2026年3月");
    // 完整日期的尾点同理被裁，不影响已成立的两点归属
    assert_eq!(
        generate_date_candidates("2025.12.25."),
        generate_date_candidates("2025.12.25")
    );
}

// ── 数字 / 金额 ──

#[test]
fn test_number_integer_candidates() {
    let c = generate_number_candidates("123", 6);
    assert_eq!(
        c,
        vec![
            "壹佰贰拾叁元整",
            "一百二十三",
            "壹佰贰拾叁",
            "一二三",
            "123"
        ]
    );
    // 不规范/无场景的两条已移除
    assert!(!c.contains(&"一百二十三元整".to_string()));
    assert!(!c.contains(&"壹贰叁".to_string()));
}

#[test]
fn test_number_thousands() {
    let c = generate_number_candidates("1234567", 6);
    assert!(
        c.contains(&"1,234,567".to_string()),
        "千分位，实际: {:?}",
        c
    );
    assert!(
        c.contains(&"一百二十三万四千五百六十七".to_string()),
        "中文大数，实际: {:?}",
        c
    );
}

#[test]
fn test_number_decimal_amount() {
    let c = generate_number_candidates("123.45", 6);
    assert_eq!(
        c,
        vec![
            "壹佰贰拾叁元肆角伍分",
            "一百二十三点四五",
            "壹佰贰拾叁点肆伍",
            "一二三点四五",
            "123.45"
        ]
    );
}

#[test]
fn test_number_decimal_thousands() {
    // 小数也给千分位（整数部分分组，小数部分不分组）
    let c = generate_number_candidates("1234567.89", 6);
    assert!(
        c.contains(&"1,234,567.89".to_string()),
        "小数千分位，实际: {:?}",
        c
    );
}

#[test]
fn test_number_amount_zheng_rules() {
    // 到元写整、到角写整、到分不写整（《会计基础工作规范》第五十二条）
    assert_eq!(generate_number_candidates("100", 6)[0], "壹佰元整");
    assert_eq!(generate_number_candidates("100.5", 6)[0], "壹佰元伍角整");
    assert_eq!(generate_number_candidates("100.56", 6)[0], "壹佰元伍角陆分");
    assert_eq!(generate_number_candidates("100.06", 6)[0], "壹佰元零陆分");
}

#[test]
fn test_number_rejected_after_second_dot() {
    // 打日期到第三段（`2026.2.`）时不该再出金额：尾点裁掉后剩下的 `2026.2` 是合法小数，
    // 但用户显然正在打 `2026.2.3`。
    assert!(
        generate_number_candidates("2026.2.", 6).is_empty(),
        "实际: {:?}",
        generate_number_candidates("2026.2.", 6)
    );
    assert!(generate_number_candidates("12.25.", 6).is_empty());
    // 反向对照：判据只能是「点的个数」，不能是「首段像不像年份」——
    // `5000.5`/`2026.2` 与年月形态同构，按首段砍会砍掉常见金额。
    assert_eq!(generate_number_candidates("5000.5", 6)[0], "伍仟元伍角整");
    assert_eq!(
        generate_number_candidates("2026.2", 6)[0],
        "贰仟零贰拾陆元贰角整"
    );
    // 单个尾点仍是「小数位还没打」，与无尾点同解（不能连这条一起收掉）
    assert_eq!(
        generate_number_candidates("123.", 6),
        generate_number_candidates("123", 6)
    );
    // 日期一侧不受影响：中途照常给年月候选
    assert_eq!(generate_date_candidates("2026.2.").len(), 4);
}

#[test]
fn test_number_from_calc_result() {
    // 算完顺手要金额：表达式先求值再转
    let c = generate_number_candidates("123*4", 6);
    assert_eq!(c[0], "肆佰玖拾贰元整", "实际: {:?}", c);
    assert!(c.contains(&"四百九十二".to_string()));
    // 负结果无金额读法
    assert!(generate_number_candidates("1-5", 6).is_empty());
}

#[test]
fn test_number_with_zeros() {
    // 连续零合并
    assert_eq!(
        number_to_chinese("10001", &LOWER_DIGITS, &LOWER_UNITS),
        "一万零一"
    );
    assert_eq!(
        number_to_chinese("100", &LOWER_DIGITS, &LOWER_UNITS),
        "一百"
    );
}

#[test]
fn test_zero_char_only_in_per_digit_reading() {
    // 逐位改写用「〇」（GB/T 15835）
    assert_eq!(digits_to_chinese_chars("2026"), "二〇二六");
    assert_eq!(digits_to_chinese_chars("100"), "一〇〇");
    assert_eq!(digits_to_chinese_chars("0"), "〇");

    // 反向对照：位值读法与财务大写仍必须是「零」，两者不可混用
    let c = generate_number_candidates("10001", 6);
    assert!(c.contains(&"一万零一".to_string()), "实际: {:?}", c);
    assert!(c.contains(&"壹万零壹元整".to_string()), "实际: {:?}", c);
    assert!(c.contains(&"一〇〇〇一".to_string()), "实际: {:?}", c);
    // 小数读法的 0 同属位值语义（「一点零五」），不受影响
    let d = generate_number_candidates("1.05", 6);
    assert!(d.contains(&"一点零五".to_string()), "实际: {:?}", d);
    assert!(d.contains(&"壹元零伍分".to_string()), "实际: {:?}", d);
}

// ── 合并入口 ──

#[test]
fn test_merge_calc_first_then_number() {
    // 3*3：计算结果 9 首选，等式次之，随后是结果的金额读法
    let c = generate_quick_input_candidates("3*3", 6);
    assert_eq!(c[0], "9");
    assert_eq!(c[1], "3*3=9");
    assert!(c.contains(&"玖元整".to_string()), "实际: {:?}", c);
}

#[test]
fn test_pure_number_via_merge() {
    // 纯整数经合并入口产出金额候选
    let c = generate_quick_input_candidates("123", 6);
    assert_eq!(c[0], "壹佰贰拾叁元整");
    assert!(c.contains(&"一百二十三".to_string()));
}

#[test]
fn test_date_and_number_are_mutually_exclusive() {
    // "12.25" 既是金额也是月日，一个点是真歧义：归数字独占，日期整组让开。
    let c = generate_quick_input_candidates("12.25", 6);
    assert_eq!(c[0], "壹拾贰元贰角伍分");
    assert!(
        !c.iter().any(|s| s.contains('月')),
        "一个点不该出日期，实际: {:?}",
        c
    );
    // 想要日期就多打一个点：数字整组被隔离，日期独占候选面且首选是短月日
    let d = generate_quick_input_candidates("12.25.", 6);
    assert_eq!(d[0], "12月25日");
    assert!(
        !d.iter().any(|s| s.contains('元')),
        "第二个点应隔离数字，实际: {:?}",
        d
    );
    // 年月一侧同构：`2026.2` 归金额，`2026.2.` 归年月
    let e = generate_quick_input_candidates("2026.2", 6);
    assert_eq!(e[0], "贰仟零贰拾陆元贰角整");
    assert!(!e.iter().any(|s| s.contains('月')), "实际: {:?}", e);
    assert_eq!(
        generate_quick_input_candidates("2026.2.", 6)[0],
        "2026年2月"
    );
}

#[test]
fn test_month_day_forms_prefer_short_writing() {
    // 只打两段：不带年的短写法在前，补年的两条留作次选（用户没打年份，首选不替他补）
    let c = generate_date_candidates("12.25.");
    let year = chrono::Local::now().year();
    assert_eq!(
        c[..4],
        ["12月25日", "十二月二十五日", "12-25", "12/25"],
        "实际: {:?}",
        c
    );
    assert!(c.contains(&format!("{}年12月25日", year)), "实际: {:?}", c);
    assert!(c.contains(&format!("{}-12-25", year)), "实际: {:?}", c);
    // 三段输入不受影响：年份是自己打的，仍走 date 类且首选带年
    assert_eq!(generate_date_candidates("2025.12.25")[0], "2025年12月25日");
    // 月日类不产出 date 类那两条长写法（`20251225` / `2025/12/25`）。
    // 期望值按当前年拼出——写死 "20261225" 的话跨年后这条断言恒真，形同没写。
    assert!(
        !c.iter()
            .any(|s| s == &format!("{}1225", year) || s == &format!("{}/12/25", year)),
        "实际: {:?}",
        c
    );
}
