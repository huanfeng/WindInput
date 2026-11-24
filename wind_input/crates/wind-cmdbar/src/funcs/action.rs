//! §3.4 动作函数。`pure=false`，运行时从
//! [`EvalContext::services`](crate::context::EvalContext::services) 取服务；缺服务返回
//! [`CmdbarError::ServiceUnavailable`](crate::error::CmdbarError)。
//!
//! 注意：`type` 不在此——它由 eval 在解析 `$CC` 动作时拦截为文本上屏（ActionText）。

use super::func_specs;
use super::util::{runtime_err, services};
use crate::context::EvalContext;
use crate::error::{CmdbarError, Result};
use crate::registry::FuncSpec;

pub fn specs() -> Vec<FuncSpec> {
    func_specs! {
        "open"       : Action (1, 1)  effect => fn_open,        "打开 URL / 程序 / 文件 (通用 ShellExecute 语义)", "open(\"https://baidu.com\")";
        "proc.run"   : Proc   (1, -1) effect => fn_run,         "启动外部程序, 可带参数", "proc.run(\"notepad.exe\")"
            named(fn_run_named,
                  "cwd"  = "工作目录; 省略时取被启动程序所在目录",
                  "verb" = "动作: open(默认)/runas(管理员)/edit/print/explore/properties; 仅 Windows",
                  "show" = "初始窗口: normal(默认)/min/max/hidden; 仅 Windows");
        "proc.any"   : Proc   (1, -1) effect => fn_any,         "按顺序尝试启动多个程序 (只写程序名, 不带参数), 第一个启动成功的即停", "proc.any(\"gnome-calculator\", \"kcalc\", \"xcalc\")";
        "proc.shell" : Proc   (1, 2)  effect => fn_shell,      "通过 shell 执行命令行; 第二参可选 flag (term/pwsh)", "proc.shell(\"echo hi\")"
            named(fn_shell_named, "cwd" = "工作目录; 省略时取用户主目录");
        "key.tap"    : Key    (1, 1)  effect => fn_key_tap,     "模拟单次按键组合, 如 Ctrl+C / Shift+End / Enter", "key.tap(\"Ctrl+C\")";
        "key.seq"    : Key    (1, -1) effect => fn_key_seq,     "顺序模拟多个按键组合", "key.seq(\"Home\", \"Shift+End\", \"Delete\")";
        "key.hold"   : Key    (1, 1)  effect => fn_key_hold,    "按下并保持按键组合 (需与 key.release 成对)", "key.hold(\"Shift\")";
        "key.release": Key    (1, 1)  effect => fn_key_release, "抬起之前 key.hold 按下的组合", "key.release(\"Shift\")";
        "key.type"   : Key    (1, 1)  effect => fn_key_type,    "以 Unicode 扫描码直接输入文本, 不依赖键盘布局", "key.type(\"hello\")";
        "clip.copy"  : Clip   (1, 1)  effect => fn_clip_copy,   "把文本写入系统剪贴板", "clip.copy(last())";
        "clip.paste" : Clip   (0, 0)  effect => fn_clip_paste,  "模拟 Ctrl+V 粘贴剪贴板内容", "clip.paste()";
        "web.search" : Web    (2, 2)  effect => fn_search,      "用搜索引擎搜索 (engine ∈ baidu/bing/google/zdic)", "web.search(\"baidu\", last())";
        "wind.cli"   : Proc   (1, -1) effect => fn_wind_cli,    "以主程序 CLI 执行子命令 (单参按空白拆分; 多参逐个原样传递)", "wind.cli(\"schema dict disable wubi86 fl\")"
            named(fn_wind_cli_named,
                  "toast" = "执行结果提示: on(默认)/off/error(仅失败); 服务侧已有提示的子命令(restart/config set)请设 off",
                  "ok"    = "成功文案; 省略时用命令自身输出的最后一行",
                  "wait"  = "等待命令结束的毫秒上限(默认 10000); 0=不等待, 此时无结果可报");
        "ui.toast"   : Action (1, 1)  effect => fn_ui_toast,    "弹一条桌面提示", "ui.toast(\"导入完成\", kind=\"success\")"
            named(fn_ui_toast_named,
                  "kind"  = "类型: info(默认)/success/error, 决定强调条颜色",
                  "color" = "自定义强调色 #RRGGBB 或 #RRGGBBAA; 给了则压过 kind",
                  "pos"   = "位置: bottom_center(默认)/center/top_center/top_left/top_right/bottom_left/bottom_right",
                  "ms"    = "显示毫秒数; 省略用默认 2500");
        "ask"        : Action (1, 1)  effect => fn_unimpl,      "弹小输入框, 阻塞返回用户输入 (未实现)", "ask(\"提示\")";
        "pick"       : Action (1, -1) effect => fn_unimpl,      "弹下拉列表选择 (未实现)", "pick(\"a\", \"b\")";
    }
}

