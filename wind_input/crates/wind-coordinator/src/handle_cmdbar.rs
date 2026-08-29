//! 命令栏（cmdbar）宿主集成
//!
//! 对照 Go `wind_input/internal/coordinator/cmdbar_context.go` + `cmdbar_services.go`。
//! 负责三件事：
//! 1. [`Coordinator::init_cmdbar`]：构造后装配 [`Services`] 与自身 Weak 引用；
//! 2. [`CmdbarCtx`]：把 coordinator 运行时状态适配为 [`EvalContext`]；
//! 3. 控制器（[`CoordIme`] / [`CoordDict`]）：把 cmdbar 动作映射到 coordinator 能力。
//!
//! **平台缺口**：search 留 `None`（经 open 默认可用），相关动作缺失时返回
//! ServiceUnavailable（宿主侧记 WARN 降级）；已接通 ime.toggle/ime.schema/
//! dict.add/proc/open/clip/keys/config（get/set/toggle 注册表校验 + 热重载）/
//! wind.cli（自身 exe 跑 CLI 子命令）。
//!
//! **线程/锁**：动作经独立线程执行（见 `Coordinator::spawn_command`），故控制器回调
//! 自锁的 coordinator 方法是安全的（此刻按键处理已释放 state 锁）。

use crate::coordinator::Coordinator;
use chrono::{DateTime, Local};
use std::process::Command;
use std::sync::{Arc, Weak};
use tracing::warn;
use wind_cmdbar::{
    ClipboardService, ConfigService, DictService, EvalContext, ImeController, ProcSpawn,
    ProcessRunner, Services, UrlOpener,
};
use wind_ui_types::{ToastKind, ToastPosition};

impl Coordinator {
    /// 构造后装配 cmdbar：自身 Weak 引用 + Services。一次性，幂等。
    pub(crate) fn init_cmdbar(self: &Arc<Self>) {
        let _ = self.self_weak.set(Arc::downgrade(self));
        let weak = Arc::downgrade(self);
        let mut svc = Services::new();
        svc.ime = Some(Arc::new(CoordIme(weak.clone())));
        svc.dict = Some(Arc::new(CoordDict(weak.clone())));
        // 无需 coordinator 回调的能力：进程启动、打开 URL/文件、写剪贴板（纯平台/std）。
        svc.proc = Some(Arc::new(CoordProc(weak.clone())));
        svc.open = Some(Arc::new(CoordOpener(weak.clone())));
        svc.clip = Some(Arc::new(SysClip(weak.clone())));
        // 按键合成：macOS 服务进程（LaunchAgent）无辅助功能授权无法 post CGEvent，改推 IPC 帧
        // 给 .app 侧 KeySynthesizer 合成（见 handle_cmdbar_macos）；其它平台进程内 SendInput/CGEvent。
        #[cfg(target_os = "macos")]
        {
            svc.keys = Some(crate::handle_cmdbar_macos::make_keys(weak.clone()));
        }
        #[cfg(not(target_os = "macos"))]
        {
            svc.keys = Some(Arc::new(wind_keys::key_inject::SysKeys));
        }
        // 配置读写：config.get/set/toggle 接通用户配置（注册表校验 + 热重载）。
        svc.config = Some(Arc::new(CoordConfig(weak.clone())));
        // search：经 open 默认可用，留 None。
        let _ = self.cmdbar_services.set(svc);
    }

