//! Store 桥接层：把 wind-store 的用户词 / 临时词包成 `DictLayer`，挂进 CompositeDict。
//!
//! 见 docs/redesign/dict.md §3。词频/shadow 不在此（词频是排序独立维度 frequency.md；
//! shadow 是 ShadowProvider，引擎排序后应用）——本文件只负责"用户词/临时词作为查询层"。

use crate::layer::{DictLayer, LayerType};
use std::sync::Arc;
use wind_candidate::{Candidate, better};
use wind_store::Store;
use wind_store::user_words::UserWordRecord;

/// 把用户/临时词记录映射为候选；`is_temp` 决定 meta 标记，`is_prefix` 标记前缀补全。
///
/// ⚠️ **`boundary` 必须显式带上**。P2a 贯通边界时改的是 `SystemDictLayer`，但有两条旁路
/// 绕开它：`PinyinEngine` 直接持有 `CachedDict`（P2b 已补），以及本层——记录里
/// （`user_words.rs` 的 24B value）明明存着边界、`search_user_words_prefix` 也读了出来，
/// 到这里被 `..Default::default()` 吃掉，于是**用户词候选的 boundary 恒为 0**，
/// 双拼边界校验、长词上浮判据、自动造词沿用边界四处一并失效。
/// 见 docs/design/pinyin-code-domains.md §3 L2。
fn record_to_candidate(r: UserWordRecord, is_temp: bool, is_prefix: bool) -> Candidate {
    let mut c = Candidate {
        // 库里存的就是真实文本（含真换行），**此处不做任何转义处理**。
        //
        // 转义只发生在系统边界上：文本文件（`.dict.yaml`/导入/导出）与设置页 UI
        // 各自进出时转换，数据库与内存中一律是真实文本。若在这里反转义，等于要求
        // 每个写入端都先转义过——而写入端有 8 条（设置页 ×3、快捷加词、自动造词、
        // 导入、临时词、备份还原），漏一条，该路径写入的 `C:\note` 就会被展开成
        // `C:` + 换行 + `ote`，且静默无感。
        text: r.text,
        code: r.code,
        weight: r.weight,
        boundary: r.boundary,
        is_prefix,
        // 入库先后序号 → 候选的自然序（t80）。`better()` 的排序链是
        // weight 降 → base_order 升 → **natural_order 升** → code → text：此前这里取
        // `Default`（恒 0），同码等权的用户词全被打平，退化成按 text 字典序，于是从别的
        // 平台迁进来的词库丢掉了原有词序。dict 侧本就用二进制格式里的 `order` 做这一档，
        // 补上它两条路径才同口径。
        //
        // `min` 是防御性饱和：order 是 u32、natural_order 是 i32，真跑到 21 亿条也不会
        // 折成负数（负数会让这些词跳到所有词前面，比无序更糟）。
        natural_order: r.order.min(i32::MAX as u32) as i32,
        ..Default::default()
    };
    c.meta.raw_weight = r.weight;
    if is_temp {
        c.meta.is_temp_dict = true;
    } else {
        c.meta.is_user_dict = true;
    }
    c
}

fn sort_trunc(mut v: Vec<Candidate>, limit: usize) -> Vec<Candidate> {
    v.sort_by(better);
    if limit > 0 {
        v.truncate(limit);
    }
    v
}

/// 用户造词层（redb 后端，可变；写经 Store 的 add/remove/update）。
pub struct StoreUserLayer {
    store: Arc<Store>,
    schema_id: String,
    name: String,
}

impl StoreUserLayer {
    pub fn new(store: Arc<Store>, schema_id: impl Into<String>) -> Self {
        let schema_id = schema_id.into();
        let name = format!("user:{schema_id}");
        Self {
            store,
            schema_id,
            name,
        }
    }
}