fn fn_open(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("open", ctx)?;
    let open = s
        .open
        .as_ref()
        .ok_or_else(|| CmdbarError::service("open"))?;
    open.open(&args[0]).map_err(|e| runtime_err("open", e))?;
    Ok(String::new())
}

/// 取具名参数值；名字白名单已在 [`crate::eval`] 的 `call_func` 校验过，此处只取。
fn named_val<'a>(named: &'a [(String, String)], key: &str) -> &'a str {
    named
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

/// `proc.run` 的 `verb` 取值白名单（对应 ShellExecuteW 的 lpOperation）。
///
/// 收白名单而不是透传：拼错的动词交给 ShellExecuteW 只会换回一个泛化的
/// "没有关联程序" 错误码，用户无从知道问题出在动词上。
const RUN_VERBS: &[&str] = &["open", "runas", "edit", "print", "explore", "properties"];

/// `proc.run` 的 `show` 取值白名单（对应 ShellExecuteW 的 nShowCmd）。
const RUN_SHOWS: &[&str] = &["normal", "min", "max", "hidden"];

/// 校验枚举型具名参数的**值**。名字的合法性由 registry 白名单在调用前保证，
/// 这里管的是值。
///
/// 刻意不带 `cfg(windows)`：短语文件跟着用户跨机器走，校验口径必须与平台无关；
/// 平台差异只体现在执行端（不支持的平台记 WARN 忽略）。
pub(crate) fn check_enum(func: &str, key: &str, val: &str, allowed: &[&str]) -> Result<()> {
    if val.is_empty() || allowed.contains(&val) {
        return Ok(());
    }
    Err(runtime_err(
        func,
        anyhow::anyhow!("{key} 不支持 {val:?}（支持: {}）", allowed.join(" / ")),
    ))
}

fn fn_run(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_run_named(ctx, args, &[])
}

/// `proc.run(cmd, args..., cwd="…", verb="…", show="…")`。
///
/// 所有具名参数空串 = 用默认：`cwd` 交宿主按默认策略决定（**不是**继承调用方当前
/// 目录），`verb` 为 `open`，`show` 为 `normal`。
fn fn_run_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let s = services("proc.run", ctx)?;
    let proc = s
        .proc
        .as_ref()
        .ok_or_else(|| CmdbarError::service("proc.run"))?;
    let spec = crate::services::ProcSpawn {
        cmd: &args[0],
        args: &args[1..],
        cwd: named_val(named, "cwd"),
        verb: named_val(named, "verb"),
        show: named_val(named, "show"),
    };
    // 值校验在服务调用之前：先把能静态判死的写法挡下，避免拿一个非法动词去
    // 启动进程再从平台错误码倒推。
    check_enum("proc.run", "verb", spec.verb, RUN_VERBS)?;
    check_enum("proc.run", "show", spec.show, RUN_SHOWS)?;
    proc.run(&spec).map_err(|e| runtime_err("proc.run", e))?;
    Ok(String::new())
}