    /// 执行一个 `$CC` 命令源：解析 → 求值 → **按列表顺序**跑动作链。
    /// type() 文本经 push 管道上屏；其余为副作用。文本上屏后稍候再跑后续副作用，
    /// 让落字先于后续按键（如 `type("「」")` 后 `key.tap("Left")` 才能把光标落到括号中间）。
    /// **必须在独立线程、未持 state 锁时调用**（控制器会回调自锁的 coordinator 方法）。
    ///
    /// 失败一律弹 toast：此前求值/动作错误只进 `warn!` 日志，用户侧是「选了没反应」的
    /// 哑失败——短语写错一个函数名或变量名，除了翻日志没有任何线索。这里是动作链的
    /// 唯一失败出口，各动作自身不再重复弹（成功回显仍归各动作，见 `cmd_dict_add`）。
    /// 整条链只弹**第一个**错误：链上后续动作往往因同一根因连环失败，逐个弹会刷屏。
    pub(crate) fn run_command_candidate(&self, src: &str, input: &str) {
        let Some(services) = self.cmdbar_services.get() else {
            return;
        };
        let (front_app, front_title, front_sel) = self.front_ctx_snapshot();
        let ctx = CmdbarCtx {
            input: input.to_string(),
            now: Local::now(),
            last: self.recent_commits_snapshot(),
            front_app,
            front_title,
            front_sel,
            services,
            host: self.host_services().clone(),
            coord: self,
        };
        let reg = wind_cmdbar::default_registry();
        let actions = match wind_cmdbar::evaluate_phrase(src, &ctx, reg) {
            Ok(wind_cmdbar::PhraseEval::Single { actions, .. }) => actions,
            // $SS 数组的动作在各元素自身选中时执行，整组选中不跑动作。
            Ok(wind_cmdbar::PhraseEval::Array(_)) => return,
            Err(e) => {
                warn!("cmdbar 命令求值失败 ({:?}): {}", src, e);
                self.show_command_error(&e.to_string());
                return;
            }
        };
        let mut text_pending = false;
        let mut first_text = true;
        // 只留第一个错误：链上后续动作常因同一根因连环失败，逐个弹 toast 会刷屏。
        let mut first_err: Option<String> = None;
        for a in &actions {
            match a.kind {
                wind_cmdbar::ActionKind::Text => match a.run(&ctx, reg) {
                    Ok(t) if !t.is_empty() => {
                        // 首次上屏前稍候：让选词返回的 ClearComposition 先到达客户端，
                        // 避免命令线程的 push 文本与清 composition 竞争（顺序错乱）。
                        if first_text {
                            std::thread::sleep(std::time::Duration::from_millis(30));
                            first_text = false;
                        }
                        self.push_commit_text(&t);
                        text_pending = true;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("cmdbar type 动作失败: {}", e);
                        first_err.get_or_insert_with(|| e.to_string());
                    }
                },
                wind_cmdbar::ActionKind::Effect => {
                    if text_pending {
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        text_pending = false;
                    }
                    if let Err(e) = a.run(&ctx, reg) {
                        warn!("cmdbar 动作失败: {}", e);
                        first_err.get_or_insert_with(|| e.to_string());
                    }
                }
            }
        }
        if let Some(msg) = first_err {
            self.show_command_error(&msg);
        }
    }

    /// 测试入口：按**用户同层**的路径执行一条命令源（如 `ime.schema("pinyin")`）。
    ///
    /// 直接调 `cmd_set_schema` 之类的内部函数验不出「这条命令是否真能走到那里」——
    /// 求值、动作分派、`Services` 装配三段都在这条链上，任一段断了内部函数照样通过。
    pub fn debug_run_command(&self, src: &str) {
        self.run_command_candidate(src, "");
    }

    /// 命令动作失败的用户可见反馈。消息取 `CmdbarError` 的 Display（形如
    /// `open: …` / `unknown function: …`），足以指认是哪个函数、什么问题。
    ///
    /// toast 是 UI 通道不落盘，不受日志隐私红线约束；但错误消息本身仍会进 `warn!`
    /// 日志，故各动作的错误文案不得携带用户输入内容（见 `handle_addword` 的说明）。
    fn show_command_error(&self, msg: &str) {
        self.show_toast(
            &format!("命令执行失败：{msg}"),
            ToastPosition::BottomCenter,
            ToastKind::Error,
        );
    }

