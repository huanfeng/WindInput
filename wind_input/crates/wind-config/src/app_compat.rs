//! 应用兼容性规则
//!
//! 与 Go 版本 `wind_input/pkg/config/compat.go` 对齐：按进程名为特定应用提供候选窗
//! 定位 / 光标获取等兼容修正。文件格式为 TOML 的 `[[apps]]` 数组表，加载顺序：
//! 系统预置（`{data_dir}/compat.toml`）→ 定制版（`data_custom/compat.toml`）→
//! 用户覆盖（`{user_config_dir}/compat.toml`），靠后层的同进程名规则整条覆盖靠前层。

use crate::config::SmartMethod;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 默认兼容规则文件名。
pub const COMPAT_FILE_NAME: &str = "compat.toml";

/// 写回用户层 compat.toml 时的固定文件头。
///
/// 用户层由右键菜单自动管理，每次切换都会**整份重写**（TOML 序列化不保留注释），
/// 故必须在文件里就把这件事讲明白，否则用户手写的说明被吞掉时无从得知原因。
/// 完整的字段文档留在系统层 `data/compat.toml`——那份不会被程序改写。
const USER_COMPAT_HEADER: &str = "\
# 用户层应用兼容规则（覆盖 / 追加系统层 data/compat.toml）
#
# ⚠ 本文件由输入法右键菜单自动管理，每次通过菜单切换开关都会整份重写，
#   手写的注释与排版不会保留。需要长期留存的说明请写在系统层 compat.toml。
#
# 合并语义：同名进程（不区分大小写）整条覆盖系统层，系统层其余规则保留。
# 字段说明见系统层 data/compat.toml 顶部注释。

";

/// `skip_serializing_if` 用：省略默认为 false 的开关，避免写回时铺满一堆 `= false`。
fn is_false(b: &bool) -> bool {
    !*b
}

/// 候选窗首显策略：新组合的候选窗**何时**显示。
///
/// 背景：宿主插入组合内容后要 reflow 才能给出正确的光标坐标，而 reflow 需要时间
/// （实测首帧 GetTextExt 到稳定值要 85~95ms）。这三档是「快」与「准」之间的取舍。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstShowMode {
    /// 等宿主 reflow 后的权威坐标才显示。最准，代价是 85~95ms 首显延迟，
    /// 快速连打时候选窗只来得及显示几毫秒，观感「迟钝」。
    ///
    /// 2026-08-03 起不再是默认档。它的「准」有很大一部分是**碰巧**的：Excel 那类
    /// 慢宿主上它靠 `caret_pending` 的 600ms 延长兜住，宿主再慢 50ms 一样会错位
    /// （实测 Excel 需要 808ms 的那次它就没兜住）。真正解决错位的是首帧信任门，
    /// 而那条判据 `fast` 同样享有。
    Wait,
    /// 仍等坐标，但等到「可信」即放行：DLL 在首帧 reflow 期间连发几条试探坐标，
    /// 取第一条「与上一轮权威坐标不同」的采用（宿主未 reflow 时返回的正是上一轮那个
    /// 位置，一旦变化即说明新位置已就绪）。连续快速输入时更进一步——直接采信首条。
    /// 实测 EverEdit ~3ms、WPS ~11ms 出候选窗。
    ///
    /// **默认档**（2026-08-03 起）。此前不敢作默认，是因为它在焦点切换/鼠标移动光标
    /// 之后的首帧会拿一份属于别处的旧坐标去定位；首帧信任门补上这个洞之后
    /// （`caret_cache_verified`，见 `docs/redesign/candidate-window-positioning.md`
    /// 第 6 层），它在「坐标不可信」的那一刻会自动退回去等真值，其余时候保持 25ms
    /// 短兜底。实测常规连打首帧中位 7ms，焦点后首帧中位 105ms 且位置正确。
    #[default]
    Fast,
    /// 完全不等，首帧直接沿用上一次的坐标。最快，但只要光标位置变动过
    /// （手动移动、换行、文本重排）那个位置就是错的，会先错位显示再跳回。
    Instant,
}

impl FirstShowMode {
    /// 配置串 → 枚举。无法识别返回 `None`。
    ///
    /// ⚠ 2026-09-02 由「回落默认档」改为返回 `Option`：`first_show_mode` 现在有了
    /// 「跟随全局」这一档（per-app 规则的 `None`），而全局默认值本身也变成了可配置的
    /// `ui.candidate.first_show_mode`。回落动作因此只应发生在**全局层那一处**
    /// （`Self::from_config(s).unwrap_or_default()`），per-app 层认不出的值退化为
    /// 「没配」＝跟随全局——仍然满足「写错了和没写行为一致」，且不会把一个拼错的值
    /// 悄悄固化成对该应用的显式覆盖。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "wait" => Some(Self::Wait),
            "fast" => Some(Self::Fast),
            "instant" => Some(Self::Instant),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Fast => "fast",
            Self::Instant => "instant",
        }
    }
}

/// 应用独立的初始中英状态取值。
///
/// 语义是**初始值而非锁定**：进入该应用时套用，用户随后可自由手动切换，
/// 停留在该应用期间不再被改写（详见 `Coordinator::initial_chinese_mode_for`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitialMode {
    English,
    Chinese,
}

impl InitialMode {
    /// 配置串 → 枚举。无法识别返回 `None`（＝不干预），不 panic 也不回落到某一档：
    /// 「用户拼错了」与「用户想要英文」是两回事，后者必须是显式写对才成立。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "english" | "en" => Some(Self::English),
            "chinese" | "zh" => Some(Self::Chinese),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::English => "english",
            Self::Chinese => "chinese",
        }
    }
    /// 落到 `chinese_mode` / `chinese_punct` 这类布尔状态。
    pub fn is_chinese(self) -> bool {
        matches!(self, Self::Chinese)
    }
    /// 布尔 → 枚举（菜单写盘时把当前状态反写成规则用）。
    pub fn from_chinese(chinese: bool) -> Self {
        if chinese {
            Self::Chinese
        } else {
            Self::English
        }
    }
}

/// 容错反序列化 `Option<InitialMode>`：无法识别的值退化为 `None`（＝不干预）。
///
/// ⚠ 不能直接 `#[derive(Deserialize)]` 让 serde 自己认字符串：`load_file` 解析失败时
/// 返回 `None` 会**整份 compat.toml 静默跳过**，于是一个字段拼错就让该文件里所有应用的
/// 所有规则一起失效，且日志里毫无痕迹。单字段容错把爆炸半径限制在这一个字段内。
fn de_initial_mode<'de, D>(d: D) -> Result<Option<InitialMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.as_deref().and_then(InitialMode::from_config))
}

/// 容错反序列化 `Option<FirstShowMode>`：无法识别的值退化为 `None`（＝跟随全局）。
///
/// ⚠ 与 [`de_initial_mode`] 同理，不能直接让 serde 认字符串：`load_file` 解析失败会
/// **整份 compat.toml 静默跳过**，一个字段拼错就让该文件里所有应用的所有规则一起失效。
fn de_first_show_mode<'de, D>(d: D) -> Result<Option<FirstShowMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.as_deref().and_then(FirstShowMode::from_config))
}

