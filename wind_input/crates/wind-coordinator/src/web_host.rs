//! [`WebDataHost`]：设置页数据 RPC（wind-webdata crate）消费宿主能力的窄面。
//!
//! RPC 本体在 `wind-webdata`（trait `WebDataRpc: WebDataHost` 的默认方法）；
//! 依赖方向是 wind-webdata → wind-coordinator，本 crate 不依赖 wind-transfer/fontdb，
//! Android 闭包（不含 wind-webdata）因此无任何 C 依赖。

use std::sync::{Arc, RwLock};

use wind_engine::EngineManager;
use wind_reverse::ReverseLookup;
use wind_store::Store;
use wind_store::stat_collector::StatCollector;

use crate::coordinator::Coordinator;

/// webdata 消费宿主能力的**窄面**：设置页数据 RPC 对 Coordinator 的全部依赖收敛于此。
///
/// ★ webdata 不碰输入态（`State` 与 Coordinator 的 80 余个字段），只消费引擎/存储/
/// 统计/主题句柄与少数重建入口——新增 RPC 若需新依赖，**必须加在本 trait 上**，
/// 勿在默认方法里绕道取宿主其它状态（那会无声地把窄面重新擑宽，也阻断后续
/// 独立成 crate 的路）。
///
/// 方法与 Coordinator 固有方法同名时固有优先；转发 impl 内一律用完全限定路径
/// `Coordinator::xxx(self)` 消歧，否则就是自调递归。
pub trait WebDataHost {
    fn engine_mgr(&self) -> &EngineManager;
    fn user_store(&self) -> Option<&Arc<Store>>;
    fn stat_collector(&self) -> Option<&StatCollector>;
    fn reverse_lookup(&self) -> &RwLock<ReverseLookup>;
    // 曾有 `themes_dir() -> Option<&Path>`（安装目录的 themes 根），已删：它只有一个
    // 用途——让 wind-webdata 自己拼一份「用户目录 + 安装目录」的搜索链，而那正是
    // `theme_search_dirs()` 的第二份实现。加 data_custom 层时两份各改各的就会分叉。
    // 需要主题目录一律走 `theme_search_dirs()`（已按层序展开）。
    fn rebuild_phrases(&self);
    fn restore_missing_system_phrases(&self, reason: &str);
    fn restore_system_phrases(&self) -> usize;
    fn sync_comment_dicts(&self);
    fn sync_chaizi_assets(&self);
    fn reload_user_config(&self) -> bool;
    fn push_theme(&self, name: &str, is_dark: bool);
    fn theme_search_dirs(&self) -> Vec<std::path::PathBuf>;
    fn list_themes_full(&self) -> Vec<(String, String, bool)>;
    /// 当前生效主题 id（快照）。
    fn current_theme_name(&self) -> String;
    /// 当前明暗（system 档按系统实时判定）。语义方法而非暴露 `Mutex<ThemeStyle>`：
    /// 窄面签名不携带宿主内部类型与锁形态。
    fn current_theme_is_dark(&self) -> bool;

    /// 加词界面的默认上下文（目标方案 + 最近上屏文本），见
    /// [`crate::handle_addword::AddWordContext`]。
    ///
    /// 走窄面而不是让 webdata 自己拼：目标方案要经混输解析（`add_word_target_schema`），
    /// 最近上屏更是纯输入态——webdata 按设计不碰 `State`。
    fn add_word_context(&self) -> crate::handle_addword::AddWordContext;

    /// 快捷输入格式表的设置页全貌（含被停用的条目）。
    fn quick_format_rows(&self) -> Vec<crate::handle_quick_format::QuickFormatRow>;

    /// 字符类：设置页列表（全部类，按 `order` 升序＝仲裁顺序）。
    fn charset_rows(&self) -> Vec<crate::handle_charset::CharsetClassRow>;

    /// 改一个类的属性：写用户层 `charsets/<key>.yaml` **并立即热载**。
    fn charset_edit(
        &self,
        key: &str,
        edit: &crate::handle_charset::CharsetEdit,
    ) -> anyhow::Result<()>;

    /// 撤掉用户层对某个类的全部调整。
    fn charset_reset(&self, key: &str) -> anyhow::Result<()>;

    /// 清理压在某个类上的冗余逐条覆盖（方向与当前默认相同的那些）。
    fn charset_clear_redundant(
        &self,
        key: &str,
    ) -> anyhow::Result<crate::handle_charset::CharsetCleanupOutcome>;

    /// 常用字表：设置页列表（**全表**，`query` 非空时只留出现在查询串里的字）。
    fn common_char_rows(
        &self,
        query: &str,
        only_modified: bool,
    ) -> Vec<crate::handle_common_chars::CommonCharRow>;

    /// 某个字的当前状态：出厂判定 / 覆盖 / 是否受管辖。设置页「添加」时预览与校验用。
    fn common_char_state(&self, ch: &str) -> crate::handle_common_chars::CommonCharState;

