//! 「按用到加载」的需求汇总 [`DataNeeds`]（设计 `docs/design/memory-footprint.md` §4.1）。
//!
//! 一批结构（反查索引预热、用户层编码索引、拆字表、简繁表）此前**不看功能是否在用**就加载、
//! 加载后永不释放。本模块回答「当前配置下哪些数据有消费者」，各加载点只问它，不各自判。
//!
//! # 为什么不放进 `ConfigBundle`
//!
//! 判定材料有一部分要问 `EngineManager`：辅助码与自动造词随**活跃方案**变（切方案不重建
//! bundle）。快照进 bundle 就会在切方案、或 `reload_user_config` 先建 bundle 后
//! `reload_from_config` 的那个窗口里过期。故判定是一个
//! 纯函数 [`DataNeeds::derive`]（配置 + 注释模板库 + [`SchemaFacts`]），由 `Coordinator::data_needs`
//! 在用到的时刻现算——**判据只此一处**，调用点只是取值的时机不同。
//!
//! # 不在这里的
//!
//! - 拼音表（`pinyin_map.txt`）维持恒加载：出厂悬停「拼音」段开着，且加词回退要用。
//! - 运行期才出现的格式串（cmdbar `dict.rev`、短语里的反查）无法事先判定。它们用到的数据
//!   由求值处自己在首次用到时派后台加载、本次返回空：拆字表见 `Coordinator::reverse_render`
//!   / `ensure_chaizi_async`，主码表反查索引见 `Coordinator::eval_text_var` 的 `code_rev*` 分支。

use crate::comment::Template;
use wind_config::Config;
use wind_config::comment_lib::{CommentLibrary, SchemaType};
use wind_engine::EngineManager;

/// 编码反查（主码表反查索引 + 用户层编码索引）的变量名，含永久兼容别名。
///
/// `code_rev` 也算用户层：它取的 `hint_code` 在系统层该长度没有码时会回落用户码。
const CODE_REV_VARS: &[&str] = &["code_rev", "code", "code_rev_all", "code_all"];
/// 正确拼音提示的变量名。
const CORRECT_PINYIN_VAR: &str = "correct_pinyin";
/// 拆字表的变量名。
const CHAIZI_VARS: &[&str] = &["chaizi", "chaizi_code", "chaizi_all", "chaizi_code_all"];

/// 当前配置下哪些按需数据有消费者。字段为 `false` ＝ 不加载（已加载的应释放）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DataNeeds {
    /// 主码表反查索引（预热）：悬停[编码]、注释 `${code_rev}`、联想、辅助码、自动造词任一在用。
    pub(crate) reverse_index: bool,
    /// 用户层「词 → 编码」索引（`UserTextIndex`）：悬停[编码]、注释 `${code_rev*}`、辅助码。
    pub(crate) user_text: bool,
    /// 拆字表：任一被分配的注释模板或启用的悬停段引用 `${chaizi*}`。
    pub(crate) chaizi: bool,
    /// 简入繁出转换器（`input.s2t.enabled`）。
    pub(crate) s2t: bool,
    /// 繁入简出转换器（`input.t2s.enabled`，与 s2t 互斥时让位）。
    pub(crate) t2s: bool,
    /// 任一被分配的注释模板或启用的悬停段引用 `${correct_pinyin}`：`hint` 关着也要让拼音引擎算模糊音
    /// 提示（`EngineManager::set_correct_pinyin_in_templates`）。
    pub(crate) correct_pinyin: bool,
    /// 联想「下一词」表：联想档位与开关让 Bigram 源出场（智能联想档）。
    pub(crate) assoc_bigram: bool,
}

/// 判定材料里要问 `EngineManager` 的那部分。
#[derive(Debug, Clone, Default)]
pub(crate) struct SchemaFacts {
    /// 辅助码当下引用了码表方案（`aux_code_schemas_in_use` 非空）。
    pub(crate) aux_code_in_use: bool,
    /// 自动造词开着（码表 / 混输方案 + 开关）。造词的查重要主码表反查索引。
    pub(crate) auto_phrase: bool,
    /// 可用方案及其注释方案类型：编码反查要不要预载，按它们各自落到的模板判（见
    /// [`DataNeeds::derive`]）。为空时（测试构造）退回「任一引用到的模板」的保守判法。
    pub(crate) schemas: Vec<(String, SchemaType)>,
}

impl SchemaFacts {
    /// 取自 `EngineManager` 的缓存（按活跃方案 id），按应用自动切方案这类高频路径上调也不重算。
    pub(crate) fn collect(mgr: &EngineManager) -> Self {
        let active = mgr.active_data_facts();
        Self {
            aux_code_in_use: active.aux_code_in_use,
            auto_phrase: active.auto_phrase,
            schemas: mgr
                .available_schemas()
                .iter()
                .map(|id| (id.clone(), crate::comment::schema_type_of(mgr, id)))
                .collect(),
        }
    }
}

