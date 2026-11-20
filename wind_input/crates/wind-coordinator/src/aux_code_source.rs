//! 辅助码的运行时来源：`[engine.aux_code].files` 按序解析出的各层，查询时顺序拼接。
//!
//! 方案来源不能预先合并成一张表：它的码来自「系统词库 + 用户词库」，用户层会随造词后台
//! 重建，只能在每次筛选时取当下的视图（`EngineManager::text_codes`）。文件来源仍是进来时读
//! 一次的静态表，且**相邻的文件来源照 wind-aux-code 的规矩 `merge` 坍缩成一张**——只有方案
//! 来源各占一层，层数 = 方案来源数 + 被它们隔开的文件段数。设计见
//! `docs/design/aux-code-schema-source.md` §4、§5。

use std::sync::Arc;
use wind_aux_code::{AuxCodeLookup, AuxCodeTable};
use wind_engine::{AuxSource, EngineManager, TextCodeView};

pub(crate) struct AuxCodeRuntime {
    /// 由哪组来源建成——缓存键。来源变了（改了 override、换了方案）就重建。
    key: Vec<AuxSource>,
    /// 模式指示用的名字：首个来源的名（码表 `# name:` 或方案名）。
    name: String,
    layers: Vec<Layer>,
}

enum Layer {
    Table(AuxCodeTable),
    Schema(String),
}

impl AuxCodeRuntime {
    /// 文件来源同步读（码表小，与改动前同一时机）；方案来源只记 id，数据在查询时取。
    ///
    /// 相邻的文件来源 `merge` 成一张表（`load_merged`：先出现 = 高优、跨表同码去重），
    /// 被方案来源隔开的各段分别合并——保持「清单顺序即优先级」不变。
    pub(crate) fn build(sources: &[AuxSource], engine: &EngineManager) -> Self {
        let mut name = String::new();
        let mut layers = Vec::new();
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        // 把攒着的一段相邻文件来源合成一层。名字取首个非空，与 `merge` 的取名规则同序。
        let flush =
            |files: &mut Vec<std::path::PathBuf>, layers: &mut Vec<Layer>, name: &mut String| {
                if files.is_empty() {
                    return;
                }
                let t = wind_aux_code::load_merged(files);
                files.clear();
                if name.is_empty() {
                    *name = t.name.clone();
                }
                layers.push(Layer::Table(t));
            };
        for s in sources {
            match s {
                AuxSource::File(p) => files.push(p.clone()),
                AuxSource::Schema(id) => {
                    flush(&mut files, &mut layers, &mut name);
                    if name.is_empty() {
                        name = engine.schema_name(id);
                    }
                    layers.push(Layer::Schema(id.clone()));
                }
            }
        }
        flush(&mut files, &mut layers, &mut name);
        Self {
            key: sources.to_vec(),
            name,
            layers,
        }
    }

    /// 层数（文件段合并后）。
    #[cfg(test)]
    pub(crate) fn layer_count(&self) -> usize {
        self.layers.len()
    }

