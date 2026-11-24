//! 软键盘：常驻符号面板的状态与按键接管。
//!
//! 软键盘不组码、不产候选、不进 [`ModeKind`](crate::pipeline::ModeKind)，只把键位换成符号，
//! 与 `State.active` 正交，状态也不住在 `State` 里（见 `Coordinator::softkeyboard_active`）。
//! 设计见 `docs/design/soft-keyboard.md`。
//!
//! # 两种面
//!
//! | | 符号面（默认） | 键盘面（`send_keys`） |
//! |---|---|---|
//! | 画的是 | 键盘打不出的字符 | 键盘本来就有的键 |
//! | 点键帽 | 查表 → 上屏那个字符 | 合成一次真实按键 |
//! | 敲物理键 | 查表 → 上屏 | 完全不接管，落回常规输入链路 |
//! | 打 `nihao` | 出五个符号 | 出「你好」 |
//!
//! 键盘面上点击与物理键都要汇进常规输入链路，所以 C++ 侧也要知道当前是哪种面
//! （`STATUS_SOFT_KEYBOARD_KEYS`）：键盘面不启用软键盘总闸，否则英文模式下会「吃了不发」。
//!
//! # 谁被接管，谁不被接管（符号面）
//!
//! | 键 | 处置 |
//! |---|---|
//! | 布局里的符号键位（字母 / 数字 / 符号） | 接管 → 查表上屏；无映射则吃掉忽略 |
//! | 布局里的特殊键（Tab/Enter/退格/空格/Ins/Del/方向键） | 透传（封闭集，不可映射） |
//! | 布局之外的一切（F1–F12、小键盘、Ctrl/Alt 组合） | 透传 |
//! | Esc / 翻页键 | 面板自己的控制键 |
//!
//! 本模块不列举特殊键：不在布局表里的键一律不接管，透传是自然结果，不存在第二份清单。
//!
//! 物理键长按由系统 auto-repeat 连发 keydown，逐次走本模块，不要去抖；鼠标长按归 UI 侧
//! （设计文档 §5）。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use tracing::{debug, warn};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_ipc::protocol::{FocusLostReason, MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;

use wind_ui_types::{SoftKeyCap, UiCommand, slot_layer};

use crate::coordinator::{Coordinator, State};
use wind_config::Config;

/// 加载软键盘映射表：出厂画布 ⊕ 定制层按面覆盖 ⊕ 用户层按面覆盖。
///
/// `with_user = false` 只叠到定制层为止——设置端编辑器的「下层」（恢复默认回到哪里）。
///
/// 不走 `resolve_data_file`（整份取代）：软键盘要按面合并，只改一个键不该失去其余的面，
/// 也不该看不到出厂新增的面。故显式取各层路径，出厂铺底、逐层叠加。
pub(crate) fn load_softkeyboard_table(
    data_dir: Option<&std::path::Path>,
    with_user: bool,
) -> wind_softkeyboard::SoftKeyboardTable {
    let layers = Config::resource_layers_named_with(data_dir);
    // 出厂画布只认 data 层，缺了回落内置兜底表。定制层是叠加，不能当画布。
    let system = layers
        .iter()
        .find(|l| l.name == "data")
        .map(|l| l.path.join(wind_softkeyboard::FILE_NAME));
    let mut table = wind_softkeyboard::SoftKeyboardTable::load(system.as_deref());
    // `resource_layers_named_with` 是优先级从高到低的 `[user, custom, data]`，叠加须从低到高，
    // 故 `.rev()`；反了会让定制层盖回用户的改动。
    for layer in layers
        .iter()
        .rev()
        .filter(|l| l.name != "data" && (with_user || l.name != "user"))
    {
        let path = layer.path.join(wind_softkeyboard::FILE_NAME);
        if !path.exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match table.merge_user(&text) {
                Ok(()) => Config::log_layer_override(
                    layer.name,
                    "softkeyboard",
                    wind_softkeyboard::FILE_NAME,
                    &path,
                    // 按面合并里每一层都真的生效（不是「谁赢了」），恒是覆盖。
                    true,
                ),
                Err(e) => warn!(
                    "软键盘: [{}]层覆盖解析失败，已忽略 {}: {}",
                    layer.name,
                    path.display(),
                    e
                ),
            },
            Err(e) => warn!(
                "软键盘: [{}]层覆盖读取失败 {}: {}",
                layer.name,
                path.display(),
                e
            ),
        }
    }
    table
}

