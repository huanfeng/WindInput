use super::*;

fn hints(text: &str) -> Vec<Hint> {
    lint_phrase(text).expect("解析应成功")
}

fn q() -> char {
    char::from_u32(34).unwrap()
}

fn bs() -> char {
    char::from_u32(92).unwrap()
}

#[test]
fn plain_text_has_no_hints() {
    assert!(hints("(＾▽＾)").is_empty());
}

/// 字符串里提到函数名不等于调用它——文本匹配式判定会在这条上误报。
#[test]
fn function_name_inside_literal_is_not_a_call() {
    assert!(hints("用 proc.run 可以启动程序").is_empty());
}

/// ★ 双写（约定写法）不该报提示。
#[test]
fn double_backslash_path_has_no_hint() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!(
        "$CC({q}x{q}, proc.run({q}D:{bs}{bs}notes{bs}{bs}a.exe{q}))"
    ));
    assert!(r.is_empty(), "实得 {r:?}");
}

/// ★ 单写时 lexer 把反斜杠紧跟 n 当成换行吃掉，路径里就出现了控制字符。
#[test]
fn single_backslash_path_is_hinted() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!(
        "$CC({q}x{q}, proc.run({q}D:{bs}notes{bs}a.exe{q}))"
    ));
    assert_eq!(r, vec![Hint::ControlCharInPath("proc.run".into())]);
}

/// `proc.any` 的候选同样是程序名 / 路径，单写反斜杠照样该提示。
#[test]
fn proc_any_path_is_checked_too() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!(
        "$CC({q}x{q}, proc.any({q}D:{bs}tools{q}, {q}kcalc{q}))"
    ));
    assert_eq!(r, vec![Hint::ControlCharInPath("proc.any".into())]);
}

#[test]
fn bad_cwd_is_hinted() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!(
        "$CC({q}x{q}, proc.run({q}a.exe{q}, cwd={q}D:{bs}tools{q}))"
    ));
    assert_eq!(r, vec![Hint::ControlCharInPath("proc.run".into())]);
}

/// 输入文本类函数的换行是正常用法（`key.type` 会把它发成回车键）。
#[test]
fn newline_in_typed_text_is_not_hinted() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!("$CC({q}x{q}, key.type({q}第一行{bs}n第二行{q}))"));
    assert!(r.is_empty(), "实得 {r:?}");
}

/// 中文目录名单写恰好能用（反斜杠紧跟中文是未知转义、原样保留）——这正是
/// 「以为单写没问题」的来源。本判据对它不报，是刻意的：没有控制字符就没有
/// 确定证据，宁可漏报也不误报。
#[test]
fn cjk_dir_name_single_backslash_is_not_hinted() {
    let (q, bs) = (q(), bs());
    assert!(hints(&format!("$CC({q}x{q}, open({q}D:{bs}我的文档{q}))")).is_empty());
}

/// 命令短语本身不再被当成可疑内容——只要路径写对，一条 proc.shell 也是干净的。
#[test]
fn command_phrases_are_not_flagged_by_themselves() {
    let q = q();
    assert!(hints(&format!("$CC({q}跑{q}, proc.shell({q}echo hi{q}))")).is_empty());
    assert!(
        hints(&format!(
            "$CC({q}管理员{q}, proc.run({q}x.exe{q}, verb={q}runas{q}))"
        ))
        .is_empty()
    );
    assert!(hints(&format!("$CC({q}输入{q}, key.type({q}abc{q}))")).is_empty());
}

#[test]
fn syntax_error_is_reported_not_swallowed() {
    let q = q();
    assert!(lint_phrase(&format!("$CC({q}未闭合")).is_err());
}

#[test]
fn array_phrase_elements_are_walked() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!(
        "$SS({q}组{q}, {q}纯文本{q}, $CC({q}跑{q}, proc.run({q}D:{bs}notes{q})))"
    ));
    assert_eq!(r, vec![Hint::ControlCharInPath("proc.run".into())]);
}

/// 同一函数多处出问题只报一次，别把一行提示刷成三行。
#[test]
fn duplicate_hints_are_deduped() {
    let (q, bs) = (q(), bs());
    let r = hints(&format!(
        "$CC({q}x{q}, proc.run({q}D:{bs}notes{q}), proc.run({q}E:{bs}tools{q}))"
    ));
    assert_eq!(r.len(), 1);
}