/// 单个应用的兼容性规则。
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AppCompatRule {
    /// 进程名（不区分大小写），如 "Weixin.exe"。
    #[serde(default)]
    pub process: String,
    /// 说明（仅文档用途）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// 使用 caret rect 的 top 而非 bottom 定位候选窗。
    /// 适用于 GetTextExt 返回的 height 不稳定的 WebView 应用（如微信 Qt 输入框，
    /// height 在 1↔20px 间跳变 → bottom 漂移 ~20px，但 top 始终稳定）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub caret_use_top: bool,
    /// 拦截「组合期间上报的 caret rect 仍停在上一次组合位置」的宿主。
    ///
    /// 微信（Qt WebView）实测：用户上屏后移动光标（打空格 / 换行）再输入，它在 composition
    /// 期间报的 rect 仍是**上一次组合**的位置，与真实插入点差 136~419px。而 probe 判据 1
    /// （「≠ 上一轮权威坐标 ⇒ 已 reflow」）对此没有判断力——正确答案和陈旧值**都** ≠ 那个
    /// 基准。开启后，probe 若与「组合前宿主主动上报的空闲坐标」矛盾即判为陈旧、不予采信，
    /// 让兜底用那份空闲坐标首显（见 `Coordinator::handle_caret_probe`）。
    ///
    /// ⚠ **必须逐宿主开启，不能做成全局默认**。曾试过按位置关系写一条通用判据，被真机连
    /// 推翻三次（字宽、换行、终端重排）。根因是两类宿主的正确答案**恰好相反**：
    ///   - 微信：probe 陈旧、组合前缓存新   ⇒ 该信缓存
    ///   - WindTerm：probe 是重排后的新位置、缓存已过时 ⇒ 该信 probe
    /// 同一份位置关系推不出该信谁，任何位置判据都不可能同时答对两者。这是宿主缺陷，
    /// 按宿主处理——与隔壁 `caret_use_top`（同样为微信而加）同一个理由。
    #[serde(default, skip_serializing_if = "is_false")]
    pub stale_probe_guard: bool,
    /// 候选窗首显策略；`None` = 不干预，跟随全局 `ui.candidate.first_show_mode`。
    ///
    /// 三档互斥——做成枚举而不是几个 bool：布尔开关可以同时打开，实测就因此出过一次
    /// 「fast 配了却从未生效」（instant 优先、抢先放行，fast 的判据根本没机会跑），
    /// 日志里 630 条试探坐标一条没被消费。互斥语义要由类型保证。
    ///
    /// **必须是 `Option`**（2026-09-02，同 `initial_mode` 的理由）：全局默认档一旦可配，
    /// 「没配过这个应用」与「显式给这个应用配了 fast」就必须能区分，否则用户改了全局默认，
    /// 所有从未配过的应用会照旧被当成显式 fast——per-app 覆盖凭空长出来，且无从撤销。
    #[serde(
        default,
        deserialize_with = "de_first_show_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_show_mode: Option<FirstShowMode>,
    /// 进入本应用时的初始中英状态；`None` = 不干预，沿用全局逻辑。
    ///
    /// **必须是 `Option` 不能是 `bool`**：`#[serde(default)]` 下的 bool 会让所有未配置
    /// 规则的应用都拿到 `false`，等于给全世界配了「初始英文」。
    #[serde(
        default,
        deserialize_with = "de_initial_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial_mode: Option<InitialMode>,
    /// 进入本应用时的初始中英标点；`None` = 不干预。
    ///
    /// 显式值**压过** `input.punct.follow_mode` 的推导，否则用户配了它却恰好开着
    /// follow_mode 时会完全无效且无痕迹。
    #[serde(
        default,
        deserialize_with = "de_initial_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial_punct: Option<InitialMode>,
    /// 该进程加入 HostRender 白名单（受限宿主如 Win11 开始菜单 SearchHost.exe，候选窗由
    /// 服务进程渲染后经共享内存转交宿主进程内的 DLL 上屏，绕开普通窗口盖不过的 Band 层级）。
    ///
    /// 原为独立的 `config.toml` 全局列表 `compat.host_render_processes`，现并入按进程名
    /// 匹配的兼容规则表——与 `caret_use_top` 等字段同一套查找路径，不再是第二个真相源。
    /// 消费点须按**事件源 PID 直查** `AppCompat::host_render_processes()` 现算的白名单
    /// （`HostRenderManager::is_process_whitelisted`），不得经 `ActiveCompat` 全局焦点槽缓存
    /// ——开始菜单弹出会连带激活兄弟进程，焦点槽会被污染，详见
    /// `docs/redesign/host-render-windows-port.md` §11.2。
    #[serde(default, skip_serializing_if = "is_false")]
    pub host_render: bool,
    /// 该应用是否启用符号自动配对；`None` = 不干预，沿用全局 `input.auto_pair.*`。
    ///
    /// **必须是 `Option` 不能是 `bool`**：理由同 `initial_mode`——`#[serde(default)]` 下的
    /// bool 会让所有未配置规则的应用都拿到 `false`，等于给全世界关掉了自动配对。
    ///
    /// 典型用途是表格类宿主（Excel / WPS 表格）：配对后要把光标退回两符号之间，而它们在
    /// 「输入态」下把方向键解释成"确认单元格并移动"，光标回退无法实现（TSF `SetSelection`
    /// 路线已实测失败，见 project_pair_caret_tsf_setselection_rejected）。关掉配对是目前
    /// 唯一可行的兼容策略。
    ///
    /// ⚠ 消费点有**三条**，缺一即半截修复：`active_pairs()`（中文标点态）、
    /// `english_pairs_via_pipeline()`（英文标点流水线）、`push_english_pair_config()`
    /// （纯英文模式由 C++ `_englishPairEngine` 独立处理，协调器根本收不到那些键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pair: Option<bool>,
    /// 该应用的智能符号替换方案；`None` = 沿用全局 `input.symbol.smart_method`。
    ///
    /// `DeleteReplace`（全局默认）依赖对宿主做删改，在 Tabby 一类终端上会出严重错误；
    /// `HoldComposition` 全程不做删改、兼容性更好。两者本就是现成的全局枚举，这里只是
    /// 让它可以按宿主覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart_method: Option<SmartMethod>,
    /// 光标坐标水平校正（dp，96dpi 基准逻辑像素，正=右）。
    ///
    /// 用于宿主报告的 caret 坐标**系统性偏移**的场景（如 Windows Terminal，其它输入法
    /// 同样偏），与主题里的候选窗偏移不是一回事：那个是候选窗相对光标的**布局**（样式层），
    /// 这个修的是光标坐标本身（兼容层），故候选窗/状态气泡/HUD 等所有消费者一并受益。
    ///
    /// 单位是 dp 而非物理像素：宿主上报的 caret 坐标是物理像素，同一份配置若直接按物理
    /// 像素相加，在不同缩放的显示器（尤其多屏混插 100%/150%/200%）上观感会不一致——按
    /// 目标点所在显示器的 DPI 换算成物理像素在协调器侧完成（`apply_caret_compat`），
    /// 本字段本身只管「用户想要的视觉量」。
    ///
    /// 用 `i32` 而非 `Option`：0 就是"不偏移"，语义无歧义，不存在 bool 那种"默认值污染"。
    ///
    /// ⚠ 消费点有**两处**（`apply_focus_caret` / `handle_caret_update`），与 `caret_use_top`
    /// 同层同处；漏一处的症状是「有时生效有时不生效」。
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub caret_offset_x: i32,
    /// 光标坐标垂直校正（dp，96dpi 基准逻辑像素，正=下）。语义见 [`Self::caret_offset_x`]。
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub caret_offset_y: i32,
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

/// 在一组规则上就地修改指定进程的**某一个**字段。
///
/// 纯函数（不碰文件系统），故可直接单测——本仓凡涉 `%APPDATA%` 落盘的逻辑都要这样
/// 抽出来，否则端到端测试会真写用户配置目录（见 project_dict_override_sparse_merge 的教训）。
///
/// 进程名不区分大小写匹配；命中则只改 `edit` 触碰的字段、其余保持不动；未命中则**追加**
/// 一条只带该字段的新规则（不是整表快照，避免把系统层的其它字段冻结进用户层）。
pub fn upsert_rule(
    rules: &mut Vec<AppCompatRule>,
    process: &str,
    edit: impl FnOnce(&mut AppCompatRule),
) {
    let key = process.to_ascii_lowercase();
    for r in rules.iter_mut() {
        if r.process.to_ascii_lowercase() == key {
            edit(r);
            return;
        }
    }
    let mut fresh = AppCompatRule {
        process: process.to_string(),
        ..Default::default()
    };
    edit(&mut fresh);
    rules.push(fresh);
}

/// 在一组规则上设置指定进程的首显策略（`None` = 清除规则，回到跟随全局）。语义见 [`upsert_rule`]。
pub fn set_first_show_mode(
    rules: &mut Vec<AppCompatRule>,
    process: &str,
    mode: Option<FirstShowMode>,
) {
    upsert_rule(rules, process, |r| r.first_show_mode = mode);
}

/// 在一组规则上设置指定进程的初始中英状态（`None` = 清除规则，回到跟随全局）。
pub fn set_initial_mode(rules: &mut Vec<AppCompatRule>, process: &str, mode: Option<InitialMode>) {
    upsert_rule(rules, process, |r| r.initial_mode = mode);
}

/// 在一组规则上设置指定进程的初始中英标点（`None` = 清除规则，回到跟随全局）。
pub fn set_initial_punct(rules: &mut Vec<AppCompatRule>, process: &str, mode: Option<InitialMode>) {
    upsert_rule(rules, process, |r| r.initial_punct = mode);
}

/// 在一组规则上设置指定进程是否加入 HostRender 白名单。语义见 [`upsert_rule`]。
pub fn set_host_render(rules: &mut Vec<AppCompatRule>, process: &str, enabled: bool) {
    upsert_rule(rules, process, |r| r.host_render = enabled);
}

/// 在一组规则上设置指定进程的符号自动配对开关（`None` = 清除规则，回到跟随全局）。
pub fn set_auto_pair(rules: &mut Vec<AppCompatRule>, process: &str, enabled: Option<bool>) {
    upsert_rule(rules, process, |r| r.auto_pair = enabled);
}

/// 在一组规则上设置指定进程的智能符号替换方案（`None` = 清除规则，回到跟随全局）。
pub fn set_smart_method(
    rules: &mut Vec<AppCompatRule>,
    process: &str,
    method: Option<SmartMethod>,
) {
    upsert_rule(rules, process, |r| r.smart_method = method);
}

/// 在一组规则上设置指定进程的光标坐标校正偏移（dp，正=右/下；0 = 不偏移）。
pub fn set_caret_offset(rules: &mut Vec<AppCompatRule>, process: &str, dx: i32, dy: i32) {
    upsert_rule(rules, process, |r| {
        r.caret_offset_x = dx;
        r.caret_offset_y = dy;
    });
}

/// 把规则集渲染成用户层 compat.toml 全文（含固定文件头）。纯函数，便于单测断言产物。
///
/// ⚠ `initial_mode_scope` 必须原样带回：本函数是**整份重写**，漏掉哪一段哪一段就没了。
/// 用户手写的 `[[initial_mode_scope]]` 覆盖会在下一次菜单开关时静默消失，而那种缺陷
/// 「配置改了不生效」的现场与写回路径隔着一次重启，极难归因。
pub fn render_user_compat(
    rules: &[AppCompatRule],
    initial_mode_scope: &[InitialModeScopeRule],
) -> Result<String, toml::ser::Error> {
    let file = AppCompatFile {
        apps: rules.to_vec(),
        initial_mode_scope: initial_mode_scope.to_vec(),
    };
    Ok(format!("{USER_COMPAT_HEADER}{}", toml::to_string(&file)?))
}

/// 就地修改用户层 compat.toml 中指定进程的规则（load-modify-save）。
///
/// 只读写**用户层**：系统层 `data/compat.toml` 不受影响（合并时用户层同名进程整条覆盖它）。
/// 文件或目录不存在时自动创建；解析失败按空规则集处理（宁可重建也不要把菜单卡死，
/// 用户手改坏了 TOML 时仍能通过菜单恢复到可用状态）。
pub fn update_user_rule(
    user_dir: &Path,
    process: &str,
    edit: impl FnOnce(&mut AppCompatRule),
) -> Result<(), std::io::Error> {
    let path = user_dir.join(COMPAT_FILE_NAME);
    let mut file = load_file(&path).unwrap_or_default();
    upsert_rule(&mut file.apps, process, edit);
    // initial_mode_scope 原样透传：菜单只管 [[apps]]，另一段不属于它，不能顺手抹掉。
    let text = render_user_compat(&file.apps, &file.initial_mode_scope)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::create_dir_all(user_dir)?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// 设置用户层 compat.toml 中指定进程的首显策略（`None` = 清除规则，回到跟随全局）。
/// 语义见 [`update_user_rule`]。
pub fn set_user_first_show_mode(
    user_dir: &Path,
    process: &str,
    mode: Option<FirstShowMode>,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| r.first_show_mode = mode)
}

/// 设置用户层 compat.toml 中指定进程的初始中英状态（`None` = 清除规则）。
pub fn set_user_initial_mode(
    user_dir: &Path,
    process: &str,
    mode: Option<InitialMode>,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| r.initial_mode = mode)
}

