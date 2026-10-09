//! 「按用到加载」的需求汇总 [`DataNeeds`]（设计 `docs/design/memory-footprint.md` §4.1）。
//!
//! 一批结构（反查索引预热、用户层编码索引、拆字表、简繁表）此前**不看功能是否在用**就加载、
//! 加载后永不释放。本模块回答「当前配置下哪些数据有消费者」，各加载点只问它，不各自判。
//!
//! # 为什么不放进 `ConfigBundle`
//!
//! 判定材料有一半要问 `EngineManager`：方案级 / overlay 的注释模板住在方案文件里，辅助码与
//! 自动造词随**活跃方案**变（切方案不重建 bundle）。快照进 bundle 就会在切方案、或
//! `reload_user_config` 先建 bundle 后 `reload_from_config` 的那个窗口里过期。故判定是一个
//! 纯函数 [`DataNeeds::derive`]（配置 + [`SchemaFacts`]），由 `Coordinator::data_needs`
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
use wind_config::config::CodeHintSource;
use wind_engine::EngineManager;

/// 编码反查（主码表反查索引 + 用户层编码索引）的变量名，含永久兼容别名。
///
/// `code_rev` 也算用户层：它取的 `hint_code` 在系统层该长度没有码时会回落用户码。
const CODE_REV_VARS: &[&str] = &["code_rev", "code", "code_rev_all", "code_all"];
/// 拆字表的变量名。
const CHAIZI_VARS: &[&str] = &["chaizi", "chaizi_code", "chaizi_all", "chaizi_code_all"];

/// 当前配置下哪些按需数据有消费者。字段为 `false` ＝ 不加载（已加载的应释放）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DataNeeds {
    /// 主码表反查索引（预热）：悬停[编码]、注释 `${code_rev}`、联想、辅助码、自动造词任一在用。
    pub(crate) reverse_index: bool,
    /// 用户层「词 → 编码」索引（`UserTextIndex`）：悬停[编码]、注释 `${code_rev*}`、辅助码。
    pub(crate) user_text: bool,
    /// 拆字表：任一层注释模板或启用的悬停段引用 `${chaizi*}`。
    pub(crate) chaizi: bool,
    /// 简入繁出转换器（`input.s2t.enabled`）。
    pub(crate) s2t: bool,
    /// 繁入简出转换器（`input.t2s.enabled`，与 s2t 互斥时让位）。
    pub(crate) t2s: bool,
}

/// 判定材料里要问 `EngineManager` 的那部分。
#[derive(Debug, Clone, Default)]
pub(crate) struct SchemaFacts {
    /// **全部已安装方案**的方案级注释模板（`[candidate].comment_template_*`，含覆盖层）。
    ///
    /// 取并集而不只是活跃方案：方案级模板随切方案生效，而切方案不重算本结构
    /// （`sync_chaizi_assets` 那条除外，它在切方案时也会重算）。并集是保守侧——多装一份拆字表，
    /// 不会出现「切过去拆字段是空的」。
    pub(crate) schema_templates: Vec<String>,
    /// overlay 方案（特殊模式）`[overlay]` 段的注释模板。在主方案编码来源下渲染。
    pub(crate) overlay_templates: Vec<String>,
    /// 辅助码当下引用了码表方案（`aux_code_schemas_in_use` 非空）。
    pub(crate) aux_code_in_use: bool,
    /// 自动造词开着（码表 / 混输方案 + 开关）。造词的查重要主码表反查索引。
    pub(crate) auto_phrase: bool,
    /// 可用方案里有混输方案（`engine.type = "mixed"`）。`schema.mix.pinyin_code_hint` 只在
    /// 这时才有消费者。
    pub(crate) mixed_available: bool,
}

impl SchemaFacts {
    /// 取自 `EngineManager` 的两份缓存（方案文件那份按失效代次、活跃方案那份按方案 id），
    /// 按应用自动切方案这类高频路径上调也不重读方案文件。
    pub(crate) fn collect(mgr: &EngineManager) -> Self {
        let templates = mgr.schema_data_facts();
        let active = mgr.active_data_facts();
        Self {
            schema_templates: templates.schema_templates.clone(),
            overlay_templates: templates.overlay_templates.clone(),
            aux_code_in_use: active.aux_code_in_use,
            auto_phrase: active.auto_phrase,
            mixed_available: mgr
                .available_schemas()
                .iter()
                .any(|id| mgr.schema_engine_type(id).as_deref() == Some("mixed")),
        }
    }
}

fn refs_any(tpl: &str, names: &[&str]) -> bool {
    let t = Template::parse(tpl);
    names.iter().any(|n| t.references(n))
}