/// 虚拟键码 → 软键盘键位名。
///
/// 由 [`wind_softkeyboard::all_slots`] 反查构建，不手写第二份映射，免得与布局表分叉。
fn vk_to_slot() -> &'static HashMap<u32, &'static str> {
    static MAP: OnceLock<HashMap<u32, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::with_capacity(wind_softkeyboard::SLOT_COUNT);
        for slot in wind_softkeyboard::all_slots() {
            // 数字键不在 wind-keys 的 KEY_TABLE 里（那张表只收符号键），ASCII 直映射。
            let vk = if slot.len() == 1 && slot.as_bytes()[0].is_ascii_digit() {
                Some(0x30 + u32::from(slot.as_bytes()[0] - b'0'))
            } else {
                keymap::key_name_to_vk_with_letters(slot)
            };
            match vk {
                Some(vk) => {
                    m.insert(vk, slot);
                }
                None => {
                    // 布局里出现了 wind-keys 不认识的键名。`tests/key_name_parity.rs` 会在
                    // CI 里先一步拦下，这里只是运行期兜底。
                    warn!("软键盘: 键位名 {slot:?} 解析不出虚拟键码，该键位不可用");
                }
            }
        }
        m
    })
}

/// 送进软键盘关闭判据的焦点信号，见 [`Coordinator::softkeyboard_should_close_on`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SoftKbFocusSignal {
    /// 某宿主获得焦点；`pid` 取自 `client_token >> 32`，0 = 取不到。
    Gained { pid: u32 },
    /// 失焦，按 reason 分档（同 [`FocusLostReason`]）。
    Lost(FocusLostReason),
    /// 切走了本输入法。
    ImeDeactivated,
}

impl Coordinator {
    /// 软键盘是否开着。
    pub(crate) fn softkeyboard_is_open(&self) -> bool {
        self.softkeyboard_active.load(Ordering::Relaxed)
    }

    /// 当前面的下标（已按表长夹取）。
    pub(crate) fn softkeyboard_page_idx(&self) -> usize {
        let n = self.softkeyboard_table().len();
        if n == 0 {
            return 0;
        }
        self.softkeyboard_page.load(Ordering::Relaxed).min(n - 1)
    }

    /// 当前面是不是键盘面（按键交还输入法）。
    pub(crate) fn softkeyboard_send_keys(&self) -> bool {
        self.softkeyboard_table()
            .pages()
            .get(self.softkeyboard_page_idx())
            .is_some_and(|p| p.send_keys)
    }