/// 可达上下文里有没有落到引用编码反查变量的模板，见 [`DataNeeds::derive`]。
fn rev_reachable(cfg: &Config, lib: &CommentLibrary, facts: &SchemaFacts) -> bool {
    use wind_config::comment_lib::{CommentScope, CommentState};
    if facts.schemas.is_empty() {
        return lib
            .referenced_texts()
            .iter()
            .any(|t| refs_any(t, CODE_REV_VARS));
    }
    let mut modes: Vec<String> = cfg
        .schema
        .mix_modes
        .iter()
        .map(|m| format!("mix:{}", m.id))
        .collect();
    if cfg.input.temp_pinyin.enabled {
        modes.push("temp_pinyin".into());
    }
    facts.schemas.iter().any(|(id, ty)| {
        // 码表、英文方案的主路不出拼音来源候选，`${code_rev}` 在那里恒空，不算消费者。
        let main = (!matches!(ty, SchemaType::CodeTable | SchemaType::English)).then_some(None);
        main.into_iter()
            .chain(modes.iter().map(|m| Some(m.as_str())))
            .any(|mode| {
                let scope = CommentScope {
                    mode,
                    schema: id,
                    schema_type: *ty,
                };
                [CommentState::Default, CommentState::Detail]
                    .into_iter()
                    .flat_map(|st| [(st, false), (st, true)])
                    .any(|(st, v)| refs_any(lib.template_text(&scope, st, v), CODE_REV_VARS))
            })
    })
}

fn refs_any(tpl: &str, names: &[&str]) -> bool {
    let t = Template::parse(tpl);
    names.iter().any(|n| t.references(n))
}

impl DataNeeds {
    /// 唯一判定处。规则逐条对应求值端的门控（`Coordinator::eval_var` / 悬停候选循环）：
    ///
    /// - 注释模板取 `lib.referenced_texts()`：任一格引用到的模板都算，不论态、排布与作用范围
    ///   （态随 Alt 即时切换，第一次切过去不能是空的）。
    /// - 注释段的 `${code_rev*}` 按**可达上下文实际落到的模板**判（设计 §13.6）：每个可用方案的
    ///   主路（码表、英文方案的主路不出拼音来源候选，跳过），以及进得去的临时拼音、各快捷输入，
    ///   两态两排布逐一解析，任一引用即要。纯拼音方案主路默认态是 `none`，但临拼、快捷输入出厂
    ///   分到「编码」，所以出厂仍要（与退役前一致）。
    /// - 悬停段：逐字段走 `eval_text_var`，那里的 `${code_rev}` 不带门控；
    ///   `${word_code}` / `${code_source}` 任一被引用，候选循环就按词查码（含用户层）。
    /// - 拆字不受任何档门控，模板里写了就要。
    /// - 两个总开关在上面这些之上：`ui.comment.enabled` 关 ⇒ 注释模板都不算；
    ///   `ui.tooltip.enabled` 关 ⇒ 悬停段都不算（设计 §4.3）。
    pub(crate) fn derive(
        cfg: &Config,
        lib: &CommentLibrary,
        facts: &SchemaFacts,
        overlay: wind_config::ConfigOverlay,
    ) -> Self {
        let tip = crate::tooltip::CompiledTooltip::compile(&cfg.ui.tooltip);
        let tip_any = |names: &[&str]| names.iter().any(|n| tip.references(n));

        // 注释总开关关着：模板一律不渲染（`comment_template_for` 给空模板），都不算需求。
        // 悬停总开关不必在这里判：关着时 `CompiledTooltip::compile` 已编出空段列表。
        let templates = if cfg.ui.comment.enabled {
            lib.referenced_texts()
        } else {
            Vec::new()
        };
        let comment_any = |names: &[&str]| templates.iter().any(|t| refs_any(t, names));
        let comment_rev = cfg.ui.comment.enabled && rev_reachable(cfg, lib, facts);
        let comment_chaizi = comment_any(CHAIZI_VARS);
        let correct_pinyin = comment_any(&[CORRECT_PINYIN_VAR]) || tip_any(&[CORRECT_PINYIN_VAR]);

        let tip_code = tip_any(&["word_code", "code_source"]) || tip_any(CODE_REV_VARS);
        let mobile =
            (overlay == wind_config::ConfigOverlay::Mobile).then_some(&cfg.mobile.association);
        let assoc = wind_assoc::AssocConfig::from_config(&cfg.input.association, mobile);
        let assoc_on = assoc.kind != wind_assoc::AssocKind::Off;
        let (s2t, t2s) = cfg.input.conversion_directions();

        Self {
            reverse_index: tip_code
                || comment_rev
                || assoc_on
                || facts.aux_code_in_use
                || facts.auto_phrase,
            user_text: tip_code || comment_rev || facts.aux_code_in_use,
            chaizi: comment_chaizi || tip_any(CHAIZI_VARS),
            s2t,
            t2s,
            correct_pinyin,
            assoc_bigram: assoc.source_enabled(wind_assoc::AssocSource::Bigram),
        }
    }
}

impl crate::coordinator::Coordinator {
    /// 当前配置 + 当前活跃方案下的 [`DataNeeds`]。现算（读方案文件有缓存），只在配置生效、
    /// 切方案、预热时调，不进按键链路。
    pub(crate) fn data_needs(&self) -> DataNeeds {
        DataNeeds::derive(
            &self.rt().config,
            &self.comment_lib(),
            &SchemaFacts::collect(&self.engine_mgr),
            self.profile.config_overlay,
        )
    }
}

#[cfg(test)]
#[path = "data_needs_tests.rs"]
mod tests;
