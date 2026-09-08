//! 命令栏服务依赖
//!
//! 对照 Go `wind_input/internal/cmdbar/services.go`。动作函数所需的宿主副作用能力，
//! 全部以 trait 注入；任一字段可为 `None`，动作函数在缺失时返回
//! [`CmdbarError::ServiceUnavailable`](crate::error::CmdbarError)，供宿主降级。

use std::sync::Arc;

/// 剪贴板服务：`clip.copy` / `clip.paste`。
pub trait ClipboardService: Send + Sync {
    fn set_text(&self, text: &str) -> anyhow::Result<()>;
    fn get_text(&self) -> anyhow::Result<String>;
    /// 把剪贴板内容送入当前输入框（Windows 合成 Ctrl+V）。
    fn paste(&self) -> anyhow::Result<()>;
}

/// 按键模拟：`key.tap` / `key.seq` / `key.hold` / `key.release` / `key.type`。
pub trait KeyInjector: Send + Sync {
    fn tap(&self, combo: &str) -> anyhow::Result<()>;
    fn sequence(&self, combos: &[String]) -> anyhow::Result<()>;
    fn hold(&self, combo: &str) -> anyhow::Result<()>;
    fn release(&self, combo: &str) -> anyhow::Result<()>;
    fn type_text(&self, text: &str) -> anyhow::Result<()>;
}

/// 打开 URL / 程序 / 文件：`open`（及默认的 `web.search`）。
pub trait UrlOpener: Send + Sync {
    fn open(&self, target: &str) -> anyhow::Result<()>;
}

/// 一次 `proc.run` 启动请求。
///
/// **刻意用结构体而不是继续加形参**：这些选项都来自 `proc.run` 的具名参数，会随
/// 需求增长。加字段时每个实现点都会编译失败、被迫面对新选项；而多加一个形参
/// 很容易被某个实现原样忽略掉，表现为「参数写了不生效」且毫无痕迹。
///
/// 空串一律表示"未指定，用默认"。各字段的取值已在 cmdbar 层校验过白名单，
/// 宿主收到的一定是合法值（或空串）。
#[derive(Debug, Clone, Copy)]
pub struct ProcSpawn<'a> {
    /// 目标程序 / 文件 / URL。
    pub cmd: &'a str,
    /// 命令行参数，引号处理由宿主负责。
    pub args: &'a [String],
    /// 工作目录；空串 = 宿主按默认策略决定（**不是**"继承调用方当前目录"）。
    pub cwd: &'a str,
    /// ShellExecute 动词：`open`(默认) / `runas` / `edit` / `print` / `explore` / `properties`。
    /// 仅 Windows 有效，其它平台由宿主记 WARN 并忽略。
    pub verb: &'a str,
    /// 初始窗口状态：`normal`(默认) / `min` / `max` / `hidden`。
    /// 仅 Windows 有效，其它平台由宿主记 WARN 并忽略。
    pub show: &'a str,
}

impl<'a> ProcSpawn<'a> {
    /// 只给目标与参数的最简形式（其余走默认），供测试与内部调用。
    pub fn new(cmd: &'a str, args: &'a [String]) -> Self {
        ProcSpawn {
            cmd,
            args,
            cwd: "",
            verb: "",
            show: "",
        }
    }
}

/// 一次 `wind.cli` 执行请求：子命令 argv + 执行完的用户反馈策略。
///
/// **反馈策略必须随请求一起下去，不能由宿主自己拍板**：同一个 `wind.cli` 既用来跑
/// `dict import`（服务侧不会有任何提示，必须由这里报），也用来跑 `config set` /
/// `restart`（服务侧本就会弹「设置已更新」/「服务已重启」，再报一次就是双提示）。
/// 撞不撞车只有写词条的人当场知道，宿主无从判断，也不该去维护一张子命令名单。
#[derive(Debug, Clone, Copy)]
pub struct CliSpawn<'a> {
    /// 子命令 argv（不含 exe 自身）。
    pub args: &'a [String],
    /// 反馈模式：`on`(默认，成功与失败都报) / `off`(都不报) / `error`(仅失败报)。
    pub toast: &'a str,
    /// 成功文案；空 = 用 CLI 自己打印的最后一行（如「✓ 用户词库: 新增 12 · 更新 3」）。
    pub ok_text: &'a str,
    /// 等待子进程退出的上限毫秒。0 = 不等（发射后不管，此时无结果可报）。
    pub wait_ms: u64,
}

