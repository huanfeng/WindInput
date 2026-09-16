//! 词库 / IME / 设置动作（对照 Go funcs/dict_ime.go）。`pure=false`，经
//! [`DictService`](crate::services::DictService) / [`ImeController`](crate::services::ImeController) /
//! [`ConfigService`](crate::services::ConfigService) 取真实后端。

use super::func_specs;
use super::util::{parse_arg_int, runtime_err, services};
use crate::context::EvalContext;
use crate::error::{CmdbarError, Result};
use crate::registry::FuncSpec;

pub fn specs() -> Vec<FuncSpec> {
    func_specs! {
        "dict.add"        : Dict    (1, 2) effect => fn_dict_add,    "把文本加入用户词库; code 可选, 不传时按当前方案规则推导", "dict.add(clip())";
        "dict.rev"        : Dict    (1, 2) pure   => fn_dict_rev,    "反查文本中某个字的编码与读音; n 为第几个字 (1 起, 默认 1), 超出字数返回空串", "dict.rev(clip())"
            named(fn_dict_rev_named, "format" = "版式模板, 同候选注释段语法; 变量 ${char}/${code_rev_all}(全部码位)/${code_rev}(仅全码)/${code_schema}(本方案击键)/${pinyin}/${chaizi}/${chaizi_code}/${dict}; 省略='${char}: ${code_rev_all} ${pinyin}'");
        "ime.toggle"      : Ime     (1, 1) effect => fn_ime_toggle,  "切换 IME 状态 (cn-en / fullshape / layout / candwin / s2t / preedit / toolbar)", "ime.toggle(\"cn-en\")";
        "ime.schema"      : Ime     (1, 1) effect => fn_ime_schema,  "切换输入方案并持久化", "ime.schema(\"pinyin\")";
        "ime.theme"       : Ime     (1, 1) effect => fn_ime_theme,   "切换主题并持久化 (= config.set ui.theme.name)", "ime.theme(\"msime\")";
        "ime.theme_cycle" : Ime     (0, 1) effect => fn_theme_cycle, "循环切换主题并持久化; dir 可选 next(默认)/prev", "ime.theme_cycle()";
        "ime.undo_commit" : Ime     (0, 0) effect => fn_undo_commit,"撤销最近一次上屏 (删刚上屏的字符数; 焦点变化或又输入其它内容后退化删 1 个)", "ime.undo_commit()";
        "ime.pair"        : Ime     (2, 2) effect => fn_ime_pair,   "上屏配对文本并激活配对状态, 光标落两段之间, 可用跳出键 (Tab/Enter) 越过右段", "ime.pair(\"《\", \"》\")"
            named(fn_ime_pair_named, "jump" = "跳出时光标右移的格数; 省略=按右段字符数");
        "setting.open"    : Setting (1, 2) effect => fn_setting_open,"打开设置窗口的指定页面 (schema/input/keys/ui/dict/advanced/about/import; 空串=默认页); args 可选, 原样直通给设置程序", "setting.open(\"dict\", \"--schema=wubi86 --type=shadow\")";
        "setting.web"     : Setting (1, 2) effect => fn_setting_web, "打开设置页 (page 同 setting.open; --web 已废弃, 降级为原生设置页)", "setting.web(\"\")";
    }
}

/// 配置键：主题名（与 Go configkey.UiThemeName 对齐）。
const UI_THEME_NAME: &str = "ui.theme.name";

fn fn_dict_add(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("dict.add", ctx)?;
    let dict = s
        .dict
        .as_ref()
        .ok_or_else(|| CmdbarError::service("dict.add"))?;
    let code = args.get(1).map(String::as_str).unwrap_or("");
    dict.add_word(&args[0], code)
        .map_err(|e| runtime_err("dict.add", e))?;
    Ok(String::new())
}