    pub(crate) fn matches(&self, sources: &[AuxSource]) -> bool {
        self.key == sources
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn schema_ids(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().filter_map(|l| match l {
            Layer::Schema(id) => Some(id.as_str()),
            Layer::Table(_) => None,
        })
    }

    /// 本次筛选用的查询对象：方案层取当下视图（用户层过期会在这里触发后台重建）。
    ///
    /// 持有运行时的 `Arc` 而不是借用：直接辅助码要把它装进交给引擎的准入闭包
    /// （`ConvertOptions::admit`，`'static`），借用版本进不去。
    pub(crate) fn lookup(self: &Arc<Self>, engine: &EngineManager) -> AuxLookupNow {
        AuxLookupNow {
            views: self
                .layers
                .iter()
                .map(|l| match l {
                    Layer::Table(_) => None,
                    Layer::Schema(id) => Some(engine.text_codes(id)),
                })
                .collect(),
            rt: self.clone(),
        }
    }
}

/// 一次查询用的来源快照：运行时各层 + 方案层当下的视图（与 `rt.layers` 一一对应，文件层为 `None`）。
pub(crate) struct AuxLookupNow {
    rt: Arc<AuxCodeRuntime>,
    views: Vec<Option<TextCodeView>>,
}

impl AuxLookupNow {
    /// 系统层（反查索引）没就绪的方案来源。
    ///
    /// 进入时门卫已要求它们就绪，但会话中途索引可能被清掉（改了方案设置、词库启用集变了、
    /// 主码表重载都会整表清空反查索引）。这时只剩用户层的码可比，几乎所有候选都会被滤掉
    /// ——调用方据此把本次当「未就绪」处理：原样放行 + 后台重建。
    pub(crate) fn unready_schemas(&self) -> Vec<&str> {
        self.rt
            .layers
            .iter()
            .zip(&self.views)
            .filter_map(|(l, v)| match (l, v) {
                (Layer::Schema(id), Some(v)) if !v.system_ready() => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }
}

impl AuxCodeLookup for AuxLookupNow {
    fn any_code(&self, ch: char, pred: &mut dyn FnMut(&str) -> bool) -> bool {
        let mut buf = [0u8; 4];
        let text: &str = ch.encode_utf8(&mut buf);
        self.rt
            .layers
            .iter()
            .zip(&self.views)
            .any(|(l, v)| match (l, v) {
                (Layer::Table(t), _) => t.any_code(ch, pred),
                (Layer::Schema(_), Some(v)) => v.any_code(text, pred),
                (Layer::Schema(_), None) => false,
            })
    }

    fn is_empty(&self) -> bool {
        self.rt
            .layers
            .iter()
            .zip(&self.views)
            .all(|(l, v)| match (l, v) {
                (Layer::Table(t), _) => AuxCodeLookup::is_empty(t),
                (Layer::Schema(_), Some(v)) => !v.has_any(),
                (Layer::Schema(_), None) => true,
            })
    }
}

impl wind_engine::AuxCharLookup for AuxLookupNow {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        AuxCodeLookup::any_code_starts_with(self, ch, prefix)
    }

    fn is_empty(&self) -> bool {
        AuxCodeLookup::is_empty(self)
    }

    /// 各层（文件层、方案层的系统 + 用户层）的码各过一遍，零分配。
    fn letter_mask(&self, ch: char) -> u32 {
        let mut mask = 0;
        AuxCodeLookup::any_code(self, ch, &mut |code| {
            mask |= wind_engine::engine::aux_code_letter_bit(code);
            false
        });
        mask
    }
}

#[cfg(test)]
pub(crate) mod aux_char_lookup_tests {
    use super::*;
    use wind_engine::AuxCharLookup;

    fn now(table: AuxCodeTable) -> AuxLookupNow {
        AuxLookupNow {
            rt: Arc::new(AuxCodeRuntime {
                key: Vec::new(),
                name: String::new(),
                layers: vec![Layer::Table(table)],
            }),
            views: vec![None],
        }
    }

    fn check(table_rows: Vec<(char, &str)>) {
        let reference = AuxCodeTable::from_rows(table_rows.clone());
        let lk = now(AuxCodeTable::from_rows(table_rows));
        for (ch, prefix) in [
            ('好', "h"),
            ('好', "hn"),
            ('好', "x"),
            ('无', "h"),
            ('是', "u"),
        ] {
            assert_eq!(
                AuxCharLookup::any_code_starts_with(&lk, ch, prefix),
                AuxCodeLookup::any_code_starts_with(&reference, ch, prefix),
                "{ch} {prefix}"
            );
        }
        assert_eq!(
            AuxCharLookup::is_empty(&lk),
            AuxCodeLookup::is_empty(&reference)
        );
        for ch in ['好', '是', '无'] {
            assert_eq!(lk.letter_mask(ch), default_mask(&lk, ch), "{ch}");
        }
    }

    /// 掩码的默认实现（逐字母 `any_code_starts_with`），作快速实现的参照。
    pub(crate) fn default_mask(lk: &dyn AuxCharLookup, ch: char) -> u32 {
        let mut buf = [0u8; 4];
        (b'a'..=b'z').enumerate().fold(0, |m, (i, b)| {
            let one = char::from(b).encode_utf8(&mut buf);
            m | (u32::from(lk.any_code_starts_with(ch, one)) << i)
        })
    }

    #[test]
    fn matches_aux_code_lookup_on_small_table() {
        check(vec![('好', "hn"), ('好', "nv"), ('是', "uu"), ('是', "Ux")]);
    }

    #[test]
    fn matches_aux_code_lookup_on_empty_table() {
        check(vec![]);
    }

    #[test]
    fn is_send_sync_trait_object() {
        let arc: Arc<dyn AuxCharLookup> =
            Arc::new(now(AuxCodeTable::from_rows(vec![('好', "hn")])));
        assert!(arc.any_code_starts_with('好', "h"));
    }
}