    /// 当前面名（日志与 UI 用；表为空时给空串）。
    pub(crate) fn softkeyboard_page_name(&self) -> String {
        self.softkeyboard_table()
            .pages()
            .get(self.softkeyboard_page_idx())
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    /// 软键盘开合 / 换面之后要做的三件事，一处收口（关闭路径有多条，各记一次注定要漏）：
    /// 1. 推 C++（`STATUS_SOFT_KEYBOARD*` 位，吃键判定要跟上）；
    /// 2. 刷工具栏（图标高亮 = `softkeyboard_is_open()`）；
    /// 3. 记住当前面（换面才写盘，见 [`Self::save_softkeyboard_page`]）。
    ///
    /// 只能在不持 `State` 锁时调用（`notify_toolbar` 要读 state）。按键路径上由
    /// `SoftKeyboardPushOnDrop` 负责，它析构时 guard 已经还了。
    pub(crate) fn after_softkeyboard_change(&self) {
        self.push_state_update();
        self.notify_toolbar();
        self.save_softkeyboard_page();
    }

    /// 这次该不该记当前面、记什么（`None` = 不记）。判据在这里、写盘在
    /// [`Self::save_softkeyboard_page`]，分层是为了让判据可测。
    pub(crate) fn softkeyboard_page_to_persist(&self) -> Option<String> {
        // 开着时不记，见 `save_softkeyboard_page`。
        if self.softkeyboard_is_open() {
            return None;
        }
        let id = self
            .softkeyboard_table()
            .pages()
            .get(self.softkeyboard_page_idx())
            .map(|p| p.id.clone())?;
        if *self
            .softkeyboard_page_saved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            == id
        {
            return None;
        }
        Some(id)
    }

    /// 把当前面的 id 记进 `state.toml`（存 id 而非下标，见 `RuntimeState::last_softkeyboard_page`）。
    ///
    /// 只在面板关着时写、且同一面不重复写：翻页键走 auto-repeat 且刻意不去抖，开着时写会在
    /// 按键路径上高频写盘。这也正是语义（记上次用完停在哪一面）；关闭是必经路径，代价只是
    /// 开着面板直接杀进程时丢掉最后一次换面。
    fn save_softkeyboard_page(&self) {
        let Some(id) = self.softkeyboard_page_to_persist() else {
            return;
        };
        // 无 store 的实例（测试夹具）不写本机状态，这道门收在 `StateWriter::new(store.is_some(), ..)`。
        // 去重镜像即时更新、不等写成功：写失败的重试由写入器整批重放负责（`MAX_RETRIES`）。
        let saved = id.clone();
        self.state_writer.schedule("softkeyboard_page", move |rs| {
            rs.last_softkeyboard_page = saved.clone();
        });
        *self
            .softkeyboard_page_saved
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = id;
    }

    /// 开关软键盘。
    ///
    /// `page` 非空时无论开关状态都切到那一面（直通车语义），否则给两个面各配直通键会互相打架。
    /// 想关就用不带面的绑定，或 Esc。
    pub(crate) fn toggle_softkeyboard(&self, page: Option<&str>) -> KeyAction {
        match page {
            Some(id) if self.softkeyboard_is_open() => {
                if self.softkeyboard_goto(id) {
                    KeyAction::Consumed
                } else {
                    // 面不存在：退化成普通开关，别让这个键什么都不做。
                    self.close_softkeyboard()
                }
            }
            _ if self.softkeyboard_is_open() => self.close_softkeyboard(),
            _ => self.open_softkeyboard(page),
        }
    }

    /// 热键入口：先处置正在打的编码，再开关面板。
    ///
    /// 必须先收掉未上屏的编码：面板一开就接管主键区，旧候选挂着像卡住了。处置沿用切方案那套
    /// （`keys.commit_on_switch`）。空文本也要走 `InsertText`：C++ `CommitText` 空文本也会
    /// `EndComposition` 清掉宿主残留编码，只发状态更新清不掉。
    ///
    /// 内部要取 `State` 锁，只能在不持锁时调用。
    pub(crate) fn softkeyboard_hotkey(&self, page: Option<&str>) -> KeyAction {
        // 关闭方向不必处置：软键盘态下本就没有输入会话。
        if self.softkeyboard_is_open() && page.is_none() {
            return self.close_softkeyboard();
        }
        let commit = self.take_input_on_schema_switch();
        self.toggle_softkeyboard(page);
        if commit.text.is_empty() && !commit.had_pending {
            return KeyAction::Consumed;
        }
        let chinese_mode = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.chinese_mode
        };
        KeyAction::InsertText {
            text: commit.text,
            new_composition: None,
            // 软键盘不改中英模式，别让图标白刷一次。
            mode_changed: false,
            chinese_mode,
            has_new_composition: false,
        }
    }

    /// 菜单入口（无按键上下文）：开关面板；开出来了才处置正在打的编码并收候选窗。
    ///
    /// 同 [`Self::softkeyboard_hotkey`]，只是编码经 push 出口交还宿主。以「开出来了」为准：
    /// 映射表为空时开不出，不能白清用户的码。
    pub(crate) fn softkeyboard_menu_toggle(&self, page: Option<&str>) {
        let was_open = self.softkeyboard_is_open();
        self.toggle_softkeyboard(page);
        if !was_open && self.softkeyboard_is_open() {
            let commit = self.take_input_on_schema_switch();
            // `take_input_on_mode_switch` 只清状态不动 UI，收候选窗由调用方负责。
            self.notify_ui_hide();
            self.push_switch_commit(&commit);
        }
    }

    /// 开启软键盘。表为空时不开。宿主贴不了面板（见 [`Self::host_has_panels`]）时也不开、
    /// 键交还宿主，否则主键区被接管而屏上什么都没有。
    pub(crate) fn open_softkeyboard(&self, page: Option<&str>) -> KeyAction {
        if !self.host_has_panels() {
            debug!("软键盘: 宿主没声明面板能力，忽略开启请求");
            return KeyAction::PassThrough;
        }
        if self.softkeyboard_table().is_empty() {
            warn!("软键盘: 映射表为空，忽略开启请求");
            return KeyAction::Consumed;
        }
        if let Some(id) = page {
            self.softkeyboard_goto(id);
        }
        self.softkeyboard_active.store(true, Ordering::Relaxed);
        self.softkeyboard_dirty.store(true, Ordering::Relaxed);
        *self
            .softkeyboard_opened_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        // 记下它服务的宿主进程（活动 token 高 32 位即 pid，同 `is_background_sender`）。
        self.softkeyboard_host_pid.store(
            (self.push_server.active_token() >> 32) as u32,
            Ordering::Relaxed,
        );
        debug!(
            "softkeyboard: opened at page {} ({})",
            self.softkeyboard_page_idx(),
            self.softkeyboard_page_name()
        );
        // 钉住是持久偏好，UI 侧隐藏时会把按钮复位，故每次打开都把真相下发一次。
        // 排在 Show 之前：面板隐藏时 `set_pinned` 只记值不重画，Show 那一帧就画对，不闪。
        let _ = self.ui_tx.send(UiCommand::SoftKeyboardPinned(
            self.softkeyboard_pinned.load(Ordering::Relaxed),
        ));
        self.push_softkeyboard_view();
        KeyAction::Consumed
    }