/// `proc.any(a, b, c…)`：按顺序逐个启动，第一个**启动成功**的即停，后面的不再尝试。
///
/// 用途是跨桌面环境的内置词条：同一个「计算器」在 GNOME / KDE / Deepin / Xfce 下是不同的
/// 程序，词条里列出常见的几个，装了哪个就开哪个。
///
/// 「成功」只看 spawn，起来后自己失败不回落（等它退出会卡住动作链）。不存在与起不来两类
/// 失败都继续试下一个。Windows 转给 TSF 侧 ShellExecute、恒为成功，只会启动第一个，
/// 故内置词条在 Windows 上用 `proc.run`。
///
/// [`ProcessRunner::run`]: crate::services::ProcessRunner::run
fn fn_any(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("proc.any", ctx)?;
    let proc = s
        .proc
        .as_ref()
        .ok_or_else(|| CmdbarError::service("proc.any"))?;
    // 先整体校验再启动：空名交给宿主只会换回一个说不清的平台错误，而且报错之前
    // 不该已经把排在前面的程序开出去了。
    if args.iter().any(|a| a.trim().is_empty()) {
        return Err(runtime_err(
            "proc.any",
            anyhow::anyhow!("候选程序名不能为空"),
        ));
    }
    let mut missing: Vec<&str> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for cand in args {
        let cmd = cand.trim();
        match proc.run(&crate::services::ProcSpawn::new(cmd, &[])) {
            Ok(()) => return Ok(String::new()),
            Err(e) if is_not_found(&e) => missing.push(cmd),
            Err(e) => failed.push(format!("{cmd}（{e}）")),
        }
    }
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("未找到 {}", missing.join(" / ")));
    }
    if !failed.is_empty() {
        parts.push(format!("启动失败 {}", failed.join(" / ")));
    }
    Err(runtime_err(
        "proc.any",
        anyhow::anyhow!("没有可启动的程序：{}", parts.join("；")),
    ))
}

/// 宿主的错误链里是否有「文件不存在」。
///
/// 看整条链而不是只看最外层：宿主可能给 io 错误加了上下文再返回。
fn is_not_found(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

/// `wind.cli` 的 `toast` 取值白名单。
const CLI_TOAST_MODES: &[&str] = &["on", "off", "error"];

/// `ui.toast` 的 `kind` 取值白名单。
const TOAST_KINDS: &[&str] = &["info", "success", "error"];

/// `ui.toast` 的 `pos` 取值白名单。与 `wind_ui_types::ToastPosition::parse` 认的值域一致——
/// 那边未知值降级成 bottom_center 是**渲染端对脏数据的兜底**，不能替代这里的校验：
/// 词条里把 `top_right` 写成 `right` 必须当场报错，否则用户只会看到「位置参数没生效」。
const TOAST_POSITIONS: &[&str] = &[
    "center",
    "top_center",
    "top",
    "bottom_center",
    "top_left",
    "top_right",
    "bottom_left",
    "bottom_right",
];

/// 解析毫秒型具名参数。空串 = 用默认；非数字报错而不是静默取默认——
/// `ms="5秒"` 静默降级的话，用户只会以为时长参数根本没实现。
fn parse_ms(func: &str, key: &str, val: &str, default: u64) -> Result<u64> {
    if val.trim().is_empty() {
        return Ok(default);
    }
    val.trim()
        .parse::<u64>()
        .map_err(|_| runtime_err(func, anyhow::anyhow!("{key} 需要毫秒数, 收到 {val:?}")))
}

/// 校验 `ui.toast` 的 kind / color / pos 三个值（空串 = 未指定）。
///
/// 公开给 RPC `ui.toast` 共用，避免两个入口校验口径分裂；渲染端 `parse` 的未知值降级
/// 只是脏数据兜底，替代不了入口校验。
pub fn validate_toast_args(kind: &str, color: &str, position: &str) -> Result<()> {
    check_enum("ui.toast", "kind", kind, TOAST_KINDS)?;
    check_enum("ui.toast", "pos", position, TOAST_POSITIONS)?;
    check_color("ui.toast", color)?;
    // kind 与 color 同时给：不猜哪个赢。两者都是「强调色」的写法，同时出现只能是
    // 写错了，静默取一个会让另一个看起来"没生效"。
    if !kind.is_empty() && !color.is_empty() {
        return Err(runtime_err(
            "ui.toast",
            anyhow::anyhow!("kind 与 color 只能给一个"),
        ));
    }
    Ok(())
}

/// 校验 `#RRGGBB` / `#RRGGBBAA` 形态。空串 = 未指定。
fn check_color(func: &str, val: &str) -> Result<()> {
    if val.is_empty() {
        return Ok(());
    }
    let hex = val.strip_prefix('#').unwrap_or("");
    if matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(runtime_err(
        func,
        anyhow::anyhow!("color 需要 #RRGGBB 或 #RRGGBBAA, 收到 {val:?}"),
    ))
}

fn fn_wind_cli(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_wind_cli_named(ctx, args, &[])
}

/// `wind.cli`：以主程序自身 exe 跑 CLI 子命令。单参形式按空白拆分
/// （`wind.cli("config set ui.theme.name dark")`）；多参形式逐个原样传递，
/// 供含空格的参数（如文件路径）精确传参（`wind.cli("backup", "create", path)`）。
///
/// 具名参数 `toast` / `ok` / `wait` 只决定**执行完怎么反馈**，不影响命令本身。
fn fn_wind_cli_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let s = services("wind.cli", ctx)?;
    let proc = s
        .proc
        .as_ref()
        .ok_or_else(|| CmdbarError::service("wind.cli"))?;
    let argv: Vec<String> = if args.len() == 1 {
        args[0].split_whitespace().map(String::from).collect()
    } else {
        args.to_vec()
    };
    if argv.is_empty() {
        return Err(runtime_err("wind.cli", anyhow::anyhow!("子命令为空")));
    }
    let toast = named_val(named, "toast");
    check_enum("wind.cli", "toast", toast, CLI_TOAST_MODES)?;
    let wait_ms = parse_ms(
        "wind.cli",
        "wait",
        named_val(named, "wait"),
        crate::services::DEFAULT_CLI_WAIT_MS,
    )?;
    let spec = crate::services::CliSpawn {
        args: &argv,
        toast,
        ok_text: named_val(named, "ok"),
        wait_ms,
    };
    proc.run_self(&spec)
        .map_err(|e| runtime_err("wind.cli", e))?;
    Ok(String::new())
}