    /// 顶码等同步场景：求值命令源，动作链**全为 Text**（无副作用）时返回拼接文本 `Some(text)`；
    /// 含任一 Effect（shell/key/clip 等需异步回调 coordinator 锁）返回 `None`，交异步 spawn 执行。
    ///
    /// **不跑任何 Effect**——纯文本求值只碰 `CmdbarCtx` 读快照（input/last/clip/now/env）与
    /// 反查表读锁，无副作用，可在持 state 锁的按键线程内安全调用。
    /// `$SS` 组 / 求值失败 / services 未装配亦返回 None。
    ///
    /// ⚠️ 「无锁」已不再成立：`dict.rev` 经 `reverse_render` 取 `self.reverse` 的**读**锁。
    /// 这与候选构建路径（`build_candidates` 在同一按键线程、同样持 state 锁时取该读锁）
    /// 是同一个加锁顺序，故不引入新的死锁面；但新增会取其它锁的 `EvalContext` 方法时
    /// 必须回到这里重新核对顺序。
    pub(crate) fn eval_command_text_only(&self, src: &str, input: &str) -> Option<String> {
        let services = self.cmdbar_services.get()?;
        let (front_app, front_title, front_sel) = self.front_ctx_snapshot();
        let ctx = CmdbarCtx {
            input: input.to_string(),
            now: Local::now(),
            last: self.recent_commits_snapshot(),
            front_app,
            front_title,
            front_sel,
            services,
            host: self.host_services().clone(),
            coord: self,
        };
        let reg = wind_cmdbar::default_registry();
        let actions = match wind_cmdbar::evaluate_phrase(src, &ctx, reg) {
            Ok(wind_cmdbar::PhraseEval::Single { actions, .. }) => actions,
            _ => return None,
        };
        // 含副作用 → None（交异步 spawn 执行，见 top_commit_command_with_remainder）。
        if actions
            .iter()
            .any(|a| a.kind != wind_cmdbar::ActionKind::Text)
        {
            return None;
        }
        // 纯文本：按序拼接（此刻 act.run 只求值文本表达式，不回调 coordinator 锁）。
        let mut text = String::new();
        for a in &actions {
            text.push_str(&a.run(&ctx, reg).ok()?);
        }
        Some(text)
    }
}

/// 命令栏求值上下文（coordinator 适配）。提供 input/now/env + 上屏历史 last + 剪贴板 clip + services；
/// sel/app/title 待前台窗口能力补齐后接入（与 Go 早期实现一致先留空）。
struct CmdbarCtx<'a> {
    input: String,
    now: DateTime<Local>,
    /// 上屏历史快照（index 0 = 最近一次），触发命令时冻结。
    last: Vec<String>,
    /// 前台上下文快照（app/title/sel），darwin 经 CMD_FRONT_CONTEXT 于聚焦时上报；
    /// 其它平台为空。触发命令时冻结。
    front_app: String,
    front_title: String,
    front_sel: String,
    services: &'a Services,
    /// 宿主服务（clip() 取剪贴板）；随构造从协调器 clone 注入。
    host: Arc<dyn crate::host_services::HostServices>,
    /// 协调器自身（`dict.rev` 的反查渲染需要引擎与反查表）。两个构造点都在
    /// `impl Coordinator` 里，直接借 `&self` 即可，无需 Weak 升级。
    coord: &'a Coordinator,
}

impl EvalContext for CmdbarCtx<'_> {
    fn input(&self) -> String {
        self.input.clone()
    }
    fn last(&self, n: i64) -> String {
        if n < 1 {
            return String::new();
        }
        self.last.get((n - 1) as usize).cloned().unwrap_or_default()
    }
    fn clip(&self, _n: i64) -> String {
        // 仅当前剪贴板（n>1 历史栈未实现）。macOS 走 pbpaste（与 SysClip::get_text 一致），
        // 让 clip() 取值与 clip.copy 写入对称——此前 macOS 硬编码返回空是 bug。
        // 读取失败（含不支持平台）降级空串：clip() 是求值上下文，报错没有落点。
        self.host.clipboard_get_text().unwrap_or_default()
    }
    fn reverse_lookup(&self, text: &str, format: &str) -> String {
        self.coord.reverse_render(text, format)
    }
    fn sel(&self) -> String {
        self.front_sel.clone()
    }
    fn app(&self) -> String {
        self.front_app.clone()
    }
    fn title(&self) -> String {
        self.front_title.clone()
    }
    fn env(&self, name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }
    fn now(&self) -> DateTime<Local> {
        self.now
    }
    fn services(&self) -> Option<&Services> {
        Some(self.services)
    }
}

/// IME 控制器：ime.toggle / ime.schema 接通；setting.* / theme_cycle 待平台能力补齐。
struct CoordIme(Weak<Coordinator>);

