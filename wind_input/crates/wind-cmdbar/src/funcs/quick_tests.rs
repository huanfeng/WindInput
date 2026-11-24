use super::*;
use crate::context::MemoryContext;
use std::collections::HashMap;

/// 带 quick_var 的测试上下文：直接喂一张名→值的表。
struct QuickTestCtx {
    inner: MemoryContext,
    vars: HashMap<&'static str, &'static str>,
}

impl EvalContext for QuickTestCtx {
    fn input(&self) -> String {
        self.inner.input()
    }
    fn last(&self, n: i64) -> String {
        self.inner.last(n)
    }
    fn clip(&self, n: i64) -> String {
        self.inner.clip(n)
    }
    fn sel(&self) -> String {
        self.inner.sel()
    }
    fn app(&self) -> String {
        self.inner.app()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn env(&self, name: &str) -> String {
        self.inner.env(name)
    }
    fn reverse_lookup(&self, text: &str, format: &str, min_len: usize) -> String {
        self.inner.reverse_lookup(text, format, min_len)
    }
    fn now(&self) -> chrono::DateTime<chrono::Local> {
        self.inner.now()
    }
    fn services(&self) -> Option<&crate::services::Services> {
        self.inner.services()
    }
    fn quick_var(&self, name: &str) -> Option<String> {
        self.vars.get(name).map(|s| s.to_string())
    }
}

fn ctx() -> QuickTestCtx {
    QuickTestCtx {
        inner: MemoryContext::new(),
        vars: HashMap::from([
            ("Y", "2025"),
            ("YC", "二〇二五"),
            ("M", "6"),
            ("MC", "六"),
            ("D", "5"),
            ("DC", "五"),
            ("N", "1234567.89"),
            ("CNL", "一百二十三"),
            ("CNU", "壹佰贰拾叁"),
            ("DIG", "一二三"),
            ("THOU", "1,234,567.89"),
            ("AMT", "壹佰贰拾叁元整"),
            ("EXPR", "1+2*3"),
            ("RESULT", "7"),
            ("EXACT", "7"),
            // 农历（2026-06-19 端午当天的取值）
            ("LMD", "五月初五"),
            ("LM", "五月"),
            ("LD", "初五"),
            ("LY", "丙午"),
            ("LYN", "2026"),
            ("LZ", "马"),
            ("LF", "端午节"),
        ]),
    }
}

/// 农历取不到值的上下文（超出 1900–2100 / 非节日 / 根本不在日期上下文）。
fn ctx_without_lunar() -> QuickTestCtx {
    let mut c = ctx();
    for k in ["LMD", "LM", "LD", "LY", "LYN", "LZ", "LF"] {
        c.vars.remove(k);
    }
    c
}

fn nm(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn lunar_parts() {
    let c = ctx();
    assert_eq!(fn_lunar(&c, &[]).unwrap(), "五月初五");
    let part = |p: &str| fn_lunar_named(&c, &[], &nm(&[("part", p)])).unwrap();
    assert_eq!(part("md"), "五月初五");
    assert_eq!(part("month"), "五月");
    assert_eq!(part("day"), "初五");
    assert_eq!(part("ganzhi"), "丙午");
    assert_eq!(part("zodiac"), "马");
    assert_eq!(part("year"), "2026");
    assert_eq!(part("festival"), "端午节");
    assert_eq!(part("full"), "丙午年五月初五");
    // 无参 ≡ part='md'
    assert_eq!(fn_lunar(&c, &[]).unwrap(), part("md"));
}

#[test]
fn lunar_rejects_unknown_part() {
    let c = ctx();
    let e = fn_lunar_named(&c, &[], &nm(&[("part", "nope")]));
    assert!(e.is_err(), "未知 part 必须报错而不是给空串");
}

/// 取不到农历时报错而非返回空串：空串会让 `农历{lunar()}` 把「农历」二字单独上屏。
#[test]
fn lunar_errors_instead_of_returning_empty() {
    let c = ctx_without_lunar();
    assert!(fn_lunar(&c, &[]).is_err());
    for p in [
        "md", "month", "day", "ganzhi", "zodiac", "year", "festival", "full",
    ] {
        assert!(
            fn_lunar_named(&c, &[], &nm(&[("part", p)])).is_err(),
            "part={p} 取不到值时应报错"
        );
    }
    // 对照：公历函数在同一上下文里仍给空串（既有约定）
    assert_eq!(fn_year(&c, &[]).unwrap(), "2025");
}

/// 无参调用必须与对应的 `$` 变量逐字相同——两条路径同源是整个设计的前提。
#[test]
fn bare_calls_equal_factory_variables() {
    let c = ctx();
    assert_eq!(fn_year(&c, &[]).unwrap(), "2025");
    assert_eq!(fn_month(&c, &[]).unwrap(), "6");
    assert_eq!(fn_day(&c, &[]).unwrap(), "5");
    assert_eq!(fn_cn(&c, &[]).unwrap(), "一百二十三");
    assert_eq!(fn_dig(&c, &[]).unwrap(), "一二三");
    assert_eq!(fn_thou(&c, &[]).unwrap(), "1,234,567.89");
    assert_eq!(fn_amt(&c, &[]).unwrap(), "壹佰贰拾叁元整");
    assert_eq!(fn_result(&c, &[]).unwrap(), "7");
}

#[test]
fn pad_and_cn_on_date_parts() {
    let c = ctx();
    assert_eq!(fn_month_named(&c, &[], &nm(&[("pad", "2")])).unwrap(), "06");
    assert_eq!(fn_day_named(&c, &[], &nm(&[("pad", "3")])).unwrap(), "005");
    assert_eq!(
        fn_year_named(&c, &[], &nm(&[("cn", "true")])).unwrap(),
        "二〇二五"
    );
    // cn 胜过 pad：给「六」补零没有意义
    assert_eq!(
        fn_month_named(&c, &[], &nm(&[("cn", "1"), ("pad", "2")])).unwrap(),
        "六"
    );
    // 已够宽则不动
    assert_eq!(
        fn_year_named(&c, &[], &nm(&[("pad", "2")])).unwrap(),
        "2025"
    );
}

#[test]
fn thousands_separator_and_group() {
    let c = ctx();
    assert_eq!(
        fn_thou_named(&c, &[], &nm(&[("sep", " ")])).unwrap(),
        "1 234 567.89"
    );
    // group≠3 必须从原数重切，不能在成品串上替换
    assert_eq!(
        fn_thou_named(&c, &[], &nm(&[("group", "4")])).unwrap(),
        "123,4567.89"
    );
    assert_eq!(
        fn_thou_named(&c, &[], &nm(&[("group", "4"), ("sep", "'")])).unwrap(),
        "123'4567.89"
    );
}

#[test]
fn amount_unit_and_zheng() {
    let c = ctx();
    assert_eq!(
        fn_amt_named(&c, &[], &nm(&[("unit", "圆")])).unwrap(),
        "壹佰贰拾叁圆整"
    );
    assert_eq!(
        fn_amt_named(&c, &[], &nm(&[("zheng", "false")])).unwrap(),
        "壹佰贰拾叁元"
    );
    assert_eq!(
        fn_amt_named(&c, &[], &nm(&[("zheng", "0"), ("unit", "圆")])).unwrap(),
        "壹佰贰拾叁圆"
    );
}

/// 「本次输入无金额写法」时任何参数都不得把空串变成非空。
#[test]
fn amount_stays_empty_when_not_applicable() {
    let mut c = ctx();
    c.vars.insert("AMT", "");
    assert_eq!(
        fn_amt_named(&c, &[], &nm(&[("unit", "圆"), ("zheng", "false")])).unwrap(),
        ""
    );
}

/// 写错的开关值必须报错，不能静默当 false——否则用户以为功能坏了。
#[test]
fn bad_bool_is_an_error_not_a_silent_false() {
    let c = ctx();
    assert!(fn_amt_named(&c, &[], &nm(&[("zheng", "nope")])).is_err());
    assert!(fn_month_named(&c, &[], &nm(&[("cn", "")])).is_err());
    assert!(fn_month_named(&c, &[], &nm(&[("pad", "x")])).is_err());
}

/// 不在快捷输入上下文（短语/命令栏）时一律空串，而不是拿当前编码硬解。
#[test]
fn outside_quick_context_yields_empty() {
    let c = MemoryContext::new().with_input("2025.12.25");
    assert_eq!(fn_year(&c, &[]).unwrap(), "");
    assert_eq!(fn_amt(&c, &[]).unwrap(), "");
    assert_eq!(fn_raw(&c, &[]).unwrap(), "");
    assert_eq!(fn_pct(&c, &[]).unwrap(), "");
}

/// `pct()` 默认 ×100 保两位小数并去尾零；具名参数可换成千分比/基点等任意倍数与后缀。
///
/// 换算基于 `$EXACT` 而非 `$RESULT`；`ctx()` 里二者都是 "7"，这里换成 1/3 才能验出不二次舍入。
#[test]
fn percent_default_and_custom() {
    let mut c = ctx();
    c.vars.insert("EXACT", "0.3333333333333333");
    assert_eq!(
        fn_pct(&c, &[]).unwrap(),
        "33.33%",
        "默认 scale=100 decimals=2"
    );
    assert_eq!(
        fn_pct_named(
            &c,
            &[],
            &nm(&[("scale", "1000"), ("decimals", "1"), ("suffix", "‰")])
        )
        .unwrap(),
        "333.3‰",
        "换个倍数与后缀即千分比，不必另开函数"
    );
    // decimals 是上限，整除的结果要去尾零，不能定死两位
    c.vars.insert("EXACT", "0.5");
    assert_eq!(fn_pct(&c, &[]).unwrap(), "50%");
}

/// 写错的 scale/decimals 必须报错，不能静默当默认值——否则用户以为参数生效了。
#[test]
fn percent_rejects_bad_named_args() {
    let c = ctx();
    assert!(fn_pct_named(&c, &[], &nm(&[("scale", "abc")])).is_err());
    assert!(fn_pct_named(&c, &[], &nm(&[("decimals", "-1")])).is_err());
}

/// 宽度/位数类参数必须有上界，否则一条模板就能让求值线程卡在分配上。
/// 上限值放行、多一个拒绝，两端都测。
#[test]
fn width_args_are_bounded() {
    let c = ctx();
    for (func, key) in [("pct", "decimals"), ("pad", "pad"), ("thou", "group")] {
        assert!(
            parse_width(func, key, &MAX_WIDTH_ARG.to_string()).is_ok(),
            "{func}({key}) 恰好等于上限应放行"
        );
        let over = (MAX_WIDTH_ARG + 1).to_string();
        let err = parse_width(func, key, &over).expect_err("超上限应报错");
        assert!(
            err.to_string().contains(key),
            "错误信息要点名是哪个参数：{err}"
        );
    }
    // 走一遍真实调用链，证明拦截确实接在函数上而不只是那个 helper 里。
    assert!(
        fn_pct_named(&c, &[], &nm(&[("decimals", "99999999999")])).is_err(),
        "pct 的 decimals 超限必须报错，而不是去分配一个天文数字长的字符串"
    );
}

/// scale 写得过大时换算会溢出成 inf——不能照着 format 出去。
/// `{:.2}` 对 inf 给的是字面 "inf"，拼上后缀就是 "inf%" 这么一条谁也用不上的候选。
#[test]
fn percent_rejects_non_finite_result() {
    let mut c = ctx();
    c.vars.insert("EXACT", "1e300");
    assert!(fn_pct_named(&c, &[], &nm(&[("scale", "1e300")])).is_err());
    // 反向对照：乘出来仍是有限值的大数照常出结果。按形状断言，因为 `{:.*}` 打印 f64
    // 精确值，1e300 展开后半截是尾数噪声。
    let big = fn_pct_named(&c, &[], &nm(&[("scale", "1")])).unwrap();
    assert!(
        big.starts_with('1') && big.ends_with('%') && big.len() > 300,
        "大但有限 → 照常展开，实得 {} 字符",
        big.len()
    );
}

/// quick 函数名不得与既有内置函数重名：`Registry::register` 同名静默覆盖（如 `num` 已是进制转换）。
#[test]
fn no_name_clash_with_builtins() {
    let builtins = crate::registry::Registry::full();
    for s in specs() {
        assert!(
            builtins.lookup(s.name).is_none(),
            "quick 函数 {:?} 与既有内置函数重名，请改名",
            s.name
        );
    }
}

/// 白名单与求值入口成对声明（registry.rs 的同名检查只遍历 `full()`，覆盖不到 quick）。
#[test]
fn named_params_and_eval_named_are_declared_together() {
    for s in specs() {
        assert_eq!(
            s.named_params.is_empty(),
            s.eval_named.is_none(),
            "{}: named_params 与 eval_named 必须同时声明或同时省略",
            s.name
        );
    }
}