fn fn_ui_toast(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_ui_toast_named(ctx, args, &[])
}

/// `ui.toast(文案, kind=…, color=…, pos=…, ms=…)`：弹一条桌面提示。
///
/// 只有文案是位置参数，其余全具名。底色/文字色/字号刻意不开放，跟随主题。
fn fn_ui_toast_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let s = services("ui.toast", ctx)?;
    let notify = s
        .notify
        .as_ref()
        .ok_or_else(|| CmdbarError::service("ui.toast"))?;
    let kind = named_val(named, "kind");
    let color = named_val(named, "color");
    let position = named_val(named, "pos");
    check_enum("ui.toast", "kind", kind, TOAST_KINDS)?;
    validate_toast_args(kind, color, position)?;
    let duration_ms = parse_ms("ui.toast", "ms", named_val(named, "ms"), 0)?;
    let spec = crate::services::ToastSpec {
        text: &args[0],
        kind,
        color,
        position,
        duration_ms,
    };
    notify
        .toast(&spec)
        .map_err(|e| runtime_err("ui.toast", e))?;
    Ok(String::new())
}

fn fn_shell(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_shell_named(ctx, args, &[])
}

/// `proc.shell(cmdline, "flagA,flagB", cwd="…")`。
///
/// 与 `proc.run` 不同，命令行是整串交给 shell 的，无法从中认出目标程序，
/// 所以 `cwd` 省略时只能落到中性目录——需要相对路径的场景必须显式写 `cwd`。
fn fn_shell_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let s = services("proc.shell", ctx)?;
    let proc = s
        .proc
        .as_ref()
        .ok_or_else(|| CmdbarError::service("proc.shell"))?;
    let flags: Vec<String> = if args.len() > 1 {
        args[1]
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect()
    } else {
        Vec::new()
    };
    proc.shell(&args[0], &flags, named_val(named, "cwd"))
        .map_err(|e| runtime_err("proc.shell", e))?;
    Ok(String::new())
}