    /// 关闭软键盘。
    ///
    /// 保留当前面下标，再开时停在同一面。
    pub(crate) fn close_softkeyboard(&self) -> KeyAction {
        if self.softkeyboard_active.swap(false, Ordering::Relaxed) {
            // 钉住是持久偏好，关闭不复位（UI 侧 `hide` 只复位按钮外观，下次打开再下发）。
            self.softkeyboard_dirty.store(true, Ordering::Relaxed);
            debug!("softkeyboard: closed");
            let _ = self.ui_tx.send(UiCommand::HideSoftKeyboard);
        }
        KeyAction::Consumed
    }

    /// 开关「钉住」，返回切换后的状态；面板没开时不动、返回 `false`。
    ///
    /// 钉住 = 焦点怎么变都不自动关闭。持久偏好，关闭不复位，经 `state.toml` 跨重启保留
    /// （鼠标点击无 auto-repeat，直接落盘）。这是钉住的唯一入口，将来绑快捷键也只调它。
    pub(crate) fn toggle_softkeyboard_pin(&self) -> bool {
        if !self.softkeyboard_is_open() {
            return false;
        }
        let pinned = !self.softkeyboard_pinned.fetch_xor(true, Ordering::Relaxed);
        debug!("softkeyboard: pinned={pinned}");
        let _ = self.ui_tx.send(UiCommand::SoftKeyboardPinned(pinned));
        self.state_writer
            .schedule("softkeyboard_pinned", move |rs| {
                rs.softkeyboard_pinned = pinned;
            });
        pinned
    }

    /// 焦点事件到达时，面板是否该关。纯判据，便于单测。
    ///
    /// 焦点变化绝大多数是同宿主内的噪声（如 Excel 换单元格），不代表用户不再需要面板。
    /// 只有两类才算离开：
    ///
    /// - 整个应用失去前台（`Thread`）或切走本输入法（`ImeDeactivated`）；
    /// - 焦点落到另一个宿主进程（`Gained` 的 pid 与开启时记的不同）。
    ///
    /// 任一 pid 为 0 时保守地不关：误关打断录入，误留只是多一个面板。
    pub(crate) fn softkeyboard_should_close_on(signal: SoftKbFocusSignal, host_pid: u32) -> bool {
        match signal {
            SoftKbFocusSignal::Lost(reason) => reason.clears_ime_active(),
            SoftKbFocusSignal::ImeDeactivated => true,
            SoftKbFocusSignal::Gained { pid } => host_pid != 0 && pid != 0 && pid != host_pid,
        }
    }