impl DictLayer for StoreUserLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn layer_type(&self) -> LayerType {
        LayerType::User
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        let recs = self
            .store
            .get_user_words(&self.schema_id, code)
            .unwrap_or_default();
        let cands = recs
            .into_iter()
            .map(|r| record_to_candidate(r, false, false))
            .collect();
        sort_trunc(cands, limit)
    }

    /// ⚠️ **已知局限：`limit` 先截断、后排序**（t80 的已知边界，非本次引入）。
    ///
    /// `search_user_words_prefix` 按 redb 的 **key 字典序**扫，数够 `limit` 条就 `break`；
    /// 随后 `sort_trunc` 才按 `better()`（含 `natural_order`）排。于是「该排第一但 key
    /// 字典序靠后」的词条可能**压根没进这个列表**，order 再正确也救不回来。
    ///
    /// 举例：`abc` 前缀下有 60 个词，导入时排第 1 的那条其 key 字典序在第 55 位，
    /// `limit = 30` ⇒ 它不在候选里。
    ///
    /// 改动前 `natural_order` 恒 0，截断只会漏掉低权重词，危害有限；补上 order 之后，
    /// 这条局限会直接表现为「导入词序有时对、有时不对」。真要修得让截断也走排序口径
    /// （堆选 top-k，或不截断后排），那会动到候选路径的性能与既有语义，不在 t80 的范围内。
    ///
    /// 导出路径不受影响：`collect_user_word_rows` 传的是 `limit = 0`（不截断）。
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        let recs = self
            .store
            .search_user_words_prefix(&self.schema_id, prefix, limit)
            .unwrap_or_default();
        let cands = recs
            .into_iter()
            .map(|r| record_to_candidate(r, false, true))
            .collect();
        sort_trunc(cands, limit)
    }

    /// `is_prefix = false`：简拼命中的是**整词**，不是前缀补全。引擎侧会据此
    /// 把它归入简拼层（`is_abbrev`），与 `is_prefix` 的补全层是两回事。
    fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        let recs = self
            .store
            .search_user_words_by_abbrev(&self.schema_id, abbrev, limit)
            .unwrap_or_default();
        let cands = recs
            .into_iter()
            .map(|r| record_to_candidate(r, false, false))
            .collect();
        sort_trunc(cands, limit)
    }
}

/// 临时学习词层（redb 后端，可变）。
pub struct StoreTempLayer {
    store: Arc<Store>,
    schema_id: String,
    name: String,
}

impl StoreTempLayer {
    pub fn new(store: Arc<Store>, schema_id: impl Into<String>) -> Self {
        let schema_id = schema_id.into();
        let name = format!("temp:{schema_id}");
        Self {
            store,
            schema_id,
            name,
        }
    }
}

impl DictLayer for StoreTempLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn layer_type(&self) -> LayerType {
        LayerType::Temp
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        let recs = self
            .store
            .get_temp_words(&self.schema_id, code)
            .unwrap_or_default();
        let cands = recs
            .into_iter()
            .map(|r| record_to_candidate(r, true, false))
            .collect();
        sort_trunc(cands, limit)
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        let recs = self
            .store
            .search_temp_words_prefix(&self.schema_id, prefix, limit)
            .unwrap_or_default();
        let cands = recs
            .into_iter()
            .map(|r| record_to_candidate(r, true, true))
            .collect();
        sort_trunc(cands, limit)
    }

    /// 临时词同样要走索引：简拼召回是**跨层**的（引擎侧的 DictManager 同时注册了
    /// 用户层与临时层），只索引其一等于只修一半。
    fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        let recs = self
            .store
            .search_temp_words_by_abbrev(&self.schema_id, abbrev, limit)
            .unwrap_or_default();
        let cands = recs
            .into_iter()
            .map(|r| record_to_candidate(r, true, false))
            .collect();
        sort_trunc(cands, limit)
    }
}

/// 自动造词的**草稿层**（redb 后端，只读）。
///
/// 滑窗切出的猜测词住在这里，用户真的用它上屏过一次才跃迁进临时词库
/// （设计见 `docs/design/auto-phrase-draft-layer.md`）。
///
/// # 只响应精确查询 —— 这是设计的一部分，不是没实现完
///
/// [`search_prefix`](DictLayer::search_prefix) **恒返回空**。草稿层的杂词率极高
/// （模型使然：先记一堆、用过的才留），而码表打字是前缀式的（w → wq → wqv…）——
/// 草稿若参与前缀召回，那些杂词会在**每一次按键**上涌进候选列表。
/// 只精确命中意味着「打满这个词的完整码才出来」，正是草稿该有的语义，
/// 也把杂词整个挡在了前缀阶段之外。
///
/// [`search_abbrev`](DictLayer::search_abbrev) 走 trait 的默认实现（返回空、不回退全表扫）：
/// 草稿表**刻意没有简拼索引**，那是为了不让写放大跟着草稿的写入量翻上去。
/// 简拼召回等草稿跃迁进临时词库之后自然就有。
pub struct StoreDraftLayer {
    store: Arc<Store>,
    schema_id: String,
    name: String,
    /// 草稿有效期（秒）。0 = 永不过期。**过期判定在查询里做**，不能只靠定期清理——
    /// 清理线程没跑到的窗口里，过期草稿照样会被召回。
    ttl_secs: i64,
}