/// `dict.rev` 省略 `format` 时的默认版式 → `我: q/trn/trnt wǒ`。
///
/// 变量名与候选注释段的模板变量**同名同义**（见 `Coordinator::eval_var`）：用户学一次。
///
/// 用 `code_rev_all` 而不是 `code_rev`：后者只给最长的那个全码（`我` → `trnt`），而反查回答的是
/// 「这个字怎么打」—— 简码 `q` 才是最有用的答案。见 `Coordinator::eval_text_var`。
const DEFAULT_REV_FORMAT: &str = "${char}: ${code_rev_all} ${pinyin}";

fn fn_dict_rev(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_dict_rev_named(ctx, args, &[])
}

/// `dict.rev(text, n, format="…")`：取 `text` 的第 n 个字（1 起，默认 1）交宿主反查。
///
/// 本层**只负责挑字与填默认模板**，渲染在宿主侧（见
/// [`EvalContext::reverse_lookup`](crate::context::EvalContext::reverse_lookup)）。
///
/// # 越界与非法 n 一律返回空串而不报错
///
/// 这个函数的主场是 `$SS` 多候选展开——词条里固定写 N 个元素、各查第 1..N 个字，
/// 剪贴板不足 N 字是**常态**而非错误。报错会让整条短语求值失败、连带前几个查得到的
/// 字一起消失（`evaluate_phrase` 的 `Err` 分支是整条丢弃）。返回空串则只让那一条候选
/// 落空，由短语层的空串守卫丢掉它。
///
/// `n` 本身写错（非数字）仍然报错——那是词条作者的笔误，不是运行时的正常输入。
fn fn_dict_rev_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let n = match args.get(1) {
        Some(s) => parse_arg_int("dict.rev", s)?,
        None => 1,
    };
    if n < 1 {
        return Ok(String::new());
    }
    // 按 rune 取字：扩展区汉字走代理对，按字节索引会切在半个字上。
    let Some(ch) = args[0].chars().nth((n - 1) as usize) else {
        return Ok(String::new());
    };
    // 空 format 视同省略：`format=""` 多半是词条拼错，退回默认版式比产出空串好查。
    let format = named
        .iter()
        .find(|(k, _)| k == "format")
        .map(|(_, v)| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_REV_FORMAT);
    let mut buf = [0u8; 4];
    Ok(ctx.reverse_lookup(ch.encode_utf8(&mut buf), format))
}

fn fn_ime_toggle(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let ime = ime(ctx, "ime.toggle")?;
    ime.toggle(&args[0])
        .map_err(|e| runtime_err("ime.toggle", e))?;
    Ok(String::new())
}

fn fn_ime_schema(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let ime = ime(ctx, "ime.schema")?;
    ime.set_schema(&args[0])
        .map_err(|e| runtime_err("ime.schema", e))?;
    Ok(String::new())
}

fn fn_theme_cycle(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let ime = ime(ctx, "ime.theme_cycle")?;
    let dir = args.first().map(String::as_str).unwrap_or("");
    let next = ime
        .theme_cycle(dir)
        .map_err(|e| runtime_err("ime.theme_cycle", e))?;
    Ok(next)
}

fn fn_ime_pair(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_ime_pair_named(ctx, args, &[])
}

/// `ime.pair(left, right, jump="N")`。
///
/// `jump` 省略（或空串）时取 `right` 的 **char 数**，而不是 UTF-16 单元数：跳出靠合成
/// VK_RIGHT，多数宿主一次越过整个字素簇，按 UTF-16 算会在 emoji 右段上多移一格。反过来
/// 也有宿主按单元走，所以留 `jump` 这个显式开口兜底——推导只是默认值，不是唯一真相。
fn fn_ime_pair_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let ime = ime(ctx, "ime.pair")?;
    let (left, right) = (&args[0], &args[1]);
    let raw = named
        .iter()
        .find(|(k, _)| k == "jump")
        .map(|(_, v)| v.trim())
        .unwrap_or("");
    let jump_steps = if raw.is_empty() {
        right.chars().count() as u32
    } else {
        raw.parse::<u32>().map_err(|_| {
            runtime_err(
                "ime.pair",
                anyhow::anyhow!("jump 需为非负整数，收到 {raw:?}"),
            )
        })?
    };
    ime.pair(left, right, jump_steps)
        .map_err(|e| runtime_err("ime.pair", e))?;
    Ok(String::new())
}