impl ImeController for CoordIme {
    fn toggle(&self, target: &str) -> anyhow::Result<()> {
        if let Some(c) = self.0.upgrade() {
            c.cmd_ime_toggle(target);
        }
        Ok(())
    }
    fn open_setting(&self, page: &str, args: &str) -> anyhow::Result<()> {
        if let Some(c) = self.0.upgrade() {
            c.open_settings_with(if page.is_empty() { None } else { Some(page) }, args);
        }
        Ok(())
    }
    fn open_setting_web(&self, page: &str, args: &str) -> anyhow::Result<()> {
        // web 配置已废弃，降级到 native 设置
        if let Some(c) = self.0.upgrade() {
            c.open_settings_with(if page.is_empty() { None } else { Some(page) }, args);
        }
        Ok(())
    }
    fn set_schema(&self, id: &str) -> anyhow::Result<()> {
        if let Some(c) = self.0.upgrade() {
            c.cmd_set_schema(id);
        }
        Ok(())
    }
    fn theme_cycle(&self, dir: &str) -> anyhow::Result<String> {
        match self.0.upgrade() {
            Some(c) => Ok(c.cmd_theme_cycle(dir)),
            None => Ok(String::new()),
        }
    }
    fn undo_commit(&self) -> anyhow::Result<()> {
        if let Some(c) = self.0.upgrade() {
            c.cmd_undo_commit();
        }
        Ok(())
    }
    fn pair(&self, left: &str, right: &str, jump_steps: u32) -> anyhow::Result<()> {
        if let Some(c) = self.0.upgrade() {
            c.cmd_pair_commit(left, right, jump_steps);
        }
        Ok(())
    }
}

/// 词库控制器：dict.add 接通用户词层。
struct CoordDict(Weak<Coordinator>);

impl DictService for CoordDict {
    fn add_word(&self, text: &str, code: &str) -> anyhow::Result<()> {
        if let Some(c) = self.0.upgrade() {
            c.cmd_dict_add(text, code)?;
        }
        Ok(())
    }
}

/// 进程启动：proc.run 经 TSF 侧执行（前台权限）；proc.shell 仍走本地 shell。
struct CoordProc(Weak<Coordinator>);

impl ProcessRunner for CoordProc {
    fn run(&self, spec: &ProcSpawn<'_>) -> anyhow::Result<()> {
        let dir = resolve_workdir("proc.run", spec.cmd, spec.cwd);
        // macOS：进程内直接 spawn（无需 IPC 转 TSF）；其它平台经 push_shell_exec 借前台权限。
        #[cfg(target_os = "macos")]
        {
            let _ = &self.0;
            // verb/show 是 ShellExecuteW 的概念，本平台没有对应物。**必须留 WARN**：
            // 词条能跨机器走，用户在 macOS 上写了 verb="runas" 却什么都不发生时，
            // 日志是唯一能说明「被忽略了」而不是「没生效」的线索。
            if !spec.verb.is_empty() || !spec.show.is_empty() {
                warn!(
                    "proc.run: verb/show 在本平台无效，已忽略（verb={:?} show={:?}）",
                    spec.verb, spec.show
                );
            }
            crate::handle_cmdbar_macos::run_native(spec.cmd, spec.args, &dir)
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self.0.upgrade() {
                Some(c) => c.push_shell_exec(
                    spec.cmd,
                    &shell_quote_args(spec.args),
                    &dir,
                    spec.verb,
                    spec.show,
                ),
                None => warn!("proc.run: coordinator 已释放，跳过执行 {:?}", spec.cmd),
            }
            Ok(())
        }
    }
    fn shell(&self, cmdline: &str, _flags: &[String], cwd: &str) -> anyhow::Result<()> {
        // flags(term/pwsh)暂未区分，统一走默认 shell（待平台 shell 选择补齐）。
        // 命令行是整串交给 shell 的，认不出目标程序，故默认只能落中性目录。
        shell_spawn(cmdline, &resolve_workdir("proc.shell", "", cwd))
    }
    fn run_self(&self, args: &[String]) -> anyhow::Result<()> {
        // wind.cli：以服务自身 exe 跑 CLI 子命令。CLI 进程经控制管道回连本服务
        // 执行（热重载/重建等），fire-and-forget；GUI 子系统下 spawn 无控制台闪窗。
        let exe = std::env::current_exe()?;
        std::process::Command::new(exe).args(args).spawn()?;
        Ok(())
    }
}