impl<'a> CliSpawn<'a> {
    /// 只给 argv 的最简形式（其余走默认），供测试与内部调用。
    pub fn new(args: &'a [String]) -> Self {
        CliSpawn {
            args,
            toast: "",
            ok_text: "",
            wait_ms: DEFAULT_CLI_WAIT_MS,
        }
    }
}

/// `wind.cli` 等待子进程的默认上限。取 10s：`dict import` 十万条量级实测在秒级，
/// 而超时并不代表失败（只是不再等），放宽的代价仅是命令线程多占一会儿。
pub const DEFAULT_CLI_WAIT_MS: u64 = 10_000;

/// 进程启动 / shell 执行：`proc.run` / `proc.shell` / `wind.cli`。
///
/// `cwd` 不是可选形参：「忘了接工作目录」的宿主会静默把 CWD 继承给子进程
/// （在 Windows 上就是前台应用的当前目录，且会随文件对话框漂移），没有任何
/// 报错——这类半接线只能靠签名本身杜绝。
pub trait ProcessRunner: Send + Sync {
    fn run(&self, spec: &ProcSpawn<'_>) -> anyhow::Result<()>;
    /// `flags` 为 `proc.shell(cmd, "flagA,flagB")` 拆出的标志集（可空）。
    /// 无 verb/show：命令行交给 shell 执行，那两个是 ShellExecute 的概念。
    fn shell(&self, cmdline: &str, flags: &[String], cwd: &str) -> anyhow::Result<()>;
    /// 以主程序自身 exe 执行 CLI 子命令（`wind.cli`）：宿主自取 exe 路径，
    /// 词条无需硬编码安装位置。默认未支持（测试/精简宿主）。
    ///
    /// 返回 `Err` 仅表示**没能跑起来或跑失败了**；子进程自己的成功/失败提示由宿主
    /// 按 `spec.toast` 决定，不经返回值（cmdbar 层没有 UI 能力）。
    fn run_self(&self, _spec: &CliSpawn<'_>) -> anyhow::Result<()> {
        anyhow::bail!("run_self: 宿主未支持")
    }
}

/// 一次 `ui.toast` 请求。字段与 `wind-ui-types` 的 `ToastPosition` / `ToastKind` 对应，
/// 但**刻意用字符串**：cmdbar 是纯逻辑 crate，不依赖 UI 类型；解析（含未知值降级）
/// 是渲染端既有的 `parse` 函数的职责，这里只做白名单校验后原样透传。
#[derive(Debug, Clone, Copy)]
pub struct ToastSpec<'a> {
    /// 文案。宿主负责压成单行并截断，防刷屏。
    pub text: &'a str,
    /// 类型：`info`(默认) / `success` / `error`，决定左侧强调条颜色。
    pub kind: &'a str,
    /// 自定义强调条颜色 `#RRGGBB` / `#RRGGBBAA`；非空时压过 `kind`。
    pub color: &'a str,
    /// 屏幕位置：`bottom_center`(默认) / `center` / `top_center` / 四角。
    pub position: &'a str,
    /// 显示时长毫秒；0 = 用默认。
    pub duration_ms: u64,
}