impl DataNeeds {
    /// 唯一判定处。规则逐条对应求值端的门控（`Coordinator::eval_var` / 悬停候选循环）：
    ///
    /// - 注释段的 `${code_rev*}` 只在编码来源档 `allows_reverse()` 时求值。档分三份：临拼 / 快捷
    ///   输入期间读 `input.temp_pinyin.code_hint_source`，混输方案读 `schema.mix.pinyin_code_hint`，
    ///   其余读 `schema.pinyin.code_hint_source`（`Coordinator::comment_hint_source`）。后两份
    ///   同属「主档」。全局 / 方案级模板两种期间都可能渲染（模式层没意见
    ///   时落到它们），故两档任一放行即算；临拼 / 快捷输入自己的模式级模板只看临拼那档。
    /// - 悬停段**不看**编码来源档：逐字段走 `eval_text_var`，那里的 `${code_rev}` 不带门控；
    ///   `${word_code}` / `${code_source}` 任一被引用，候选循环就按词查码（含用户层）。
    /// - 拆字不受任何档门控，模板里写了就要。
    /// - 两个总开关在上面这些之上：`ui.candidate.comment_enabled` 关 ⇒ 三层注释模板都不算；
    ///   `ui.tooltip.enabled` 关 ⇒ 悬停段都不算（设计 §4.3）。
    pub(crate) fn derive(cfg: &Config, facts: &SchemaFacts) -> Self {
        let tip = crate::tooltip::CompiledTooltip::compile(&cfg.ui.tooltip);
        let tip_any = |names: &[&str]| names.iter().any(|n| tip.references(n));

        // 主档：拼音方案那份，或混输方案那份（装了混输方案才有消费者）。
        let main_rev =
            CodeHintSource::from_config(&cfg.schema.pinyin.code_hint_source, CodeHintSource::Off)
                .allows_reverse()
                || (facts.mixed_available && cfg.schema.mix.pinyin_code_hint);
        // 临拼档只在临拼 / 快捷输入进得去时才有消费者。
        let temp_reachable = cfg.input.temp_pinyin.enabled || !cfg.schema.mix_modes.is_empty();
        let temp_rev = temp_reachable
            && CodeHintSource::from_config(
                &cfg.input.temp_pinyin.code_hint_source,
                CodeHintSource::Auto,
            )
            .allows_reverse();

        // (模板, 是否会以临拼档渲染, 是否会以主档渲染)
        let both = |t: &str| (t.to_string(), true, true);
        let mut layers: Vec<(String, bool, bool)> = Vec::new();
        for v in [true, false] {
            layers.push(both(cfg.ui.candidate.comment_template(v)));
        }
        layers.extend(facts.schema_templates.iter().map(|t| both(t)));
        // 模式级：临拼 / 快捷输入只以临拼档渲染，其余模式只以主档渲染。
        let temp_modes = cfg
            .schema
            .mix_modes
            .iter()
            .flat_map(|m| [&m.comment_template_vertical, &m.comment_template_horizontal])
            .chain([
                &cfg.input.temp_pinyin.comment_template_vertical,
                &cfg.input.temp_pinyin.comment_template_horizontal,
            ]);
        layers.extend(temp_modes.flatten().map(|t| (t.clone(), true, false)));
        let main_modes = [
            &cfg.input.temp_english.comment_template_vertical,
            &cfg.input.temp_english.comment_template_horizontal,
            &cfg.input.url.comment_template_vertical,
            &cfg.input.url.comment_template_horizontal,
            &cfg.input.email.comment_template_vertical,
            &cfg.input.email.comment_template_horizontal,
            &cfg.input.unicode.comment_template_vertical,
            &cfg.input.unicode.comment_template_horizontal,
        ];
        layers.extend(
            main_modes
                .into_iter()
                .flatten()
                .chain(&facts.overlay_templates)
                .map(|t| (t.clone(), false, true)),
        );

        // 注释总开关关着：三层模板一律不渲染（`comment_template_for` 给空模板），都不算需求。
        // 悬停总开关不必在这里判：关着时 `CompiledTooltip::compile` 已编出空段列表。
        let comment_on = cfg.ui.candidate.comment_enabled;
        let comment_rev = comment_on
            && layers.iter().any(|(t, in_temp, in_main)| {
                ((*in_temp && temp_rev) || (*in_main && main_rev)) && refs_any(t, CODE_REV_VARS)
            });
        let comment_chaizi = comment_on && layers.iter().any(|(t, _, _)| refs_any(t, CHAIZI_VARS));

        let tip_code = tip_any(&["word_code", "code_source"]) || tip_any(CODE_REV_VARS);
        let mobile = crate::handle_assoc::use_mobile_overrides().then_some(&cfg.mobile.association);
        let assoc_on = wind_assoc::AssocConfig::from_config(&cfg.input.association, mobile).kind
            != wind_assoc::AssocKind::Off;
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
        }
    }
}