/// 配置服务：cmdbar `config.get` / `config.set` / `config.toggle`。
/// 写路径与 CLI `config set` 同构：注册表解析校验 → 写用户配置文件 → 热重载。
struct CoordConfig(Weak<Coordinator>);

impl CoordConfig {
    /// 读某键当前值（四层合并后），字符串裸值、其余紧凑 JSON。
    ///
    /// `require_trustworthy` = 调用方要拿这个值**当种子写回**（`toggle`）。此时本次加载
    /// 若在该键处降级过就必须报错而不是照读：降级时读到的是出厂值，翻转它写回去等于
    /// 把用户真实的设置改成「出厂值的反面」——单键版的整表抹除，同样静默、同样不可逆。
    /// 纯读（`get`）不设这道闸：显示一个出厂值不造成任何损失。
    fn load_value(key: &str, require_trustworthy: bool) -> anyhow::Result<String> {
        use wind_config::config_schema::is_known_key;
        if !is_known_key(key) {
            anyhow::bail!("未登记的配置键: {key}");
        }
        let cfg = wind_config::Config::load(wind_config::Config::data_dir().as_deref())?;
        if require_trustworthy && cfg.degradation.blocks_write_back(key, "config.toggle") {
            anyhow::bail!(
                "本次配置加载中 [{key}] 所在段解析失败并回落了出厂默认值，\
                 翻转它会覆盖你的真实设置，故拒绝；请先修好报错的配置键（见日志 WARN）。"
            );
        }
        let full = serde_json::to_value(cfg)?;
        let mut cur = &full;
        for part in key.split('.') {
            cur = cur
                .get(part)
                .ok_or_else(|| anyhow::anyhow!("配置缺少键 {key}"))?;
        }
        Ok(match cur {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }
}

impl ConfigService for CoordConfig {
    fn get(&self, key: &str) -> anyhow::Result<String> {
        Self::load_value(key, false)
    }

    fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        use wind_config::config_schema::{parse_str_value, validate};
        let v = parse_str_value(key, value).map_err(|e| anyhow::anyhow!("{e}"))?;
        validate(key, &v).map_err(|e| anyhow::anyhow!("{e}"))?;
        let parts: Vec<&str> = key.split('.').collect();
        wind_config::Config::set_user_value(&parts, v)?;
        if let Some(c) = self.0.upgrade() {
            c.reload_user_config();
        }
        Ok(())
    }

    fn toggle(&self, key: &str) -> anyhow::Result<String> {
        use wind_config::config_schema::{FieldType, field};
        let fld = field(key).ok_or_else(|| anyhow::anyhow!("未登记的配置键: {key}"))?;
        // 取当前值即取写回的种子，故必须过降级闸。
        let cur = Self::load_value(key, true)?;
        let next: String = match fld.ty {
            FieldType::Bool => (cur != "true").to_string(),
            FieldType::Enum(vals) => {
                let pos = vals.iter().position(|v| *v == cur);
                // 当前值不在枚举内（异常状态）时回落第一项。
                let next_pos = pos.map(|p| (p + 1) % vals.len()).unwrap_or(0);
                vals[next_pos].to_string()
            }
            _ => anyhow::bail!("config.toggle 仅支持 bool / 枚举键: {key}"),
        };
        self.set(key, &next)?;
        Ok(next)
    }
}

/// 将 argv 列表拼成 ShellExecuteW lpParameters 字符串，含空格/引号的参数加双引号。
/// 仅非 macOS（经 push_shell_exec 转 TSF 侧 ShellExecuteW）路径使用。
#[cfg(not(target_os = "macos"))]
fn shell_quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.contains(' ') || a.contains('"') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn shell_spawn(cmdline: &str, cwd: &str) -> anyhow::Result<()> {
    let mut c = Command::new("cmd");
    c.args(["/C", cmdline]);
    if !cwd.is_empty() {
        c.current_dir(cwd);
    }
    c.spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn shell_spawn(cmdline: &str, cwd: &str) -> anyhow::Result<()> {
    let mut c = Command::new("sh");
    c.args(["-c", cmdline]);
    if !cwd.is_empty() {
        c.current_dir(cwd);
    }
    c.spawn()?;
    Ok(())
}

/// 被启动进程落到默认工作目录的原因（供调用方拼 WARN；纯函数不直接记日志以便单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkdirFallback {
    /// 词条显式写了 cwd，但那个路径不是一个存在的目录。
    ExplicitMissing,
    /// 未显式指定，也无法从目标反推出目录（裸命令名 / URL / 协议）。
    NoTargetDir,
    /// 连主目录都定位不到——只能交回系统默认（继承调用方 CWD）。
    Unresolved,
}

/// 解析被启动进程的工作目录，并在需要时记 WARN。
///
/// 所有经 [`Coordinator::push_shell_exec`] 拉起进程的调用方都应先过这里——该通路
/// 在 TSF 侧是**由前台宿主应用进程执行**的，不定目录就等于把宿主的当前目录
/// （随文件对话框漂移）连同它的 DLL 搜索路径一起交给子进程。
pub(crate) fn resolve_workdir(func: &str, cmd: &str, explicit: &str) -> String {
    let (dir, fallback) = resolve_workdir_with(cmd, explicit, |p| p.is_dir(), home_dir);
    match fallback {
        Some(WorkdirFallback::ExplicitMissing) => {
            warn!("{func}: cwd 不是存在的目录，已改用默认工作目录 {dir:?}（cwd={explicit:?}）")
        }
        Some(WorkdirFallback::Unresolved) => {
            warn!("{func}: 定位不到默认工作目录，子进程将继承本进程当前目录（结果不确定）")
        }
        // NoTargetDir 是 URL / 裸命令名的常态，不值得每次都刷日志。
        Some(WorkdirFallback::NoTargetDir) | None => {}
    }
    dir
}

/// [`resolve_workdir`] 的纯逻辑内核：文件系统探测与主目录解析都从外部注入，
/// 便于用假映射做确定性单测（与 `cli_util::expand_with` 同款做法）。
///
/// 默认策略 = **目标程序所在目录**，等价于在资源管理器里双击它；靠相对路径找
/// 数据文件的程序（词典等）正是按这个假设写的。定位不到目标目录时退到主目录，
/// 刻意**不**用输入法自身目录：那会把第三方进程的 CWD 挂到我们的安装目录上，
/// 既可能被写入垃圾文件，也让它的 DLL 搜索路径指向我们这里。
///
/// 返回空串仅当连主目录都拿不到——空串下游即"不设置"，也就是继承调用方的当前
/// 目录。在 Windows 上那是**前台宿主应用**的 CWD（还会被文件对话框改掉），
/// 正是本函数要消除的不确定性，所以它是最后手段而非默认路径。
fn resolve_workdir_with(
    cmd: &str,
    explicit: &str,
    is_dir: impl Fn(&std::path::Path) -> bool,
    home: impl Fn() -> Option<std::path::PathBuf>,
) -> (String, Option<WorkdirFallback>) {
    let home_or_empty = |reason| match home() {
        Some(h) => (h.to_string_lossy().into_owned(), Some(reason)),
        None => (String::new(), Some(WorkdirFallback::Unresolved)),
    };

    if !explicit.trim().is_empty() {
        let p = std::path::Path::new(explicit);
        if is_dir(p) {
            return (explicit.to_string(), None);
        }
        // 写了却不存在：不能直接把它交给 ShellExecuteW（整个启动会失败），
        // 用户的意图是启动程序而非校验目录，故降级并留 WARN。
        return home_or_empty(WorkdirFallback::ExplicitMissing);
    }

    // URL / 协议目标没有"所在目录"可言；`Path::parent` 会从 `https://a/b` 切出
    // `https:/a` 这种似是而非的路径，先挡掉免得判据落到错误的维度上。
    if cmd.contains("://") {
        return home_or_empty(WorkdirFallback::NoTargetDir);
    }
    // 裸命令名（`notepad.exe`）由 ShellExecute 走 PATH 解析，此处 parent 是空串。
    match std::path::Path::new(cmd).parent() {
        Some(p) if !p.as_os_str().is_empty() && is_dir(p) => {
            (p.to_string_lossy().into_owned(), None)
        }
        _ => home_or_empty(WorkdirFallback::NoTargetDir),
    }
}

/// 用户主目录。不引新依赖：Windows 取 `USERPROFILE`，其余取 `HOME`。
fn home_dir() -> Option<std::path::PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// 打开 URL / 程序 / 文件：经 TSF 侧 ShellExecuteW 在前台应用进程中执行。
struct CoordOpener(Weak<Coordinator>);

impl UrlOpener for CoordOpener {
    fn open(&self, target: &str) -> anyhow::Result<()> {
        // macOS：进程内经 `open` CLI 拉起（无需 IPC 转 TSF）；其它平台经 push_shell_exec。
        #[cfg(target_os = "macos")]
        {
            let _ = &self.0;
            crate::handle_cmdbar_macos::open_native(target)
        }
        #[cfg(not(target_os = "macos"))]
        {
            // open 的 target 可能是 URL、文档或程序：同一套默认策略——是本地文件
            // 就落到它所在目录，否则主目录，总之不继承宿主应用的当前目录。
            let dir = resolve_workdir("open", target, "");
            match self.0.upgrade() {
                // open 不暴露 verb/show：它的语义就是"按系统默认方式打开"，
                // 需要指定动词或窗口状态时用 proc.run。
                Some(c) => c.push_shell_exec(target, "", &dir, "", ""),
                None => warn!("open: coordinator 已释放，跳过执行 {target:?}"),
            }
            Ok(())
        }
    }
}

/// 系统剪贴板服务（clip.copy / clip.get / clip.paste）。
///
/// set/get 经协调器的 [`crate::host_services::HostServices`] 注入面（桌面实现直通
/// `wind_ui::popup_menu`：Windows 走 CF_UNICODETEXT，macOS 走 `pbcopy`/`pbpaste`
/// 子进程；其它 Unix 暂无统一通道）。
/// paste 经按键注入合成粘贴热键（macOS 推 CmdKeyTap 给 .app，见 [`SysClip::paste`]）。
struct SysClip(Weak<Coordinator>);

impl ClipboardService for SysClip {
    fn set_text(&self, text: &str) -> anyhow::Result<()> {
        // 失败传播（OpenClipboard 被占用重试后仍失败等），run_actions 记 warn；
        // 菜单"复制"等 best-effort 路径仍走 UiCommand::CopyToClipboard 由 UI 侧执行。
        match self.0.upgrade() {
            Some(c) => c.host_services().clipboard_set_text(text),
            None => anyhow::bail!("clip.copy: coordinator 已释放"),
        }
    }
    fn get_text(&self) -> anyhow::Result<String> {
        match self.0.upgrade() {
            Some(c) => c.host_services().clipboard_get_text(),
            None => anyhow::bail!("clip.get: coordinator 已释放"),
        }
    }
    fn paste(&self) -> anyhow::Result<()> {
        // macOS：不合成 ⌘V，经 IMKit insertText 上屏剪贴板文本（见 handle_cmdbar_macos::paste_via_ime）。
        #[cfg(target_os = "macos")]
        {
            crate::handle_cmdbar_macos::paste_via_ime(&self.0);
            Ok(())
        }
        // Windows/Linux：沿用进程内合成 Ctrl+V（有 HID 层修饰键状态，直接生效；且保留富文本粘贴）。
        #[cfg(not(target_os = "macos"))]
        {
            let _ = &self.0;
            use wind_cmdbar::KeyInjector;
            wind_keys::key_inject::SysKeys.tap("Ctrl+v")
        }
    }
}

#[cfg(test)]
mod workdir_tests {
    use super::{WorkdirFallback, resolve_workdir_with};
    use std::path::{Path, PathBuf};