/// 设置用户层 compat.toml 中指定进程的初始中英标点（`None` = 清除规则）。
pub fn set_user_initial_punct(
    user_dir: &Path,
    process: &str,
    mode: Option<InitialMode>,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| r.initial_punct = mode)
}

/// 设置用户层 compat.toml 中指定进程是否加入 HostRender 白名单。
pub fn set_user_host_render(
    user_dir: &Path,
    process: &str,
    enabled: bool,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| r.host_render = enabled)
}

/// 设置用户层 compat.toml 中指定进程的符号自动配对开关（`None` = 清除规则）。
pub fn set_user_auto_pair(
    user_dir: &Path,
    process: &str,
    enabled: Option<bool>,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| r.auto_pair = enabled)
}

/// 设置用户层 compat.toml 中指定进程的智能符号替换方案（`None` = 清除规则）。
pub fn set_user_smart_method(
    user_dir: &Path,
    process: &str,
    method: Option<SmartMethod>,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| r.smart_method = method)
}

/// 设置用户层 compat.toml 中指定进程的光标坐标校正偏移（像素，正=右/下）。
pub fn set_user_caret_offset(
    user_dir: &Path,
    process: &str,
    dx: i32,
    dy: i32,
) -> Result<(), std::io::Error> {
    update_user_rule(user_dir, process, |r| {
        r.caret_offset_x = dx;
        r.caret_offset_y = dy;
    })
}