/// 用户可见通知：`ui.toast`。
///
/// 与 `ime.toggle` 的状态泡刻意分开：状态泡表达「输入法当前是什么状态」、跟随光标、
/// 会被独占全屏抑制；toast 表达「刚才那件事的结果」，是一次性的，不该被抑制掉。
pub trait NotifyService: Send + Sync {
    fn toast(&self, spec: &ToastSpec<'_>) -> anyhow::Result<()>;
}

/// 词库：`dict.add`。`code` 为空时由实现按当前方案规则推导。
pub trait DictService: Send + Sync {
    fn add_word(&self, text: &str, code: &str) -> anyhow::Result<()>;
}

/// IME 状态控制：`ime.toggle` / `ime.schema` / `ime.theme_cycle` / `setting.open` / `setting.web`。
pub trait ImeController: Send + Sync {
    /// 切换 IME 状态（cn-en / fullshape / layout / candwin / s2t / t2s / preedit / toolbar）。
    ///
    /// `s2t` = 简入繁出，`t2s` = 繁入简出；两者互斥，开一个自动关另一个。
    fn toggle(&self, target: &str) -> anyhow::Result<()>;
    /// 打开设置窗口的指定页面。`page` 为规范页 id（schema/input/keys/ui/dict/
    /// advanced/about），空串打开默认页。未知 id 由设置端忽略并落到默认页。
    ///
    /// `args` 是**原样直通**给设置程序的附加命令行参数（如
    /// `--schema=wubi86 --type=shadow` 定位到五笔的候选调整），空串=无附加参数。
    /// 宿主刻意不解析、不校验其内容：设置端每加一个新参数都要改一遍这里，
    /// 才是真正难维护的地方。含空白的值请自行用引号包裹。
    fn open_setting(&self, page: &str, args: &str) -> anyhow::Result<()>;
    /// 以 --web 参数启动设置 Web 版。`args` 语义同 [`Self::open_setting`]。
    fn open_setting_web(&self, page: &str, args: &str) -> anyhow::Result<()>;
    /// 切换输入方案（持久化）。
    fn set_schema(&self, id: &str) -> anyhow::Result<()>;
    /// 循环切换主题；dir="next"/"" 向后，"prev" 向前，返回新主题 ID。
    fn theme_cycle(&self, dir: &str) -> anyhow::Result<String>;
    /// 撤销最近一次上屏（`ime.undo_commit`）：按上屏历史删除对应字符数，
    /// 无历史时删 1 个。默认未支持（测试/精简宿主）。
    fn undo_commit(&self) -> anyhow::Result<()> {
        anyhow::bail!("undo_commit: 宿主未支持")
    }
    /// 上屏配对文本并激活配对状态（`ime.pair`）：插入 `left + right`、光标落在两段之间，
    /// 同时把这一层压入配对栈，使跳出键（Tab/Enter）能越过 `right`。
    ///
    /// `jump_steps` = 跳出时光标右移的格数。
    ///
    /// 与自动配对的分工：自动配对由标点按键触发、右段恒为单字符；本方法由词条显式调用，
    /// 右段可以是任意文本。**受 `input.auto_pair` 总开关约束**——关闭时由宿主退化为纯上屏
    /// （整串上屏、光标落末尾、不压栈），判定在宿主侧，本层不做。
    fn pair(&self, left: &str, right: &str, jump_steps: u32) -> anyhow::Result<()> {
        let _ = (left, right, jump_steps);
        anyhow::bail!("pair: 宿主未支持")
    }
}

/// 配置读写：`config.get` / `config.set` / `config.toggle`，key 为 YAML 路径。
pub trait ConfigService: Send + Sync {
    fn get(&self, key: &str) -> anyhow::Result<String>;
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()>;
    /// 循环切换枚举或翻转 bool，返回新值。
    fn toggle(&self, key: &str) -> anyhow::Result<String>;
}

/// 可选搜索引擎定制：默认实现合成 URL 转发给 [`UrlOpener`]，仅在宿主需要不同语义时覆盖。
pub trait SearchEngine: Send + Sync {
    fn search(&self, engine: &str, query: &str) -> anyhow::Result<()>;
}

/// 注入到 [`EvalContext`](crate::context::EvalContext) 的副作用依赖束。每个字段可为 `None`。
#[derive(Default, Clone)]
pub struct Services {
    pub clip: Option<Arc<dyn ClipboardService>>,
    pub keys: Option<Arc<dyn KeyInjector>>,
    pub open: Option<Arc<dyn UrlOpener>>,
    pub proc: Option<Arc<dyn ProcessRunner>>,
    pub dict: Option<Arc<dyn DictService>>,
    pub ime: Option<Arc<dyn ImeController>>,
    pub config: Option<Arc<dyn ConfigService>>,
    pub search: Option<Arc<dyn SearchEngine>>,
    pub notify: Option<Arc<dyn NotifyService>>,
}

impl Services {
    pub fn new() -> Self {
        Self::default()
    }
}
