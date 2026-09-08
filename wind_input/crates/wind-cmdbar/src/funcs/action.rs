//! §3.4 动作函数（对照 Go funcs/action.go）。`pure=false`，运行时从
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
        "proc.shell" : Proc   (1, 2)  effect => fn_shell,       "通过 shell 执行命令行; 第二参可选 flag (term/pwsh)", "proc.shell(\"echo hi\")"
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
/// 刻意**不带 `cfg(windows)`**：verb/show 只在 Windows 生效，但值校验必须跨平台
/// 一致，否则同一条词条在 macOS 上写错值不报错、拿到 Windows 才失败——短语文件
/// 是跟着用户跨机器走的，校验口径必须与平台无关。平台差异只体现在执行端
/// （不支持的平台记 WARN 忽略），不体现在能不能写。
fn check_enum(func: &str, key: &str, val: &str, allowed: &[&str]) -> Result<()> {
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
/// **公开**：toast 有两个入口——短语里的 `ui.toast(…)` 与 RPC `ui.toast`（CLI /
/// 外部脚本）。若各校验各的，必然出现「短语里写错报错、命令行里写错静默降级」
/// 这种按入口分裂的行为；渲染端的 `ToastKind::parse` / `ToastPosition::parse`
/// 未知值降级成默认，是**对脏数据的兜底**，替代不了入口处的校验。
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
/// 只有文案是位置参数，其余全具名、与顺序无关。底色/文字色/字号**刻意不开放**——
/// 那些跟随主题（`toast_bg` / `toast_text` / `theme.views.toast`），在词条里硬写会与
/// 用户主题打架；短语能定的只有「这条提示是什么性质」（kind/color）与何时何地出现。
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
    // 空查询词直接报错，**不打开浏览器**。
    //
    // 这是「静默失败」最标准的形状：`web.search("baidu", sel())` 在取不到文本时照样开一个
    // `baidu.com/s?wd=` 的空搜索页 —— 命令看上去执行了，结果什么也没搜到，用户能拿到的
    // 唯一线索是「浏览器开了但搜索框是空的」，无从判断是取值没成、还是引擎名写错了、
    // 还是命令压根没跑。报错会经 `show_command_error` 弹到屏幕上。
    //
    // ⚠️ `sel()` 在 Windows 上**恒为空串**（`front_ctx` 只有 macOS 的 `.app` 会写），
    // 故 `web.search("baidu", sel())` 这一类写法在 Windows 上必然走进本分支。
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
mod tests {
    use super::*;
    use crate::context::MemoryContext;
    use crate::services::{Services, UrlOpener};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordOpener(Mutex<Vec<String>>);
    impl UrlOpener for RecordOpener {
        fn open(&self, target: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(target.to_string());
            Ok(())
        }
    }

    /// 一次 `wind.cli` 调用：argv / toast 模式 / 成功文案 / 等待上限。
    type CliCall = (Vec<String>, String, String, u64);
    /// 一次 `ui.toast` 调用：文案 / kind / color / 位置 / 时长。
    type ToastCall = (String, String, String, String, u64);

    /// 记录 `wind.cli` 收到的 argv 与反馈策略。
    #[derive(Default)]
    struct RecSelf(Mutex<Vec<CliCall>>);
    impl crate::services::ProcessRunner for RecSelf {
        fn run(&self, _spec: &crate::services::ProcSpawn<'_>) -> anyhow::Result<()> {
            unreachable!()
        }
        fn shell(&self, _cmdline: &str, _flags: &[String], _cwd: &str) -> anyhow::Result<()> {
            unreachable!()
        }
        fn run_self(&self, spec: &crate::services::CliSpawn<'_>) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((
                spec.args.to_vec(),
                spec.toast.to_string(),
                spec.ok_text.to_string(),
                spec.wait_ms,
            ));
            Ok(())
        }
    }

    /// 记录 `ui.toast` 收到的完整 spec。
    #[derive(Default)]
    struct RecToast(Mutex<Vec<ToastCall>>);
    impl crate::services::NotifyService for RecToast {
        fn toast(&self, spec: &crate::services::ToastSpec<'_>) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((
                spec.text.to_string(),
                spec.kind.to_string(),
                spec.color.to_string(),
                spec.position.to_string(),
                spec.duration_ms,
            ));
            Ok(())
        }
    }

    #[test]
    fn open_dispatches_to_service() {
        let rec = Arc::new(RecordOpener::default());
        let mut svc = Services::new();
        svc.open = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);
        fn_open(&ctx, &["https://x".into()]).unwrap();
        assert_eq!(rec.0.lock().unwrap().as_slice(), &["https://x".to_string()]);
    }

    #[test]
    fn search_composes_url() {
        let rec = Arc::new(RecordOpener::default());
        let mut svc = Services::new();
        svc.open = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);
        fn_search(&ctx, &["baidu".into(), "a b".into()]).unwrap();
        assert_eq!(rec.0.lock().unwrap()[0], "https://www.baidu.com/s?wd=a+b");
    }

    /// 空查询词**不开浏览器**，报错。
    ///
    /// 此前会开一个空搜索页 —— 命令看上去跑了、结果什么也没搜到，这正是最难归因的那种
    /// 失败：用户分不清是取值没成、引擎名写错、还是命令压根没执行。
    ///
    /// 变异判据：去掉 `fn_search` 里的空查询守卫，两条断言同时转红。
    #[test]
    fn search_refuses_an_empty_query_instead_of_opening_a_blank_page() {
        let rec = Arc::new(RecordOpener::default());
        let mut svc = Services::new();
        svc.open = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);
        // 纯空白同样算空：取值函数返回的常常是一个换行或若干空格。
        for q in ["", "   ", "\n"] {
            assert!(
                fn_search(&ctx, &["baidu".into(), q.into()]).is_err(),
                "空查询 {q:?} 应当报错"
            );
        }
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "报错的同时绝不能已经把浏览器开出去了"
        );
    }

    /// 记录 `ProcessRunner` 收到的全部字段，供两个测试共用。
    /// 把每个字段都拼进字符串是刻意的：漏传某个选项会直接体现在断言串上，
    /// 而只断言关心的那个字段会让"其它选项被悄悄丢掉"逃过检查。
    #[derive(Default)]
    struct RecProc(Mutex<Vec<String>>);
    impl crate::services::ProcessRunner for RecProc {
        fn run(&self, spec: &crate::services::ProcSpawn<'_>) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(format!(
                "run:{}:{}:cwd={}:verb={}:show={}",
                spec.cmd,
                spec.args.join(","),
                spec.cwd,
                spec.verb,
                spec.show
            ));
            Ok(())
        }
        fn shell(&self, cmdline: &str, flags: &[String], cwd: &str) -> anyhow::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("shell:{cmdline}:{}:cwd={cwd}", flags.join("|")));
            Ok(())
        }
    }

    #[test]
    fn proc_run_dispatches_cmd_and_args() {
        let rec = Arc::new(RecProc::default());
        let mut svc = Services::new();
        svc.proc = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);
        fn_run(&ctx, &["notepad.exe".into(), "a.txt".into()]).unwrap();
        fn_shell(&ctx, &["echo hi".into()]).unwrap();
        fn_shell(&ctx, &["echo hi".into(), "term,pwsh".into()]).unwrap();
        // 具名参数直达服务层（不写则为空串 = 交宿主定默认）。
        fn_run_named(
            &ctx,
            &["dict.exe".into()],
            &[
                ("cwd".into(), "D:/Dict".into()),
                ("verb".into(), "runas".into()),
                ("show".into(), "min".into()),
            ],
        )
        .unwrap();
        fn_shell_named(
            &ctx,
            &["dict -q x".into()],
            &[("cwd".into(), "D:/Dict".into())],
        )
        .unwrap();
        let log = rec.0.lock().unwrap();
        assert_eq!(log[0], "run:notepad.exe:a.txt:cwd=:verb=:show=");
        assert_eq!(log[1], "shell:echo hi::cwd=");
        assert_eq!(log[2], "shell:echo hi:term|pwsh:cwd=");
        assert_eq!(log[3], "run:dict.exe::cwd=D:/Dict:verb=runas:show=min");
        assert_eq!(log[4], "shell:dict -q x::cwd=D:/Dict");
    }

    /// 枚举型具名参数的**值**要校验白名单，且报错要列出合法取值。
    ///
    /// 这一条**不带 cfg(windows)**：verb/show 只在 Windows 生效，但校验必须跨平台
    /// 一致——短语文件跟着用户跨机器走，不能在 macOS 上写错了不报、到 Windows 才炸。
    #[test]
    fn proc_run_validates_enum_named_values_on_all_platforms() {
        let rec = Arc::new(RecProc::default());
        let mut svc = Services::new();
        svc.proc = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);

        let err = fn_run_named(&ctx, &["x.exe".into()], &[("verb".into(), "RUNAS".into())])
            .expect_err("大小写不同的动词不应被接受");
        assert!(err.to_string().contains("runas"), "{err}");
        let err = fn_run_named(&ctx, &["x.exe".into()], &[("show".into(), "tiny".into())])
            .expect_err("未知窗口状态应报错");
        assert!(err.to_string().contains("normal"), "{err}");

        // 合法值全都放行
        for v in RUN_VERBS {
            fn_run_named(&ctx, &["x.exe".into()], &[("verb".into(), (*v).into())]).unwrap();
        }
        for s in RUN_SHOWS {
            fn_run_named(&ctx, &["x.exe".into()], &[("show".into(), (*s).into())]).unwrap();
        }

        // 非法值必须在调用服务**之前**挡下：上面两次报错不该留下任何启动记录，
        // 否则就成了"报了错但程序已经用错参数启动了"。
        assert_eq!(
            rec.0.lock().unwrap().len(),
            RUN_VERBS.len() + RUN_SHOWS.len()
        );
    }

    #[test]
    fn wind_cli_splits_single_arg_and_passes_multi_verbatim() {
        let rec = Arc::new(RecSelf::default());
        let mut svc = Services::new();
        svc.proc = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);
        // 单参：按空白拆分
        fn_wind_cli(&ctx, &["schema dict disable wubi86 fl".into()]).unwrap();
        // 多参：原样传递（路径含空格不被拆散）
        fn_wind_cli(
            &ctx,
            &[
                "backup".into(),
                "create".into(),
                "D:/我的 备份/a.zip".into(),
            ],
        )
        .unwrap();
        // 空白单参：报错
        assert!(fn_wind_cli(&ctx, &["   ".into()]).is_err());
        let log = rec.0.lock().unwrap();
        assert_eq!(log[0].0, vec!["schema", "dict", "disable", "wubi86", "fl"]);
        assert_eq!(log[1].0, vec!["backup", "create", "D:/我的 备份/a.zip"]);
        // 不给具名参数时的默认：提示开着（空串=on）、无自定义文案、等待上限走默认。
        // 默认必须是「开」——这个功能存在的理由就是直通命令此前没有任何反馈。
        assert_eq!(log[0].1, "");
        assert_eq!(log[0].3, crate::services::DEFAULT_CLI_WAIT_MS);
    }

    /// `wind.cli` 的三个具名参数**原样落到 spec**，且非法值在调用服务前挡下。
    ///
    /// 变异判据：把 `check_enum("wind.cli", "toast", …)` 删掉，第二段断言转红——
    /// 那正是「toast 写错一个字母就静默变成不提示」的形态。
    #[test]
    fn wind_cli_named_feedback_params_reach_host_and_validate() {
        let rec = Arc::new(RecSelf::default());
        let mut svc = Services::new();
        svc.proc = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);

        fn_wind_cli_named(
            &ctx,
            &["dict import flypy x.yaml".into()],
            &[
                ("toast".into(), "error".into()),
                ("ok".into(), "词库已更新".into()),
                ("wait".into(), "30000".into()),
            ],
        )
        .unwrap();

        let err = fn_wind_cli_named(&ctx, &["restart".into()], &[("toast".into(), "no".into())])
            .expect_err("未知 toast 模式应报错");
        assert!(err.to_string().contains("off"), "{err}");
        let err = fn_wind_cli_named(&ctx, &["restart".into()], &[("wait".into(), "5秒".into())])
            .expect_err("非数字的 wait 应报错");
        assert!(err.to_string().contains("毫秒"), "{err}");

        let log = rec.0.lock().unwrap();
        assert_eq!(log.len(), 1, "两次非法调用都不该真的启动子进程");
        assert_eq!(log[0].1, "error");
        assert_eq!(log[0].2, "词库已更新");
        assert_eq!(log[0].3, 30_000);
    }

    /// `ui.toast` 的具名参数与顺序无关，且四类非法写法都当场报错。
    #[test]
    fn ui_toast_named_params_are_order_free_and_validated() {
        let rec = Arc::new(RecToast::default());
        let mut svc = Services::new();
        svc.notify = Some(rec.clone());
        let ctx = MemoryContext::new().with_services(svc);

        // 顺序颠倒不影响落位
        fn_ui_toast_named(
            &ctx,
            &["导入完成".into()],
            &[
                ("ms".into(), "5000".into()),
                ("pos".into(), "top_right".into()),
                ("kind".into(), "success".into()),
            ],
        )
        .unwrap();
        // 自定义色（与 kind 互斥，此处单给）
        fn_ui_toast_named(
            &ctx,
            &["完成".into()],
            &[("color".into(), "#52C41A".into())],
        )
        .unwrap();

        for bad in [
            vec![("kind".into(), "warn".into())],
            vec![("pos".into(), "right".into())],
            vec![("color".into(), "52C41A".into())],
            vec![("ms".into(), "一会儿".into())],
            // kind 与 color 都给：不猜哪个赢
            vec![
                ("kind".into(), "success".into()),
                ("color".into(), "#52C41A".into()),
            ],
        ] {
            assert!(
                fn_ui_toast_named(&ctx, &["x".into()], &bad).is_err(),
                "{bad:?} 应报错"
            );
        }

        let log = rec.0.lock().unwrap();
        assert_eq!(log.len(), 2, "非法写法一条都不该弹出去");
        assert_eq!(
            log[0],
            (
                "导入完成".into(),
                "success".into(),
                String::new(),
                "top_right".into(),
                5000
            )
        );
        assert_eq!(log[1].2, "#52C41A");
        // ms 省略 = 0 = 交给宿主取默认，而不是在这里硬写一个 2500。
        assert_eq!(log[1].4, 0);
    }

    #[test]
    fn missing_service_errors() {
        let ctx = MemoryContext::new().with_services(Services::new());
        assert!(matches!(
            fn_open(&ctx, &["x".into()]),
            Err(CmdbarError::ServiceUnavailable { .. })
        ));
        // 完全无 services
        let bare = MemoryContext::new();
        assert!(matches!(
            fn_open(&bare, &["x".into()]),
            Err(CmdbarError::ServiceUnavailable { .. })
        ));
    }
}