/// 「初始模式作用域」规则：某进程的 per-app **初始模式**只在哪些**窗口类**上重算。
///
/// 为什么需要它：per-app 规则（`[[apps]]`）的身份是**进程映像名**，而 `explorer.exe`
/// 一个名字同时承载语义相反的两类焦点——桌面是「停留型」，任务栏 / Alt+Tab / 任务视图 /
/// 溢出区 / 资源管理器是「路过或另有用途」。用户为桌面配 `initial_mode = "english"`
/// 时，那些窗口会被一并命中，而它们恰是每次切换应用的必经之路。
/// 实测样本（2026-08-18）：非桌面焦点 169 次、桌面 12 次，**14:1**。
///
/// ★★★ **判据方向必须是白名单「规则在哪生效」，不能是黑名单「哪些是过渡窗口」**。
/// 本段初版正是黑名单（`shell_transient`，列出任务栏那几个类），当天即被实测推翻：
///   17:24:08.579  Client connected to bridge pipe        ← explorer 新起一个 TSF 连接
///   17:24:08.581  handle_focus_gained token=…0002  caret src=last_known
///   17:24:08.583  语言栏图标已发布 label=英             ← 闪
/// 新连接的头一个 focus_gained 拿不到窗口类（TSF 此刻还没有 view，连 caret 都退到
/// last_known），空类名不在黑名单里 ⇒ 判成「不是过渡窗口」⇒ 套上 explorer 的英文规则。
/// 黑名单还有第二个失效面：Windows 每个版本都在新增 XAML 岛窗口类，漏一个就套错一次。
/// 反过来做白名单，两种失效同时消失——「不知道在哪」与「新出现的类」都自动落在作用域外，
/// 也就是**保持现状**，而保持现状恰是这两种情况下唯一安全的答案。
///
/// ⚠ **刻意独立成段，不做成 `[[apps]]` 的字段**：`merge_rules` 是「同名进程整条覆盖」
/// 而非字段级合并，塞进 apps 的话，用户只要为 `explorer.exe` 写任何一条自己的规则
/// （哪怕只调 `caret_offset`），内置的作用域清单就会整条消失、症状复发且极难查。
/// 两段各自独立合并，互不牵连。同类前车之鉴见 project_dict_override_sparse_merge。
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct InitialModeScopeRule {
    /// 进程映像名（不区分大小写），如 `explorer.exe`。
    pub process: String,
    /// 说明（仅文档用途）。与 `AppCompatRule::comment` 同理**必须存在于结构体里**：
    /// serde 默认静默忽略未知字段，只声明在 TOML 注释里的话，用户层写回时会被丢掉。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// 该进程下**允许重算初始模式**的顶层窗口类名（不区分大小写）。
    /// 空清单 = 该进程的初始模式规则在任何窗口上都不重算。
    #[serde(default)]
    pub classes: Vec<String>,
}

/// 所有应用兼容性规则 + 运行时查找表。
#[derive(Debug, Clone, Default)]
pub struct AppCompat {
    apps: Vec<AppCompatRule>,
    /// 小写进程名 → `apps` 下标。
    lookup: HashMap<String, usize>,
    /// 小写进程名 → 该进程允许重算初始模式的窗口类名集合（小写）。
    /// **进程不在表内 = 不受限制**（绝大多数应用走这条路，零行为变化）。
    mode_scope: HashMap<String, std::collections::HashSet<String>>,
}

/// 序列化中间体：承载 TOML 的两个顶层数组表，避免把 `lookup` 暴露给 TOML。
#[derive(Debug, Deserialize, Serialize, Default)]
struct AppCompatFile {
    #[serde(default)]
    apps: Vec<AppCompatRule>,
    /// ⚠ 用户层 compat.toml 由右键菜单**整份重写**（`render_user_compat`），本字段
    /// 必须一并渲染回去，否则用户写的覆盖会在下一次菜单开关时被静默删掉。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    initial_mode_scope: Vec<InitialModeScopeRule>,
}

impl AppCompat {
    /// 从一组规则构建（含查找表）。作用域清单为空 ⇒ 所有进程都不受限。
    pub fn from_rules(apps: Vec<AppCompatRule>) -> Self {
        Self::from_parts(apps, Vec::new())
    }

    /// 从两段规则构建（含查找表）。
    pub fn from_parts(apps: Vec<AppCompatRule>, scope: Vec<InitialModeScopeRule>) -> Self {
        let mut c = AppCompat {
            apps,
            lookup: HashMap::new(),
            mode_scope: HashMap::new(),
        };
        c.build_lookup();
        c.build_mode_scope(scope);
        c
    }

    /// 该焦点窗口是否落在「允许重算 per-app 初始模式」的作用域内。
    ///
    /// 返回 false 表示调用方应**保持现状**（不重算初始模式、不推进模式归属）。
    ///
    /// 三种取值来源，必须一并理解：
    /// - 该进程**没配**作用域 ⇒ true。绝大多数应用走这条，行为与本机制引入前完全一致。
    /// - 配了且窗口类命中 ⇒ true。
    /// - 配了但窗口类不命中，**含窗口类为空** ⇒ false。空类名是「拿不到窗口标识」，
    ///   不是「窗口不在清单里」，但对本函数要回答的问题，两者的正确答案都是「别重算」。
    ///   曾把空类名当作「不是过渡窗口」放行，实测当场闪英，见 `InitialModeScopeRule` 注释。
    pub fn initial_mode_applies_to_window(&self, process_name: &str, window_class: &str) -> bool {
        match self.mode_scope.get(&process_name.to_ascii_lowercase()) {
            None => true,
            Some(set) => {
                !window_class.is_empty() && set.contains(&window_class.to_ascii_lowercase())
            }
        }
    }

    /// 按进程名（不区分大小写）查规则，未匹配返回 None。
    pub fn get_rule(&self, process_name: &str) -> Option<&AppCompatRule> {
        self.lookup
            .get(&process_name.to_ascii_lowercase())
            .map(|&i| &self.apps[i])
    }

    /// 现算 HostRender 白名单：所有 `host_render = true` 的进程名（原始大小写）。
    ///
    /// 供 `HostRenderManager::set_whitelist` 消费；调用方须按事件源 PID 直查，
    /// 不得经 `ActiveCompat` 全局焦点槽缓存，理由见 [`AppCompatRule::host_render`]。
    pub fn host_render_processes(&self) -> Vec<String> {
        self.apps
            .iter()
            .filter(|r| r.host_render)
            .map(|r| r.process.clone())
            .collect()
    }

    fn build_lookup(&mut self) {
        self.lookup = self
            .apps
            .iter()
            .enumerate()
            .map(|(i, r)| (r.process.to_ascii_lowercase(), i))
            .collect();
    }

    fn build_mode_scope(&mut self, rules: Vec<InitialModeScopeRule>) {
        self.mode_scope = rules
            .into_iter()
            .filter(|r| !r.process.is_empty())
            .map(|r| {
                (
                    r.process.to_ascii_lowercase(),
                    r.classes
                        .iter()
                        .map(|c| c.to_ascii_lowercase())
                        .collect::<std::collections::HashSet<_>>(),
                )
            })
            .collect();
    }

    /// 加载兼容规则：系统层（`{data_dir}/compat.toml`）+ 定制层
    /// （`data_custom/compat.toml`）+ 用户层覆盖（`{user_dir}/compat.toml`）。
    /// 任一文件缺失/解析失败均静默跳过。
    ///
    /// 定制层由 [`crate::config::Config::custom_data_dir`] 决定（清单在场才有），不经参数传入：本函数
    /// 有五个跨 crate 调用点，而定制层的位置是进程级事实、不随调用方的 `data_dir` 变。
    /// 测试需要指定定制层时用 [`Self::load_layered`]。
    pub fn load(data_dir: Option<&Path>, user_dir: Option<&Path>) -> Self {
        Self::load_layered(
            data_dir,
            crate::config::Config::custom_data_dir().as_deref(),
            user_dir,
        )
    }