fn fn_undo_commit(ctx: &dyn EvalContext, _args: &[String]) -> Result<String> {
    let ime = ime(ctx, "ime.undo_commit")?;
    ime.undo_commit()
        .map_err(|e| runtime_err("ime.undo_commit", e))?;
    Ok(String::new())
}

fn fn_setting_open(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let ime = ime(ctx, "setting.open")?;
    let extra = args.get(1).map(String::as_str).unwrap_or("");
    ime.open_setting(&args[0], extra)
        .map_err(|e| runtime_err("setting.open", e))?;
    Ok(String::new())
}

fn fn_setting_web(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let ime = ime(ctx, "setting.web")?;
    let extra = args.get(1).map(String::as_str).unwrap_or("");
    ime.open_setting_web(&args[0], extra)
        .map_err(|e| runtime_err("setting.web", e))?;
    Ok(String::new())
}

/// `ime.theme(name)` 经 ConfigService 设置主题名（与 config.set 等价）。
fn fn_ime_theme(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("ime.theme", ctx)?;
    let config = s
        .config
        .as_ref()
        .ok_or_else(|| CmdbarError::service("ime.theme"))?;
    config
        .set(UI_THEME_NAME, &args[0])
        .map_err(|e| runtime_err("ime.theme", e))?;
    Ok(String::new())
}

fn ime<'a>(
    ctx: &'a dyn EvalContext,
    func: &str,
) -> Result<&'a std::sync::Arc<dyn crate::services::ImeController>> {
    let s = services(func, ctx)?;
    s.ime
        .as_ref()
        .ok_or_else(|| CmdbarError::service(func.to_string()))
}

#[cfg(test)]
mod tests {
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
        c.reverse = Some(Box::new(|text: &str, format: &str| {
            format!("{text}|{format}")
        }));
        c
    }

    /// 省略 n 取第 1 个字；省略 format 填默认版式。
    #[test]
    fn rev_defaults_to_first_char_and_default_format() {
        let ctx = rev_echo_ctx();
        let out = fn_dict_rev(&ctx, &["好人".to_string()]).unwrap();
        assert_eq!(out, format!("好|{DEFAULT_REV_FORMAT}"));
    }

    /// n 按 **rune** 定位，不是字节：扩展区汉字走代理对，按字节索引会切在半个字上。
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

    /// `format=""` 视同省略 —— 多半是词条拼错，退回默认版式比产出空串好查。
    #[test]
    fn rev_empty_format_falls_back_to_default() {
        let ctx = rev_echo_ctx();
        let named = vec![("format".to_string(), String::new())];
        let out = fn_dict_rev_named(&ctx, &["好".to_string()], &named).unwrap();
        assert_eq!(out, format!("好|{DEFAULT_REV_FORMAT}"));
    }

    /// ★ 越界与 n<1 返回**空串而非错误**。
    ///
    /// `$SS` 展开时词条固定写 N 个元素、各查第 1..N 个字，剪贴板不足 N 字是常态；
    /// 若报错，`evaluate_phrase` 的 `Err` 分支会把**整条短语**丢掉——连前几个查得到的
    /// 字一起消失。这条守着「只落空那一条候选」。
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

    /// n 写成非数字是**词条作者的笔误**，不是运行时正常输入 —— 必须报错而非静默取默认。
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

    /// `jump` 省略时按**右段 char 数**推导，不是 UTF-16 单元数——emoji 右段按单元算会多移一格。
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
}
