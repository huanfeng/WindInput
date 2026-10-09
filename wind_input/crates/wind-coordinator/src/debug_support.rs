//! 测试/诊断支撑接口（debug_*）：仅测试与诊断代码消费，生产路径不调用。
//!
//! （自 coordinator.rs 平移，纯搬运。生产 tooltip 用的 DebugSchemaCtx 一族
//! 名字带 debug 但在真实路径上，不在此文件。）

use wind_ui_types::UiEvent;

use crate::coordinator::Coordinator;
use crate::pipeline::ModeKind;

impl Coordinator {
    /// 当前**已启用**的方案列表（`schema.available`，测试/诊断用）。
    ///
    /// 与 [`Self::active_schema_id`] 不同，这里回答的是「哪些方案会被启动预热覆盖」。
    /// 测试用它守住「目标方案确实未启用」这个前提——失去前提的回归用例会在已启用
    /// 方案上空跑一遍、永远绿。
    pub fn debug_available_schemas(&self) -> Vec<String> {
        self.engine_mgr.available_schemas()
    }

    /// 点击一个菜单项（测试/诊断用）：经 [`Coordinator::handle_ui_event`] 派发
    /// `UiEvent::MenuAction`，与 UI 线程回传点击同一入口——菜单项的 kind 与「打开时记下的
    /// 目标下标」那段接线都在这条路上。
    pub fn debug_menu_action(&self, kind: wind_ui_types::MenuKind) {
        self.handle_ui_event(UiEvent::MenuAction(kind));
    }

    /// 弹出候选右键菜单（测试/诊断用）：直通生产入口 [`Self::show_candidate_menu`]，菜单项经
    /// UI 通道下发（`UiCommand::ShowCandidateMenu`）。Windows / macOS 的右键由 UI 线程触发、
    /// Linux 的入口 `menu.open` 只在 `ext_presenter` 构建里编译，集成测试都够不着。
    pub fn debug_show_candidate_menu(&self, page_local: usize) {
        self.show_candidate_menu(page_local, 0, 0);
    }

    /// 某个 mix 实例**实际生效的成员方案**（测试/诊断用）：[`Self::mix_members`] 的直通，
    /// 不另算一遍（另算一遍的 debug 方法证明不了生产路径接对了）。
    ///
    /// ★ 暴露它是因为「某个成员被跳过了」**在候选面上不可观察**：被跳过的成员本来就不
    /// 产候选，少了几条与「那个方案没词」无从区分。定制版 `[schemas] hide` 掉一个 mix
    /// 成员正是这种形状——跳过是对的，但「其余成员照常」必须能被断言，否则一个把整个
    /// mix 判空的实现同样全绿。
    pub fn debug_mix_members(&self, idx: u8) -> Vec<String> {
        self.mix_members(idx)
    }

