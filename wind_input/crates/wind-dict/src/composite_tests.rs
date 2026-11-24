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
    fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        _limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        self.items
            .iter()
            .filter(|c| crate::layer::pattern_matches(pattern, wildcard, &c.code, with_prefix))
            .cloned()
            .collect()
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

/// 同 text 去重不得吞掉来源归属：右键删除按 `is_user_dict`/`is_temp_dict` 决定去哪张表删，
/// 而 `redb` 的 `remove` 对不存在的 key 静默成功，丢掉标记就是「点多少次都无作用」。
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

/// 跨层「更长后继」判据必须逐层原样问，不能走 `merge_search`。
///
/// 合并期按 text 去重并「同 text 取最短码」，同一个词高优先层有短码、系统层有长码时只剩短码，
/// 正好抹掉本判据要找的信息，会把「还能接着打」误判成「已到底」，触发不该有的自动上屏 / 顶码。
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

    // 对照：合并去重后同 text 取最短码 "ok"，长度不大于输入 → 漏判为 false。
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

/// boundary 必须与 code 同进同出：二者描述同一个音节切分，换了码却留着旧码的边界会配出
/// 自相矛盾的候选，下游按错位边界校验会静默误杀候选。
#[test]
fn boundary_travels_with_code_on_merge() {
    // ① 同 text 去重：code/boundary 保留首个出现层（高优先层），只有 weight 跨层继承。
    let c = CompositeDict::new();
    c.register_layer(Box::new(MockLayer {
        name: "user".into(),
        ltype: LayerType::User,                          // 优先级高于 System
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
// 同一个词被多个词库收录、权重不同时按最高权重参与排序；关掉出该权重的库，就回退到
// 剩余启用库里的最高值。下面在三条查询路径 + 开关组合 + 排序层级上钉死这条语义。
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

/// 三条查询路径共用同一套 max 语义。
///
/// 扩展词库场景下候选大多来自前缀补全与简拼召回，只守精确查询的话那两条失守也全绿，
/// 表现为「开了高权重的扩展库却没提前」。
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

/// 关闭词库 → 权重回退到剩余启用层的最高值。
///
/// 回退是「每次查询按当前启用集现算」的自然结果（`merge_search` 开头跳过禁用层）。同时守
/// 「关掉不残留」与「开回来即恢复」两个方向，只测一个方向时把 max 缓存进快照的实现也能过。
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

/// 同 code 时，max 与注册顺序、层类型优先级都无关；而 natural_order 恒随高优先层。
/// 拿到「权重来自扩展库、出现序来自用户层」的候选是预期行为。
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

/// 权重不跨码位漂移：同一个字在两层有不同编码时，各自的权重互不继承。
///
/// `zhong`→「中」与 `z`→「中」是两个独立词条。呈现上按 text 去重只留一条，但留下那条必须带
/// 它自己码位的权重；码表方案里简码条目拿到全码条目的高权重会直接改掉首选。
/// 「按最高权重算」的主键是 `(code, text)` 而不是 `text`，见
/// `max_weight_independent_of_layer_order_and_type`。
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

/// 前缀查询换成最短码时，权重与码整组同换（换码即改换词条）。
///
/// 与 [`weight_does_not_leak_across_code_positions`] 互为两面：只钉「不继承」的话，换了 code
/// 却留着旧码权重的实现照样能过那条测试。
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

/// 跨库权重比较不需要两库配同一个 `base_order`：`better` 的层级是「weight 降 → base_order 升
/// → natural_order 升」，base_order 只在等权时决定库间先后。
///
/// 反向对照：`[engine.codetable].base_sort = "natural"` 走 `by_natural`，完全忽略权重，
/// 跨库 max 取到多高都不改变呈现顺序——「调了权重没生效」的头号嫌疑就是它。
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

/// 同 text 合并后 `base_order` 跟着首个出现层走（与 code/natural_order 同组），只有 weight
/// 跨层继承。后果：等权时同一个词装不装主库，落的档位不一样。
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

/// 通配合并：同 text 同 code 才合并（取更高权重）；同字不同码各留一条，学码要看到每个码位。
/// 排序等长档在前，高权重的更长编码也排不到等长之前。
#[test]
fn pattern_merges_by_text_and_code_equal_length_first() {
    let slot = crate::layer::WILDCARD_SLOT;
    let c = CompositeDict::new();
    c.register_layer(Box::new(MockLayer {
        name: "user".into(),
        ltype: LayerType::User,
        items: vec![cand("工", "aaaa", 50, 0)],
    }));
    c.register_layer(Box::new(MockLayer {
        name: "sys".into(),
        ltype: LayerType::System,
        items: vec![
            cand("工", "aaaa", 80, 0),
            cand("工", "abaa", 70, 1),
            cand("字", "abaaq", 9999, 2),
            cand("式", "aa", 10, 3),
        ],
    }));
    let p = format!("a{slot}{slot}a");
    let got: Vec<(String, String, i32)> = c
        .search_pattern(&p, slot, 10, true)
        .into_iter()
        .map(|x| (x.text, x.code, x.weight))
        .collect();
    assert_eq!(
        got,
        [
            ("工".to_string(), "aaaa".to_string(), 80),
            ("工".to_string(), "abaa".to_string(), 70),
            ("字".to_string(), "abaaq".to_string(), 9999),
        ]
    );
    assert_eq!(
        c.search_pattern(&p, slot, 10, false).len(),
        2,
        "不带补全时无更长档"
    );
}