    /// 焦点切换时关闭。
    ///
    /// 与工具栏的失焦隐藏不同，这里是关闭，切回来不自动恢复。判据见
    /// [`Self::softkeyboard_should_close_on`]。依赖面板不抢焦点（`WS_EX_NOACTIVATE`），
    /// 否则点面板上的键就会把自己关掉。
    pub(crate) fn close_softkeyboard_on_focus_change(&self, signal: SoftKbFocusSignal) {
        if !self.softkeyboard_is_open() {
            return;
        }
        let host_pid = self.softkeyboard_host_pid.load(Ordering::Relaxed);
        // 钉住期间或宿主未知时，宿主跟着最近获焦的进程走，否则取消钉住后在新应用里第一次
        // 换焦点就会关掉面板。
        if let SoftKbFocusSignal::Gained { pid } = signal
            && pid != 0
            && (host_pid == 0 || self.softkeyboard_pinned.load(Ordering::Relaxed))
        {
            self.softkeyboard_host_pid.store(pid, Ordering::Relaxed);
            if host_pid == 0 {
                return;
            }
        }
        if self.softkeyboard_pinned.load(Ordering::Relaxed) {
            debug!("softkeyboard: 已钉住，忽略焦点事件 {signal:?}");
            return;
        }
        let host_pid = self.softkeyboard_host_pid.load(Ordering::Relaxed);
        if !Self::softkeyboard_should_close_on(signal, host_pid) {
            debug!("softkeyboard: 焦点事件 {signal:?}（宿主 pid={host_pid}）不算离开，保留");
            return;
        }
        // 守卫期：跨宿主切换时旧宿主的 focus_lost 晚约 100ms 到达，从工具栏点开面板正落在
        // 这个窗口里，不设守卫会「弹出即消失」。复用菜单的守卫期常量。
        if let Some(at) = *self
            .softkeyboard_opened_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < crate::handle_menu::MENU_FOCUS_GUARD
        {
            debug!(
                "softkeyboard: 焦点事件({signal:?})距打开 {:?} < 守卫期，忽略",
                at.elapsed()
            );
            return;
        }
        debug!("softkeyboard: closed by focus change ({signal:?})");
        self.close_softkeyboard();
        self.after_softkeyboard_change();
    }