    /// 设置页对一个字的编辑：**写库 + 回灌运行时镜像**一并完成（同 `quick_format_edit`）。
    /// 管辖域外的字符返回 Err——放行只会存下一条永不生效的记录，且全程无报错。
    fn common_char_edit(
        &self,
        ch: &str,
        edit: crate::handle_common_chars::CommonCharEdit,
    ) -> anyhow::Result<()>;

    /// 按某个字所属的 Unicode 块整类设常用/生僻；`apply=false` 只预览不写库。
    fn common_char_bulk_by_block(
        &self,
        ch: &str,
        common: bool,
        apply: bool,
    ) -> anyhow::Result<crate::handle_common_chars::CommonCharBulkOutcome>;

    /// 导出用户的常用字调整为 TOML 文本（只含与默认不同的字）。
    fn common_chars_export(&self) -> anyhow::Result<String>;

    /// 导入预览：只解析与计数，不写库。
    fn common_chars_preview_import(
        &self,
        content: &str,
    ) -> anyhow::Result<crate::handle_common_chars::CommonCharsImportPreview>;

    /// 导入常用字调整；`replace` 为真时先清空现有调整。
    fn common_chars_import(
        &self,
        content: &str,
        replace: bool,
    ) -> anyhow::Result<crate::handle_common_chars::CommonCharsImportOutcome>;

    /// 设置页对一条格式的编辑：**写库 + 回灌运行时镜像**一并完成。
    ///
    /// `kind` 收字符串而不是 `FormatKind`：它从 RPC 的 JSON 来，本就是字符串，在这里解析
    /// 能给出「未知类别 xxx」这种可读错误，也省得 wind-webdata 为一个枚举去依赖
    /// wind-quick-input（那是本窄面存在的意义——webdata 只经此面触宿主）。
    fn quick_format_edit(
        &self,
        kind: &str,
        id: &str,
        edit: crate::handle_quick_format::QuickFormatEdit,
    ) -> anyhow::Result<()>;