    /// 三层显式加载，层序 `data < data_custom < user`，靠后者覆盖靠前者。
    pub fn load_layered(
        data_dir: Option<&Path>,
        custom_dir: Option<&Path>,
        user_dir: Option<&Path>,
    ) -> Self {
        let mut apps: Vec<AppCompatRule> = Vec::new();
        let mut scope: Vec<InitialModeScopeRule> = Vec::new();
        if let Some(d) = data_dir
            && let Some(sys) = load_file(&d.join(COMPAT_FILE_NAME))
        {
            apps = sys.apps;
            scope = sys.initial_mode_scope;
        }
        // ⚠️ 两段**各自独立**合并（下同）：用户/定制者为某进程写 [[apps]] 规则不会连带
        // 丢掉更低层给该进程配的 [[initial_mode_scope]]，反之亦然。合并语义相同
        // （同名进程整条覆盖），但**刻意不合并成一段** —— 见 `merge_mode_scope` 的文档。
        if let Some(c) = custom_dir
            && let Some(custom) = load_file(&c.join(COMPAT_FILE_NAME))
        {
            apps = merge_rules(apps, custom.apps);
            scope = merge_mode_scope(scope, custom.initial_mode_scope);
        }
        if let Some(u) = user_dir
            && let Some(user) = load_file(&u.join(COMPAT_FILE_NAME))
        {
            apps = merge_rules(apps, user.apps);
            scope = merge_mode_scope(scope, user.initial_mode_scope);
        }
        Self::from_parts(apps, scope)
    }
}

/// 解析单个 compat.toml；文件不存在或解析失败返回 None。
fn load_file(path: &Path) -> Option<AppCompatFile> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str::<AppCompatFile>(&text).ok()
}

/// 合并两组规则：user 中同名进程（不区分大小写）覆盖 base，其余 base 规则保留，
/// 末尾追加全部 user 规则（与 Go `mergeCompatRules` 对齐）。
fn merge_rules(base: Vec<AppCompatRule>, user: Vec<AppCompatRule>) -> Vec<AppCompatRule> {
    if user.is_empty() {
        return base;
    }
    let user_keys: std::collections::HashSet<String> = user
        .iter()
        .map(|r| r.process.to_ascii_lowercase())
        .collect();
    let mut merged: Vec<AppCompatRule> = base
        .into_iter()
        .filter(|r| !user_keys.contains(&r.process.to_ascii_lowercase()))
        .collect();
    merged.extend(user);
    merged
}