impl StoreDraftLayer {
    pub fn new(store: Arc<Store>, schema_id: impl Into<String>, ttl_secs: i64) -> Self {
        let schema_id = schema_id.into();
        let name = format!("draft:{schema_id}");
        Self {
            store,
            schema_id,
            name,
            ttl_secs,
        }
    }
}

impl DictLayer for StoreDraftLayer {
    fn name(&self) -> &str {
        &self.name
    }

    fn layer_type(&self) -> LayerType {
        LayerType::Draft
    }

    fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        let texts = self
            .store
            .search_drafts(&self.schema_id, code, self.ttl_secs)
            .unwrap_or_default();
        let cands: Vec<Candidate> = texts
            .into_iter()
            .map(|text| Candidate {
                text,
                code: code.to_string(),
                // 草稿层不存 weight：它在候选里恒沉底（`Candidate::is_draft` 排在
                // `candidate_display_order` 的 weight 之前），weight 不参与任何比较。
                // 取 0 而非某个正值，还顺带保证 `CompositeDict::merge_search` 的
                // 「跨层同 (code,text) 继承更高 weight」不会被草稿意外抬权。
                weight: 0,
                is_draft: true,
                ..Default::default()
            })
            .collect();
        sort_trunc(cands, limit)
    }

    /// 恒空，理由见结构体文档——**不要顺手实现它**。
    fn search_prefix(&self, _prefix: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::CompositeDict;

    fn store(name: &str) -> Arc<Store> {
        let p = std::env::temp_dir().join(format!("wind_storelayer_{name}.redb"));
        let _ = std::fs::remove_file(&p);
        Arc::new(Store::open(&p).unwrap())
    }

    /// ★ **草稿绝不能进前缀召回** —— 钉在 `CompositeDict` 这个跨层合并的消费点上。
    ///
    /// 草稿层的杂词率极高（模型使然：先记一堆、用过的才留），而码表打字是前缀式的
    /// （w → wq → wqv…）。草稿若参与前缀召回，杂词会在**每一次按键**上涌进候选列表——
    /// 那会让整个功能不可用。只精确命中是设计的一部分，不是没实现完。
    ///
    /// 判据刻意钉在 `CompositeDict::search_prefix` 而不是直接调 `StoreDraftLayer` 的方法：
    /// 层自己返回空、合并时却从别的路径把它捞回来，是本仓踩过的那类「护栏绕开消费点」
    /// 的故障形态。
    #[test]
    fn drafts_never_surface_through_prefix_queries() {
        let s = store("draft_prefix");
        s.add_drafts("wb", &[("wqvb".to_string(), "你好".to_string())])
            .unwrap();
        // 同时放一条**临时词**做对照组：同样的前缀，它必须照常召回。
        // 没有这个对照，一个「前缀查询整个坏掉」的实现也能让本用例变绿。
        s.learn_temp_word("wb", "wqvb", "拟好", 800, 0).unwrap();

        let composite = CompositeDict::new();
        composite.register_layer(Box::new(StoreDraftLayer::new(s.clone(), "wb", 0)));
        composite.register_layer(Box::new(StoreTempLayer::new(s.clone(), "wb")));

        let by_prefix = composite.search_prefix("wq", 50);
        let texts: Vec<&str> = by_prefix.iter().map(|c| c.text.as_str()).collect();
        assert!(
            texts.contains(&"拟好"),
            "对照组：临时词的前缀召回必须照常工作，否则本用例测不出东西：{texts:?}"
        );
        assert!(
            !texts.contains(&"你好"),
            "草稿从前缀查询里冒出来了 —— 每次按键都会涌进一堆杂词：{texts:?}"
        );
        assert!(
            by_prefix.iter().all(|c| !c.is_draft),
            "任何标着 is_draft 的候选都不该出现在前缀结果里"
        );

        // 打满完整码：草稿必须出得来，否则它永远没机会被用过、也就永远转不了正。
        let exact = composite.search("wqvb", 50);
        let texts: Vec<&str> = exact.iter().map(|c| c.text.as_str()).collect();
        assert!(
            texts.contains(&"你好"),
            "精确命中时草稿必须召回，这是它转正的唯一途径：{texts:?}"
        );
        assert!(
            exact.iter().any(|c| c.is_draft && c.text == "你好"),
            "草稿候选必须带上 is_draft 标记，否则排序层无从沉底"
        );
    }

    /// **边界必须穿过「记录 → 候选」这一层**（P2a 漏掉的第二条旁路，见 record_to_candidate 注释）。
    ///
    /// 存储层一直存着边界（24B value），`search_user_words_prefix` 也一直读得出来，
    /// 但转候选时被 `..Default::default()` 吃掉 ⇒ 用户词候选 boundary 恒 0，
    /// 双拼校验 / 长词上浮 / 自动造词沿用边界一并静默失效。
    ///
    /// 三条取候选的路径都要守（精确、前缀、临时层），漏一条就等于没修。
    #[test]
    fn boundary_reaches_candidate_from_store() {
        let s = store("boundary_passthrough");
        // ni|hao → 起始 {0,2}；xi|an|ning → {0,2,4}
        s.add_user_word("pinyin", "nihao", "你好", 500, 0b101)
            .unwrap();
        s.learn_temp_word("pinyin", "xianning", "西安宁", 800, 0b10101)
            .unwrap();

        let ul = StoreUserLayer::new(s.clone(), "pinyin");
        assert_eq!(ul.search("nihao", 10)[0].boundary, 0b101, "精确查询");
        assert_eq!(
            ul.search_prefix("ni", 10)[0].boundary,
            0b101,
            "前缀补全（用户长词上浮判据吃这个值）"
        );

        let tl = StoreTempLayer::new(s.clone(), "pinyin");
        assert_eq!(tl.search("xianning", 10)[0].boundary, 0b10101, "临时层");

        // 无边界记录仍是 0（旧数据 / 手输码 → 消费方降级回 DAG），不得凭空造出边界
        s.add_user_word("pinyin", "abcd", "工作", 100, 0).unwrap();
        assert_eq!(ul.search("abcd", 10)[0].boundary, 0);
    }

    /// 简拼召回走索引，且**跨层合并语义与改用索引之前一致**。
    ///
    /// 此前这条路是 `search_prefix("", 0)` 全层枚举后由 merge_search 合并；换成索引后
    /// 仍必须经同一套合并——否则同一个词同时在用户层与临时层时会返回两条、
    /// 权重不再取 max，排序静默变化。索引换的是取候选的代价，不是候选本身。
    #[test]
    fn abbrev_recall_goes_through_the_index_and_merges_across_layers() {
        let s = store("abbrev_recall");
        s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
        s.learn_temp_word("py", "nihao", "你好", 900, 0b101)
            .unwrap();
        // ni|hao|ma → 起始 {0,2,5}；zai|jian → 起始 {0,3}
        s.add_user_word("py", "nihaoma", "你好吗", 300, 0b100101)
            .unwrap();
        s.add_user_word("py", "zaijian", "再见", 700, 0b1001)
            .unwrap();

        let dm = crate::manager::DictManager::new();
        dm.register_layer(Box::new(StoreUserLayer::new(s.clone(), "py")));
        dm.register_layer(Box::new(StoreTempLayer::new(s.clone(), "py")));

        let nh = dm.search_abbrev("nh", 0);
        assert_eq!(nh.len(), 1, "nh 只该命中「你好」（nhm 是三音节，不同组）");
        assert_eq!(nh[0].text, "你好");
        assert_eq!(
            nh[0].weight, 900,
            "跨层同 text 须继承更高权重（临时层 900），与改用索引前一致"
        );
        assert_eq!(nh[0].code, "nihao", "保留全拼码，不得覆盖成简拼串");
        assert_eq!(nh[0].boundary, 0b101, "边界必须穿过来，双拼校验吃这个值");
        assert!(!nh[0].is_prefix, "简拼命中的是整词，不是前缀补全");

        assert_eq!(dm.search_abbrev("nhm", 0)[0].text, "你好吗");
        assert_eq!(dm.search_abbrev("zj", 0)[0].text, "再见");
        assert!(dm.search_abbrev("zg", 0).is_empty(), "无关声母串不该有产出");
    }

    /// 没有声母索引的层（系统词库层等）默认**不产出**，而非回退到全层枚举。
    /// 静默的全表扫正是简拼卡顿的根因，让它召不回、在测试里立刻暴露，好过悄悄慢下去。
    #[test]
    fn layers_without_an_index_yield_nothing_rather_than_scanning() {
        use crate::layer::{DictLayer, LayerType};
        struct Bare;
        impl DictLayer for Bare {
            fn name(&self) -> &str {
                "bare"
            }
            fn layer_type(&self) -> LayerType {
                LayerType::System
            }
            fn search(&self, _: &str, _: usize) -> Vec<Candidate> {
                vec![Candidate {
                    text: "不该出现".into(),
                    ..Default::default()
                }]
            }
            fn search_prefix(&self, _: &str, _: usize) -> Vec<Candidate> {
                vec![Candidate {
                    text: "更不该出现".into(),
                    ..Default::default()
                }]
            }
        }
        let composite = CompositeDict::new();
        composite.register_layer(Box::new(Bare));
        assert!(composite.search_abbrev("nh", 0).is_empty());
    }

    #[test]
    fn test_user_layer_search() {
        let s = store("user_search");
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        s.add_user_word("wb", "abc", "啊吧次", 50, 0).unwrap();
        let layer = StoreUserLayer::new(s.clone(), "wb");
        assert_eq!(layer.layer_type(), LayerType::User);
        let exact = layer.search("a", 10);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].text, "工");
        assert!(exact[0].meta.is_user_dict);
        // 前缀 "a" 命中 a / abc
        assert_eq!(layer.search_prefix("a", 10).len(), 2);
    }

    #[test]
    fn test_is_prefix_flag_search_vs_search_prefix() {
        // TDD: 验证 search 返回 is_prefix=false，search_prefix 返回 is_prefix=true
        let s = store("is_prefix_flag");
        s.add_user_word("wb", "abc", "啊吧次", 50, 0).unwrap();
        let layer = StoreUserLayer::new(s.clone(), "wb");

        // 精确匹配：is_prefix 应为 false
        let exact = layer.search("abc", 10);
        assert_eq!(exact.len(), 1);
        assert!(
            !exact[0].is_prefix,
            "search() 返回的候选 is_prefix 应为 false"
        );

        // 前缀匹配：is_prefix 应为 true
        let prefix = layer.search_prefix("ab", 10);
        assert_eq!(prefix.len(), 1);
        assert!(
            prefix[0].is_prefix,
            "search_prefix() 返回的候选 is_prefix 应为 true"
        );
    }

    #[test]
    fn test_temp_layer_is_prefix_flag() {
        // StoreTempLayer 的 search_prefix 同样应标记 is_prefix=true
        let s = store("temp_is_prefix_flag");
        s.learn_temp_word("wb", "xyz", "某词", 100, 0).unwrap();
        let layer = StoreTempLayer::new(s.clone(), "wb");

        let exact = layer.search("xyz", 10);
        assert_eq!(exact.len(), 1);
        assert!(!exact[0].is_prefix, "临时层 search() is_prefix 应为 false");

        let prefix = layer.search_prefix("xy", 10);
        assert_eq!(prefix.len(), 1);
        assert!(
            prefix[0].is_prefix,
            "临时层 search_prefix() is_prefix 应为 true"
        );
    }

    #[test]
    fn test_temp_layer_and_composite() {
        let s = store("temp_composite");
        s.add_user_word("wb", "ni", "你", 100, 0).unwrap();
        s.learn_temp_word("wb", "ni", "拟", 800, 0).unwrap();
        let composite = CompositeDict::new();
        composite.register_layer(Box::new(StoreUserLayer::new(s.clone(), "wb")));
        composite.register_layer(Box::new(StoreTempLayer::new(s.clone(), "wb")));
        // composite 跨层查 "ni" → 你(user) + 拟(temp)
        let got = composite.search("ni", 10);
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|c| c.text == "你" && c.meta.is_user_dict));
        assert!(got.iter().any(|c| c.text == "拟" && c.meta.is_temp_dict));
    }
}