fn fn_key_tap(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let keys = keys(ctx, "key.tap")?;
    keys.tap(&args[0]).map_err(|e| runtime_err("key.tap", e))?;
    Ok(String::new())
}

fn fn_key_seq(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let keys = keys(ctx, "key.seq")?;
    keys.sequence(args).map_err(|e| runtime_err("key.seq", e))?;
    Ok(String::new())
}

fn fn_key_hold(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let keys = keys(ctx, "key.hold")?;
    keys.hold(&args[0])
        .map_err(|e| runtime_err("key.hold", e))?;
    Ok(String::new())
}

fn fn_key_release(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let keys = keys(ctx, "key.release")?;
    keys.release(&args[0])
        .map_err(|e| runtime_err("key.release", e))?;
    Ok(String::new())
}

fn fn_key_type(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let keys = keys(ctx, "key.type")?;
    keys.type_text(&args[0])
        .map_err(|e| runtime_err("key.type", e))?;
    Ok(String::new())
}

fn keys<'a>(
    ctx: &'a dyn EvalContext,
    func: &str,
) -> Result<&'a std::sync::Arc<dyn crate::services::KeyInjector>> {
    let s = services(func, ctx)?;
    s.keys
        .as_ref()
        .ok_or_else(|| CmdbarError::service(func.to_string()))
}

fn fn_clip_copy(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("clip.copy", ctx)?;
    let clip = s
        .clip
        .as_ref()
        .ok_or_else(|| CmdbarError::service("clip.copy"))?;
    clip.set_text(&args[0])
        .map_err(|e| runtime_err("clip.copy", e))?;
    Ok(String::new())
}

fn fn_clip_paste(ctx: &dyn EvalContext, _args: &[String]) -> Result<String> {
    let s = services("clip.paste", ctx)?;
    let clip = s
        .clip
        .as_ref()
        .ok_or_else(|| CmdbarError::service("clip.paste"))?;
    clip.paste().map_err(|e| runtime_err("clip.paste", e))?;
    Ok(String::new())
}

/// engine id → 查询 URL 前缀（%s 处接 URL 编码后的 query）。
const SEARCH_URLS: &[(&str, &str)] = &[
    ("baidu", "https://www.baidu.com/s?wd="),
    ("bing", "https://www.bing.com/search?q="),
    ("google", "https://www.google.com/search?q="),
    ("zdic", "https://www.zdic.net/hans/"),
];

fn fn_search(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("web.search", ctx)?;
    let engine = args[0].trim().to_lowercase();
    let query = &args[1];
    // 空查询词直接报错、不打开浏览器：开一个空搜索页是静默失败，报错会经
    // `show_command_error` 弹出。注意 `sel()` 在 Windows 上恒为空串（`front_ctx` 只有
    // macOS 会写），`web.search("baidu", sel())` 在 Windows 上必然走进本分支。
    if query.trim().is_empty() {
        return Err(CmdbarError::runtime(
            "web.search",
            "没有可搜索的内容（取到的文本为空）",
        ));
    }
    // 宿主自定义搜索优先。
    if let Some(search) = &s.search {
        search
            .search(&engine, query)
            .map_err(|e| runtime_err("web.search", e))?;
        return Ok(String::new());
    }
    // 默认：合成 URL 转发给 open。
    let prefix = SEARCH_URLS
        .iter()
        .find(|(k, _)| *k == engine)
        .map(|(_, v)| *v)
        .ok_or_else(|| CmdbarError::runtime("web.search", format!("unknown engine {engine:?}")))?;
    let open = s
        .open
        .as_ref()
        .ok_or_else(|| CmdbarError::service("web.search"))?;
    let target = format!("{prefix}{}", super::text::query_escape(query));
    open.open(&target)
        .map_err(|e| runtime_err("web.search", e))?;
    Ok(String::new())
}

fn fn_unimpl(_: &dyn EvalContext, _args: &[String]) -> Result<String> {
    Err(CmdbarError::NotImplemented {
        name: "ask/pick".into(),
    })
}

#[cfg(test)]
#[path = "action_tests.rs"]
mod tests;