    /// 切到指定 id 的面。面不存在返回 `false` 并告警，不静默（否则与「键没绑上」无从分辨）。
    fn softkeyboard_goto(&self, id: &str) -> bool {
        match self.softkeyboard_table().index_of(id) {
            Some(idx) => {
                self.softkeyboard_page.store(idx, Ordering::Relaxed);
                // 切面可能把键盘面换成符号面（或反之），`STATUS_SOFT_KEYBOARD_KEYS`
                // 随之改变，C++ 的吃键判定要跟上。
                self.softkeyboard_dirty.store(true, Ordering::Relaxed);
                true
            }
            None => {
                warn!(
                    "软键盘: 面 {:?} 不存在（可用: {}）",
                    id,
                    self.softkeyboard_table()
                        .pages()
                        .iter()
                        .map(|p| p.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                false
            }
        }
    }

    /// 相对切面（翻页键用）。空表时空转。
    fn softkeyboard_cycle_page(&self, delta: i32) {
        let n = self.softkeyboard_table().len();
        if n == 0 {
            return;
        }
        let next = (self.softkeyboard_page_idx() as i32 + delta).rem_euclid(n as i32) as usize;
        self.softkeyboard_page.store(next, Ordering::Relaxed);
        // 见 `softkeyboard_goto`：面的类型变了，C++ 的吃键判定要跟上。
        self.softkeyboard_dirty.store(true, Ordering::Relaxed);
        debug!(
            "softkeyboard: page -> {next} ({})",
            self.softkeyboard_page_name()
        );
        self.push_softkeyboard_view();
    }

    /// 软键盘态的按键处理。
    ///
    /// 返回 `None` = 本模块不管这个键，调用方继续走常规链路（特殊键与布局外按键即如此）。
    pub(crate) fn handle_softkeyboard_key(
        &self,
        state: &State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        if !self.softkeyboard_is_open() {
            return None;
        }
        let vk = data.key_code;
        let send_keys = self.softkeyboard_send_keys();
        // 键盘面上正在打字时，Esc / 翻页让位给输入本身（清码 / 翻候选页）。
        let typing = send_keys && (!state.input_buffer.is_empty() || !state.candidates.is_empty());

        // ── 面板自己的控制键 ──
        if vk == keymap::VK_ESCAPE {
            if typing {
                return None;
            }
            return Some(self.close_softkeyboard());
        }
        if vk == keymap::VK_PRIOR && !typing {
            self.softkeyboard_cycle_page(-1);
            return Some(KeyAction::Consumed);
        }
        if vk == keymap::VK_NEXT && !typing {
            self.softkeyboard_cycle_page(1);
            return Some(KeyAction::Consumed);
        }

        // 带 Ctrl/Alt/Win 的组合一律不接管：那是宿主的快捷键，软键盘开着也不该抢。
        // Shift 不在此列——它是本面板的层选择器。
        if data.modifiers & MOD_SHORTCUT != 0 {
            return None;
        }

        // ── 键盘面：只画按下反馈，`None` 放行；C++ 侧对称地不吃这些键 ──
        if send_keys {
            if let Some(slot) = vk_to_slot().get(&vk) {
                self.softkeyboard_press_feedback(slot, data.modifiers & MOD_SHIFT != 0);
            }
            return None;
        }

        // ── 符号键位 ──
        let slot = *vk_to_slot().get(&vk)?;
        let shift = data.modifiers & MOD_SHIFT != 0;
        self.softkeyboard_press_feedback(slot, shift);
        let table = self.softkeyboard_table();
        let page = table.pages().get(self.softkeyboard_page_idx())?;
        // 取档与面板显示同一份判据（`slot_layer`），否则显示与输出分叉。
        let layer = slot_layer(slot, shift, state.caps_lock);
        match page.output(slot, layer) {
            Some(text) => {
                let text = text.to_string();
                debug!(
                    "softkeyboard: {slot}{} -> {text:?}",
                    if layer { "+shift" } else { "" }
                );
                Some(self.softkeyboard_commit(state, &text))
            }
            None => {
                // 空键位吃掉并忽略：透传会在灰键位上打出字母。
                debug!("softkeyboard: {slot} 是空键位，已忽略");
                Some(KeyAction::Consumed)
            }
        }
    }

    /// 面板跟随物理按键：切层与键帽高亮。
    ///
    /// 高亮不需要 keyup 配对：面板自行短时清除，长按连发 keydown 不断续期。
    fn softkeyboard_press_feedback(&self, slot: &str, shift: bool) {
        let _ = self.ui_tx.send(UiCommand::SoftKeyboardLayer { shift });
        let _ = self.ui_tx.send(UiCommand::SoftKeyboardKeyState {
            slot: slot.to_string(),
            down: true,
        });
    }

    /// 把一个符号交给上屏出口。
    ///
    /// 必须过 `maybe_convert`，不能把字面量直接塞进 `InsertText`。
    fn softkeyboard_commit(&self, state: &State, text: &str) -> KeyAction {
        KeyAction::InsertText {
            text: self.maybe_convert(state, text),
            new_composition: None,
            mode_changed: false,
            chinese_mode: state.chinese_mode,
            has_new_composition: false,
        }
    }
}

impl Coordinator {
    /// 把当前面下发给渲染端（开启 / 切面 / 内容变都走这里）。
    ///
    /// 只发当前面：切面时本就要重排整块面板，一次发全部面大多用不上。
    pub(crate) fn push_softkeyboard_view(&self) {
        let idx = self.softkeyboard_page_idx();
        let pages: Vec<String> = self
            .softkeyboard_table()
            .pages()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let table = self.softkeyboard_table();
        let page = table.pages().get(idx);
        let keys: Vec<SoftKeyCap> = wind_softkeyboard::all_slots()
            .map(|slot| SoftKeyCap {
                slot: slot.to_string(),
                base: page
                    .and_then(|p| p.output(slot, false))
                    .unwrap_or_default()
                    .to_string(),
                shift: page
                    .and_then(|p| p.output(slot, true))
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        // 位置随每次刷新重发：切面会改面板尺寸，锚右下角让尺寸变化朝屏幕内侧展开。
        // 内存镜像随拖动即时更新，这里恒是最新值。
        let placement = self.softkeyboard_placement();
        let _ = self.ui_tx.send(UiCommand::ShowSoftKeyboard {
            pages,
            current: idx,
            keys,
            send_keys: page.is_some_and(|p| p.send_keys),
            anchor: placement.anchor,
            work_area: placement.work,
        });
    }

    /// 面板上点了一个符号键帽。
    ///
    /// 走 push 上屏而不是返回 `KeyAction`：UI 事件不在按键路径上。文本仍要过 `maybe_convert`。
    pub(crate) fn ui_softkeyboard_key(&self, slot: &str, shift: bool, ctrl: bool) {
        let table = self.softkeyboard_table();
        let Some(page) = table.pages().get(self.softkeyboard_page_idx()) else {
            return;
        };
        // ── Ctrl 粘滞：一律合成组合键，不看当前是哪种面，故排在取 `output` 之前 ──
        if ctrl {
            let combo = if shift {
                format!("ctrl+shift+{slot}")
            } else {
                format!("ctrl+{slot}")
            };
            debug!("softkeyboard: 点击 {slot} -> 合成组合键 {combo:?}");
            self.softkeyboard_tap(&combo);
            return;
        }
        let Some(text) = page.output(slot, shift) else {
            return; // 空键位：面板上是灰的，点了不该有反应
        };
        // ── 键盘面：合成一次真实按键（发键位而非画布上的字），经常规输入链路出中文 ──
        // 不会被自己拦回来：C++ 不启用总闸，`handle_softkeyboard_key` 也对键盘面放行。
        if page.send_keys {
            let combo = if shift {
                format!("shift+{slot}")
            } else {
                slot.to_string()
            };
            debug!("softkeyboard: 点击 {slot} -> 合成按键 {combo:?}");
            self.softkeyboard_tap(&combo);
            return;
        }
        let out = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.maybe_convert(&st, text)
        };
        debug!(
            "softkeyboard: 点击 {slot}{} -> {out:?}",
            if shift { "+shift" } else { "" }
        );
        self.push_commit_text_from(crate::session_exit::ActionOrigin::SoftKeyboardClick, &out);
    }

    /// 面板上点了标签行 / 面名键。
    pub(crate) fn ui_softkeyboard_page(&self, idx: usize) {
        if idx >= self.softkeyboard_table().len() {
            warn!(
                "软键盘: 面下标 {idx} 越界（共 {} 面）",
                self.softkeyboard_table().len()
            );
            return;
        }
        self.softkeyboard_page.store(idx, Ordering::Relaxed);
        self.push_softkeyboard_view();
        // UI 事件没有按键路径的 RAII guard 兜底，得自己推。
        self.after_softkeyboard_change();
    }

    /// 面板上点了关闭按钮。
    pub(crate) fn ui_softkeyboard_close(&self) {
        self.close_softkeyboard();
        self.after_softkeyboard_change();
    }

    /// 面板上点了特殊键（退格 / Tab / 回车 / 空格 / Ins / Del）。
    ///
    /// 只能让那个键真的发生一次。「勿用按键模拟实现 `type()`」的禁令不适用，见设计文档 §7.4。
    pub(crate) fn ui_softkeyboard_fn_key(&self, name: &str) {
        self.softkeyboard_tap(name);
    }

    /// 合成一次真实按键。功能键点击与键盘面的键帽点击共用。
    fn softkeyboard_tap(&self, combo: &str) {
        #[cfg(windows)]
        {
            use wind_cmdbar::KeyInjector;
            if let Err(e) = wind_keys::key_inject::SysKeys.tap(combo) {
                warn!("软键盘: 合成按键 {combo:?} 失败: {e}");
            }
        }
        // macOS 不能在本进程 post CGEvent：服务没有辅助功能授权，`CGEventPost` 返回成功但事件
        // 被丢弃。按键合成推下行帧交给 `.app` 的 `KeySynthesizer`，见设计文档 §11.7。
        #[cfg(ext_presenter)]
        {
            let (key, mods) = crate::handle_cmdbar_macos::split_combo(combo);
            if key.is_empty() {
                warn!("软键盘: 合成按键 {combo:?} 解析不出键名，已忽略");
                return;
            }
            debug!("软键盘: 合成按键 {combo:?} -> .app (key={key:?} mods={mods:?})");
            self.push_cmdbar_key_frame(&wind_ipc::codec::encode_key_tap(&key, &mods));
        }
        #[cfg(not(any(windows, ext_presenter)))]
        {
            let _ = combo;
        }
    }
}

// ───────────────────────── 映射表的读取、重载与设置端编辑 ─────────────────────────

/// 设置端编辑器的整批写入互斥：读用户层 → 改 → 写回全程持锁，防多个连接交错覆盖。
static SOFTKEYBOARD_EDIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Coordinator {
    /// 当前映射表的快照。取完即放锁；持有期间即便发生重载，手里这份仍完整可用。
    pub(crate) fn softkeyboard_table(
        &self,
    ) -> std::sync::Arc<wind_softkeyboard::SoftKeyboardTable> {
        self.softkeyboard
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn softkeyboard_data_dir(&self) -> Option<&std::path::Path> {
        self.compat_dirs.0.as_deref()
    }

    /// 用户层文件路径（与加载同一口径：`resource_layers_named_with` 的 user 层）。
    fn softkeyboard_user_file(&self) -> Option<std::path::PathBuf> {
        Config::resource_layers_named_with(self.softkeyboard_data_dir())
            .into_iter()
            .find(|l| l.name == "user")
            .map(|l| l.path.join(wind_softkeyboard::FILE_NAME))
    }

    /// 重新加载映射表（设置端编辑器写完用户层后）。
    ///
    /// 当前面按 id 找回（编辑可能增删了面），找不到回第一面。面板开着时当场重发视图，表空了就关掉。
    pub(crate) fn reload_softkeyboard(&self) {
        // 与按键路径同一个收口：面的类型（send_keys）可能变了，C++ 吃键判定要跟上。
        let _push = SoftKeyboardPushOnDrop(self);
        let fresh = load_softkeyboard_table(self.softkeyboard_data_dir(), true);
        let cur_id = self
            .softkeyboard_table()
            .pages()
            .get(self.softkeyboard_page_idx())
            .map(|p| p.id.clone());
        let idx = cur_id.and_then(|id| fresh.index_of(&id)).unwrap_or(0);
        let empty = fresh.is_empty();
        *self.softkeyboard.write().unwrap_or_else(|e| e.into_inner()) = std::sync::Arc::new(fresh);
        self.softkeyboard_page.store(idx, Ordering::Relaxed);
        self.softkeyboard_dirty.store(true, Ordering::Relaxed);
        debug!("softkeyboard: 映射表已重载，当前面 {idx}");
        if self.softkeyboard_is_open() {
            if empty {
                self.close_softkeyboard();
            } else {
                self.push_softkeyboard_view();
            }
        }
    }

    /// 编辑视图（`softkeyboard.get`）：下层 ⊕ 用户层，逐键标出默认值与改动。
    pub(crate) fn softkeyboard_editor_view(&self) -> anyhow::Result<serde_json::Value> {
        let lower = load_softkeyboard_table(self.softkeyboard_data_dir(), false);
        let text = self.read_softkeyboard_user_file()?;
        let pages = wind_softkeyboard::admin::view(&lower, &text).map_err(anyhow::Error::msg)?;
        Ok(serde_json::json!({ "pages": pages }))
    }

    /// 整批编辑（`softkeyboard.apply`）：稀疏写用户层 → 重载 → 返回新视图。
    ///
    /// 任何一步失败整批作废、文件不动（`admin::apply` 不产出半成品）。
    pub(crate) fn softkeyboard_editor_apply(
        &self,
        ops: &[wind_softkeyboard::admin::Op],
    ) -> anyhow::Result<serde_json::Value> {
        {
            let _guard = SOFTKEYBOARD_EDIT_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let path = self
                .softkeyboard_user_file()
                .ok_or_else(|| anyhow::anyhow!("无用户配置目录，无法保存软键盘"))?;
            // 拿不到用户配置目录时的兜底可能让「用户层」落在出厂目录上：写进去就改了出厂文件。
            if let Some(data) = self.softkeyboard_data_dir()
                && path.parent() == Some(data)
            {
                anyhow::bail!("用户配置目录与系统预置目录相同，拒绝改写软键盘文件");
            }
            let lower = load_softkeyboard_table(self.softkeyboard_data_dir(), false);
            let text = self.read_softkeyboard_user_file()?;
            let out =
                wind_softkeyboard::admin::apply(&lower, &text, ops).map_err(anyhow::Error::msg)?;
            if out != text {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                wind_config::app_compat::write_atomic(&path, &out)
                    .map_err(|e| anyhow::anyhow!("写入 {} 失败：{e}", path.display()))?;
                tracing::info!(
                    "软键盘: 设置端写入用户层 {}（{} 项操作）",
                    path.display(),
                    ops.len()
                );
            }
        }
        self.reload_softkeyboard();
        self.softkeyboard_editor_view()
    }

    /// 用户层原文；文件不存在 = 空串（还没改过任何东西）。
    fn read_softkeyboard_user_file(&self) -> anyhow::Result<String> {
        let Some(path) = self.softkeyboard_user_file() else {
            return Ok(String::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(t) => Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(anyhow::anyhow!("读取 {} 失败：{e}", path.display())),
        }
    }
}

/// 按键返回时把软键盘状态推给 C++（`STATUS_SOFT_KEYBOARD` 位）。
///
/// 用 RAII 而不是在几十个 return 点手写，漏一个就两侧状态不一致（数字行整行失效）。
///
/// 必须声明在函数最开头：最早声明、最晚析构，那时 `state` 的 `MutexGuard` 已经还了。
pub(crate) struct SoftKeyboardPushOnDrop<'a>(pub(crate) &'a Coordinator);

impl Drop for SoftKeyboardPushOnDrop<'_> {
    fn drop(&mut self) {
        if self.0.softkeyboard_dirty.swap(false, Ordering::Relaxed) {
            // 这一支已推完整快照（含热键会话位），只需对齐缓存，免得下次按键白推一次。
            self.0.after_softkeyboard_change();
            self.0.sync_hotkey_session_cache();
            return;
        }
        let _pushed = self.0.push_hotkey_session_if_changed();
    }
}

#[cfg(test)]
#[path = "handle_softkeyboard_tests.rs"]
mod tests;
