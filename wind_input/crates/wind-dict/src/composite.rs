//! 多层复合词典
//!
//! 与 Go 版本 `wind_input/internal/dict/composite.go` 对齐。

use crate::layer::DictLayer;
use std::collections::HashMap;
use std::sync::RwLock;
use wind_candidate::Candidate;

/// 多层复合词典
#[derive(Default)]
pub struct CompositeDict {
    layers: RwLock<Vec<Box<dyn DictLayer>>>,
}

/// `merge_search` 的查询种类：决定问各层哪个方法，其余合并逻辑三者共用。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Query {
    /// 精确码
    Exact,
    /// 前缀补全
    Prefix,
    /// 声母串（简拼召回）
    Abbrev,
}

impl CompositeDict {
    pub fn new() -> Self {
        Self {
            layers: RwLock::new(Vec::new()),
        }
    }

    /// 注册词典层（按 layer_type 稳定排序：相同类型保持注册顺序，
    /// 故同为 System 的主库与扩展库按注册先后决定层内优先级）。
    pub fn register_layer(&self, layer: Box<dyn DictLayer>) {
        let mut layers = self.layers.write().unwrap();
        layers.push(layer);
        layers.sort_by_key(|l| l.layer_type() as u8);
    }

    /// 按名注销词典层
    pub fn unregister_layer(&self, name: &str) {
        let mut layers = self.layers.write().unwrap();
        layers.retain(|l| l.name() != name);
    }

    /// 运行时启停某层（按名）：用于码表扩展词库热插拔，无需重建引擎。
    /// 返回是否命中该层。仅需读锁（层的 enabled 是内部原子标志）。
    pub fn set_layer_enabled(&self, name: &str, enabled: bool) -> bool {
        let layers = self.layers.read().unwrap();
        let mut hit = false;
        for l in layers.iter() {
            if l.name() == name {
                l.set_enabled(enabled);
                hit = true;
            }
        }
        hit
    }

