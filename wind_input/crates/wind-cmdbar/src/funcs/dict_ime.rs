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
        "dict.rev"        : Dict    (1, 2) pure   => fn_dict_rev,    "反查文本的编码与读音; 省略 n 时整串查 (词/字皆可), 给了 n 则取第 n 个字 (1 起), 越界返回空串", "dict.rev(clip())"
            named(fn_dict_rev_named, "min_len" = "只保留码长 >= 该值的编码 (作用于 ${code_rev_all}/${code_rev}; 未启用扩展库的 * 前缀不计入码长); 省略或 0 = 不过滤, 如 min_len=4 只要四码及以上的全码", "format" = "版式模板, 同候选注释段语法; 变量 ${char}/${code_rev_all}(全部码位)/${code_rev}(仅全码)/${code_schema}(本方案击键)/${pinyin}/${chaizi}/${chaizi_code}/${dict}; 省略='${char}: ${code_rev_all} ${pinyin}'");
        "ime.toggle"      : Ime     (1, 1) effect => fn_ime_toggle,  "切换 IME 状态 (cn-en / fullshape / layout / candwin / s2t / t2s / preedit / toolbar / comment[注释默认↔详细])；数据开关 emoji / emoji:<模块id> / dict:<注释库id>", "ime.toggle(\"cn-en\")";
        "ime.set"         : Ime     (2, 2) effect => fn_ime_set,     "把开关设为指定状态, 已是该状态则只提示不动作; target 为输入状态 (cn-en / fullshape / s2t / t2s / toolbar; cn-en 的 on=中文) 或数据开关 (emoji / emoji:<模块id> / dict:<注释库id>), 状态 on/off (也认 true/false、1/0、开/关); target 为 comment 时状态是 default / detail (候选注释的默认 / 详细态)", "ime.set(\"emoji:frost_greek\", \"on\")";
        "ime.schema"      : Ime     (1, 1) effect => fn_ime_schema,  "切换输入方案并持久化", "ime.schema(\"pinyin\")";
        "ime.theme"       : Ime     (1, 1) effect => fn_ime_theme,   "切换主题并持久化 (= config.set ui.theme.name)", "ime.theme(\"msime\")";
        "ime.theme_cycle" : Ime     (0, 1) effect => fn_theme_cycle, "循环切换主题并持久化; dir 可选 next(默认)/prev", "ime.theme_cycle()";
        "ime.undo_commit" : Ime     (0, 0) effect => fn_undo_commit,"撤销最近一次上屏 (删刚上屏的字符数; 焦点变化或又输入其它内容后退化删 1 个)", "ime.undo_commit()";
        "ime.reload"      : Ime     (0, 0) effect => fn_ime_reload,"重新读取用户配置并应用 (等同设置端保存后的热重载; 手改 config.toml 后用)", "ime.reload()";
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