/// 合并两组初始模式作用域规则：语义与 [`merge_rules`] 完全一致（同名进程整条覆盖）。
///
/// 「整条覆盖」意味着用户想在内置清单上**增删一项**时要把整份 `classes` 抄一遍。
/// 这是刻意与 `[[apps]]` 保持一致——两段用两套合并语义会更难解释，而系统层
/// `data/compat.toml` 里已把内置值完整列出，抄一遍的成本很低。
fn merge_mode_scope(
    base: Vec<InitialModeScopeRule>,
    user: Vec<InitialModeScopeRule>,
) -> Vec<InitialModeScopeRule> {
    if user.is_empty() {
        return base;
    }
    let user_keys: std::collections::HashSet<String> = user
        .iter()
        .map(|r| r.process.to_ascii_lowercase())
        .collect();
    let mut merged: Vec<InitialModeScopeRule> = base
        .into_iter()
        .filter(|r| !user_keys.contains(&r.process.to_ascii_lowercase()))
        .collect();
    merged.extend(user);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个新增 per-app 字段的解析。`auto_pair` / `smart_method` 是 `Option`，
    /// **未配置必须是 `None` 而不是 `Some(false)`/`Some(默认值)`**——那是「跟随全局」
    /// 与「显式关掉」的分界，退化成 bool 就等于给所有未配规则的应用都关掉了功能。
    #[test]
    fn parse_per_app_pair_symbol_and_offset() {
        let toml = r#"
            [[apps]]
            process = "EXCEL.EXE"
            auto_pair = false
            caret_offset_x = -2
            caret_offset_y = 3

            [[apps]]
            process = "Tabby.exe"
            smart_method = "hold_composition"

            [[apps]]
            process = "plain.exe"
            caret_use_top = true
        "#;
        let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(toml).unwrap().apps);

        let excel = compat.get_rule("excel.exe").unwrap();
        assert_eq!(excel.auto_pair, Some(false));
        assert_eq!((excel.caret_offset_x, excel.caret_offset_y), (-2, 3));
        assert_eq!(excel.smart_method, None, "未配 = 跟随全局");

        let tabby = compat.get_rule("TABBY.EXE").unwrap();
        assert_eq!(tabby.smart_method, Some(SmartMethod::HoldComposition));
        assert_eq!(tabby.auto_pair, None, "未配 = 跟随全局");

        // 只配了别的字段的规则，三个新字段必须全部是"不干预"，不能被默认值污染。
        let plain = compat.get_rule("plain.exe").unwrap();
        assert_eq!(plain.auto_pair, None);
        assert_eq!(plain.smart_method, None);
        assert_eq!((plain.caret_offset_x, plain.caret_offset_y), (0, 0));
    }

    /// 写回只落被触碰的字段，且 `None`/0 不进 TOML（`skip_serializing_if`）——
    /// 否则用户层会把「未配置」冻结成显式值，日后改全局默认对老用户静默失效。
    #[test]
    fn per_app_writeback_is_sparse() {
        let mut rules = Vec::new();
        set_auto_pair(&mut rules, "EXCEL.EXE", Some(false));
        set_caret_offset(&mut rules, "WindowsTerminal.exe", 0, -4);

        let out = render_user_compat(&rules, &[]).unwrap();
        assert!(out.contains("auto_pair = false"));
        assert!(out.contains("caret_offset_y = -4"));
        // dx 为 0：不落盘。
        assert!(
            !out.contains("caret_offset_x"),
            "0 偏移不应写进 TOML: {out}"
        );
        // 没碰过的字段一律不出现。
        assert!(!out.contains("smart_method"), "未触碰字段不应落盘: {out}");
        assert!(!out.contains("caret_use_top"), "未触碰字段不应落盘: {out}");

        // 清除规则（回到跟随全局）后该字段整个消失，而不是写成 auto_pair = true。
        set_auto_pair(&mut rules, "EXCEL.EXE", None);
        let cleared = render_user_compat(&rules, &[]).unwrap();
        assert!(
            !cleared.contains("auto_pair"),
            "清除后不应残留该键: {cleared}"
        );
    }

    #[test]
    fn parse_apps_array_and_lookup_case_insensitive() {
        let toml = r#"
            [[apps]]
            process = "Weixin.exe"
            comment = "微信"
            caret_use_top = true
        "#;
        let file: AppCompatFile = toml::from_str(toml).unwrap();
        let compat = AppCompat::from_rules(file.apps);

        // 进程名匹配不区分大小写。
        let rule = compat
            .get_rule("weixin.exe")
            .expect("应命中 Weixin.exe 规则");
        assert!(rule.caret_use_top);
        assert!(compat.get_rule("WEIXIN.EXE").unwrap().caret_use_top);
        // 未配置的进程无规则。
        assert!(compat.get_rule("notepad.exe").is_none());
    }

    #[test]
    fn caret_use_top_defaults_false_when_absent() {
        let toml = r#"
            [[apps]]
            process = "Foo.exe"
        "#;
        let file: AppCompatFile = toml::from_str(toml).unwrap();
        let compat = AppCompat::from_rules(file.apps);
        let rule = compat.get_rule("foo.exe").unwrap();
        assert!(!rule.caret_use_top);
        // 缺字段 = 不干预（跟随全局），与 initial_mode 同语义。若它退化成 Some(默认档)，
        // 等于给所有未配置的应用都写死了一份 per-app 覆盖，用户改全局默认时全部失效。
        assert_eq!(rule.first_show_mode, None);
        // 缺字段 = 不干预。若这两个退化成 Some(English)，等于给所有未配置的应用
        // 都配上了「初始英文」——这正是字段必须用 Option 而非 bool 的原因。
        assert_eq!(rule.initial_mode, None);
        assert_eq!(rule.initial_punct, None);
    }

    #[test]
    fn initial_mode_parses_both_values() {
        let toml = r#"
            [[apps]]
            process = "Everything.exe"
            initial_mode = "english"
            initial_punct = "chinese"
        "#;
        let file: AppCompatFile = toml::from_str(toml).unwrap();
        let compat = AppCompat::from_rules(file.apps);
        let rule = compat.get_rule("everything.exe").unwrap();
        assert_eq!(rule.initial_mode, Some(InitialMode::English));
        assert_eq!(rule.initial_punct, Some(InitialMode::Chinese));
        assert!(!rule.initial_mode.unwrap().is_chinese());
        assert!(rule.initial_punct.unwrap().is_chinese());
    }

    /// 单字段拼错只让**该字段**退化为「不干预」，不得连累同规则的其它字段、
    /// 也不得让整份 compat.toml 解析失败（`load_file` 失败会静默跳过整个文件，
    /// 于是一个错别字就让所有应用的所有规则一起失效且毫无痕迹）。
    #[test]
    fn unknown_initial_mode_degrades_to_none_without_killing_the_file() {
        let toml = r#"
            [[apps]]
            process = "Everything.exe"
            initial_mode = "englsh"
            caret_use_top = true

            [[apps]]
            process = "Weixin.exe"
            initial_mode = "chinese"
        "#;
        let file: AppCompatFile = toml::from_str(toml).expect("拼错的值不得让整份文件解析失败");
        let compat = AppCompat::from_rules(file.apps);
        let bad = compat.get_rule("everything.exe").unwrap();
        assert_eq!(bad.initial_mode, None, "无法识别 → 不干预");
        assert!(bad.caret_use_top, "同规则的其它字段不受牵连");
        // 后续规则完整存活。
        assert_eq!(
            compat.get_rule("weixin.exe").unwrap().initial_mode,
            Some(InitialMode::Chinese)
        );
    }

    #[test]
    fn user_rules_override_system_by_process_name() {
        let base = vec![AppCompatRule {
            process: "Weixin.exe".into(),
            caret_use_top: true,
            ..Default::default()
        }];
        let user = vec![AppCompatRule {
            process: "weixin.exe".into(), // 大小写不同仍视为同进程
            caret_use_top: false,
            ..Default::default()
        }];
        let merged = AppCompat::from_rules(merge_rules(base, user));
        // 用户层关闭了 caret_use_top，应覆盖系统层。
        assert!(!merged.get_rule("Weixin.exe").unwrap().caret_use_top);
        // 合并后只剩一条（同进程去重）。
        assert_eq!(merged.apps.len(), 1);
    }

    #[test]
    fn empty_user_keeps_base() {
        let base = vec![AppCompatRule {
            process: "Weixin.exe".into(),
            caret_use_top: true,
            ..Default::default()
        }];
        let merged = merge_rules(base, vec![]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].caret_use_top);
    }

    #[test]
    fn set_mode_on_existing_rule_keeps_other_fields() {
        // 命中已有规则：只改 first_show_mode，caret_use_top / comment 不得被动。
        let mut rules = vec![AppCompatRule {
            process: "Weixin.exe".into(),
            comment: "微信".into(),
            caret_use_top: true,
            ..Default::default()
        }];
        set_first_show_mode(&mut rules, "weixin.exe", Some(FirstShowMode::Fast)); // 大小写无关
        assert_eq!(rules.len(), 1, "命中时不得追加新规则");
        assert_eq!(rules[0].first_show_mode, Some(FirstShowMode::Fast));
        assert!(rules[0].caret_use_top, "其它字段不得被连带修改");
        assert_eq!(rules[0].comment, "微信");
        // 三档互斥：再设一次直接覆盖，不存在「两档同时生效」的中间态
        // ——正是布尔开关时代那个「fast 配了却从未生效」的成因。
        set_first_show_mode(&mut rules, "Weixin.EXE", Some(FirstShowMode::Instant));
        assert_eq!(rules[0].first_show_mode, Some(FirstShowMode::Instant));
        // 第四档「跟随全局」＝清除该字段，用户才有撤销 per-app 覆盖的出路。
        set_first_show_mode(&mut rules, "Weixin.EXE", None);
        assert_eq!(rules[0].first_show_mode, None);
        assert!(rules[0].caret_use_top, "清除首显档不得连带清掉其它字段");
    }

    #[test]
    fn set_mode_appends_minimal_rule_when_absent() {
        // 未命中：追加**只带该字段**的最小规则，不做整表快照（否则会把系统层其它字段
        // 冻结进用户层，正是 project_dict_override_sparse_merge 记录过的坑）。
        let mut rules: Vec<AppCompatRule> = Vec::new();
        set_first_show_mode(&mut rules, "EverEdit.exe", Some(FirstShowMode::Fast));
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].process, "EverEdit.exe", "应保留原始大小写");
        assert_eq!(rules[0].first_show_mode, Some(FirstShowMode::Fast));
        assert!(!rules[0].caret_use_top);
    }

    #[test]
    fn mode_parses_from_config_and_rejects_unknown() {
        assert_eq!(
            FirstShowMode::from_config("fast"),
            Some(FirstShowMode::Fast)
        );
        assert_eq!(
            FirstShowMode::from_config(" INSTANT "),
            Some(FirstShowMode::Instant)
        );
        assert_eq!(
            FirstShowMode::from_config("wait"),
            Some(FirstShowMode::Wait)
        );
        // 未知值 = None。在 per-app 层它等于「没配」＝跟随全局；在全局层由调用点
        // `.unwrap_or_default()` 回落到默认档——回落只发生在一处，不再有两处独立事实。
        assert_eq!(FirstShowMode::from_config("turbo"), None);
        assert_eq!(FirstShowMode::Fast.as_config(), "fast");
    }

    /// 拼错的档位名不得让整份 compat.toml 失效，也不得固化成显式覆盖。
    #[test]
    fn unknown_mode_in_toml_degrades_to_follow_global() {
        let toml = r#"
            [[apps]]
            process = "Foo.exe"
            first_show_mode = "turbo"
            caret_use_top = true
        "#;
        let file: AppCompatFile = toml::from_str(toml).expect("单字段拼错不得整份解析失败");
        assert_eq!(file.apps[0].first_show_mode, None, "认不出 = 跟随全局");
        assert!(file.apps[0].caret_use_top, "同一条规则的其它字段仍须生效");
    }

    /// 默认档位是产品决策，单独钉一条，改动时必须显式过这一关。
    ///
    /// 2026-08-03 由 `wait` 改为 `fast`：`fast` 此前不敢作默认，是因为焦点切换/鼠标移动
    /// 光标后的首帧会拿一份属于别处的旧坐标定位；首帧信任门补上该洞后，它在坐标不可信时
    /// 会自动退回去等真值。实测常规连打首帧中位 7ms，焦点后首帧中位 105ms 且位置正确。
    #[test]
    fn default_mode_is_fast() {
        assert_eq!(FirstShowMode::default(), FirstShowMode::Fast);
    }

    /// 全局默认档现在有**两处**表达：枚举的 `#[default]`（认不出的值回落到它）与
    /// `config.toml` 出厂值 `ui.candidate.first_show_mode`（用户实际读到的那一份）。
    /// 两处分叉的表现是「配置文件写着 fast，某些路径却按另一档跑」，没有任何编译信号，
    /// 故把「必须一致」钉成不变量。顺带也验出厂串本身是个合法档位名（拼错即 None）。
    #[test]
    fn global_default_config_value_matches_enum_default() {
        let factory = crate::config::UiCandidateConfig::default().first_show_mode;
        assert_eq!(
            FirstShowMode::from_config(&factory),
            Some(FirstShowMode::default()),
            "config.toml 出厂档 {factory:?} 与枚举 #[default] 不一致"
        );
    }

    #[test]
    fn render_omits_false_flags_and_roundtrips() {
        // 渲染产物：false 开关与空 comment 全部省略（不铺 `= false`），且能被自己解析回来。
        let rules = vec![AppCompatRule {
            process: "EverEdit.exe".into(),
            first_show_mode: Some(FirstShowMode::Fast),
            ..Default::default()
        }];
        let text = render_user_compat(&rules, &[]).expect("渲染失败");
        assert!(text.contains(r#"first_show_mode = "fast""#), "产物: {text}");
        assert!(
            !text.contains("caret_use_top"),
            "false 开关不应写出: {text}"
        );
        assert!(!text.contains("comment"), "空 comment 不应写出: {text}");
        assert!(text.starts_with("# 用户层应用兼容规则"), "缺少文件头警示");

        let parsed: AppCompatFile = toml::from_str(&text).expect("产物应可解析");
        assert_eq!(parsed.apps.len(), 1);
        assert_eq!(parsed.apps[0].first_show_mode, Some(FirstShowMode::Fast));
        assert_eq!(parsed.apps[0].process, "EverEdit.exe");
    }

    #[test]
    fn set_initial_mode_upserts_and_clears() {
        let mut rules = vec![AppCompatRule {
            process: "Everything.exe".into(),
            comment: "搜索框默认英文".into(),
            caret_use_top: true,
            ..Default::default()
        }];
        // 命中：只改 initial_mode，其它字段不得被连带修改。
        set_initial_mode(&mut rules, "everything.exe", Some(InitialMode::English));
        assert_eq!(rules.len(), 1, "命中时不得追加新规则");
        assert_eq!(rules[0].initial_mode, Some(InitialMode::English));
        assert!(rules[0].caret_use_top, "其它字段不得被连带修改");
        assert_eq!(rules[0].comment, "搜索框默认英文");

        // 标点是独立维度，设置它不得动中英。
        set_initial_punct(&mut rules, "Everything.EXE", Some(InitialMode::English));
        assert_eq!(rules[0].initial_punct, Some(InitialMode::English));
        assert_eq!(rules[0].initial_mode, Some(InitialMode::English));

        // None = 清除规则，回到跟随全局（菜单的「跟随全局」档走这条）。
        set_initial_mode(&mut rules, "Everything.exe", None);
        assert_eq!(rules[0].initial_mode, None);
        assert_eq!(
            rules[0].initial_punct,
            Some(InitialMode::English),
            "只清中英"
        );

        // 未命中：追加只带该字段的最小规则。
        set_initial_mode(&mut rules, "cmd.exe", Some(InitialMode::English));
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].process, "cmd.exe", "应保留原始大小写");
        assert_eq!(rules[1].initial_mode, Some(InitialMode::English));
        assert!(!rules[1].caret_use_top);
    }

    #[test]
    fn render_omits_none_initial_mode_and_roundtrips() {
        let rules = vec![AppCompatRule {
            process: "Everything.exe".into(),
            initial_mode: Some(InitialMode::English),
            ..Default::default()
        }];
        let text = render_user_compat(&rules, &[]).expect("渲染失败");
        assert!(text.contains(r#"initial_mode = "english""#), "产物: {text}");
        assert!(!text.contains("initial_punct"), "None 字段不应写出: {text}");

        let parsed: AppCompatFile = toml::from_str(&text).expect("产物应可解析");
        assert_eq!(parsed.apps[0].initial_mode, Some(InitialMode::English));
        assert_eq!(parsed.apps[0].initial_punct, None);
    }

    #[test]
    fn host_render_defaults_false_and_omitted_from_render() {
        let toml = r#"
            [[apps]]
            process = "Foo.exe"
        "#;
        let file: AppCompatFile = toml::from_str(toml).unwrap();
        let compat = AppCompat::from_rules(file.apps);
        assert!(!compat.get_rule("foo.exe").unwrap().host_render);

        let rules = vec![AppCompatRule {
            process: "Foo.exe".into(),
            ..Default::default()
        }];
        let text = render_user_compat(&rules, &[]).expect("渲染失败");
        assert!(!text.contains("host_render"), "false 开关不应写出: {text}");
    }

    #[test]
    fn set_host_render_upserts_and_host_render_processes_collects_only_enabled() {
        let mut rules = vec![
            AppCompatRule {
                process: "Weixin.exe".into(),
                caret_use_top: true,
                ..Default::default()
            },
            AppCompatRule {
                process: "SearchHost.exe".into(),
                ..Default::default()
            },
        ];
        set_host_render(&mut rules, "searchhost.exe", true); // 大小写无关命中
        assert_eq!(rules.len(), 2, "命中时不得追加新规则");
        assert!(rules[1].host_render);
        assert!(rules[0].caret_use_top, "其它规则不受牵连");

        let compat = AppCompat::from_rules(rules);
        assert_eq!(
            compat.host_render_processes(),
            vec!["SearchHost.exe".to_string()],
            "只收集 host_render=true 的进程，且保留原始大小写"
        );
    }

    #[test]
    fn render_omits_false_host_render_but_keeps_true() {
        let rules = vec![
            AppCompatRule {
                process: "A.exe".into(),
                host_render: true,
                ..Default::default()
            },
            AppCompatRule {
                process: "B.exe".into(),
                host_render: false,
                ..Default::default()
            },
        ];
        let text = render_user_compat(&rules, &[]).expect("渲染失败");
        assert!(text.contains("host_render = true"), "产物: {text}");

        let parsed: AppCompatFile = toml::from_str(&text).expect("产物应可解析");
        let compat = AppCompat::from_rules(parsed.apps);
        assert_eq!(compat.host_render_processes(), vec!["A.exe".to_string()]);
    }

    // ── [[initial_mode_scope]] ──

    fn sample_scope() -> Vec<InitialModeScopeRule> {
        vec![InitialModeScopeRule {
            process: "explorer.exe".into(),
            comment: String::new(),
            classes: vec!["Progman".into(), "WorkerW".into()],
        }]
    }

    #[test]
    fn mode_scope_match_is_case_insensitive_on_both_keys() {
        let c = AppCompat::from_parts(Vec::new(), sample_scope());
        assert!(c.initial_mode_applies_to_window("explorer.exe", "Progman"));
        assert!(c.initial_mode_applies_to_window("EXPLORER.EXE", "progman"));
        // 作用域外：任务栏 / Alt+Tab / 溢出区 —— 保持现状
        assert!(!c.initial_mode_applies_to_window("explorer.exe", "Shell_TrayWnd"));
        assert!(!c.initial_mode_applies_to_window("explorer.exe", "ForegroundStaging"));
    }

    /// ★★★ 未配作用域的进程**完全不受影响**。这条守的是「引入本机制不会波及其它应用」，
    /// 也是把判据从黑名单反转成白名单后唯一可能出的新缺陷（把所有人都关进作用域）。
    #[test]
    fn process_without_scope_entry_is_unrestricted() {
        let c = AppCompat::from_parts(Vec::new(), sample_scope());
        assert!(c.initial_mode_applies_to_window("notepad.exe", "Notepad"));
        // 连窗口类都拿不到时也照常生效——没配作用域就没有任何限制
        assert!(c.initial_mode_applies_to_window("notepad.exe", ""));
        // 空进程名同理（macOS / 取名失败）
        assert!(c.initial_mode_applies_to_window("", ""));
    }

    /// ★★★ 本轮缺陷的钉子：作用域内的进程，窗口类为空时必须**保持现状**。
    ///
    /// 实测现场（2026-08-18 17:24:08）：explorer 新起一个 TSF 连接，其首个 focus_gained
    /// 拿不到窗口类（caret 也退到 last_known），旧的黑名单判据把空类名放行 ⇒ 套上
    /// explorer 的 `initial_mode = "english"` ⇒ 语言栏图标闪英。
    #[test]
    fn empty_window_class_stays_outside_scope() {
        let c = AppCompat::from_parts(Vec::new(), sample_scope());
        assert!(!c.initial_mode_applies_to_window("explorer.exe", ""));
    }

    /// 空 classes = 该进程的初始模式规则在任何窗口上都不重算（不是"不受限"）。
    #[test]
    fn empty_class_list_blocks_everything_for_that_process() {
        let c = AppCompat::from_parts(
            Vec::new(),
            vec![InitialModeScopeRule {
                process: "explorer.exe".into(),
                comment: String::new(),
                classes: Vec::new(),
            }],
        );
        assert!(!c.initial_mode_applies_to_window("explorer.exe", "Progman"));
        assert!(c.initial_mode_applies_to_window("notepad.exe", "Notepad"));
    }

    /// ★ 本文件最重要的一条回归：两段**独立合并**。
    ///
    /// 用户只为 explorer.exe 写了一条 `[[apps]]`（比如调 caret_offset），系统层给
    /// explorer.exe 配的 `[[initial_mode_scope]]` 必须原样保留。若哪天有人把作用域清单
    /// 挪进 `[[apps]]` 的字段里，本测试会红——那正是要防的设计退化。
    #[test]
    fn user_apps_override_does_not_drop_system_mode_scope() {
        let apps = merge_rules(
            vec![AppCompatRule {
                process: "explorer.exe".into(),
                caret_use_top: true,
                ..Default::default()
            }],
            vec![AppCompatRule {
                process: "explorer.exe".into(),
                caret_offset_x: 3,
                ..Default::default()
            }],
        );
        let scope = merge_mode_scope(sample_scope(), Vec::new());
        let c = AppCompat::from_parts(apps, scope);

        // [[apps]] 确实被用户层整条覆盖了（caret_use_top 丢失是既有的预期语义）
        let rule = c.get_rule("explorer.exe").expect("规则应存在");
        assert_eq!(rule.caret_offset_x, 3);
        assert!(!rule.caret_use_top);
        // 但 [[initial_mode_scope]] 毫发无损
        assert!(c.initial_mode_applies_to_window("explorer.exe", "Progman"));
    }

    #[test]
    fn user_mode_scope_overrides_whole_entry_for_that_process() {
        let merged = merge_mode_scope(
            sample_scope(),
            vec![InitialModeScopeRule {
                process: "EXPLORER.EXE".into(),
                comment: String::new(),
                classes: vec!["CabinetWClass".into()],
            }],
        );
        let c = AppCompat::from_parts(Vec::new(), merged);
        assert!(c.initial_mode_applies_to_window("explorer.exe", "CabinetWClass"));
        // 整条覆盖：内置那两项不再保留（语义与 [[apps]] 一致，见 merge_mode_scope 注释）
        assert!(!c.initial_mode_applies_to_window("explorer.exe", "Progman"));
    }

    /// 菜单写回是**整份重写**，漏渲染哪一段哪一段就没了。
    #[test]
    fn user_writeback_preserves_mode_scope() {
        let rules = vec![AppCompatRule {
            process: "notepad.exe".into(),
            caret_use_top: true,
            ..Default::default()
        }];
        let text = render_user_compat(&rules, &sample_scope()).expect("渲染失败");
        let parsed: AppCompatFile = toml::from_str(&text).expect("产物应可解析");
        assert_eq!(parsed.initial_mode_scope, sample_scope());

        let c = AppCompat::from_parts(parsed.apps, parsed.initial_mode_scope);
        assert!(c.initial_mode_applies_to_window("explorer.exe", "WorkerW"));
    }

    /// 没有作用域时不该在用户层文件里留下空的 `[[initial_mode_scope]]` 噪声。
    #[test]
    fn empty_mode_scope_is_not_serialized() {
        let text = render_user_compat(&[], &[]).expect("渲染失败");
        assert!(!text.contains("initial_mode_scope"), "产物: {text}");
    }

    /// 守随发布的系统层 `data/compat.toml`（路径写法同 config.rs 的既有先例）。
    ///
    /// 必要性：`load_file` 解析失败是**静默跳过整份文件**——一个 TOML 笔误就会让所有
    /// 应用的所有兼容规则一起失效，且日志里毫无痕迹。这条测试是那种缺陷唯一的早期信号。
    /// 同时钉住内置作用域的两侧：桌面在内（用户为桌面配的 initial_mode 必须生效，那正是
    /// 他们配它的目的），任务栏与"拿不到窗口类"在外。
    #[test]
    fn shipped_system_compat_parses_and_scopes_explorer_to_desktop() {
        let data_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data"));
        let file = load_file(&data_dir.join(COMPAT_FILE_NAME))
            .expect("随发布的 data/compat.toml 必须能解析（失败会静默吞掉全部规则）");
        let c = AppCompat::from_parts(file.apps, file.initial_mode_scope);

        // 桌面：规则必须照常生效
        for class in ["Progman", "WorkerW"] {
            assert!(
                c.initial_mode_applies_to_window("explorer.exe", class),
                "{class} 是桌面，排除它会让用户为桌面配的 initial_mode 彻底失效"
            );
        }
        // 作用域外：路过型窗口 + 拿不到窗口类
        for class in [
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "XamlExplorerHostIslandWindow",
            "ForegroundStaging",
            "TopLevelWindowForOverflowXamlIsland",
            "",
        ] {
            assert!(
                !c.initial_mode_applies_to_window("explorer.exe", class),
                "class={class:?} 不该重算初始模式（每次切应用的必经之路）"
            );
        }
        // 其它进程不受任何影响
        assert!(c.initial_mode_applies_to_window("notepad.exe", ""));
    }
}