    /// 假文件系统：只有列出的路径算存在的目录。
    fn dirs(list: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| list.iter().any(|d| Path::new(d) == p)
    }

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("C:/Users/me"))
    }

    fn no_home() -> Option<PathBuf> {
        None
    }

    /// 默认策略的主用例：目标是本地程序 → 落到它自己所在目录。
    /// 词典类程序按相对路径找词库，靠的就是这个语义（等价于资源管理器双击）。
    #[test]
    fn defaults_to_directory_of_target_program() {
        let (dir, fb) = resolve_workdir_with("D:/Dict/dict.exe", "", dirs(&["D:/Dict"]), home);
        assert_eq!(dir, "D:/Dict");
        assert_eq!(fb, None);
    }

    /// 裸命令名由 ShellExecute 走 PATH 解析，反推不出目录 → 中性目录。
    /// 关键是**不能**返回空串：空串下游即继承前台宿主应用的当前目录。
    #[test]
    fn bare_command_name_falls_back_to_home_not_empty() {
        let (dir, fb) = resolve_workdir_with("notepad.exe", "", dirs(&[]), home);
        assert_eq!(dir, "C:/Users/me");
        assert_eq!(fb, Some(WorkdirFallback::NoTargetDir));
    }

    /// URL 不能走 Path::parent：`https://a/b/c` 会切出 `https://a/b` 这种似是而非的路径。
    ///
    /// ⚠️ 白名单里放的正是 `Path::parent()` 对该 URL **真会产出**的那个值——这样
    /// 一旦 `://` 短路被删掉，本例就会返回那个假路径而变红。若随便填一个对不上的
    /// 字符串，`is_dir` 恒 false 会让它无论有没有短路都绿，成为典型的假绿用例。
    #[test]
    fn url_target_does_not_go_through_path_parent() {
        const URL: &str = "https://www.zdic.net/hans/x";
        let bogus_parent = Path::new(URL)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (dir, fb) =
            resolve_workdir_with(URL, "", move |p: &Path| p == Path::new(&bogus_parent), home);
        assert_eq!(dir, "C:/Users/me", "URL 不应被当成路径拆父目录");
        assert_eq!(fb, Some(WorkdirFallback::NoTargetDir));
    }

    /// 显式 cwd 优先于默认策略。
    #[test]
    fn explicit_cwd_wins_over_target_dir() {
        let (dir, fb) = resolve_workdir_with(
            "D:/Dict/dict.exe",
            "E:/Data",
            dirs(&["D:/Dict", "E:/Data"]),
            home,
        );
        assert_eq!(dir, "E:/Data");
        assert_eq!(fb, None);
    }

    /// 显式 cwd 写错：降级而非让整个启动失败（ShellExecuteW 收到不存在的目录会整体失败）。
    #[test]
    fn missing_explicit_cwd_degrades_with_reason() {
        let (dir, fb) =
            resolve_workdir_with("D:/Dict/dict.exe", "E:/typo", dirs(&["D:/Dict"]), home);
        assert_eq!(dir, "C:/Users/me");
        assert_eq!(fb, Some(WorkdirFallback::ExplicitMissing));
    }

    /// 目标目录不存在（词条写了个错路径）时同样退到中性目录，而不是把不存在的目录传下去。
    #[test]
    fn nonexistent_target_dir_is_not_passed_through() {
        let (dir, _) = resolve_workdir_with("D:/gone/x.exe", "", dirs(&[]), home);
        assert_eq!(dir, "C:/Users/me");
    }

    /// 连主目录都拿不到才返回空串——这是最后手段，且必须报告出来。
    #[test]
    fn empty_only_when_nothing_resolvable() {
        let (dir, fb) = resolve_workdir_with("notepad.exe", "", dirs(&[]), no_home);
        assert_eq!(dir, "");
        assert_eq!(fb, Some(WorkdirFallback::Unresolved));
    }

    /// 纯空白的 cwd 视同未指定（词条里 `cwd=" "` 是笔误，不该被当成合法目录）。
    #[test]
    fn blank_explicit_cwd_is_treated_as_unset() {
        let (dir, fb) = resolve_workdir_with("D:/Dict/d.exe", "   ", dirs(&["D:/Dict"]), home);
        assert_eq!(dir, "D:/Dict");
        assert_eq!(fb, None);
    }
}