    /// 每个类别可用的模板变量清单：`[(kind, [(变量名, 说明)])]`。
    ///
    /// 静态数据，但仍走本窄面：`wind-webdata` 不依赖 `wind-quick-input`（那是本 trait 存在的
    /// 意义）。设置页的模板输入框据此提示，故清单的真相源必须在 core——设置仓硬编码一份
    /// 会在加新变量时静默过时。
    fn quick_format_var_hints(&self) -> Vec<(&'static str, Vec<(&'static str, &'static str)>)>;

    /// 新增一条用户自定义格式，返回分配到的 id。
    ///
    /// 没有「改出厂条目模板」的对应方法：那条路径被刻意否决（见
    /// `Coordinator::add_quick_format` 的 doc），出厂条目只能停用与调序。
    fn quick_format_add(&self, kind: &str, text: &str) -> anyhow::Result<String>;

    /// 改写**用户条目**的模板。出厂条目会被拒绝。
    fn quick_format_set_text(&self, kind: &str, id: &str, text: &str) -> anyhow::Result<()>;

    /// 删除**用户条目**（连带它的调序/停用规则）。出厂条目会被拒绝——它们只能停用。
    fn quick_format_delete(&self, kind: &str, id: &str) -> anyhow::Result<()>;

    /// 导出用户改动（调序 / 停用 / 自定义条目）为 TOML 文本。
    fn quick_format_export(&self) -> anyhow::Result<String>;

    /// 导入预览：只解析与计数，不写库。
    fn quick_format_preview_import(
        &self,
        content: &str,
    ) -> anyhow::Result<crate::handle_quick_format::QuickImportPreview>;

    /// 导入用户改动；`replace` 为真时先清空现有调整。
    fn quick_format_import(
        &self,
        content: &str,
        replace: bool,
    ) -> anyhow::Result<crate::handle_quick_format::QuickImportOutcome>;
}

impl WebDataHost for Coordinator {
    fn engine_mgr(&self) -> &EngineManager {
        &self.engine_mgr
    }
    fn user_store(&self) -> Option<&Arc<Store>> {
        self.store.as_ref()
    }
    fn stat_collector(&self) -> Option<&StatCollector> {
        self.stat_collector.as_ref()
    }
    fn reverse_lookup(&self) -> &RwLock<ReverseLookup> {
        &self.reverse
    }
    fn add_word_context(&self) -> crate::handle_addword::AddWordContext {
        Coordinator::add_word_context(self)
    }
    fn rebuild_phrases(&self) {
        Coordinator::rebuild_phrases(self);
    }
    fn restore_missing_system_phrases(&self, reason: &str) {
        Coordinator::restore_missing_system_phrases(self, reason);
    }
    fn restore_system_phrases(&self) -> usize {
        Coordinator::restore_system_phrases(self)
    }
    fn sync_comment_dicts(&self) {
        Coordinator::sync_comment_dicts(self);
    }
    fn sync_chaizi_assets(&self) {
        Coordinator::sync_chaizi_assets(self);
    }
    fn reload_user_config(&self) -> bool {
        Coordinator::reload_user_config(self)
    }
    fn push_theme(&self, name: &str, is_dark: bool) {
        Coordinator::push_theme(self, name, is_dark);
    }
    fn theme_search_dirs(&self) -> Vec<std::path::PathBuf> {
        Coordinator::theme_search_dirs(self)
    }
    fn list_themes_full(&self) -> Vec<(String, String, bool)> {
        Coordinator::list_themes_full(self)
    }
    fn current_theme_name(&self) -> String {
        self.theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn current_theme_is_dark(&self) -> bool {
        self.theme_style
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolve_dark()
    }
    fn quick_format_rows(&self) -> Vec<crate::handle_quick_format::QuickFormatRow> {
        Coordinator::quick_format_rows(self)
    }
    fn charset_rows(&self) -> Vec<crate::handle_charset::CharsetClassRow> {
        Coordinator::charset_rows(self)
    }
    fn charset_edit(
        &self,
        key: &str,
        edit: &crate::handle_charset::CharsetEdit,
    ) -> anyhow::Result<()> {
        Coordinator::charset_edit(self, key, edit)
    }
    fn charset_reset(&self, key: &str) -> anyhow::Result<()> {
        Coordinator::charset_reset(self, key)
    }
    fn charset_clear_redundant(
        &self,
        key: &str,
    ) -> anyhow::Result<crate::handle_charset::CharsetCleanupOutcome> {
        Coordinator::charset_clear_redundant(self, key)
    }
    fn common_char_rows(
        &self,
        query: &str,
        only_modified: bool,
    ) -> Vec<crate::handle_common_chars::CommonCharRow> {
        Coordinator::common_char_rows(self, query, only_modified)
    }
    fn common_char_state(&self, ch: &str) -> crate::handle_common_chars::CommonCharState {
        Coordinator::common_char_state(self, ch)
    }
    fn common_char_edit(
        &self,
        ch: &str,
        edit: crate::handle_common_chars::CommonCharEdit,
    ) -> anyhow::Result<()> {
        Coordinator::common_char_edit(self, ch, edit)
    }
    fn common_char_bulk_by_block(
        &self,
        ch: &str,
        common: bool,
        apply: bool,
    ) -> anyhow::Result<crate::handle_common_chars::CommonCharBulkOutcome> {
        Coordinator::common_char_bulk_by_block(self, ch, common, apply)
    }
    fn common_chars_export(&self) -> anyhow::Result<String> {
        Coordinator::export_common_chars(self)
    }
    fn common_chars_preview_import(
        &self,
        content: &str,
    ) -> anyhow::Result<crate::handle_common_chars::CommonCharsImportPreview> {
        Coordinator::preview_common_chars_import(self, content)
    }
    fn common_chars_import(
        &self,
        content: &str,
        replace: bool,
    ) -> anyhow::Result<crate::handle_common_chars::CommonCharsImportOutcome> {
        Coordinator::import_common_chars(self, content, replace)
    }
    fn quick_format_edit(
        &self,
        kind: &str,
        id: &str,
        edit: crate::handle_quick_format::QuickFormatEdit,
    ) -> anyhow::Result<()> {
        let kind = wind_quick_input::FormatKind::parse(kind)
            .ok_or_else(|| anyhow::anyhow!("未知的快捷输入类别: {kind}"))?;
        Coordinator::edit_quick_format(self, kind, id, edit)
    }
    fn quick_format_var_hints(&self) -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
        wind_quick_input::FormatKind::ALL
            .iter()
            .map(|k| (k.as_str(), k.var_hints().to_vec()))
            .collect()
    }
    fn quick_format_add(&self, kind: &str, text: &str) -> anyhow::Result<String> {
        let kind = wind_quick_input::FormatKind::parse(kind)
            .ok_or_else(|| anyhow::anyhow!("未知的快捷输入类别: {kind}"))?;
        Coordinator::add_quick_format(self, kind, text)
    }
    fn quick_format_set_text(&self, kind: &str, id: &str, text: &str) -> anyhow::Result<()> {
        let kind = wind_quick_input::FormatKind::parse(kind)
            .ok_or_else(|| anyhow::anyhow!("未知的快捷输入类别: {kind}"))?;
        Coordinator::set_quick_format_text(self, kind, id, text)
    }
    fn quick_format_delete(&self, kind: &str, id: &str) -> anyhow::Result<()> {
        let kind = wind_quick_input::FormatKind::parse(kind)
            .ok_or_else(|| anyhow::anyhow!("未知的快捷输入类别: {kind}"))?;
        Coordinator::delete_quick_format(self, kind, id)
    }
    fn quick_format_export(&self) -> anyhow::Result<String> {
        Coordinator::export_quick_format(self)
    }
    fn quick_format_preview_import(
        &self,
        content: &str,
    ) -> anyhow::Result<crate::handle_quick_format::QuickImportPreview> {
        Coordinator::preview_quick_format_import(self, content)
    }
    fn quick_format_import(
        &self,
        content: &str,
        replace: bool,
    ) -> anyhow::Result<crate::handle_quick_format::QuickImportOutcome> {
        Coordinator::import_quick_format(self, content, replace)
    }
}