impl crate::coordinator::Coordinator {
    /// 当前配置 + 当前活跃方案下的 [`DataNeeds`]。现算（读方案文件有缓存），只在配置生效、
    /// 切方案、预热时调，不进按键链路。
    pub(crate) fn data_needs(&self) -> DataNeeds {
        DataNeeds::derive(&self.rt().config, &SchemaFacts::collect(&self.engine_mgr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::Coordinator;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    // ───────────────────────── 纯判定 ─────────────────────────

    /// 出厂配置 = L1 ⊕ L2：直接拿仓库里的 `data/config.toml` 反序列化（缺的键走 serde 默认即 L1）。
    /// 不用 `Config::load`：它会叠上本机用户层。
    fn factory() -> Config {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/config.toml");
        let text = std::fs::read_to_string(&p).expect("读出厂 data/config.toml");
        let mut c: Config = toml::from_str(&text).expect("出厂配置可解析");
        c.normalize();
        c
    }

    fn derive(c: &Config) -> DataNeeds {
        DataNeeds::derive(c, &SchemaFacts::default())
    }

    /// 关掉悬停「编码」段（出厂唯一引用 `${word_code}` 的段）。
    fn without_code_section(mut c: Config) -> Config {
        let mut hit = false;
        for s in &mut c.ui.tooltip.sections {
            if Template::parse(&s.template).references("word_code") {
                s.enabled = false;
                hit = true;
            }
        }
        assert!(
            hit,
            "前置条件：出厂悬停段里有引用 ${{word_code}} 的「编码」段"
        );
        c
    }

    #[test]
    fn factory_needs() {
        let n = derive(&factory());
        assert_eq!(
            n,
            DataNeeds {
                reverse_index: true,
                user_text: true,
                chaizi: false,
                s2t: false,
                t2s: false,
            },
            "出厂：悬停[编码]开着 ⇒ 反查与用户层要；拆字段关着、注释不含拆字；简繁两向都关"
        );
        assert_eq!(derive(&Config::default()), n, "L1 与 L2 在这几项上应一致");
    }

    /// 关掉悬停[编码]后反查仍然要：出厂全局注释模板含 `${code_rev}`，临拼 / 快捷输入的
    /// 编码来源出厂 `auto`。把那一档也关了才真正不要。
    #[test]
    fn code_section_off_leaves_temp_pinyin_code_rev_as_consumer() {
        let c = without_code_section(factory());
        let n = derive(&c);
        assert!(
            n.reverse_index && n.user_text,
            "临拼档 auto + 全局模板 code_rev 仍是消费者"
        );

        let mut off = c.clone();
        off.input.temp_pinyin.code_hint_source = "off".into();
        let n = derive(&off);
        assert!(
            !n.reverse_index && !n.user_text,
            "两档都不放行反查、悬停又没编码段 ⇒ 都不要"
        );

        // 主方案那档放行同样算。
        let mut main = off.clone();
        main.schema.pinyin.code_hint_source = "codetable".into();
        assert!(derive(&main).reverse_index && derive(&main).user_text);

        // 临拼与快捷输入都进不去时，临拼档再放行也没有消费者。
        let mut unreachable = c.clone();
        unreachable.input.temp_pinyin.enabled = false;
        unreachable.schema.mix_modes.clear();
        assert!(!derive(&unreachable).reverse_index);
    }

    /// 混输方案的编码提示开关（`schema.mix.pinyin_code_hint`，出厂开）只在**装了混输方案**
    /// 时算消费者：纯拼音用户不该为一个用不上的开关常驻反查索引。
    #[test]
    fn mix_pinyin_code_hint_counts_only_with_mixed_schema() {
        let mut off = without_code_section(factory());
        off.input.temp_pinyin.code_hint_source = "off".into();
        assert!(off.schema.mix.pinyin_code_hint, "前置条件：出厂开");
        let with_mixed = SchemaFacts {
            mixed_available: true,
            ..Default::default()
        };

        assert!(!derive(&off).reverse_index, "没装混输方案 ⇒ 不要");
        let n = DataNeeds::derive(&off, &with_mixed);
        assert!(n.reverse_index && n.user_text, "装了混输方案 ⇒ 要");

        let mut switched_off = off.clone();
        switched_off.schema.mix.pinyin_code_hint = false;
        assert!(!DataNeeds::derive(&switched_off, &with_mixed).reverse_index);
    }

    /// 其余反查来源：联想、辅助码、自动造词。用户层只跟悬停 / 注释 / 辅助码。
    #[test]
    fn other_reverse_index_consumers() {
        let mut base = without_code_section(factory());
        base.input.temp_pinyin.code_hint_source = "off".into();
        assert!(!derive(&base).reverse_index, "前置条件：基线什么都不要");

        let mut assoc = base.clone();
        assoc.input.association.kind = "word".into();
        let n = derive(&assoc);
        assert!(n.reverse_index && !n.user_text, "联想只要系统层反查");

        let aux = DataNeeds::derive(
            &base,
            &SchemaFacts {
                aux_code_in_use: true,
                ..Default::default()
            },
        );
        assert!(aux.reverse_index && aux.user_text, "辅助码两层都查");

        let auto = DataNeeds::derive(
            &base,
            &SchemaFacts {
                auto_phrase: true,
                ..Default::default()
            },
        );
        assert!(
            auto.reverse_index && !auto.user_text,
            "自动造词查重只用系统层"
        );
    }

    /// 悬停段里写 `${code_rev}`（逐字段不带编码来源门控）也算编码消费者。
    #[test]
    fn tooltip_code_rev_counts_regardless_of_hint_source() {
        let mut c = without_code_section(factory());
        c.input.temp_pinyin.code_hint_source = "off".into();
        let mut sec = c.ui.tooltip.sections[0].clone();
        sec.label = "码".into();
        sec.each = "han".into();
        sec.template = "${code_rev}".into();
        sec.enabled = true;
        c.ui.tooltip.sections.push(sec);
        let n = derive(&c);
        assert!(n.reverse_index && n.user_text);
    }

    fn chaizi_base() -> Config {
        let c = factory();
        assert!(!derive(&c).chaizi, "前置条件：出厂不要拆字");
        c
    }

    #[test]
    fn chaizi_in_global_template() {
        let mut c = chaizi_base();
        c.ui.candidate.comment_template_vertical = "${chaizi_all:／}".into();
        assert!(derive(&c).chaizi);
    }

    #[test]
    fn chaizi_in_schema_template() {
        let facts = SchemaFacts {
            schema_templates: vec!["{(${chaizi})}".into()],
            ..Default::default()
        };
        assert!(DataNeeds::derive(&chaizi_base(), &facts).chaizi);
    }

    #[test]
    fn chaizi_in_mode_template() {
        let mut c = chaizi_base();
        c.input.temp_english.comment_template_horizontal = Some("${chaizi_code}".into());
        assert!(derive(&c).chaizi, "模式级（临英）");
        let mut c = chaizi_base();
        c.schema.mix_modes[0].comment_template_vertical = Some("${chaizi}".into());
        assert!(derive(&c).chaizi, "模式级（快捷输入）");
        let facts = SchemaFacts {
            overlay_templates: vec!["${chaizi_code_all}".into()],
            ..Default::default()
        };
        assert!(
            DataNeeds::derive(&chaizi_base(), &facts).chaizi,
            "模式级（特殊模式 overlay）"
        );
    }

    #[test]
    fn chaizi_in_enabled_tooltip_section_only() {
        let mut c = chaizi_base();
        for s in &mut c.ui.tooltip.sections {
            if Template::parse(&s.template).references("chaizi") {
                s.enabled = true;
            }
        }
        assert!(derive(&c).chaizi, "出厂拆字段打开 ⇒ 要");
    }

    // ───────────────────── 两个总开关（设计 §4.3，S2） ─────────────────────

    /// 打开出厂「拆字」悬停段（出厂关着）。
    fn with_chaizi_tip_section(mut c: Config) -> Config {
        let mut hit = false;
        for s in &mut c.ui.tooltip.sections {
            if Template::parse(&s.template).references("chaizi") {
                s.enabled = true;
                hit = true;
            }
        }
        assert!(hit, "前置条件：出厂悬停段里有引用 ${{chaizi}} 的「拆字」段");
        c
    }

    /// 三层注释模板都引用拆字与编码反查：全局、模式级（临英）、方案级与 overlay（经 facts）。
    fn all_layers_reference_chaizi(mut c: Config) -> (Config, SchemaFacts) {
        c.ui.candidate.comment_template_vertical = "${code_rev}${chaizi}".into();
        c.ui.candidate.comment_template_horizontal = "${code_rev}${chaizi}".into();
        c.input.temp_english.comment_template_vertical = Some("${chaizi}".into());
        let facts = SchemaFacts {
            schema_templates: vec!["${chaizi}".into()],
            overlay_templates: vec!["${chaizi_code}".into()],
            ..Default::default()
        };
        (c, facts)
    }

    /// 两个开关都关：出厂里会让反查 / 用户层 / 拆字为真的贡献全部归零。
    ///
    /// 前提里联想关着（出厂 `off`）、辅助码与自动造词不在用（`SchemaFacts` 缺省）——它们
    /// 不属注释也不属悬停，仍能让 `reverse_index` 为真，另见下一条。
    #[test]
    fn both_toggles_off_zero_reverse_user_text_and_chaizi() {
        let (c, facts) = all_layers_reference_chaizi(with_chaizi_tip_section(factory()));
        let on = DataNeeds::derive(&c, &facts);
        assert!(
            on.reverse_index && on.user_text && on.chaizi,
            "前置条件：开关都开时三项都要：{on:?}"
        );

        let mut off = c.clone();
        off.ui.tooltip.enabled = false;
        off.ui.candidate.comment_enabled = false;
        let n = DataNeeds::derive(&off, &facts);
        assert_eq!(
            (n.reverse_index, n.user_text, n.chaizi),
            (false, false, false),
            "两开关都关 ⇒ 注释三层与悬停段的贡献都归零：{n:?}"
        );
    }

    /// 两开关都关后，注释 / 悬停以外的反查消费者照旧：联想、辅助码、自动造词。
    #[test]
    fn both_toggles_off_leave_other_reverse_consumers() {
        let mut c = factory();
        c.ui.tooltip.enabled = false;
        c.ui.candidate.comment_enabled = false;
        assert!(!derive(&c).reverse_index, "前置条件：基线什么都不要");

        let mut assoc = c.clone();
        assoc.input.association.kind = "word".into();
        let n = derive(&assoc);
        assert!(n.reverse_index && !n.user_text, "联想仍要系统层反查");

        let aux = DataNeeds::derive(
            &c,
            &SchemaFacts {
                aux_code_in_use: true,
                ..Default::default()
            },
        );
        assert!(aux.reverse_index && aux.user_text, "辅助码仍两层都查");

        let auto = DataNeeds::derive(
            &c,
            &SchemaFacts {
                auto_phrase: true,
                ..Default::default()
            },
        );
        assert!(auto.reverse_index && !auto.user_text, "自动造词仍查重");
    }

    /// 只关注释：注释的贡献没了（拆字只有注释在要 ⇒ 不要），悬停「编码」段的贡献还在。
    #[test]
    fn comment_off_keeps_tooltip_contribution() {
        let (mut c, facts) = all_layers_reference_chaizi(factory());
        assert!(
            DataNeeds::derive(&c, &facts).chaizi,
            "前置条件：拆字只由注释三层要"
        );
        c.ui.candidate.comment_enabled = false;
        let n = DataNeeds::derive(&c, &facts);
        assert!(!n.chaizi, "注释关掉 ⇒ 三层模板里的拆字都不算：{n:?}");
        assert!(
            n.reverse_index && n.user_text,
            "悬停「编码」段仍开着 ⇒ 反查与用户层照要：{n:?}"
        );
    }

    /// 只关悬停：悬停段的贡献没了（拆字只有悬停段在要 ⇒ 不要），注释的反查贡献还在。
    #[test]
    fn tooltip_off_keeps_comment_contribution() {
        let mut c = with_chaizi_tip_section(factory());
        assert!(derive(&c).chaizi, "前置条件：拆字只由悬停段要");
        c.ui.tooltip.enabled = false;
        let n = derive(&c);
        assert!(!n.chaizi, "悬停关掉 ⇒ 拆字段不算：{n:?}");
        assert!(
            n.reverse_index && n.user_text,
            "出厂全局注释 ${{code_rev}} + 临拼档 auto 仍是消费者：{n:?}"
        );
        // 反证上一句确实来自注释：再关注释就都不要了。
        c.ui.candidate.comment_enabled = false;
        let n = derive(&c);
        assert!(!n.reverse_index && !n.user_text, "{n:?}");
    }

    #[test]
    fn conversion_follows_enabled_directions() {
        let mut c = factory();
        c.input.t2s.enabled = true;
        let n = derive(&c);
        assert_eq!((n.s2t, n.t2s), (false, true));
        c.input.s2t.enabled = true;
        let n = derive(&c);
        assert_eq!((n.s2t, n.t2s), (true, false), "两向互斥时 s2t 赢，只预载它");
    }

    /// 方案级模板经 `SchemaFacts::collect` 从方案文件读到（含未激活的已安装方案）。
    #[test]
    fn facts_collect_schema_level_templates() {
        let dir = schema_fixture("facts", "zz_dn", false);
        let mut cfg = Config::default();
        cfg.schema.active = "zz_dn".into();
        cfg.schema.available = vec!["zz_dn".into()];
        let mgr = EngineManager::new(&cfg, Some(&dir));
        let facts = SchemaFacts::collect(&mgr);
        assert!(
            facts
                .schema_templates
                .iter()
                .any(|t| t.contains("${chaizi}")),
            "方案级模板应被收集到：{:?}",
            facts.schema_templates
        );
        assert!(DataNeeds::derive(&cfg, &facts).chaizi);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ───────────────────────── 协调器端到端 ─────────────────────────

    fn data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    fn has_data() -> bool {
        data_dir().join("schemas/wubi86.schema.toml").exists()
    }

    fn wubi(edit: impl FnOnce(&mut Config)) -> Config {
        let mut c = factory();
        c.schema.active = "wubi86".into();
        c.schema.available = vec!["wubi86".into()];
        edit(&mut c);
        c
    }

    /// 自造码表方案：一个字「好」、拆字库一行、方案级注释模板引用 `${chaizi}`（或不引用）。
    fn schema_fixture(tag: &str, id: &str, plain: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wind_dn_{}_{tag}", std::process::id()));
        let schemas = dir.join("schemas");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(schemas.join(id)).unwrap();
        let tpl = if plain { "${code_hint}" } else { "${chaizi}" };
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"按需\"\n\
                 [engine]\ntype = \"codetable\"\n\
                 [engine.codetable]\nmax_code_length = 4\n\
                 [engine.chaizi]\ndb_path = \"{id}/chaizi.txt\"\n\
                 [[dictionaries]]\nid = \"main\"\npath = \"{id}/{id}.dict.yaml\"\ndefault = true\n\
                 [candidate]\ncomment_template_vertical = \"{tpl}\"\n\
                 comment_template_horizontal = \"{tpl}\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            schemas.join(format!("{id}/{id}.dict.yaml")),
            format!("---\nname: {id}\nversion: \"1\"\n...\n好\tvb\n"),
        )
        .unwrap();
        std::fs::write(schemas.join(format!("{id}/chaizi.txt")), "好\t女子\tvb\n").unwrap();
        dir
    }

    fn has_chaizi(c: &Coordinator) -> bool {
        c.reverse.read().unwrap().has_chaizi()
    }

    /// 方案级模板引用拆字 ⇒ 构造期就装上；不引用 ⇒ 不装。
    #[test]
    fn schema_level_chaizi_loads_at_construction() {
        for (plain, want) in [(false, true), (true, false)] {
            let id = "zz_dn_cz";
            let dir = schema_fixture(if plain { "plain" } else { "cz" }, id, plain);
            let mut cfg = Config::default();
            cfg.schema.active = id.into();
            cfg.schema.available = vec![id.into()];
            let c = Coordinator::new_headless(cfg, Some(&dir));
            assert_eq!(has_chaizi(&c), want, "plain={plain}");
            if want {
                assert_eq!(c.reverse.read().unwrap().radicals_of("好", ""), "女子");
            }
            drop(c);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// 候选「好」按当前生效的注释模板渲染（与候选窗同一裁决与求值链，见 `alt_commit_text`）。
    fn comment_of_hao(c: &Coordinator) -> String {
        let st = c.state.lock().unwrap();
        let cand = wind_candidate::Candidate {
            text: "好".into(),
            ..Default::default()
        };
        c.alt_commit_text(&st, &cand, wind_config::config::AltCommit::Comment)
    }

    /// 拆字：出厂不装；全局模板加上 `${chaizi}` 并生效配置后装上、注释出字根；改回后卸载。
    #[test]
    fn chaizi_follows_config_load_and_unload() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let c = Coordinator::new_headless(wubi(|_| {}), Some(&data_dir()));
        assert!(!has_chaizi(&c), "出厂没有拆字消费者，不该装拆字表");

        c.refresh_config_in_memory(|cfg| {
            cfg.ui.candidate.comment_template_vertical = "${chaizi}".into();
            cfg.ui.candidate.comment_template_horizontal = "${chaizi}".into();
        });
        c.apply_data_needs();
        assert!(has_chaizi(&c), "模板引用拆字后应装上");
        let want = c.reverse.read().unwrap().radicals_of("好", "");
        assert!(!want.is_empty(), "前置条件：wubi86 拆字库收了「好」");
        assert_eq!(comment_of_hao(&c), want, "注释应出拆字");

        let factory_cfg = wubi(|_| {});
        c.refresh_config_in_memory(|cfg| {
            cfg.ui.candidate.comment_template_vertical =
                factory_cfg.ui.candidate.comment_template_vertical.clone();
            cfg.ui.candidate.comment_template_horizontal =
                factory_cfg.ui.candidate.comment_template_horizontal.clone();
        });
        c.apply_data_needs();
        assert!(!has_chaizi(&c), "不再引用后应卸载");
    }

    /// 运行期格式串（cmdbar `dict.rev`）要拆字而表没装：本次空、后台装好后下次就有。
    #[test]
    fn runtime_format_loads_chaizi_in_background() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let c = Coordinator::new_headless(wubi(|_| {}), Some(&data_dir()));
        assert!(!has_chaizi(&c), "前置条件：出厂不装");
        assert_eq!(
            c.reverse_render("好", "${chaizi}"),
            "",
            "首次用到：本次为空"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !has_chaizi(&c) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(has_chaizi(&c), "后台应装上拆字表");
        assert!(!c.reverse_render("好", "${chaizi}").is_empty(), "下次就有");
    }

    /// 预热：不需要反查时不建主码表反查索引；出厂（要）时照建。
    #[test]
    fn prewarm_skips_reverse_index_when_not_needed() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let none = wubi(|c| {
            *c = without_code_section(c.clone());
            c.input.temp_pinyin.code_hint_source = "off".into();
        });
        let c = Coordinator::new_headless(none, Some(&data_dir()));
        assert!(!c.data_needs().reverse_index, "前置条件");
        c.prewarm_indexes();
        assert!(
            c.engine_mgr.reverse_index_if_ready("wubi86").is_none(),
            "没有消费者就不该预热主码表反查索引"
        );

        let c = Coordinator::new_headless(wubi(|_| {}), Some(&data_dir()));
        c.prewarm_indexes();
        assert!(
            c.engine_mgr.reverse_index_if_ready("wubi86").is_some(),
            "出厂（悬停[编码]开着）照常预热"
        );
    }

    /// 用户层编码索引：配置不再需要时，生效后清空。
    #[test]
    fn user_text_cleared_when_not_needed() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let path = std::env::temp_dir().join(format!("wind_dn_ut_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(wind_store::Store::open(&path).unwrap());
        let c = Coordinator::new_headless_with_store(wubi(|_| {}), Some(&data_dir()), store);
        c.engine_mgr.prewarm_text_codes("wubi86");
        assert_eq!(c.engine_mgr.user_text_loaded(), 1, "前置条件：用户层已建");

        c.apply_data_needs();
        assert_eq!(c.engine_mgr.user_text_loaded(), 1, "仍有消费者时不清");

        c.refresh_config_in_memory(|cfg| {
            *cfg = without_code_section(cfg.clone());
            cfg.input.temp_pinyin.code_hint_source = "off".into();
        });
        c.apply_data_needs();
        assert_eq!(c.engine_mgr.user_text_loaded(), 0, "没有消费者 ⇒ 清空");
        drop(c);
        let _ = std::fs::remove_file(&path);
    }

    /// S3：大库上用户层编码索引（`UserTextIndex`）建完只标记待回收，库空闲满短档后回收——
    /// 预热与打字链路触发的后台重建两条路都是；建完当场不回收（不让按键线程等关库重开）。
    #[test]
    fn user_text_rebuild_reclaims_after_short_idle() {
        use std::time::Duration;
        const TICK: Duration = Duration::from_millis(20);
        const SCAN_IDLE: Duration = Duration::from_millis(150);
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let path = std::env::temp_dir().join(format!("wind_dn_s3_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(wind_store::Store::open(&path).unwrap());
        let c =
            Coordinator::new_headless_with_store(wubi(|_| {}), Some(&data_dir()), store.clone());
        // 过门槛的一份用户词：小库扫完不标记（`SCAN_RECLAIM_MIN_ROWS`）。
        let big: Vec<_> = (0..wind_store::store::SCAN_RECLAIM_MIN_ROWS)
            .map(|i| wind_store::wdict::WordIo {
                code: format!("x{i}"),
                text: format!("词{i}"),
                weight: 0,
                count: 0,
                boundary: None,
            })
            .collect();
        store.import_user_words("wubi86", &big).unwrap();
        store.spawn_idle_cache_reclaimer(Duration::from_secs(3600), SCAN_IDLE, TICK);
        let wait_drops = |n: u64| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if store.page_cache_drops() >= n {
                    return true;
                }
                std::thread::sleep(TICK / 2);
            }
            false
        };
        let d0 = store.page_cache_drops();
        assert!(c.engine_mgr.prewarm_text_codes("wubi86"));
        assert_eq!(store.page_cache_drops(), d0, "预热建完不当场回收");
        assert!(wait_drops(d0 + 1), "空闲满短档后回收");

        store.add_user_word("wubi86", "zzzz", "嗨", 0, 0).unwrap();
        let _ = c.engine_mgr.text_codes("wubi86"); // 过期 ⇒ 后台重建
        assert!(wait_drops(d0 + 2), "后台重建之后同样在空闲满短档后回收");
        drop(c);
        let _ = std::fs::remove_file(&path);
    }

    /// 端到端（S2）：注释与悬停都在要拆字 / 用户层时，关掉两个总开关并生效配置 ⇒
    /// 用户层编码索引清空、拆字表卸载。
    #[test]
    fn toggles_off_release_user_text_and_chaizi() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let path = std::env::temp_dir().join(format!("wind_dn_s2_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(wind_store::Store::open(&path).unwrap());
        let cfg = wubi(|c| {
            *c = with_chaizi_tip_section(c.clone());
            c.ui.candidate.comment_template_vertical = "${chaizi}".into();
            c.ui.candidate.comment_template_horizontal = "${chaizi}".into();
        });
        let c = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
        c.engine_mgr.prewarm_text_codes("wubi86");
        assert!(
            has_chaizi(&c),
            "前置条件：注释与悬停都引用拆字 ⇒ 构造期装上"
        );
        assert_eq!(c.engine_mgr.user_text_loaded(), 1, "前置条件：用户层已建");

        c.refresh_config_in_memory(|cfg| {
            cfg.ui.tooltip.enabled = false;
            cfg.ui.candidate.comment_enabled = false;
        });
        c.apply_data_needs();
        assert_eq!(
            c.engine_mgr.user_text_loaded(),
            0,
            "两开关关掉 ⇒ 用户层没有消费者，应清空"
        );
        assert!(!has_chaizi(&c), "两开关关掉 ⇒ 拆字表应卸载");
        drop(c);
        let _ = std::fs::remove_file(&path);
    }

    /// 简繁：出厂两个转换器都不装；开着的方向构造期就装。
    #[test]
    fn converters_preload_only_enabled_direction() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let c = Coordinator::new_headless(wubi(|_| {}), Some(&data_dir()));
        assert_eq!(c.debug_conversion().1, (false, false), "出厂两向都不预载");

        let c = Coordinator::new_headless(wubi(|c| c.input.t2s.enabled = true), Some(&data_dir()));
        assert_eq!(c.debug_conversion().1, (false, true), "只预载开着的 t2s");
    }

    /// 同步加载耗时（release 下跑：`cargo test --release -p wind-coordinator
    /// data_needs::tests::converter_load_timing -- --ignored --nocapture`）。
    #[test]
    #[ignore]
    fn converter_load_timing() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let cfg = Config::default();
        for to_traditional in [true, false, true, false, true, false] {
            let t0 = std::time::Instant::now();
            let conv = crate::coordinator::load_converter(Some(&data_dir()), &cfg, to_traditional);
            let dt = t0.elapsed();
            assert!(conv.is_some());
            println!(
                "{} 同步加载 {:.2} ms",
                if to_traditional { "s2t" } else { "t2s" },
                dt.as_secs_f64() * 1000.0
            );
        }
    }

    // ───────────────────────── 审查修复回归 ─────────────────────────

    /// 一个反查消费者都没有的五笔配置。
    fn no_consumers() -> Config {
        wubi(|c| {
            *c = without_code_section(c.clone());
            c.input.temp_pinyin.code_hint_source = "off".into();
        })
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// [HIGH] 运行期 reverse_index 从 false 变 true（设置页打开联想）：配置生效后要补建索引，
    /// 否则联想 / `${code_rev}` 只问 `reverse_index_if_ready`，永远拿不到、也不自愈。
    #[test]
    fn enabling_a_consumer_at_runtime_builds_the_reverse_index() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let c = Coordinator::new_headless(no_consumers(), Some(&data_dir()));
        assert!(!c.data_needs().reverse_index, "前置条件");
        c.prewarm_indexes();
        assert!(
            c.engine_mgr.reverse_index_if_ready("wubi86").is_none(),
            "前置条件"
        );

        c.refresh_config_in_memory(|cfg| cfg.input.association.kind = "word".into());
        c.apply_data_needs();
        assert!(
            wait_for(|| c.engine_mgr.reverse_index_if_ready("wubi86").is_some()),
            "联想打开后应在后台补建主码表反查索引"
        );
    }

    /// [HIGH] 运行期格式串里的 `${code_rev}`（cmdbar `dict.rev`）：首次用到时后台建索引，
    /// 本次为空、下次就有。
    #[test]
    fn runtime_code_rev_builds_the_reverse_index() {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
        let c = Coordinator::new_headless(no_consumers(), Some(&data_dir()));
        c.prewarm_indexes();
        let primary = c.engine_mgr.primary_codetable_id();
        assert!(!primary.is_empty(), "前置条件：有主码表");
        assert!(
            c.engine_mgr.reverse_index_if_ready(&primary).is_none(),
            "前置条件"
        );
        assert_eq!(
            c.reverse_render("工", "${code_rev}"),
            "",
            "首次：索引未就绪，本次为空"
        );
        assert!(
            wait_for(|| c.engine_mgr.reverse_index_if_ready(&primary).is_some()),
            "首次用到应触发后台构建"
        );
        assert!(
            !c.reverse_render("工", "${code_rev}").is_empty(),
            "下次就有"
        );
    }

    /// 写一个配了拆字库、但库文件**不存在**的码表方案。
    fn missing_chaizi_fixture(tag: &str, id: &str) -> PathBuf {
        let dir = schema_fixture(tag, id, true);
        std::fs::remove_file(dir.join(format!("schemas/{id}/chaizi.txt"))).unwrap();
        dir
    }

    fn fixture_coord(dir: &Path, id: &str) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.schema.active = id.into();
        cfg.schema.available = vec![id.into()];
        Coordinator::new_headless(cfg, Some(dir))
    }

    /// [MEDIUM] 运行期拆字：单飞闸要排在解析路径（读方案文件）之前；找不到库也记一笔
    /// 「试过了」，不再每键重试、每键告警。
    #[test]
    fn runtime_chaizi_resolves_at_most_once_until_sync() {
        let id = "zz_dn_miss";
        let dir = missing_chaizi_fixture("miss", id);
        let c = fixture_coord(&dir, id);
        for _ in 0..3 {
            assert_eq!(c.reverse_render("好", "${chaizi}"), "");
        }
        assert_eq!(
            c.chaizi_assets.lock().unwrap().resolve_attempts,
            1,
            "找不到库也只解析一次"
        );
        c.sync_chaizi_assets();
        assert_eq!(c.reverse_render("好", "${chaizi}"), "");
        assert_eq!(
            c.chaizi_assets.lock().unwrap().resolve_attempts,
            2,
            "sync 之后（可能换了方案）给一次重试"
        );
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [LOW] 后台拆字加载与切方案竞争：换入前比代次，过期的表不装。
    #[test]
    fn stale_runtime_chaizi_load_is_discarded() {
        let id = "zz_dn_gen";
        let dir = schema_fixture("gen", id, true);
        let c = fixture_coord(&dir, id);
        assert!(!has_chaizi(&c), "前置条件：plain 模板不装");
        let path = dir.join(format!("schemas/{id}/chaizi.txt"));
        let gen0 = c.chaizi_assets.lock().unwrap().generation;
        c.sync_chaizi_assets(); // 期间发生了一次 sync（切方案）
        let fresh = wind_reverse::ReverseLookup::load(None, Some(&path));
        c.install_runtime_chaizi(gen0, path.clone(), fresh);
        assert!(!has_chaizi(&c), "代次已变，旧加载结果不该换入");

        let gen1 = c.chaizi_assets.lock().unwrap().generation;
        let fresh = wind_reverse::ReverseLookup::load(None, Some(&path));
        c.install_runtime_chaizi(gen1, path, fresh);
        assert!(has_chaizi(&c), "代次一致时照常换入");
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [LOW] 加载线程 panic 也要复位 `loading`，否则此后永不再试。
    #[test]
    fn chaizi_loading_flag_resets_on_panic() {
        let id = "zz_dn_panic";
        let dir = schema_fixture("panic", id, true);
        let c = fixture_coord(&dir, id);
        c.chaizi_assets.lock().unwrap().loading = true;
        let weak = Arc::downgrade(&c);
        let r = std::thread::spawn(move || {
            let _g = crate::coordinator::ChaiziLoadingGuard(weak);
            panic!("模拟加载线程 panic");
        })
        .join();
        assert!(r.is_err());
        assert!(
            !c.chaizi_assets.lock().unwrap().loading,
            "panic 后 loading 应复位"
        );
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [LOW] 运行期按需装上的拆字表，配置生效时不该被卸（方案拆字库路径没变的前提下）。
    #[test]
    fn runtime_loaded_chaizi_survives_config_apply() {
        let id = "zz_dn_keep";
        let dir = schema_fixture("keep", id, true);
        let c = fixture_coord(&dir, id);
        // 首次用到只负责触发后台装载；返回空还是已装上取决于调度（macOS CI 上后台抢先装完、
        // 直接返回了「女子」），不在这里断言。
        let _ = c.reverse_render("好", "${chaizi}");
        assert!(wait_for(|| has_chaizi(&c)), "前置条件：后台装上");
        c.apply_data_needs();
        assert!(
            has_chaizi(&c),
            "配置里仍没有拆字消费者，但运行期装的不该被卸"
        );
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