    /// 精确查找：跨层合并去重。
    pub fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(code, limit, Query::Exact)
    }

    /// 前缀查找：跨层合并去重。
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(prefix, limit, Query::Prefix)
    }

    /// 按声母串查找（简拼召回）：跨层合并去重，见 [`DictLayer::search_abbrev`]。
    ///
    /// **走同一套 `merge_search`** 而不是各层结果直接拼接，是为了保住跨层合并语义：
    /// 同一个词同时在用户层与临时层时，此前经 `search_prefix` 合并成一条、权重取 max；
    /// 若改成拼接后由引擎「先到先得」去重，就会拿到用户层那条的原权重，排序静默变化。
    /// 索引换的是取候选的代价，不该顺手改掉候选本身。
    pub fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<Candidate> {
        self.merge_search(abbrev, limit, Query::Abbrev)
    }

    /// 是否存在**严格长于** `prefix` 的编码：任一**启用**层命中即 true，命中即短路。
    ///
    /// 刻意不经 `merge_search`——那条路会按 text 去重并「同 text 取最短码」
    /// （见 `merge_search` 注释），把一个字的长码换成它在别层的短码，正好抹掉本判据
    /// 要找的信息。存在性判断必须逐层原样问。
    pub fn has_longer_code(&self, prefix: &str) -> bool {
        let layers = self.layers.read().unwrap();
        layers
            .iter()
            .filter(|l| l.enabled())
            .any(|l| l.has_longer_code(prefix))
    }

    /// 全量枚举各**启用**层的 `(code, text, weight)`，供离线索引构建
    /// （见 `DictLayer::for_each_entry`）。**不去重、不排序**——跨层同 `(code, text)`
    /// 会各报一次，由调用方按自己的语义合并；这里替它去重反而会丢掉「哪一层出的」这件事。
    ///
    /// ⚠️ O(全表)，绝不能出现在按键链路上。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        let layers = self.layers.read().unwrap();
        for l in layers.iter().filter(|l| l.enabled()) {
            l.for_each_entry(f);
        }
    }

    /// 跨层合并：遍历各层收集候选，按 text 去重——
    ///   - 保留**高优先级层**(先出现)的词条信息(code/natural_order)；
    ///   - 但**继承后续层中同 text 的更高权重**(用户词不因低权重丢失码表词的自然排序位)；
    ///   - 前缀查询时，同 text 多码取**最短码**(离输入最近)及其更小 natural_order；
    ///   - 每层叠加该层 `base_order()`（设计者经 [[dictionaries]].base_order 配置），
    ///     使等权/`base_sort=natural` 时按设计者指定的层间基序排列（取代旧的按注册位置偏移）。
    ///
    /// 与 Go composite.go `searchInternal` 对齐。
    fn merge_search(&self, query: &str, limit: usize, kind: Query) -> Vec<Candidate> {
        let layers = self.layers.read().unwrap();
        let mut results: Vec<Candidate> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        // 「同 text 取最短码」**只对前缀查询成立**：那时各层给的是不同长度的补全码，
        // 取最短即「离输入最近」。精确与简拼查询给的都是整词码，谁短谁长没有这层含义。
        //
        // 简拼尤其不能换：候选的 code 必须留全拼码，词频记账走的正是它
        // （见 step6「保留全拼码」那段——换成别层的码会让同一个词在两条流下各记各的）。
        let is_prefix = matches!(kind, Query::Prefix);

        for layer in layers.iter() {
            // 禁用层（如关闭的码表扩展词库）跳过。
            if !layer.enabled() {
                continue;
            }
            let layer_results = match kind {
                Query::Exact => layer.search(query, limit),
                Query::Prefix => layer.search_prefix(query, limit),
                Query::Abbrev => layer.search_abbrev(query, limit),
            };
            // 层级基序档位：写入候选的 base_order 字段（独立排序层级，不折进 natural_order）。
            let layer_base_order = layer.base_order();
            for mut cand in layer_results {
                cand.base_order = layer_base_order;
                if let Some(&idx) = seen.get(&cand.text) {
                    // 同 text 已存在：**编码也相同**时才继承更高权重。
                    //
                    // ## 权重属于 `(code, text)` 这个词条，不属于「字」
                    //
                    // 只有 code 也相同，两条才是同一个词条在不同词库里的两份记录，取 max 才
                    // 有意义（也正是「多个词库收录同一条，按最高权重算」这条用户可见语义）。
                    // 不同码位（简码 `a` 与全码 `ab`）是**两个独立词条**、各有各的权重；
                    // 跨码位取 max 等于把两个条目的权重混成一个，让候选带着「别的码位的
                    // 权重」参与排序——码表方案尤其不能这样，那里码长本身就是分档依据。
                    //
                    // code/natural_order/boundary 仍保留首个出现层（高优先层）的值。
                    // boundary 随 code 走：用户层的码只配用户层的边界（用户手输码恒 0 →
                    // 降级 DAG），不可从系统层「借」一个边界过来。
                    let existing = &mut results[idx];
                    if cand.code == existing.code && cand.weight > existing.weight {
                        existing.weight = cand.weight;
                        // 权重来源**必须随权重一起换**：不同步换会让调试段把权重记在错的库
                        // 头上——那比不显示更糟，排查时会照着错的库名去翻词库文件。
                        existing.meta.weight_layer = cand.meta.weight_layer.take();
                    }
                    // 被丢弃这条的**来源归属**并入幸存者。
                    //
                    // 这两个标记不是装饰：右键删除按它们决定去 `user_words` 还是 `temp_words`
                    // 删，而 `redb` 的 `remove` 对不存在的 key 静默成功——丢掉标记的代价不是
                    // 报错，是「点多少次都无作用」。用户层与临时层都注册在同一个 DictManager 里
                    //（`manager.rs` 的 store_layers），同文时后注册的临时层整条被丢弃，
                    // `is_temp_dict` 随之蒸发，删除便只删得掉用户词那一半。
                    // 2026-09-03 用户实测「再也不好」即毁于这条路径的上游。
                    existing.meta.is_user_dict |= cand.meta.is_user_dict;
                    existing.meta.is_temp_dict |= cand.meta.is_temp_dict;
                    // 被丢弃这条所占的码位并入幸存者：跨层同 text 常有不同码（用户层手输码 vs
                    // 系统层全码），丢掉即让「检索范围」过滤看不见该码位的常用性，见
                    // `Candidate::merged_codes`。删除侧也读它——两层码不同时，幸存者的 `code`
                    // 只对得上其中一张表。
                    existing.absorb_codes_from(&cand);
                    // 前缀：保留最短码及其更早出现位置。
                    // boundary 描述的是 code 的音节切分，**必须与 code 同进同出**——换了码却留着
                    // 旧码的边界，会配出「A 层的 code + B 层的 boundary」这种自相矛盾的候选。
                    if is_prefix && cand.code.len() < existing.code.len() {
                        let old_code = std::mem::replace(&mut existing.code, cand.code.clone());
                        // 换码＝**改换词条**，故 weight 与其来源层必须与 code 整组同换。
                        // 留着旧码位的权重会配出「A 码位的编码 + B 码位的权重」这种混血候选
                        // ——与上面「不跨码位取 max」是同一条原则的两个面：权重恒随它自己的
                        // 码位走，不因去重而漂到别的码位上。
                        existing.weight = cand.weight;
                        existing.meta.weight_layer = cand.meta.weight_layer.take();
                        existing.boundary = cand.boundary;
                        if cand.natural_order < existing.natural_order {
                            existing.natural_order = cand.natural_order;
                        }
                        // 让位给短码的旧码位转入 merged_codes；新主码则从中剔除（主码不重复记）。
                        existing.absorb_code(&old_code);
                        existing.merged_codes.retain(|c| c != &cand.code);
                    }
                    continue;
                }
                seen.insert(cand.text.clone(), results.len());
                results.push(cand);
            }
        }

        results.sort_by(wind_candidate::better);
        // limit==0 视为「无上限」（与各 DictLayer::search 的 `if limit>0` 守卫、Go
        // searchInternal 一致），仅在 limit>0 时截断。调用方需要空结果时不应传 0。
        if limit > 0 && results.len() > limit {
            results.truncate(limit);
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::LayerType;

    /// 测试用层：固定候选集，可指定 layer_type 与名字。
    struct MockLayer {
        name: String,
        ltype: LayerType,
        items: Vec<Candidate>, // (text, code, weight, natural_order) 预置
    }

    fn cand(text: &str, code: &str, weight: i32, no: i32) -> Candidate {
        Candidate {
            text: text.into(),
            code: code.into(),
            weight,
            natural_order: no,
            ..Default::default()
        }
    }

    impl DictLayer for MockLayer {
        fn name(&self) -> &str {
            &self.name
        }
        fn layer_type(&self) -> LayerType {
            self.ltype
        }
        fn search(&self, code: &str, _limit: usize) -> Vec<Candidate> {
            self.items
                .iter()
                .filter(|c| c.code == code)
                .cloned()
                .collect()
        }
        fn search_prefix(&self, prefix: &str, _limit: usize) -> Vec<Candidate> {
            self.items
                .iter()
                .filter(|c| c.code.starts_with(prefix))
                .cloned()
                .collect()
        }
    }

    fn cand_b(text: &str, code: &str, weight: i32, no: i32, boundary: u64) -> Candidate {
        Candidate {
            boundary,
            ..cand(text, code, weight, no)
        }
    }

    /// 同 text 去重**不得吞掉来源归属**：右键删除按 `is_user_dict`/`is_temp_dict` 决定去
    /// 哪张表删，而 `redb` 的 `remove` 对不存在的 key 静默成功——丢掉标记的代价不是报错，
    /// 是「点多少次都无作用」（2026-09-03 用户实测「再也不好」）。
    ///
    /// 用户层与临时层同注册在拼音引擎的 store DictManager 里，同文时后者整条被丢弃。
    #[test]
    fn dedup_same_text_merges_source_flags() {
        let mut user = cand("再也不好", "zaiyebuhao", 800, 0);
        user.meta.is_user_dict = true;
        let mut temp = cand("再也不好", "zaiyebuhao", 800, 1);
        temp.meta.is_temp_dict = true;
        let c = CompositeDict::new();
        c.register_layer(Box::new(MockLayer {
            name: "user".into(),
            ltype: LayerType::User,
            items: vec![user],
        }));
        c.register_layer(Box::new(MockLayer {
            name: "temp".into(),
            ltype: LayerType::Temp,
            items: vec![temp],
        }));
        let r = c.search("zaiyebuhao", 10);
        assert_eq!(r.len(), 1, "同 text 应去重成一条");
        assert!(r[0].meta.is_user_dict, "用户层归属应保留");
        assert!(
            r[0].meta.is_temp_dict,
            "被丢弃那条的临时层归属必须并入，否则删除只删得掉一半"
        );
    }

    /// 跨层「更长后继」判据**必须逐层原样问**，不能走 `merge_search`。
    ///
    /// 合并期会按 text 去重并「同 text 取最短码」：同一个词在高优先层有短码、在系统层有
    /// 长码时，合并结果只剩短码——正好把本判据要找的信息抹掉。这曾是旧实现
    /// （`search_prefix(input, 64)` 再 `.any(code 更长)`）的漏判来源，会让「用户还能接着
    /// 打」的情形被误判成「已到底」，进而触发不该发生的自动上屏 / 顶码。
    #[test]
    fn has_longer_code_bypasses_dedup_shortest_code() {
        let c = CompositeDict::new();
        c.register_layer(Box::new(MockLayer {
            name: "user".into(),
            ltype: LayerType::User, // 高优先层：短码，长度 == 待查前缀
            items: vec![cand("好", "ok", 100, 0)],
        }));
        c.register_layer(Box::new(MockLayer {
            name: "system".into(),
            ltype: LayerType::System, // 同 text，但码更长
            items: vec![cand("好", "okzz", 500, 0)],
        }));

        // 旧路径：合并去重后同 text 取最短码 "ok"，长度不大于输入 → 漏判为 false。
        let merged_says = {
            let n = "ok".chars().count();
            c.search_prefix("ok", 64)
                .iter()
                .any(|x| x.code.chars().count() > n)
        };
        assert!(
            !merged_says,
            "前提校验：合并路径确实因取最短码而看不到 okzz"
        );

        // 新路径：逐层问，system 层的 okzz 如实命中。
        assert!(
            c.has_longer_code("ok"),
            "系统层存在更长码 okzz，跨层判据不得因去重而漏掉"
        );

        // 无更长后继时不得误报。
        assert!(!c.has_longer_code("okzz"), "okzz 已最长");
        assert!(!c.has_longer_code("zzz"), "无此前缀");
    }

    /// 禁用层不参与「更长后继」判据——与 `merge_search` 跳过禁用层的行为一致，
    /// 否则关掉的扩展词库仍会压住自动上屏。
    #[test]
    fn has_longer_code_skips_disabled_layers() {
        struct Toggle {
            name: String,
            on: std::sync::atomic::AtomicBool,
        }
        impl DictLayer for Toggle {
            fn name(&self) -> &str {
                &self.name
            }
            fn layer_type(&self) -> LayerType {
                LayerType::System
            }
            fn search(&self, _c: &str, _l: usize) -> Vec<Candidate> {
                Vec::new()
            }
            fn search_prefix(&self, prefix: &str, _l: usize) -> Vec<Candidate> {
                if "okzz".starts_with(prefix) {
                    vec![cand("好", "okzz", 10, 0)]
                } else {
                    Vec::new()
                }
            }
            fn enabled(&self) -> bool {
                self.on.load(std::sync::atomic::Ordering::Relaxed)
            }
            fn set_enabled(&self, e: bool) {
                self.on.store(e, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let c = CompositeDict::new();
        c.register_layer(Box::new(Toggle {
            name: "extra".into(),
            on: std::sync::atomic::AtomicBool::new(true),
        }));
        assert!(c.has_longer_code("ok"), "启用时应看到 okzz");
        c.set_layer_enabled("extra", false);
        assert!(!c.has_longer_code("ok"), "禁用后该层不得参与判据");
    }

    /// **boundary 必须与 code 同进同出**（合并期的错位陷阱）。
    /// boundary 描述的是 code 的音节切分，二者是一对；若换了码却留着旧码的边界，
    /// 就会配出「A 层的 code + B 层的 boundary」这种自相矛盾的候选，
    /// 下游按错位边界校验会静默误杀候选。
    #[test]
    fn boundary_travels_with_code_on_merge() {
        // ① 同 text 去重：code/boundary 保留**首个出现层**（高优先层），只有 weight 跨层继承。
        let c = CompositeDict::new();
        c.register_layer(Box::new(MockLayer {
            name: "user".into(),
            ltype: LayerType::User, // 优先级高于 System
            items: vec![cand_b("你好", "nihao", 100, 0, 0)], // 用户词：手输码，无边界
        }));
        c.register_layer(Box::new(MockLayer {
            name: "system".into(),
            ltype: LayerType::System,
            items: vec![cand_b("你好", "nihao", 500, 0, 0b101)], // 系统词：有真值边界
        }));
        let r = c.search("nihao", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].weight, 500, "weight 仍跨层继承更高值");
        assert_eq!(
            r[0].boundary, 0,
            "boundary 随 code 保留高优先层（用户层）的值，不可从系统层「借」一个过来"
        );

        // ② 前缀换最短码：换 code 时 boundary 必须一起换。
        let c2 = CompositeDict::new();
        c2.register_layer(Box::new(MockLayer {
            name: "user".into(),
            ltype: LayerType::User,
            items: vec![cand_b("你好", "nihaoaaa", 100, 0, 0b1)], // 长码 + 其边界
        }));
        c2.register_layer(Box::new(MockLayer {
            name: "system".into(),
            ltype: LayerType::System,
            items: vec![cand_b("你好", "nihao", 500, 0, 0b101)], // 更短的码 + 其边界
        }));
        let r2 = c2.search_prefix("ni", 10);
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].code, "nihao", "前缀应保留最短码");
        assert_eq!(
            r2[0].boundary, 0b101,
            "换成短码时 boundary 必须换成该码的，不能留着长码的边界"
        );
    }

    #[test]
    fn dedup_same_text_inherits_higher_weight() {
        let c = CompositeDict::new();
        // 主系统层：你 weight 100；扩展系统层：同 text「你」weight 500（更高）
        c.register_layer(Box::new(MockLayer {
            name: "system-main".into(),
            ltype: LayerType::System,
            items: vec![cand("你", "ni", 100, 0)],
        }));
        c.register_layer(Box::new(MockLayer {
            name: "system-extra".into(),
            ltype: LayerType::System,
            items: vec![cand("你", "ni", 500, 0)],
        }));
        let r = c.search("ni", 10);
        assert_eq!(r.len(), 1, "同 text 应去重为一条");
        assert_eq!(r[0].text, "你");
        assert_eq!(r[0].weight, 500, "应继承更高权重");
    }

    #[test]
    fn distinct_text_kept_and_base_order_breaks_ties() {
        // 两层各一条不同 text、同权重、同层内 natural_order：由 base_order 决定层间先后。
        // 文本序故意与期望相反（"主" < "扩"），以证明是 base_order 而非文本兜底决定顺序。
        struct L {
            name: String,
            items: Vec<Candidate>,
            base_order: i32,
        }
        impl DictLayer for L {
            fn name(&self) -> &str {
                &self.name
            }
            fn layer_type(&self) -> LayerType {
                LayerType::System
            }
            fn base_order(&self) -> i32 {
                self.base_order
            }
            fn search(&self, code: &str, _l: usize) -> Vec<Candidate> {
                self.items
                    .iter()
                    .filter(|c| c.code == code)
                    .cloned()
                    .collect()
            }
            fn search_prefix(&self, p: &str, _l: usize) -> Vec<Candidate> {
                self.items
                    .iter()
                    .filter(|c| c.code.starts_with(p))
                    .cloned()
                    .collect()
            }
        }
        let c = CompositeDict::new();
        // 主库 base_order 0、文本"扩"（文本序更大）；扩展库 base_order 1000、文本"主"（文本序更小）。
        c.register_layer(Box::new(L {
            name: "main".into(),
            base_order: 0,
            items: vec![cand("扩", "x", 100, 0)],
        }));
        c.register_layer(Box::new(L {
            name: "extra".into(),
            base_order: 1000,
            items: vec![cand("主", "x", 100, 0)],
        }));
        let r = c.search("x", 10);
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0].text, "扩",
            "base_order 更小的主库应排前，即便其文本序更大"
        );
        assert_eq!(r[1].text, "主");
    }

    #[test]
    fn layer_type_default_base_order_puts_nonsystem_before_system() {
        // 默认 base_order 按层类型分带：等权时用户/临时层恒排在系统词库层之前。
        let c = CompositeDict::new();
        c.register_layer(Box::new(MockLayer {
            name: "sys".into(),
            ltype: LayerType::System,
            items: vec![cand("系统", "x", 100, 0)],
        }));
        c.register_layer(Box::new(MockLayer {
            name: "user".into(),
            ltype: LayerType::User,
            items: vec![cand("用户", "x", 100, 0)],
        }));
        let r = c.search("x", 10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].text, "用户", "等权时用户层(默认带)应排在系统层前");
        assert_eq!(r[1].text, "系统");
    }

    #[test]
    fn prefix_keeps_shortest_code_for_same_text() {
        let c = CompositeDict::new();
        // 同 text「好」在两层有不同码：hao(3) 与 h(1)；前缀查应保留最短码 h
        c.register_layer(Box::new(MockLayer {
            name: "system-main".into(),
            ltype: LayerType::System,
            items: vec![cand("好", "hao", 100, 5)],
        }));
        c.register_layer(Box::new(MockLayer {
            name: "system-extra".into(),
            ltype: LayerType::System,
            items: vec![cand("好", "h", 100, 9)],
        }));
        let r = c.search_prefix("h", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "好");
        assert_eq!(r[0].code, "h", "同 text 多码前缀查应保留最短码");
    }

    #[test]
    fn disabled_layer_skipped_and_hot_toggle() {
        struct Toggle {
            name: String,
            enabled: std::sync::atomic::AtomicBool,
            items: Vec<Candidate>,
        }
        impl DictLayer for Toggle {
            fn name(&self) -> &str {
                &self.name
            }
            fn layer_type(&self) -> LayerType {
                LayerType::System
            }
            fn enabled(&self) -> bool {
                self.enabled.load(std::sync::atomic::Ordering::Relaxed)
            }
            fn set_enabled(&self, e: bool) {
                self.enabled.store(e, std::sync::atomic::Ordering::Relaxed);
            }
            fn search(&self, code: &str, _l: usize) -> Vec<Candidate> {
                self.items
                    .iter()
                    .filter(|c| c.code == code)
                    .cloned()
                    .collect()
            }
            fn search_prefix(&self, p: &str, _l: usize) -> Vec<Candidate> {
                self.items
                    .iter()
                    .filter(|c| c.code.starts_with(p))
                    .cloned()
                    .collect()
            }
        }
        let c = CompositeDict::new();
        c.register_layer(Box::new(MockLayer {
            name: "system-main".into(),
            ltype: LayerType::System,
            items: vec![cand("主", "e", 100, 0)],
        }));
        c.register_layer(Box::new(Toggle {
            name: "codetable-extra-emoji".into(),
            enabled: std::sync::atomic::AtomicBool::new(false), // 初始禁用
            items: vec![cand("😀", "e", 100, 0)],
        }));
        // 禁用时：扩展候选不出
        let r = c.search("e", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "主");
        // 热开启：无需重建，扩展候选即时出现
        assert!(c.set_layer_enabled("codetable-extra-emoji", true));
        let r = c.search("e", 10);
        assert_eq!(r.len(), 2, "开启后扩展候选应即时加入: {r:?}");
        assert!(r.iter().any(|c| c.text == "😀"));
        // 热关闭：又消失
        assert!(c.set_layer_enabled("codetable-extra-emoji", false));
        assert_eq!(c.search("e", 10).len(), 1);
        // 未命中的名字返回 false
        assert!(!c.set_layer_enabled("no-such-layer", true));
    }

    #[test]
    fn higher_priority_layer_type_wins_over_system() {
        let c = CompositeDict::new();
        // User 层权重低，但同 text 仍应继承 System 高权重，且只保留一条
        c.register_layer(Box::new(MockLayer {
            name: "system".into(),
            ltype: LayerType::System,
            items: vec![cand("中", "z", 900, 0)],
        }));
        c.register_layer(Box::new(MockLayer {
            name: "user".into(),
            ltype: LayerType::User,
            items: vec![cand("中", "z", 10, 0)],
        }));
        let r = c.search("z", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].weight, 900, "去重后继承更高权重(无视层优先级)");
    }

    // ==================================================================
    // 跨词库同词合并：权重取「已启用层的最大值」
    //
    // 用户可见语义：同一个词同时被多个词库收录、各库给的权重不同时，按**最高权重**
    // 参与排序；关掉出该权重的那个库，就回退到剩余启用库里的最高值。
    // 下面这组用例把这条语义在三条查询路径 + 开关组合 + 排序层级上钉死。
    // ==================================================================

    /// 跨库合并专用测试层：可配 `base_order`、可热启停，精确/前缀/简拼三条路径都有真实返回。
    struct FlexLayer {
        name: String,
        ltype: LayerType,
        base_order: i32,
        enabled: std::sync::atomic::AtomicBool,
        items: Vec<Candidate>,
        /// 该层响应的声母串（简拼召回）：命中即返回全部 `items`。
        /// 刻意不在测试层里重造声母投影算法——那会让用例验的是那个算法而不是合并语义。
        abbrev_key: String,
    }

    impl FlexLayer {
        fn new(name: &str, items: Vec<Candidate>) -> Self {
            Self {
                name: name.into(),
                ltype: LayerType::System,
                base_order: 0,
                enabled: std::sync::atomic::AtomicBool::new(true),
                items,
                abbrev_key: String::new(),
            }
        }
        fn with_base_order(mut self, o: i32) -> Self {
            self.base_order = o;
            self
        }
        fn with_abbrev(mut self, k: &str) -> Self {
            self.abbrev_key = k.into();
            self
        }
        fn with_ltype(mut self, t: LayerType) -> Self {
            self.ltype = t;
            self
        }
    }

    impl DictLayer for FlexLayer {
        fn name(&self) -> &str {
            &self.name
        }
        fn layer_type(&self) -> LayerType {
            self.ltype
        }
        fn base_order(&self) -> i32 {
            self.base_order
        }
        fn enabled(&self) -> bool {
            self.enabled.load(std::sync::atomic::Ordering::Relaxed)
        }
        fn set_enabled(&self, e: bool) {
            self.enabled.store(e, std::sync::atomic::Ordering::Relaxed);
        }
        fn search(&self, code: &str, _l: usize) -> Vec<Candidate> {
            self.items
                .iter()
                .filter(|c| c.code == code)
                .cloned()
                .collect()
        }
        fn search_prefix(&self, p: &str, _l: usize) -> Vec<Candidate> {
            self.items
                .iter()
                .filter(|c| c.code.starts_with(p))
                .cloned()
                .collect()
        }
        fn search_abbrev(&self, a: &str, _l: usize) -> Vec<Candidate> {
            if !self.abbrev_key.is_empty() && a == self.abbrev_key {
                self.items.clone()
            } else {
                Vec::new()
            }
        }
    }

    /// 三条查询路径**共用同一套 max 语义**。
    ///
    /// 此前只有精确查询（`dedup_same_text_inherits_higher_weight`）有守门。而扩展词库
    /// 场景下用户看到的候选大多来自前缀补全与简拼召回——那两条若失守，表现正是
    /// 「开了高权重的扩展库却没提前」，且精确查询的测试全绿，从测试上看不出任何异常。
    #[test]
    fn cross_layer_max_weight_holds_for_all_query_kinds() {
        // 主库权重 100，扩展库同词权重 500。三条路径都应合并成一条、权重 500。
        let build = || {
            let c = CompositeDict::new();
            c.register_layer(Box::new(
                FlexLayer::new("main", vec![cand("你好", "nihao", 100, 0)]).with_abbrev("nh"),
            ));
            c.register_layer(Box::new(
                FlexLayer::new("extra", vec![cand("你好", "nihao", 500, 0)]).with_abbrev("nh"),
            ));
            c
        };
        for (kind, got) in [
            ("精确", build().search("nihao", 10)),
            ("前缀", build().search_prefix("ni", 10)),
            ("简拼", build().search_abbrev("nh", 10)),
        ] {
            assert_eq!(got.len(), 1, "{kind}查询：同 text 应合并成一条");
            assert_eq!(got[0].weight, 500, "{kind}查询：应取跨库最高权重");
        }
    }

    /// **关闭词库 → 权重回退到剩余启用层的最高值**（本条是该特性的核心语义）。
    ///
    /// 回退不是一段专门的回退代码，而是「每次查询按当前启用集现算」的自然结果：
    /// `merge_search` 开头就跳过禁用层。故本用例同时守着「关掉不残留」与「开回来即恢复」
    /// 两个方向——只测其中一个方向时，把 max 缓存进快照的实现也能通过。
    #[test]
    fn disabling_layer_falls_back_to_next_highest_weight() {
        let c = CompositeDict::new();
        // 同一个词「好」在三个库里权重不同：100 / 900 / 500。
        c.register_layer(Box::new(FlexLayer::new(
            "main",
            vec![cand("好", "h", 100, 0)],
        )));
        c.register_layer(Box::new(FlexLayer::new(
            "ext-hi",
            vec![cand("好", "h", 900, 0)],
        )));
        c.register_layer(Box::new(FlexLayer::new(
            "ext-mid",
            vec![cand("好", "h", 500, 0)],
        )));

        let w = |c: &CompositeDict| {
            let r = c.search("h", 10);
            assert_eq!(r.len(), 1, "同 text 恒合并成一条");
            r[0].weight
        };
        assert_eq!(w(&c), 900, "全开：取三库最高");

        // 关掉最高的那个库 → 回退到剩余启用库的最高值（500），既不停在 900、也不掉到主库 100。
        assert!(c.set_layer_enabled("ext-hi", false));
        assert_eq!(w(&c), 500, "关掉 900 的库后应回退到次高的 500");

        assert!(c.set_layer_enabled("ext-mid", false));
        assert_eq!(w(&c), 100, "两个扩展库都关掉后回退到主库权重");

        // 重新开启：立即恢复，无需重建引擎（层只翻原子标志、始终常驻）。
        assert!(c.set_layer_enabled("ext-hi", true));
        assert_eq!(w(&c), 900, "重新开启应立即恢复最高权重");
    }

    /// **同 code 时**，max 与注册顺序、层类型优先级都无关；而 natural_order 恒随高优先层。
    ///
    /// 这两件事互不牵连，正是合并期最容易搞混的一处：拿到一条「权重来自扩展库、出现序来自
    /// 用户层」的候选是**预期行为**，不是 bug。
    #[test]
    fn max_weight_independent_of_layer_order_and_type() {
        let c = CompositeDict::new();
        // 同一个词条 `z`→「中」，两层各收录一份、权重不同 —— 这才是「同一条记录的两份副本」。
        c.register_layer(Box::new(
            FlexLayer::new("sys", vec![cand("中", "z", 900, 7)]).with_ltype(LayerType::System),
        ));
        c.register_layer(Box::new(
            FlexLayer::new("user", vec![cand("中", "z", 10, 3)]).with_ltype(LayerType::User),
        ));
        let r = c.search("z", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].weight, 900, "weight 取跨层最大值（无视层优先级）");
        assert_eq!(r[0].natural_order, 3, "natural_order 随高优先层（用户层）");
    }

    /// ★★★ **权重不跨码位漂移**：同一个字在两层有**不同编码**时，各自的权重互不继承。
    ///
    /// `zhong`→「中」与 `z`→「中」是**两个独立词条**，各有各的权重。呈现上按 text 去重只
    /// 留一条（用户不该在候选里看到两个「中」），但留下来那条必须带**它自己码位**的权重
    /// ——否则候选会带着「别的码位的权重」参与排序。码表方案尤其不能这样：那里码长本身
    /// 就是分档依据，简码条目凭空拿到全码条目的高权重会直接改掉首选。
    ///
    /// 「多个词库收录同一条、按最高权重算」这条用户可见语义，主键是 `(code, text)`
    /// 而不是 `text` —— 见 `max_weight_independent_of_layer_order_and_type` 那条对照。
    ///
    /// 曾经反了：跨码位无条件取 max，`code="z"` 的候选带着 `zhong` 的 900 分。
    #[test]
    fn weight_does_not_leak_across_code_positions() {
        let c = CompositeDict::new();
        c.register_layer(Box::new(
            FlexLayer::new("sys", vec![cand("中", "zhong", 900, 7)]).with_ltype(LayerType::System),
        ));
        c.register_layer(Box::new(
            FlexLayer::new("user", vec![cand("中", "z", 10, 3)]).with_ltype(LayerType::User),
        ));
        let r = c.search_prefix("z", 10);
        assert_eq!(r.len(), 1, "呈现上仍按 text 去重，只留一条");
        assert_eq!(r[0].code, "z", "留下的是高优先层（用户层）的码");
        assert_eq!(
            r[0].weight, 10,
            "权重必须是 `z` 这个码位自己的 10，不得继承 `zhong` 码位的 900"
        );
        assert!(
            r[0].merged_codes.iter().any(|x| x == "zhong"),
            "被丢弃那条的**码位**仍要并入（检索范围过滤依赖它）——不继承的是权重，不是码位"
        );
    }

    /// 前缀查询换成最短码时，**权重与码整组同换**（换码＝改换词条）。
    ///
    /// 与 [`weight_does_not_leak_across_code_positions`] 是同一条原则的另一面：权重恒随
    /// 它自己的码位走。只钉「不继承」不钉「换码要换权重」的话，一个换了 code 却留着旧码
    /// 权重的实现照样能让那条测试变绿，而它配出的正是「A 码位的编码 + B 码位的权重」。
    #[test]
    fn shortest_code_swap_carries_its_own_weight() {
        let c = CompositeDict::new();
        // 用户层的码更长（100），系统层有更短的码（800）：前缀查应换成短码并带上 800。
        c.register_layer(Box::new(
            FlexLayer::new("user", vec![cand("好", "haoaa", 100, 0)]).with_ltype(LayerType::User),
        ));
        c.register_layer(Box::new(
            FlexLayer::new("sys", vec![cand("好", "h", 800, 0)]).with_ltype(LayerType::System),
        ));
        let r = c.search_prefix("h", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].code, "h", "前缀应保留最短码");
        assert_eq!(
            r[0].weight, 800,
            "换码后权重必须是短码 `h` 自己的 800，不能留着长码 `haoaa` 的 100"
        );
    }

    /// **跨库权重比较不需要两库配同一个 `base_order`**：默认排序 `better` 的层级是
    /// 「weight 降 → base_order 升 → natural_order 升」，base_order 只在**等权时**决定
    /// 库间先后，压不住权重。
    ///
    /// 反向对照放在同一个用例里：`[engine.codetable].base_sort = "natural"` 的方案走
    /// `by_natural`，那条路**完全忽略权重**——此时跨库 max 取到多高都不改变呈现顺序。
    /// 「调了权重没生效」的头号嫌疑就是它，两条一起钉住才能把责任分清。
    #[test]
    fn weight_outranks_base_order_unless_base_sort_is_natural() {
        let c = CompositeDict::new();
        // 扩展库 base_order 更大（等权时本应排后），但它的词权重更高。
        c.register_layer(Box::new(
            FlexLayer::new("main", vec![cand("主", "x", 100, 0)]).with_base_order(0),
        ));
        c.register_layer(Box::new(
            FlexLayer::new("extra", vec![cand("扩", "x", 900, 0)]).with_base_order(5),
        ));
        let r = c.search("x", 10);
        assert_eq!(r.len(), 2, "不同 text 不合并");
        assert_eq!(
            r[0].text, "扩",
            "weight 高者在前：跨库权重比较无需两库 base_order 相同"
        );

        // 对照：natural 模式忽略权重，这时才轮到 base_order 说话，主库回到首位。
        let mut natural = r.clone();
        natural.sort_by(wind_candidate::by_natural);
        assert_eq!(
            natural[0].text, "主",
            "base_sort=natural 忽略权重，改由 base_order 定序"
        );
    }

    /// 同 text 合并后 `base_order` 跟着**首个出现层**走（与 code/natural_order 同组），
    /// 只有 weight 跨层继承。
    ///
    /// 后果值得知道：主库也收录该词时，合并结果落在主库档位；主库没有时，同一个词就落在
    /// 扩展库档位。等权场景下这会表现为「同一个词，装不装主库位置不一样」。
    #[test]
    fn merged_candidate_keeps_first_layer_base_order() {
        let c = CompositeDict::new();
        c.register_layer(Box::new(
            FlexLayer::new("main", vec![cand("好", "h", 100, 0)]).with_base_order(0),
        ));
        c.register_layer(Box::new(
            FlexLayer::new("extra", vec![cand("好", "h", 900, 0)]).with_base_order(5),
        ));
        let r = c.search("h", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].weight, 900, "权重来自扩展库");
        assert_eq!(r[0].base_order, 0, "base_order 仍属主库（首个出现层）");

        // 主库关掉后，同一个词改由扩展库出，档位随之变成扩展库的。
        assert!(c.set_layer_enabled("main", false));
        let r2 = c.search("h", 10);
        assert_eq!(r2[0].base_order, 5, "主库不参与后，档位落到扩展库");
    }
}