    /// 当前联想候选的文本（测试/诊断用）。空 = 这批候选不是联想来的。
    ///
    /// 集成测试（`tests/` 下、crate 外）够不着 `state`，而联想的端到端验证恰恰必须从
    /// 真实按键入口进——headless 无词库时词语联想恒空，验不出真机行为。
    ///
    /// ★ 判据走 `assoc_active()`（候选来源）而非「候选非空」：联想候选与普通候选住在
    /// 同一个 `candidates` 里，不看来源就会把正常输入的候选也当成联想报出去，
    /// 于是「联想出来了吗」这个断言恒真。
    pub fn debug_assoc_texts(&self) -> Vec<String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.assoc_active() {
            return Vec::new();
        }
        s.candidates.iter().map(|c| c.text.clone()).collect()
    }

    /// 按上文 `text` 算出的联想候选文本（不进联想态、不改状态；评测 / 诊断用）。
    pub fn debug_assoc_suggest(&self, text: &str) -> Vec<String> {
        self.assoc_hits(&self.assoc_config(), text)
            .into_iter()
            .map(|h| h.text)
            .collect()
    }

    /// 当前 `state.preedit`（测试/诊断用）。联想态在非嵌入模式下往这里写「联想输入」
    /// 标识，退出联想必须连它一起清掉——集成测试够不着 `state`，只能经这里断言。
    pub fn debug_preedit(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preedit
            .clone()
    }

    /// 推给 TSF 的 key_up 热键白名单（测试/诊断用）。
    ///
    /// 这正是 `push_activation_status` 发出去的那份，不是另算一遍——修饰键类绑定
    /// 「能不能被触发」完全取决于它在不在这里面，用旁路重算的值断言等于没测。
    pub fn debug_key_up_hotkeys(&self) -> Vec<u32> {
        self.rt().compiled_hotkeys.key_up_tsf_hashes()
    }

    /// 直接装载短语层（仅测试用）：`(code, text, weight, position, is_system)`。
    ///
    /// ★ 补的是一个**结构性**测试缺口：真机短语层经 redb `store` 建立，而 headless 测试的
    /// `store` 是 `None` → 短语层恒空 → 所有依赖短语的判据（`has_code_prefix` 的前缀命中、
    /// z 的活码身份、夺取回路的触发条件）在测试里全都走不到。测试演示的是「z 是死码」那条
    /// 分支，真机跑的是「z 有 37 条 `zz*` 前缀」那条——两边结构性分叉，测试再绿也盖不住真机。
    ///
    /// 这个缺口让「让位判据与候选构建门槛不同源」整个漏到真机（见 `has_code_prefix` 文档）。
    /// 「这个码位归短语管」的判据（测试/诊断用）。
    ///
    /// ★ 暴露它是因为它的失效**不可从候选面观察**：方案级作用域漏接时，候选面上短语已经
    /// 消失了（那两处过滤生效了），而顶码与全码自动上屏仍被否决 ⇒ 打字卡住不上屏、零日志。
    /// 测试只能直接问这一位，断言「候选里有没有短语」在漏接时照样通过。
    /// 见 `docs/design/schema-scoped-behavior.md` §6.3。
    pub fn debug_phrase_owns_code(&self, code: &str) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.phrase_owns_code(&state, code)
    }

    pub fn debug_install_phrases(&self, records: Vec<wind_phrase::PhraseSeed>) {
        *self.phrases.write().unwrap_or_else(|e| e.into_inner()) =
            wind_phrase::PhraseLayer::from_records(records);
    }

    /// 是否还有更多候选未加载（测试/诊断用）
    /// 当前激活的 overlay 模式类别名；`None` = 普通输入。仅供测试断言。
    pub fn debug_active_mode(&self) -> Option<&'static str> {
        match self.state.lock().unwrap_or_else(|e| e.into_inner()).active {
            Some(ModeKind::TempPinyin) => Some("temp_pinyin"),
            Some(ModeKind::TempEnglish) => Some("temp_english"),
            Some(ModeKind::Url) => Some("url"),
            Some(ModeKind::Email) => Some("email"),
            Some(ModeKind::Unicode) => Some("unicode"),
            Some(ModeKind::Special(_)) => Some("special"),
            Some(ModeKind::RareChar) => Some("rare_char"),
            Some(ModeKind::Reverse) => Some("reverse"),
            Some(ModeKind::Mix(_)) => Some("mix"),
            Some(ModeKind::AuxCode) => Some("aux_code"),
            None => None,
        }
    }

    /// 走一次 CapsLock 的「英文候选大小写档位循环」，返回是否真的夺取了本键。
    ///
    /// **存在理由是那条路测不到**：真实触发点是全局低级键盘钩子（`CapsLockHook`），
    /// 它在 CI 的 Linux 上装不起来，在 headless 测试里也没有键盘事件可拦。业务逻辑
    /// （三道守卫 + 档位推进 + 候选重建）却必须有守门断言——否则「开关开了没反应」
    /// 这类缺陷只能靠真机发现。
    ///
    /// 钩子回调本身只做非阻塞投递，真正的动作与本函数是同一个：`try_english_case_cycle`。
    pub fn debug_cycle_english_case(&self) -> bool {
        self.try_english_case_cycle().is_some()
    }

    pub fn debug_has_more(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_more
    }

    /// 检索范围是否处于临时放宽态（测试/诊断用）。反查模式要断言「退出后不残留」。
    pub fn debug_scope_relaxed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scope_relaxed
    }

    /// 分页信息 (当前页0-based, 页内高亮0-based, 总页数)（测试/诊断用）
    pub fn debug_page_info(&self) -> (usize, usize, usize) {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (s.current_page, s.selected_index, self.total_pages(&s))
    }

    /// 注入「候选窗当前是否反转排列」（测试/诊断用）。
    ///
    /// 刻意走 [`Coordinator::handle_ui_event`] 而非直接写字段——正式路径是 UI 线程发
    /// `UiEvent::CandidateFlipped`，测试入口跳过分发就测不到那条接线（同 `debug_candidate_op`）。
    pub fn debug_set_candidate_flipped(&self, flipped: bool) {
        self.handle_ui_event(UiEvent::CandidateFlipped(flipped));
    }

    /// 将统计采集器内存数据落库（测试/诊断用；生产由后台线程定时 flush）。
    pub fn debug_flush_stats(&self) {
        if let Some(c) = self.stat_collector.as_ref() {
            c.flush();
        }
    }

    /// 当前页候选文本列表（内部简体；测试/诊断用）
    pub fn debug_page_texts(&self) -> Vec<String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (start, end) = self.page_range(&s);
        s.candidates[start..end]
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    /// 当前页候选的"显示文本"（应用简繁后，与候选窗口一致；测试/诊断用）
    pub fn debug_page_display_texts(&self) -> Vec<String> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (start, end) = self.page_range(&s);
        s.candidates[start..end]
            .iter()
            .map(|c| self.cand_display_text(&s, c))
            .collect()
    }

    // ── webdata 契约测试(wind-webdata,crate 外)的白盒支撑 ──
    // 测试要验证「记账 → RPC 读出」的联动,记账入口是 pub(crate);经 debug_* 暴露,
    // 生产路径不调用。

    /// 上屏记账转发(仅测试)。
    pub fn debug_record_commit(
        &self,
        text: &str,
        code_len: u32,
        candidate_pos: i32,
        source: wind_store::stats::CommitSource,
    ) {
        self.record_commit(text, code_len, candidate_pos, source);
    }

    /// 顶层输入统计兜底转发(仅测试)。
    pub fn debug_record_input_stats(&self, action: &wind_bridge::handler::KeyAction) {
        self.record_input_stats(action);
    }

    /// 开始捕获统计事件（清空已捕获的；仅测试）。之后每次上屏记账（`record_commit*`，
    /// 空文本除外）追加一条 (来源, 文本)，与统计开关、有无采集器无关。
    pub fn debug_capture_stat_events(&self) {
        *self
            .debug_stat_events
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
    }

    /// 取走已捕获的统计事件（未开始捕获时为空；仅测试）。
    pub fn debug_take_stat_events(&self) -> Vec<(wind_store::stats::CommitSource, String)> {
        self.debug_stat_events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// 本次按键是否已被具体上屏路径记账(仅测试)。
    pub fn debug_stat_recorded(&self) -> bool {
        self.stat_recorded
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 短语层查询,返回命中文本(仅测试)。
    ///
    /// 反查（`dict.rev`）接**真实**实现：这个探针的用途正是「这条短语实际会显示成什么」，
    /// 塞个空桩进去就只能验到「求值没崩」。剪贴板仍留空桩——headless 测试没有剪贴板，
    /// 要验剪贴板相关短语得由调用方另行注入。
    pub fn debug_phrase_texts(&self, code: &str) -> Vec<String> {
        let clip = |_n: i64| String::new();
        let reverse = |text: &str, fmt: &str| -> String { self.reverse_render(text, fmt) };
        let host = wind_phrase::PhraseHost {
            clip: &clip,
            reverse: &reverse,
        };
        self.phrases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            // 诊断口径：看**全部**短语，不套方案级作用域——这个 API 回答的是「库里有什么」，
            // 不是「当前方案能用什么」。后者由候选路径自己过滤。
            .lookup(code, &[], &host, &wind_phrase::PhraseScope::ALL)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// 三层合并后的软键盘映射表（测试/诊断用）：**就是**运行时那一份，不另算一遍。
    ///
    /// ★ 暴露它是因为 `data < data_custom < %APPDATA%` 的按面合并在候选面/菜单上不可
    /// 观察：定制层被整层忽略、或叠加顺序反了（用户改的键被定制版盖回去），现象都只是
    /// 「某个键出的字不对」，与「面写错了」无从区分。
    pub fn debug_softkeyboard(&self) -> &wind_softkeyboard::SoftKeyboardTable {
        &self.softkeyboard
    }

    /// 走一次菜单命令分派（测试用），与右键菜单点选是同一个入口。
    ///
    /// 存在理由：会写盘的菜单项（如「密码框强制英文」）要在重定向了用户目录的独立测试
    /// 二进制里测，而那里够不着 crate 内部的 `run_menu_cmd`。
    pub fn debug_run_menu_cmd(&self, cmd: wind_ui_types::MenuCmd) {
        self.run_menu_cmd(cmd);
    }

    /// 简繁两方向：`((s2t 开, t2s 开), (s2t 转换器已加载, t2s 转换器已加载))`（测试/诊断用）。
    pub fn debug_conversion(&self) -> ((bool, bool), (bool, bool)) {
        let on = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.s2t_enabled, s.t2s_enabled)
        };
        let loaded = (
            self.s2t.lock().unwrap_or_else(|e| e.into_inner()).is_some(),
            self.t2s.lock().unwrap_or_else(|e| e.into_inner()).is_some(),
        );
        (on, loaded)
    }

    /// 密码框强制英文的运行时两态：`(策略开关, 当前是否正在抑制)`（测试/诊断用）。
    pub fn debug_password_suppress(&self) -> (bool, bool) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.password_suppress_enabled.load(Relaxed),
            self.password_suppress.load(Relaxed),
        )
    }

    /// 当前输入缓冲（测试/诊断用）。通配用例要断言「键进没进缓冲」，组合区文本会混入已上屏前缀。
    pub fn debug_input_buffer(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input_buffer
            .clone()
    }

    /// 全部候选的 `(text, code, comment)`（测试/诊断用）。通配要断言「注释是完整编码」。
    pub fn debug_candidate_triples(&self) -> Vec<(String, String, String)> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .candidates
            .iter()
            .map(|c| (c.text.clone(), c.code.clone(), c.comment.clone()))
            .collect()
    }

    /// 诊断：桌面构造器 `new` 在 `build` 之后的非 UI 装载与启动预热线程的主体，**同步**跑完。
    ///
    /// 供本机复现内存（`tests/typing_memory_repro.rs`）把 headless 协调器拉到与桌面相同的
    /// 常驻状态。两段都与 `new` 共用同一个函数（[`Self::load_data_on_start`] /
    /// [`Self::prewarm_on_start`]），次序同生产：数据装载 → 草稿清理 → 预热。
    pub fn debug_desktop_startup_and_prewarm(&self) {
        self.load_data_on_start();
        self.purge_drafts_on_start();
        self.prewarm_on_start();
    }

    /// 诊断：各懒建结构的自报大小与已加载引擎清单（多行文本，只含方案 id 与数字，不含用户内容）。
    pub fn debug_memory_report(&self) -> String {
        use std::fmt::Write;
        let kb = |b: usize| b.div_ceil(1024);
        let r = self.engine_mgr.memory_report();
        let mut o = String::new();
        let _ = writeln!(o, "engines ({}):", r.engines.len());
        for (id, ty, parts) in &r.engines {
            let _ = write!(o, "  {id} [{ty}]");
            for p in parts {
                let _ = write!(o, " {}={}KB/{}块", p.name, kb(p.bytes), p.allocs);
            }
            let _ = writeln!(o);
        }
        for (id, heap, image) in &r.reverse_index {
            let _ = writeln!(
                o,
                "reverse_index {id}: 常驻堆 {}KB / 镜像 {}KB",
                kb(*heap),
                kb(*image)
            );
        }
        for (id, heap, building) in &r.user_text {
            let _ = writeln!(o, "UserTextIndex {id}: {}KB building={building}", kb(*heap));
        }
        if let Some((id, heap)) = &r.user_assoc {
            let _ = writeln!(o, "UserAssocIndex {id}: {}KB", kb(*heap));
        }
        if let Some((id, heap, allocs)) = &r.single_char {
            let _ = writeln!(o, "SingleCharCodeCache {id}: {}KB/{allocs}块", kb(*heap));
        }
        let (chaizi, pinyin, comments) = self
            .reverse
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .heap_bytes();
        let _ = writeln!(
            o,
            "ReverseLookup: ChaiziTable {}KB PinyinTable {}KB 常驻注释库 {}KB",
            kb(chaizi),
            kb(pinyin),
            kb(comments)
        );
        // 只有 `UserTextIndex` 的扫表是一次 `with_db` 内完成的，扫表时长即持库锁时长；
        // 联想索引分两次进库（只记总耗时），读音索引读的是 mmap 词库、不碰 store。
        let b = r.user_text_builds;
        let _ = writeln!(
            o,
            "UserTextIndex 重建: {} 次, 扫表(持库锁)累计 {:?} 最大 {:?}, 总耗时累计 {:?} 最大 {:?}",
            b.builds, b.scan_total, b.scan_max, b.total, b.total_max
        );
        let b = r.user_assoc_builds;
        let _ = writeln!(
            o,
            "UserAssocIndex 重建: {} 次, 耗时(含两次进库)累计 {:?} 最大 {:?}",
            b.builds, b.total, b.total_max
        );
        let b = r.char_pinyin_builds;
        let _ = writeln!(
            o,
            "CharPinyinIndex 构建(不碰 store): {} 次, 耗时累计 {:?} 最大 {:?}",
            b.builds, b.total, b.total_max
        );
        if let Some(s) = &self.store {
            let _ = writeln!(
                o,
                "store: words_gen={} pinyin_gen={:?} page_cache_drops={}",
                s.words_generation(),
                s.words_generation_of(wind_engine::manager::PINYIN_DATA_SCHEMA),
                s.page_cache_drops()
            );
        }
        o
    }

    /// 诊断：注释 `${pinyin}` 对非拼音候选走的那条推断（会在首次调用时同步建主拼音引擎的
    /// `CharPinyinIndex`）。
    pub fn debug_word_pinyin(&self, text: &str) -> String {
        self.engine_mgr.word_pinyin_syllables(text)
    }

    /// 诊断：立刻做一次空闲回收（丢 redb 读缓存 + 整理堆），等价于回收线程过了空闲档。
    pub fn debug_reclaim_now(&self) {
        if let Some(s) = &self.store {
            let _ = s.drop_page_cache();
        }
        crate::heap_trim::release_free_heap();
    }
}
