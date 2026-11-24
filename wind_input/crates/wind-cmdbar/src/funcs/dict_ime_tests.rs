use super::*;
use crate::context::MemoryContext;
use crate::services::{ImeController, Services};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecIme(Mutex<Vec<String>>);
impl ImeController for RecIme {
    fn toggle(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn open_setting(&self, _: &str, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn open_setting_web(&self, _: &str, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn set_schema(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn theme_cycle(&self, _: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn pair(&self, left: &str, right: &str, jump_steps: u32) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("{left}|{right}|{jump_steps}"));
        Ok(())
    }
    fn set(&self, target: &str, on: bool) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(format!("set {target}={on}"));
        Ok(())
    }
    fn set_value(&self, target: &str, value: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("set_value {target}={value}"));
        Ok(())
    }
    fn reload_config(&self) -> anyhow::Result<()> {
        self.0.lock().unwrap().push("reload".into());
        Ok(())
    }
}

fn ctx_with(rec: Arc<RecIme>) -> MemoryContext {
    let mut svc = Services::new();
    svc.ime = Some(rec);
    MemoryContext::new().with_services(svc)
}

/// 反查桩：把宿主收到的 `(text, format)` 原样回显成 `text|format`，
/// 于是「挑了哪个字」与「透传了哪份模板」都能在返回值里直接断言。
fn rev_echo_ctx() -> MemoryContext {
    let mut c = MemoryContext::new();
    c.reverse = Some(Box::new(|text: &str, format: &str, min_len: usize| {
        if min_len == 0 {
            format!("{text}|{format}")
        } else {
            format!("{text}|{format}|{min_len}")
        }
    }));
    c
}

/// `ime.set` 的状态参数：几种常见写法都认、大小写与首尾空白不敏感，原样带 target 转给宿主；
/// 认不出的状态报错且不调用宿主（不猜：猜错就是把用户想开的关掉）。
#[test]
fn ime_set_parses_state_and_forwards() {
    let rec = Arc::new(RecIme(Mutex::new(Vec::new())));
    let ctx = ctx_with(rec.clone());
    for (v, want) in [
        ("on", true),
        (" OFF ", false),
        ("true", true),
        ("0", false),
        ("开", true),
        ("关", false),
    ] {
        fn_ime_set(&ctx, &["emoji:frost_greek".into(), v.into()]).unwrap();
        assert_eq!(
            rec.0.lock().unwrap().pop().unwrap(),
            format!("set emoji:frost_greek={want}"),
            "{v:?}"
        );
    }
    assert!(fn_ime_set(&ctx, &["emoji".into(), "maybe".into()]).is_err());
    assert!(rec.0.lock().unwrap().is_empty(), "认不出的状态不该调用宿主");
}

/// `ime.reload()` 交宿主重载配置；宿主没实现时报错而不是静默成功。
#[test]
fn ime_reload_calls_host() {
    let rec = Arc::new(RecIme(Mutex::new(Vec::new())));
    let ctx = ctx_with(rec.clone());
    fn_ime_reload(&ctx, &[]).unwrap();
    assert_eq!(rec.0.lock().unwrap().pop().unwrap(), "reload");
}

/// `ime.set("comment", ...)` 的状态是 default / detail：走 `set_value`，on/off 报错且不调用宿主；
/// 其它 target 仍走原来的开关解析。
#[test]
fn ime_set_comment_accepts_state_values() {
    let rec = Arc::new(RecIme(Mutex::new(Vec::new())));
    let ctx = ctx_with(rec.clone());
    fn_ime_set(&ctx, &["comment".into(), " Detail ".into()]).unwrap();
    assert_eq!(
        rec.0.lock().unwrap().pop().unwrap(),
        "set_value comment=detail"
    );
    assert!(fn_ime_set(&ctx, &["comment".into(), "on".into()]).is_err());
    assert!(rec.0.lock().unwrap().is_empty(), "非法状态不该调用宿主");
    fn_ime_set(&ctx, &["dict:x".into(), "on".into()]).unwrap();
    assert_eq!(rec.0.lock().unwrap().pop().unwrap(), "set dict:x=true");
}

/// 省略 n 时整串反查（出厂 `cofc` 即 `dict.rev(clip())`）：「好吧」查词码，不是首字。
#[test]
fn rev_defaults_to_whole_text_with_default_format() {
    let ctx = rev_echo_ctx();
    let out = fn_dict_rev(&ctx, &["好吧".to_string()]).unwrap();
    assert_eq!(out, format!("好吧|{DEFAULT_REV_FORMAT}"));
    // 单字也一样走整串，只是整串恰好一个字。
    assert_eq!(
        fn_dict_rev(&ctx, &["好".to_string()]).unwrap(),
        format!("好|{DEFAULT_REV_FORMAT}")
    );
}

/// 给了 n 就回到「取第 n 个字」：`$SS` 逐字展开依赖它，不能被整串语义吃掉。
#[test]
fn explicit_n_still_picks_that_char() {
    let ctx = rev_echo_ctx();
    let out = fn_dict_rev(&ctx, &["好吧".to_string(), "1".to_string()]).unwrap();
    assert_eq!(out, format!("好|{DEFAULT_REV_FORMAT}"));
    let out = fn_dict_rev(&ctx, &["好吧".to_string(), "2".to_string()]).unwrap();
    assert_eq!(out, format!("吧|{DEFAULT_REV_FORMAT}"));
}

/// 整串模式：前后空白（尾换行）去掉再查；内部含换行是多行文本，不查。
#[test]
fn whole_text_trims_and_rejects_multiline() {
    let ctx = rev_echo_ctx();
    assert_eq!(
        fn_dict_rev(&ctx, &[" 好吧\r\n".to_string()]).unwrap(),
        format!("好吧|{DEFAULT_REV_FORMAT}")
    );
    assert_eq!(fn_dict_rev(&ctx, &["好\n吧".to_string()]).unwrap(), "");
}

/// 纯空白文本（只复制了个换行）在整串模式下直接空串，不回显出一条只有 `: ` 的候选。
#[test]
fn whole_text_blank_yields_empty() {
    let ctx = rev_echo_ctx();
    for t in ["", "   ", "\n", "\t "] {
        assert_eq!(fn_dict_rev(&ctx, &[t.to_string()]).unwrap(), "", "{t:?}");
    }
}

/// n 按 rune 定位，不是字节：扩展区汉字走代理对，按字节索引会切在半个字上。
#[test]
fn rev_picks_nth_char_by_rune() {
    let ctx = rev_echo_ctx();
    // 「𠮷」是扩展 B 区（4 字节），其后一个字若按字节推进必然错位。
    let text = "𠮷祥".to_string();
    let first = fn_dict_rev(&ctx, &[text.clone(), "1".into()]).unwrap();
    let second = fn_dict_rev(&ctx, &[text, "2".into()]).unwrap();
    assert_eq!(first, format!("𠮷|{DEFAULT_REV_FORMAT}"));
    assert_eq!(second, format!("祥|{DEFAULT_REV_FORMAT}"));
}

/// 显式 format 原样透传（本层不解析、不渲染，渲染归宿主）。
#[test]
fn rev_passes_explicit_format_through_verbatim() {
    let ctx = rev_echo_ctx();
    let named = vec![("format".to_string(), "${pinyin}".to_string())];
    let out = fn_dict_rev_named(&ctx, &["好".to_string()], &named).unwrap();
    assert_eq!(out, "好|${pinyin}");
}

/// `min_len` 透传给宿主；省略 / 空串 / 0 走原流程（宿主收到 0 = 不过滤）。
#[test]
fn rev_min_len_is_forwarded_and_defaults_to_no_filter() {
    let ctx = rev_echo_ctx();
    let f = |v: &str| {
        let named = vec![("min_len".to_string(), v.to_string())];
        fn_dict_rev_named(&ctx, &["好".to_string()], &named).unwrap()
    };
    assert_eq!(f("4"), format!("好|{DEFAULT_REV_FORMAT}|4"));
    for v in ["", "0"] {
        assert_eq!(f(v), format!("好|{DEFAULT_REV_FORMAT}"), "{v:?}");
    }
    // 按 n 取字的路径同样透传。
    let named = vec![("min_len".to_string(), "4".to_string())];
    let out = fn_dict_rev_named(&ctx, &["好吧".to_string(), "2".into()], &named).unwrap();
    assert_eq!(out, format!("吧|{DEFAULT_REV_FORMAT}|4"));
}

/// `min_len` 写成非数字是词条笔误，报错而不是静默当 0。
#[test]
fn rev_min_len_rejects_non_number() {
    let ctx = rev_echo_ctx();
    let named = vec![("min_len".to_string(), "abc".to_string())];
    assert!(fn_dict_rev_named(&ctx, &["好".to_string()], &named).is_err());
}

/// `format=""` 视同省略 —— 多半是词条拼错，退回默认版式比产出空串好查。
#[test]
fn rev_empty_format_falls_back_to_default() {
    let ctx = rev_echo_ctx();
    let named = vec![("format".to_string(), String::new())];
    let out = fn_dict_rev_named(&ctx, &["好".to_string()], &named).unwrap();
    assert_eq!(out, format!("好|{DEFAULT_REV_FORMAT}"));
}

/// 越界与 n<1 返回空串而非错误：`$SS` 固定查第 1..N 个字，剪贴板不足 N 字是常态，
/// 报错会让 `evaluate_phrase` 丢掉整条短语。
#[test]
fn rev_out_of_range_yields_empty_not_error() {
    let ctx = rev_echo_ctx();
    for n in ["3", "99", "0", "-1"] {
        let out = fn_dict_rev(&ctx, &["好人".to_string(), n.to_string()]).unwrap();
        assert_eq!(out, "", "n={n} 应返回空串");
    }
    // 空文本同理（剪贴板为空）
    assert_eq!(fn_dict_rev(&ctx, &[String::new()]).unwrap(), "");
}

/// n 写成非数字是词条作者的笔误，不是运行时正常输入 —— 必须报错而非静默取默认。
#[test]
fn rev_non_numeric_n_errors() {
    let ctx = rev_echo_ctx();
    assert!(fn_dict_rev(&ctx, &["好".to_string(), "abc".into()]).is_err());
}

/// 未注入反查能力的上下文返回空串，不 panic（headless / 廉价导航上下文）。
#[test]
fn rev_without_host_capability_is_empty() {
    let ctx = MemoryContext::new();
    assert_eq!(fn_dict_rev(&ctx, &["好".to_string()]).unwrap(), "");
}

/// `jump` 省略时按右段 char 数推导，不是 UTF-16 单元数——emoji 右段按单元算会多移一格。
#[test]
fn jump_defaults_to_right_char_count() {
    let rec = Arc::new(RecIme::default());
    let ctx = ctx_with(rec.clone());
    fn_ime_pair(&ctx, &["《".into(), "》".into()]).unwrap();
    fn_ime_pair(&ctx, &["<!--".into(), "-->".into()]).unwrap();
    // 星标 emoji 是一个 char / 两个 UTF-16 单元：按 char 记 1。
    fn_ime_pair(&ctx, &["[".into(), "]🌟".into()]).unwrap();
    let log = rec.0.lock().unwrap();
    assert_eq!(log[0], "《|》|1");
    assert_eq!(log[1], "<!--|-->|3");
    assert_eq!(log[2], "[|]🌟|2", "emoji 按 char 计 1，与 `]` 合计 2");
}

/// 显式 `jump` 覆盖推导值——宿主对 VK_RIGHT 的越过粒度不一致时，这是唯一的兜底开口。
#[test]
fn explicit_jump_overrides_default() {
    let rec = Arc::new(RecIme::default());
    let ctx = ctx_with(rec.clone());
    fn_ime_pair_named(
        &ctx,
        &["<!--".into(), "-->".into()],
        &[("jump".into(), "1".into())],
    )
    .unwrap();
    assert_eq!(rec.0.lock().unwrap()[0], "<!--|-->|1");
}

/// 非法 `jump` 必须报错而不是静默取默认：词条写错了要看得见，
/// 静默兜底会让「跳出移错格数」变成一个查不出根因的现场。
#[test]
fn invalid_jump_errors_before_dispatch() {
    let rec = Arc::new(RecIme::default());
    let ctx = ctx_with(rec.clone());
    for bad in ["x", "-1", "1.5"] {
        let err = fn_ime_pair_named(
            &ctx,
            &["《".into(), "》".into()],
            &[("jump".into(), bad.into())],
        )
        .expect_err("非法 jump 应报错");
        assert!(err.to_string().contains("jump"), "{err}");
    }
    assert!(rec.0.lock().unwrap().is_empty(), "报错时不该已经派发出去");
}