/// `dict.rev(text, n, format="…")`：反查 `text` 的编码与读音。
///
/// 两种用法，**按给没给 `n`** 分工：
///
/// - **省略 `n`** —— 整串原样交宿主（出厂 `cofc` 写的就是 `dict.rev(clip())`）。复制
///   「好吧」查的是「好吧」**这个词**在码表里的码，复制单字自然落回单字。剪贴板反查的
///   主场就是这个：用户从别处复制来的可能是一个字，也可能是一个词。
/// - **给了 `n`** —— 取第 n 个字（1 起）：`dict.rev(clip(), 1..N)` 是 `$SS` 逐字展开的
///   原子操作（写法见 `data/system.phrases.toml` 里 `cofc` 的注释块）。
///
/// 本层**只负责挑不挑字与填默认模板**，渲染在宿主侧（见
/// [`EvalContext::reverse_lookup`](crate::context::EvalContext::reverse_lookup)）。那边收的
/// 本就是任意文本、并按**词条**查主码表反向索引（`word_codes_display` / `codetable_reverse_hint`），
/// 故整串这条路不需要宿主做任何改动。
///
/// # 越界与非法 n 一律返回空串而不报错
///
/// 这个函数的主场是 `$SS` 多候选展开——词条里固定写 N 个元素、各查第 1..N 个字，
/// 剪贴板不足 N 字是**常态**而非错误。报错会让整条短语求值失败、连带前几个查得到的
/// 字一起消失（`evaluate_phrase` 的 `Err` 分支是整条丢弃）。返回空串则只让那一条候选
/// 落空，由短语层的空串守卫丢掉它。
///
/// `n` 本身写错（非数字）仍然报错——那是词条作者的笔误，不是运行时的正常输入。
///
/// 整串模式下「查不到」（文本不是码表词条、纯空白、反查索引尚未建好）同样返回空串，
/// 由词条的 `default(…, "提示语")` 接住，见 `docs/design/clipboard-reverse-lookup.md`。
fn fn_dict_rev_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    // 空 format 视同省略：`format=""` 多半是词条拼错，退回默认版式比产出空串好查。
    let format = named
        .iter()
        .find(|(k, _)| k == "format")
        .map(|(_, v)| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_REV_FORMAT);
    // ★ 省略 n ⇒ 整串反查。宿主按词条查码表反向索引，故「好吧」拿到的是这个词的码，
    // 单字文本自然落回单字。查不到返回空串（`reverse_render` 的 found 判据），
    // 由词条的 `default(...)` 回落成提示语。
    // `min_len` 非数字报错（词条笔误）；省略/空串/0 = 不过滤，正常流程不受影响。
    let min_len = match named
        .iter()
        .find(|(k, _)| k == "min_len")
        .map(|(_, v)| v.trim())
        .filter(|s| !s.is_empty())
    {
        Some(v) => usize::try_from(parse_arg_int("dict.rev", v)?).unwrap_or(0),
        None => 0,
    };
    let Some(n) = args.get(1) else {
        // 纯空白（只复制了个换行）：反查必然空，提前挡掉，免得 `${char}` 把空白
        // 回显成一条「有内容」的候选。
        // 前后空白去掉（从终端/编辑器复制一行常带尾换行，不去就查不到词条）；
        // 去完仍含换行的是多行文本，不是一个词。长度上限由宿主侧把关。
        let text = args[0].trim();
        if text.is_empty() || text.contains(['\n', '\r']) {
            return Ok(String::new());
        }
        return Ok(ctx.reverse_lookup(text, format, min_len));
    };
    let n = parse_arg_int("dict.rev", n)?;
    if n < 1 {
        return Ok(String::new());
    }
    // 按 rune 取字：扩展区汉字走代理对，按字节索引会切在半个字上。
    let Some(ch) = args[0].chars().nth((n - 1) as usize) else {
        return Ok(String::new());
    };
    let mut buf = [0u8; 4];
    Ok(ctx.reverse_lookup(ch.encode_utf8(&mut buf), format, min_len))
}

fn fn_ime_set(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    // `comment` 的状态是 default / detail，不是开关，绕过 on/off 解析。
    if args[0] == "comment" {
        let value = args[1].trim().to_ascii_lowercase();
        if value != "default" && value != "detail" {
            return Err(runtime_err(
                "ime.set",
                anyhow::anyhow!("comment 的状态需要 default / detail，收到 {:?}", args[1]),
            ));
        }
        let ime = ime(ctx, "ime.set")?;
        ime.set_value(&args[0], &value)
            .map_err(|e| runtime_err("ime.set", e))?;
        return Ok(String::new());
    }
    let on = parse_switch(&args[1]).ok_or_else(|| {
        runtime_err(
            "ime.set",
            anyhow::anyhow!(
                "状态需要 on / off（或 true/false、1/0、开/关），收到 {:?}",
                args[1]
            ),
        )
    })?;
    let ime = ime(ctx, "ime.set")?;
    ime.set(&args[0], on)
        .map_err(|e| runtime_err("ime.set", e))?;
    Ok(String::new())
}

/// `ime.set` 的状态参数。大小写不敏感、去首尾空白；认不出返回 None（由调用方报错，不猜）。
fn parse_switch(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "开" => Some(true),
        "off" | "false" | "0" | "关" => Some(false),
        _ => None,
    }
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

fn fn_ime_reload(ctx: &dyn EvalContext, _args: &[String]) -> Result<String> {
    let ime = ime(ctx, "ime.reload")?;
    ime.reload_config()
        .map_err(|e| runtime_err("ime.reload", e))?;
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
#[path = "dict_ime_tests.rs"]
mod tests;
