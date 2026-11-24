use super::*;
use crate::context::MemoryContext;
use chrono::TimeZone;

fn ctx_at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> MemoryContext {
    let mut c = MemoryContext::new();
    c.clock = Some(Local.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap());
    c
}

#[test]
fn uuid_default_and_flags() {
    let d = generate_uuid("").unwrap();
    assert_eq!(d.len(), 36);
    assert_eq!(d.matches('-').count(), 4);
    assert_eq!(d, d.to_lowercase());
    // v4 版本位：第 15 个字符恒为 '4'
    assert_eq!(d.chars().nth(14), Some('4'));

    let n = generate_uuid("n").unwrap();
    assert_eq!(n.len(), 32);
    assert!(!n.contains('-'));

    let u = generate_uuid("U").unwrap(); // 标志大小写不敏感
    assert_eq!(u.len(), 36);
    assert_eq!(u, u.to_uppercase());

    let nu = generate_uuid("nu").unwrap();
    assert_eq!(nu.len(), 32);
    assert_eq!(nu, nu.to_uppercase());

    // 每次都是新值
    assert_ne!(generate_uuid("").unwrap(), generate_uuid("").unwrap());

    // 未知标志报错而非静默忽略
    assert!(generate_uuid("x").is_err());
    assert!(generate_uuid("nx").is_err());
}

#[test]
fn uuid_func_takes_optional_flags() {
    let ctx = ctx_at(2026, 6, 14, 9, 5, 7);
    assert_eq!(fn_uuid(&ctx, &[]).unwrap().len(), 36);
    assert_eq!(fn_uuid(&ctx, &["n".into()]).unwrap().len(), 32);
}

#[test]
fn date_format_and_offset() {
    let ctx = ctx_at(2026, 6, 14, 9, 5, 7);
    assert_eq!(fn_date(&ctx, &["YYYY-MM-DD".into()]).unwrap(), "2026-06-14");
    assert_eq!(
        fn_date(&ctx, &["YYYY-MM-DD".into(), "+1d".into()]).unwrap(),
        "2026-06-15"
    );
    assert_eq!(
        fn_date(&ctx, &["YYYY-MM".into(), "-1M".into()]).unwrap(),
        "2026-05"
    );
    assert_eq!(
        fn_date(&ctx, &["YYYY".into(), "+1y".into()]).unwrap(),
        "2027"
    );
}

/// `date()` 的记号与短语模板的同名 `$变量` 同源：同一天两边必须给出同一个答案。
#[test]
fn date_tokens_cover_template_vars() {
    // 2026-06-14 周日；农历丙午年四月廿九
    let ctx = ctx_at(2026, 6, 14, 9, 5, 7);
    let f = |fmt: &str| fn_date(&ctx, &[fmt.into()]).unwrap();
    assert_eq!(f("YC年MC月DC日"), "二〇二六年六月十四日");
    assert_eq!(f("星期WC"), "星期日");
    assert_eq!(f("LY年LMD"), "丙午年四月廿九");
    assert_eq!(f("LM|LD|LZ|LYN"), "四月|廿九|马|2026");
    // 非节日 LF 为空串，整条照常出
    assert_eq!(f("LMDLF"), "四月廿九");
    // 端午当天追加节日名
    let d = ctx_at(2026, 6, 19, 0, 0, 0);
    assert_eq!(fn_date(&d, &["LMDLF".into()]).unwrap(), "五月初五端午节");
    // 时间戳
    let t = ctx.now();
    assert_eq!(f("ts"), t.timestamp().to_string());
    assert_eq!(f("tsms"), t.timestamp_millis().to_string());
}

/// 农历记号与偏移联用：昨天 / 明天的农历。这是 `$变量` 做不到的组合。
#[test]
fn date_lunar_follows_offset() {
    let ctx = ctx_at(2026, 6, 18, 0, 0, 0);
    assert_eq!(
        fn_date(&ctx, &["LMDLF".into(), "+1d".into()]).unwrap(),
        "五月初五端午节"
    );
    assert_eq!(
        fn_date(&ctx, &["MC月DC日 星期WC".into(), "-1d".into()]).unwrap(),
        "六月十七日 星期三"
    );
}

/// 农历换算不出（超出 1900–2100）→ 报错整条不出，不能只剩「农历」二字。
#[test]
fn date_lunar_out_of_range_is_error() {
    let ctx = ctx_at(2150, 1, 1, 0, 0, 0);
    assert!(fn_date(&ctx, &["农历LMD".into()]).is_err());
    // 不含农历记号的格式不受影响
    assert_eq!(fn_date(&ctx, &["YYYY".into()]).unwrap(), "2150");
}

/// 非法格式符不能 panic（此前 `to_string()` 会）。
#[test]
fn date_bad_percent_is_error_not_panic() {
    let ctx = ctx_at(2026, 6, 14, 0, 0, 0);
    assert!(fn_date(&ctx, &["100%".into()]).is_err());
}

#[test]
fn time_and_now() {
    let ctx = ctx_at(2026, 6, 14, 9, 5, 7);
    assert_eq!(fn_time(&ctx, &["HH:mm:ss".into()]).unwrap(), "09:05:07");
    assert_eq!(fn_now(&ctx, &[]).unwrap(), "2026-06-14 09:05:07");
}

#[test]
fn code_tail_clip() {
    let mut ctx = MemoryContext::new().with_input("nihao");
    ctx.clip = "板".into();
    assert_eq!(fn_code(&ctx, &[]).unwrap(), "nihao");
    assert_eq!(fn_code(&ctx, &["3".into()]).unwrap(), "hao");
    assert_eq!(
        fn_tail(&ctx, &["abcde".into(), "2".into()]).unwrap(),
        "bcde"
    );
    assert_eq!(fn_clip(&ctx, &[]).unwrap(), "板");
}

#[test]
fn invalid_offset_errors() {
    let ctx = ctx_at(2026, 6, 14, 0, 0, 0);
    assert!(fn_date(&ctx, &["YYYY".into(), "1d".into()]).is_err());
}
