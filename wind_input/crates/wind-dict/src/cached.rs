//! 词典缓存层：yaml 首次加载后写入 .wdat 缓存，后续直接 mmap 读取
//!
//! 与 Go 版 mmap 共享池对齐，显著降低内存占用。

use crate::codetable::CodetableDict;
use crate::datformat::{WdatReader, WdatWriter};
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

/// 反查索引的存储与查询已下沉到 [`crate::reverseidx`]（`.wridx` 格式，可 mmap）。
/// 这里保留再导出，是因为它的构建入口（`build_reverse_index_from`）天然属于本模块
/// ——它要遍历 [`CachedDict`]，而消费方一直是 `use wind_dict::cached::ReverseIndex`。
pub use crate::reverseidx::{CodeList, ReverseIndex};

/// 一条查询命中，含音节边界。由 [`CachedDict::search_with_boundary`] /
/// [`CachedDict::search_prefix_with_boundary`] 返回（拼音专用）。
#[derive(Debug, Clone)]
pub struct DictHit {
    /// 命中的词典 code：精确查询即查询串本身，前缀查询为该条目的完整 code。
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub order: i32,
    /// 音节边界 bitmask，见 [`crate::binformat::DictEntry::boundary`]。0=无边界信息，降级回 DAG。
    pub boundary: u64,
}

/// 前缀查询中该条目是否**该保留**：词的音节数不超过 `max_syllables`。
///
/// 拼音「短输入音节数匹配」判据的词库层半边（策略与阈值在 `wind-engine` 的
/// `STRICT_SYLLABLE_MATCH_MAX`，`max_syllables` 由调用方按**输入自身**的音节数给出）。
///
/// ⚠️ **判据问的是「输入有几个音节」，不是「输入在这条候选的切分下占几个音节」。**
/// 后者曾被用过一版，会让 `xia` 因为存在 `xi|an`（西安）这种切分而被当成 2 音节输入、
/// 放行整批词组；同理 `ying` 会漏出 `yin|guo`（因果）。输入的音节数是**输入自己的属性**、
/// 全局唯一（`completed_len` 是词图的性质，与走哪条切分路径无关），不该外包给候选。
///
/// `boundary == 0`（无边界信息）**一律放行** —— 词库层无从判断，交调用方按需用 DAG
/// 现切兜底（见 `wind-engine` 的 `effective_boundary`）。职责分工：词库层只提前丢弃
/// **确定不合格**的条目以免它们白占 top-N 配额，最终判定仍在引擎层。
///
/// ⚠️ **`pub(crate)`，外部一律用 [`prefix_entry_keep`]** —— 本函数只是那道合成判据的
/// 「音节数」半边，单独拿它当保留判据正是 2026-09-04 那次报障的成因：`sheng` 与 `shen`
/// 的音节数完全同形（都是 1），它分不开，于是关掉模糊音也照样出 `sheng` 的字。
/// 收窄可见性是为了让编译器把关，而不是靠注释提醒下一个人别漏了对齐那半边。
pub(crate) fn prefix_syllable_keep(boundary: u64, max_syllables: u32) -> bool {
    boundary == 0 || boundary.count_ones() <= max_syllables
}

/// 前缀查询中该条目是否**音节边界对齐**：输入已完成的音节部分必须正好停在该条目的
/// 某个音节起点上，即补全只能从下一个音节开始，不能把当前音节继续拉长。
///
/// ## 这道判据补的是什么洞
///
/// wdat 的 key 是**扁平码**（`nihao` 而非 `ni hao`），这是硬性设计——带空格的 key 会让
/// `niha` 无法前缀匹配，而逐键前缀匹配是「边打边出候选」的命脉。代价是前缀匹配发生在
/// **字符**域：`shen` 是 `sheng` 的字符前缀，于是打 `shen` 会召回「生/省/声/升/胜」。
/// [`prefix_syllable_keep`] 拦不住它们——那些字都只有 1 个音节，音节数完全合格。
///
/// 真实报障（2026-09-04）：`[schema.pinyin.fuzzy] en_eng = false` 时打 `shen` 仍出
/// `sheng` 的字。实测前缀召回前 20 条里 8 条是 `sheng`，且「生」(w=8631) 权重高过
/// 「深」(w=5042)——不只是多出不该有的，还按 top-k 挤掉了本音节的低频字。
/// 波及面远超 en/eng 一组：`an/ang`、`in/ing`、`ian/iang`、`uan/uang`、`a→ai/an/ao`、
/// `e→ei/en/er`、`o→ou/ong`、`ni→nin/ning/niu/nie/nian/niang/niao` 全部同病。
///
/// 更要害的是它**旁路了模糊音开关**：`en→eng` 的等价本该只由模糊音层在用户显式开启时
/// 提供（带 `fuzzy_penalized` 惩罚、单独的匹配层级），这里却无条件、单向地白送了一份。
///
/// ## 判据取 `completed_len` 而不是击键长度
///
/// `completed_len` = 输入中**已构成完整音节**的那一段的字节长度（DAG 切分的性质）。
/// 这样残码场景天然正确，不需要额外分支：
///
/// | 输入 | `completed_len` | 判据位 | `shenme`(什么) | `sheng`(生) |
/// |---|---|---|---|---|
/// | `shen` | 4 | 4 | `0b10001` ✅ | `0b1` ❌ |
/// | `shenm` | 4 | 4 | ✅ 残码补全照常 | — |
/// | `fe`（不成音节）| 0 | 0 | 位 0 恒置位 ⇒ **全放行** | 放行 |
///
/// ★ 第二行的「—」是结构性的，不是省略：**残码位上跨音节的候选根本进不了前缀集合**。
/// `shengming` 的第 4 个字符是 `g`、`shenm` 的是 `m`，它压根不是前缀；推而广之，能延长
/// 当前音节的字符（`n`/`g`/韵母）一旦跟在后面，`Dag::maximum_match` 就会把它并进来切成
/// 更长的完整音节（`fen` + `g` 直接切成 `feng`），于是它不再是残码。「残码」与「跨越已
/// 完成音节」两个条件互斥 —— 所以本判据在残码位恒放行是自动成立的，不是特意开的口子。
/// （初版为此写的守门测试在**修改前的基线上同样是绿的**，那是条抓不住回归的假测试。）
///
/// 最后一行正是主流全拼输入法的行为、也是用户描述的规则：`fe` 不是合法拼音 ⇒ 走字符
/// 前缀匹配；`fen` 是合法音节 ⇒ 转为音节匹配，不含 `feng`。同一个判据覆盖两种形态。
///
/// ## ★★ 代价，以及为什么它可以接受（2026-09-04 用户拍板，改动前必读）
///
/// 这道判据挡住的不只是单字。`yingu` 切分为 `yin|gu`、`shaixu` 切分为 `shai|xu`，而
/// `gu`/`xu` **本身都是合法音节** —— 于是「因果」(`yin|guo`)、「筛选」(`shai|xuan`)
/// 同样被挡下，用户得打完 `yinguo` / `shaixuan`。
///
/// **不要再找「只挡单字、放行词」的更聪明判据**：`fen→feng` 与 `yingu→因果` 在音节结构
/// 上完全同形（候选码都是「把最后一个已完成音节拉长后即结束」），试过的几条路——只保护
/// 非末尾音节、要求候选多出音节、按 `distance` 分档——全都同时命中或同时放过两者。唯一
/// 存在的区分轴是「输入的音节数」或「候选是字还是词」，那是取舍不是算法。
///
/// 取严格档的理由（用户原话）：**用户要打别的词时会继续往下打，而不是翻候选去找**；
/// 顺带把「打到 `gu` 那一刻」的候选量压下来，整体更精准。实测候选数：`fen` 56 → 29、
/// `shen` 68 → 48、`ni` 92 → 34，被砍掉的全部是跨音节条目。
///
/// ★ 由此还有一条推论：出厂 `min_syllables = 4` 下，**整音节**输入（`started < 4`）不再
/// 存在跨音节的前缀补全 —— cap 把候选音节数压到 ≤ `started`，本判据又要求候选在
/// `completed_len` 处开新音节，两者合起来只剩「候选码 == 输入」即精确匹配。此时前缀补全
/// 只在**残码位**发挥作用，那正是它该在的地方。
///
/// ## 两处放行
///
/// - `boundary == 0`：无边界信息（旧词典、用户手输码、超 64 字节的长码），与
///   [`prefix_syllable_keep`] 同款降级——词库层判不了的交给引擎层的 `effective_boundary`。
/// - `code_len <= completed_len`：不是补全（精确匹配或更短的子短语），它没有预测任何
///   未输入的内容，本判据不适用。
pub fn prefix_syllable_aligned(boundary: u64, code_len: usize, completed_len: usize) -> bool {
    if boundary == 0 || code_len <= completed_len {
        return true;
    }
    // `completed_len >= 64` 时 bitmask 表达不了该位置，与 boundary 的 64 字节天花板同款
    // 降级：放行，交给引擎层。`checked_shr` 在移位量越界时返回 None，正好是这个语义。
    u32::try_from(completed_len)
        .ok()
        .and_then(|bit| boundary.checked_shr(bit))
        .is_none_or(|v| v & 1 == 1)
}

/// 前缀查询条目的完整保留判据：音节数不超限（[`prefix_syllable_keep`]）**且**音节边界
/// 对齐（[`prefix_syllable_aligned`]）。两个后端与引擎层的事后闸门共用这一份。
pub fn prefix_entry_keep(
    boundary: u64,
    code_len: usize,
    max_syllables: u32,
    completed_len: usize,
) -> bool {
    prefix_syllable_keep(boundary, max_syllables)
        && prefix_syllable_aligned(boundary, code_len, completed_len)
}

/// 最长的拼音音节（`zhuang`/`chuang`/`shuang`）的字节数。
const MAX_SYLLABLE_BYTES: usize = 6;

/// 带音节上限的前缀查询的码长上限：不超过 `max_syllables` 个音节的码最长 6×N 字节。
///
/// 作用是让 wdat 的分支限界能按深度剪枝：裸声母 `sh` 的合格条目（单字）稀疏，而每棵子树里
/// 都有高权重的多音节词，按权重上界剪不动，整棵子树被走一遍（约 30ms），按深度剪后约 1ms。
/// 权重全打平的导入词库更甚：上界恒等于第 `limit` 名，一刀都剪不掉。带边界的条目本就满足；
/// 它只额外拦下 `boundary == 0` 的超长条目，那些按任何切分都超过 N 个音节，引擎层闸门同样
/// 不会放行。
pub(crate) fn prefix_code_len_cap(max_syllables: u32) -> usize {
    (max_syllables as usize).saturating_mul(MAX_SYLLABLE_BYTES)
}

/// 带音节上限的前缀查询（`search_prefix_*_capped`）的保留判据：码长上限 +
/// [`prefix_entry_keep`]。系统词库两个后端与用户数据层共用这一份。
pub fn prefix_capped_keep(
    boundary: u64,
    code_len: usize,
    max_syllables: u32,
    completed_len: usize,
) -> bool {
    code_len <= prefix_code_len_cap(max_syllables)
        && prefix_entry_keep(boundary, code_len, max_syllables, completed_len)
}

/// `foo/bar.dict.yaml` → `foo/bar.wdat`（剥掉整个 `.dict.yaml` 后缀）。
///
/// **不能用 `Path::with_extension("wdat")`** —— 那只替换最后一级扩展名，会得到
/// `bar.dict.wdat`。本约定与 Go 版 `wdbOnlyInDir` 一致，从 Go 版迁移过来的词库文件名
/// 可直接使用。
///
/// 供 wdat-only 分发模式定位**用户投放**的二进制词库，与缓存路径无关——后者由
/// `EngineManager::cache_path` 生成并落在独立的 cache 目录。
///
/// 非 `.dict.yaml` / `.yaml` 结尾返回 `None`。
pub fn wdat_sibling(yaml_path: &Path) -> Option<std::path::PathBuf> {
    let name = yaml_path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".dict.yaml")
        .or_else(|| name.strip_suffix(".yaml"))?;
    Some(yaml_path.with_file_name(format!("{stem}.wdat")))
}

/// 缓存词典：优先使用 mmap，回退到内存模式
pub enum CachedDict {
    /// mmap 零拷贝模式（低内存，wdat DAT 格式）。
    ///
    /// reader 经 [`crate::reader_pool`] 按文件路径共享：同一个 wdat 被多个方案引用时
    /// （如 pinyin / shuangpin / 混输子引擎同指 rime_frost）只映射一份。用 `Arc` 而非
    /// 独占持有，是为了让最后一个持有者释放时 mmap 随之解除——词库重建要 rename 覆盖，
    /// Windows 上映射未解除会 Access Denied。
    Mmap(Arc<WdatReader>),
    /// 内存模式（首次加载或缓存写入失败）
    Memory(CodetableDict),
    /// 分层：首层是主库，其余是扩展库（拼音细胞词库），每层各有自己的缓存文件。
    ///
    /// 查询时逐层查、按单库的排序语义合并（见 [`merge_ranked`] / [`merge_exact`]），故开关
    /// 一张扩展库只是多挂或少挂一层——主库那份几十 MB 的合并缓存不用重建，全拼与双拼开的
    /// 扩展库不同也共用同一批缓存。只由拼音引擎构建（`PinyinEngine` 只持有一个词库对象，
    /// 码表的扩展库另有 CompositeDict 分层）。
    Layered(Vec<CachedDict>),
}

/// 只有一层有结果时原样返回它（最常见：细胞库没命中）——单层的结果本就与合并缓存一致，
/// 不付任何合并代价。都为空返回空。多层有结果返回 `None`，交给调用方合并。
fn single_nonempty(results: &mut [Vec<DictHit>]) -> Option<Vec<DictHit>> {
    let mut it = results.iter().enumerate().filter(|(_, r)| !r.is_empty());
    match (it.next(), it.next()) {
        (None, _) => Some(Vec::new()),
        (Some((i, _)), None) => Some(std::mem::take(&mut results[i])),
        _ => None,
    }
}

/// 精确查询的跨层合并，结果与「各层并成一份合并缓存」逐项一致：同 code 下按权重降序、
/// 同权按层序、层内保持原序，`order` 重排为 0,1,2…（合并缓存的写法）。
///
/// **不跨层去重**：合并缓存同样保留重复项（下游引擎自己会去重），保留它们才能逐项一致，
/// 也免得开了一张细胞库就把主库自身经 import 合出来的层内重复项一并吞掉。
fn merge_exact(mut layers: Vec<Vec<DictHit>>) -> Vec<DictHit> {
    if let Some(one) = single_nonempty(&mut layers) {
        return one;
    }
    let mut out: Vec<(usize, DictHit)> = layers
        .into_iter()
        .enumerate()
        .flat_map(|(li, hits)| hits.into_iter().map(move |h| (li, h)))
        .collect();
    out.sort_by(|(la, a), (lb, b)| {
        b.weight
            .cmp(&a.weight)
            .then(la.cmp(lb))
            .then(a.order.cmp(&b.order))
    });
    out.into_iter()
        .enumerate()
        .map(|(i, (_, mut h))| {
            h.order = i as i32;
            h
        })
        .collect()
}

/// 前缀类查询的跨层合并，结果与合并缓存逐项一致：按单库前缀查询的排序键（权重降序 →
/// order 升序 → code 字典序，见 `datformat::RankKey`）排，截到 `limit`。
///
/// ⚠ `order` 是「该词在自己 code 下的名次」，各层各算各的，不能跨层直接比。合并缓存里的
/// 名次 = 层内名次 + **其他层同 code 下排在它前面的条目数**（权重更高，或同权而层更靠前）。
/// 后者用各层的**精确查询**去数，不能拿候选池数：按音节截断的查询在读端先取名次、再做音节
/// 过滤，被滤掉的同 code 高权词不在池里，却照样占着合并缓存里的名次。
///
/// 每层各取 `limit` 条再合并是精确的：全局前 `limit` 名必在其所在层的前 `limit` 名之内。
/// 不跨层去重，理由同 [`merge_exact`]。
fn merge_ranked(
    layers: &[CachedDict],
    mut results: Vec<Vec<DictHit>>,
    limit: usize,
) -> Vec<DictHit> {
    if let Some(one) = single_nonempty(&mut results) {
        return one;
    }
    // (code, 层) → 该层该 code 下全部条目的权重。同一个 code 在池里常出现多次，只查一遍。
    let mut weights: std::collections::HashMap<(String, usize), Vec<i32>> =
        std::collections::HashMap::new();
    let mut out: Vec<DictHit> = Vec::new();
    for (li, hits) in results.into_iter().enumerate() {
        for mut h in hits {
            let mut before = 0i32;
            for (lj, layer) in layers.iter().enumerate() {
                if lj == li {
                    continue;
                }
                let ws = weights.entry((h.code.clone(), lj)).or_insert_with(|| {
                    layer
                        .search_with_boundary(&h.code)
                        .into_iter()
                        .map(|e| e.weight)
                        .collect()
                });
                before += ws
                    .iter()
                    .filter(|&&w| w > h.weight || (w == h.weight && lj < li))
                    .count() as i32;
            }
            h.order += before;
            out.push(h);
        }
    }
    out.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then(a.order.cmp(&b.order))
            .then(a.code.cmp(&b.code))
    });
    if limit > 0 {
        out.truncate(limit);
    }
    out
}

impl CachedDict {
    /// 加载词典，自动使用 .wdat 缓存
    ///
    /// 流程：
    /// 1. 检查 .wdat 缓存是否存在且比 .yaml 新
    /// 2. 如果是，直接 mmap 打开
    /// 3. 如果否，加载 .yaml，写入 .wdat 缓存，然后 mmap 打开
    pub fn load(yaml_path: &Path) -> anyhow::Result<Self> {
        let wdat_path = yaml_path.with_extension("wdat");
        Self::load_at(yaml_path, &wdat_path)
    }

    /// 加载词典，使用指定的 .wdat 缓存路径（缓存可与源分离，如放
    /// `%LOCALAPPDATA%\WindInput\cache`，避免写入只读的安装目录）。
    pub fn load_at(yaml_path: &Path, wdat_path: &Path) -> anyhow::Result<Self> {
        Self::load_at_with(yaml_path, wdat_path, false)
    }

    /// 同 [`load_at`]，`lowercase_code=true` 时把 code 列小写化（英文词库）。
    /// 缓存命中时直接 mmap（缓存内已是小写码）；缓存重建时用 `load_lowercased`。
    pub fn load_at_with(
        yaml_path: &Path,
        wdat_path: &Path,
        lowercase_code: bool,
    ) -> anyhow::Result<Self> {
        Self::load_impl(yaml_path, wdat_path, lowercase_code, None)
    }

    /// 英文词库：code 列小写化（同 `load_at_with(.., true)`），并按英文词频名次表给没写权重
    /// 的词条赋权（见 [`crate::freq_rank`]）。
    ///
    /// 名次表作为 wdat 的**第二个源**参与指纹：改了它（或它从无到有），缓存就重建。
    /// 名次表不在时按「缺失」这一稳定事实算指纹、不赋权。
    ///
    /// ⚠️ 同一份英文词库的每个读盘点都要走这里：有的带名次表、有的不带，同一槽位的指纹
    /// 就有两种，两份 wdat 来回重建。
    pub fn load_english_at(
        yaml_path: &Path,
        wdat_path: &Path,
        ranks_path: &Path,
    ) -> anyhow::Result<Self> {
        Self::load_impl(yaml_path, wdat_path, true, Some(ranks_path))
    }

    fn load_impl(
        yaml_path: &Path,
        wdat_path: &Path,
        lowercase_code: bool,
        ranks_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        // wdat-only：用户只投放编译好的二进制词库、不带 yaml 源（对齐 Go 的 wdb-only 分发）。
        //
        // 必须抢在 cache_is_valid 之前：本模式下压根没有 yaml 源，而 cache_is_valid 问的是
        // 「缓存与**源**是否一致」——对无源可比的场景，那个问题本身就没有意义。
        // （2026-08-24 起 `cache_fp::fingerprint` 把「源缺失」当作可哈希的稳定事实而非失败，
        // 故这里不再能靠「缺源必判失效」兜底，顺序更是必须的。）
        // 也必须抢在下面 `CodetableDict::load(yaml_path)?` 之前，那是硬失败点
        // （文件不存在直接 Err，后续写缓存/mmap 都不会执行）。
        if !yaml_path.is_file()
            && let Some(sidecar) = wdat_sibling(yaml_path)
            && sidecar.is_file()
        {
            return match crate::reader_pool::open_wdat(&sidecar) {
                Ok(reader) => {
                    info!(
                        "以 wdat-only 模式加载词库: {} ({} keys)",
                        sidecar.display(),
                        reader.key_count()
                    );
                    Ok(Self::Mmap(reader))
                }
                // 无 yaml 源可重建，只能明确失败：静默降级会让整个方案无引擎，
                // 而用户完全无从判断是文件版本不对还是路径没放对。
                Err(e) => Err(anyhow::anyhow!(
                    "wdat-only 词库 {} 加载失败: {}。该词库无 yaml 源，无法重建，请更新词库文件。",
                    sidecar.display(),
                    e
                )),
            };
        }

        // `wdat_path` 是**槽位**（逻辑缓存路径），实际文件是 `<stem>-<指纹>.wdat`
        // （内容寻址，见 `cache_fp` 的 S2 段）。按槽位 single-flight：同一个槽位常被多个
        // 方案引用（如 wubi86 独立引擎与混输的 primary 子引擎），缓存失效时会同时走到下面的
        // write_cache。混输子引擎的构建不经 ensure_loaded，也就不受 build_locks 保护，故须
        // 在此自锁。
        let build_lock = crate::reader_pool::file_lock(wdat_path);
        let _build_guard = build_lock.lock().unwrap_or_else(|e| e.into_inner());

        // tag 带上 lowercase_code：同一份 yaml 在 english / 非 english 两种 dict_type 下
        // 解析结果不同，不区分就会在切换后复用大小写错误的缓存。
        let tag = crate::cache_fp::dict_tag(lowercase_code);
        // 名次表也是源：它决定写进 wdat 的权重（见 `load_english_at`）。
        let sources: Vec<&Path> = std::iter::once(yaml_path).chain(ranks_path).collect();
        // 源读不出（权限 / IO 故障）⇒ 没有稳定的缓存可言：直接解析进内存，不落盘。
        let stamps_before = crate::cache_fp::stamps_of(&sources);
        let content = crate::cache_fp::resolve_slot_fp(wdat_path, &sources, tag)
            .map(|fp| (crate::cache_fp::slot_content_path(wdat_path, &fp), fp));

        // 指纹对应的文件在 ⇒ 就是它（同时充当拿锁后的复查：等待期间别的线程可能已建好）
        if let Some((path, _)) = &content
            && path.is_file()
        {
            match crate::reader_pool::open_wdat(path) {
                Ok(reader) => {
                    info!(
                        "Using mmap cache: {} ({} keys)",
                        path.display(),
                        reader.key_count()
                    );
                    crate::cache_fp::touch_cache(path);
                    return Ok(Self::Mmap(reader));
                }
                Err(e) => {
                    warn!("Failed to open mmap cache: {}, falling back to yaml", e);
                }
            }
        }

        // 加载 yaml
        let mut dict = if lowercase_code {
            CodetableDict::load_lowercased(yaml_path)?
        } else {
            CodetableDict::load(yaml_path)?
        };
        if let Some(ranks) = ranks_path.and_then(crate::freq_rank::FreqRanks::load) {
            let n = dict.apply_freq_ranks(&ranks);
            info!("英文词频名次：{} 条词条按名次赋权", n);
        }
        info!(
            "Loaded yaml: {} ({} entries)",
            yaml_path.display(),
            dict.len()
        );

        let Some((path, fp)) = content else {
            warn!(
                "源 {} 无法计算指纹（读取失败），本次退化为内存模式——词库常驻堆而非 mmap。",
                yaml_path.display()
            );
            return Ok(Self::Memory(dict));
        };
        // 指纹是解析**之前**算的；解析期间源被保存，按旧指纹命名的文件就会装着新内容。
        if !crate::cache_fp::stamps_of(&sources).unchanged_since(&stamps_before) {
            warn!(
                "源 {} 在解析期间被改写，本次不落盘（内存模式），下次启动重来。",
                yaml_path.display()
            );
            return Ok(Self::Memory(dict));
        }

        // 确保缓存目录存在后写入 .wdat 缓存。目标是**新文件名**，不覆盖任何正被 mmap 的旧文件。
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = Self::write_cache(&dict, &path) {
            // 退化成内存模式：词库整份常驻堆而非 mmap，大词库代价可观。此前只记一行
            // 「Failed to write」，看不出后果，用户也就无从解释内存为何偏高。
            warn!(
                "写入 wdat 缓存失败 {}: {}。本次退化为内存模式——词库常驻堆而非 mmap，\
                 内存占用显著高于正常路径。常见原因是缓存目录不可写或磁盘空间不足。",
                path.display(),
                e
            );
            return Ok(Self::Memory(dict));
        }
        // `.fp` 写在实际文件旁，供二级缓存（`.wridx` / 英文词组索引）经 cache_digest 取摘要。
        // 指纹已经算过（它就在文件名里），不再重读源。
        crate::cache_fp::write_cache_fp_value(&path, &fp);
        // 同槽位只留最近两份：覆盖库与自带库交替时两份都在，不再来回重建。
        crate::cache_fp::prune_slot(wdat_path, &path, 2);

        // 用 mmap 重新打开缓存
        match crate::reader_pool::open_wdat(&path) {
            Ok(reader) => {
                info!(
                    "Using mmap cache: {} ({} keys)",
                    path.display(),
                    reader.key_count()
                );
                Ok(Self::Mmap(reader))
            }
            Err(e) => {
                warn!("Failed to open mmap cache after write: {}", e);
                Ok(Self::Memory(dict))
            }
        }
    }

    /// 将内存词典写入 .wdat 缓存
    fn write_cache(dict: &CodetableDict, wdat_path: &Path) -> anyhow::Result<()> {
        let mut writer = WdatWriter::new();

        // 遍历所有键，导出到 writer
        dict.export_to_wdat(&mut writer);

        // 先取数：`write` 消费 writer（构建时就把条目释放掉），之后问不出 key_count 了。
        let key_count = writer.key_count();
        if key_count == 0 {
            anyhow::bail!("No entries to write");
        }

        writer.write(wdat_path)?;
        info!(
            "Wrote .wdat cache: {} ({} keys)",
            wdat_path.display(),
            key_count
        );
        Ok(())
    }

    /// 精确查找
    pub fn search(&self, code: &str) -> Vec<(String, i32, i32)> {
        match self {
            Self::Mmap(reader) => reader
                .search(code)
                .into_iter()
                .map(|e| (e.text, e.weight, e.order))
                .collect(),
            Self::Memory(dict) => dict.search(code),
            Self::Layered(_) => self
                .search_with_boundary(code)
                .into_iter()
                .map(|h| (h.text, h.weight, h.order))
                .collect(),
        }
    }

    /// 精确查找，**并返回音节边界**（[`DictHit::boundary`]）。
    ///
    /// 与 [`Self::search`] 并存而非替换它：边界只对拼音有意义，而 `search` 的消费方遍布
    /// 码表/英文/cmdbar/composite 等无音节概念的场景（全仓 60+ 调用点），不应被拼音的
    /// 需求污染接口。仅拼音引擎改用本方法。
    pub fn search_with_boundary(&self, code: &str) -> Vec<DictHit> {
        match self {
            Self::Mmap(reader) => reader
                .search(code)
                .into_iter()
                .map(|e| DictHit {
                    code: e.code,
                    text: e.text,
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                })
                .collect(),
            // 内存回退（yaml 直载，未走 wdat）：CodetableDict 保有 boundary，一并带出。
            Self::Memory(dict) => dict.search_with_boundary(code),
            Self::Layered(layers) => merge_exact(
                layers
                    .iter()
                    .map(|l| l.search_with_boundary(code))
                    .collect(),
            ),
        }
    }

    /// 前缀查找，**并返回音节边界**。与 [`Self::search_prefix`] 并存，理由同
    /// [`Self::search_with_boundary`]。前缀补全候选（输入 ni → 补出「你好」nihao）同样需要
    /// 边界供双拼校验。
    pub fn search_prefix_with_boundary(&self, prefix: &str, limit: usize) -> Vec<DictHit> {
        match self {
            Self::Mmap(reader) => reader
                .search_prefix(prefix, limit)
                .into_iter()
                .map(|e| DictHit {
                    code: e.code,
                    text: e.text,
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                })
                .collect(),
            Self::Memory(dict) => dict.search_prefix_with_boundary(prefix, limit),
            Self::Layered(layers) => merge_ranked(
                layers,
                layers
                    .iter()
                    .map(|l| l.search_prefix_with_boundary(prefix, limit))
                    .collect(),
                limit,
            ),
        }
    }

    /// 前缀查找（带边界），只取**音节数不超过 `max_syllables`** 且**在 `completed_len`
    /// 处音节边界对齐**的条目。
    ///
    /// 拼音短输入严格档专用：两道过滤都下推到词库层，使 top-N 直接是 N 条合格条目，而不是
    /// 让注定被丢弃的条目吃光配额（实测 `d` 取 1000 条只剩 68 条单字；`ni` 取 20 条有 14 条
    /// 是 nin/ning/niu/nie/nian/niang/niao 的字）。判据见 [`prefix_entry_keep`]，
    /// 两个后端共用同一份。
    ///
    /// `completed_len` = 输入中已构成完整音节的那一段的**字节长度**，传 0 即退化为纯字符
    /// 前缀匹配（残码/裸声母场景本就该如此，见 [`prefix_syllable_aligned`]）。
    pub fn search_prefix_with_boundary_syllable_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<DictHit> {
        match self {
            Self::Mmap(reader) => reader
                .search_prefix_syllable_capped(prefix, limit, max_syllables, completed_len)
                .into_iter()
                .map(|e| DictHit {
                    code: e.code,
                    text: e.text,
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                })
                .collect(),
            Self::Memory(dict) => dict.search_prefix_with_boundary_syllable_capped(
                prefix,
                limit,
                max_syllables,
                completed_len,
            ),
            Self::Layered(layers) => merge_ranked(
                layers,
                layers
                    .iter()
                    .map(|l| {
                        l.search_prefix_with_boundary_syllable_capped(
                            prefix,
                            limit,
                            max_syllables,
                            completed_len,
                        )
                    })
                    .collect(),
                limit,
            ),
        }
    }

    /// 是否存在**严格长于** `prefix` 的编码（存在性判据，不物化候选）。
    /// 两个后端语义一致：mmap 走 DAT 转移表 O(字节种数)，内存走 BTreeMap 有序扫常数条。
    pub fn has_longer_code(&self, prefix: &str) -> bool {
        match self {
            Self::Mmap(reader) => reader.has_longer_code(prefix),
            Self::Memory(dict) => dict.has_longer_code(prefix),
            Self::Layered(layers) => layers.iter().any(|l| l.has_longer_code(prefix)),
        }
    }

    /// 前缀查找
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<(String, String, i32, i32)> {
        match self {
            Self::Mmap(reader) => reader
                .search_prefix(prefix, limit)
                .into_iter()
                .map(|e| (e.code, e.text, e.weight, e.order))
                .collect(),
            Self::Memory(dict) => dict.search_prefix(prefix, limit),
            Self::Layered(_) => self
                .search_prefix_with_boundary(prefix, limit)
                .into_iter()
                .map(|h| (h.code, h.text, h.weight, h.order))
                .collect(),
        }
    }

    /// 通配查询。两个后端语义一致（等长档先取满），见
    /// [`crate::datformat::WdatReader::search_pattern`]。
    pub fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<DictHit> {
        match self {
            Self::Mmap(reader) => reader
                .search_pattern(pattern, wildcard, limit, with_prefix)
                .into_iter()
                .map(|e| DictHit {
                    code: e.code,
                    text: e.text,
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                })
                .collect(),
            Self::Memory(dict) => dict.search_pattern(pattern, wildcard, limit, with_prefix),
            // ⚠ 仅供拼音：拼音不走通配，这里只求不丢条目，排序按前缀查询的键，**不复刻**
            // 「等长档先取满」。将来若有码表类调用方拿到 Layered，须先补上那条语义。
            Self::Layered(layers) => merge_ranked(
                layers,
                layers
                    .iter()
                    .map(|l| l.search_pattern(pattern, wildcard, limit, with_prefix))
                    .collect(),
                limit,
            ),
        }
    }

    /// 简拼查找（声母缩写，如 `nh`）：返回该简拼对应的**全拼码**列表，已按权重降序、截断。
    /// 仅 wdat(Mmap) 的独立 AbbrevSection 支持；内存回退（yaml 未建简拼）返回空。
    ///
    /// **返回的是码不是词**（v5）——AbbrevSection 是二级索引，指向主键。调用方拿码去
    /// 主表装配候选，从而得到真实的 code 与 boundary；此前直接返回词，简拼候选只能把
    /// code 设成简拼串，词频遂与全拼输入分裂成两份计数。
    pub fn search_abbrev(&self, abbrev: &str, limit: usize) -> Vec<String> {
        self.search_abbrev_weighted(abbrev, limit)
            .into_iter()
            .map(|(code, _)| code)
            .collect()
    }

    /// [`Self::search_abbrev`] 带权重的形态：分层合并要按权重重排各层的码。
    fn search_abbrev_weighted(&self, abbrev: &str, limit: usize) -> Vec<(String, i32)> {
        match self {
            Self::Mmap(reader) => reader
                .search_abbrev(abbrev, limit)
                .into_iter()
                .map(|e| (e.text, e.weight))
                .collect(),
            Self::Memory(_) => Vec::new(),
            Self::Layered(layers) => {
                // 同码取高权重，按权重降序、同权按码字典序——合并缓存写简拼段正是这么排的
                // （`load_rime_pinyin_dict`：码 → 各源最高权重，再按 (权重降序, 码) 排）。
                let mut out: Vec<(String, i32)> = Vec::new();
                let mut at: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for l in layers {
                    for (code, w) in l.search_abbrev_weighted(abbrev, limit) {
                        match at.get(&code) {
                            Some(&i) => out[i].1 = out[i].1.max(w),
                            None => {
                                at.insert(code.clone(), out.len());
                                out.push((code, w));
                            }
                        }
                    }
                }
                out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                if limit > 0 {
                    out.truncate(limit);
                }
                out
            }
        }
    }

    /// 遍历全部条目(供反查索引构建)。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        match self {
            Self::Mmap(reader) => reader.for_each_entry(f),
            Self::Memory(dict) => dict.for_each_entry(f),
            Self::Layered(layers) => layers.iter().for_each(|l| l.for_each_entry(f)),
        }
    }

    /// 构建反查索引:汉字/词 → 词库中的**全部**编码,按「码长升序→字典序升序」排列并去重。
    /// 供悬停 [编码] 段显示完整打法列表(如 `a/ab/abc`)与拼音编码提示(取末位=最长码,
    /// 全码最稳——简码可能被一级简码等占用)。取词库实际码,避免按字生成码却打不出的错配。
    pub fn build_reverse_index(&self) -> ReverseIndex {
        build_reverse_index_from(std::slice::from_ref(self))
    }

    /// 构建**单字全码表**：汉字 → 该字在本词库中的全码。供码表造词按 `[[encoder.rules]]`
    /// 公式组装词组编码（`wind_engine::encoder`）。
    ///
    /// # 全码判据（按序）
    ///
    /// 1. **上限闸**：滤掉码长 > `max_code_length` 的编码（`0` = 不设闸）。扩展词库塞进来的
    ///    5/6 码怪码在此被排除——否则它就是「最长码」，后续判据根本没机会参与。
    /// 2. **最长码长**：简码（如「工」=`a`）位数不够公式取第 2 码，必须取全码。
    /// 3. **权重降序**：同码长多码时取权重高者。
    /// 4. **码字典序升序**：最终确定性兜底。
    ///
    /// # 为什么第 4 条不是「首次出现」
    ///
    /// `CodetableDict::for_each_entry` 遍历的是 `HashMap`，**顺序不确定**；mmap 路径则为码
    /// 字典序。取「首次出现」会让同权同长的字在两条路径下、甚至两次构建间得到不同的码。
    /// 字典序是确定性代偿，且与 mmap 路径的天然顺序一致。
    pub fn build_single_char_full_codes(
        &self,
        max_code_length: usize,
    ) -> std::collections::HashMap<char, String> {
        build_single_char_full_codes_from(std::slice::from_ref(self), max_code_length)
    }

    /// 本词库**实际读取的文件**；内存模式无文件 → `None`。
    ///
    /// 供二级缓存（`.wridx` 反查索引）判定「我派生自哪几个文件、它们变了没有」。
    /// 取的是 reader 自报的路径而非调用方重新推导——`load_dicts_individually` 里
    /// wdat 路径的推导分了三个分支（普通 / english 小写 / rime_pinyin 合并），
    /// 在别处照抄一遍必然会有一天与它分叉。
    ///
    /// `None` 有明确后果：内存模式意味着这份词库根本没有稳定的磁盘产物，
    /// 以它为源的二级缓存不该落盘（见 `EngineManager::build_reverse_index_for`）。
    pub fn source_file(&self) -> Option<&Path> {
        match self {
            Self::Mmap(reader) => Some(reader.path()),
            Self::Memory(_) => None,
            // 不是单个文件：二级缓存的来源判定走逐库加载（`load_dicts_individually`），
            // 不经分层视图；这里按「没有稳定的单一产物」报，宁可不落盘也不报一半。
            Self::Layered(_) => None,
        }
    }

    /// 总条目数（mmap 下是编码数）。分层时是各层之和——同一编码在两层里各算一次，
    /// 只作日志 / 统计用，不要拿它判断词库内容。
    pub fn len(&self) -> usize {
        match self {
            Self::Mmap(reader) => reader.key_count() as usize,
            Self::Memory(dict) => dict.len(),
            Self::Layered(layers) => layers.iter().map(|l| l.len()).sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 从**多个词库**构建反查索引，等价于「先合并成一份再构建」，但**不产出中间文件**。
///
/// # 为什么等价（这条等价性是删掉 `combined.wdat` 的全部依据）
///
/// [`ReverseIndex::build`] 自己就会按 `(text, code)` 去重、并把权重取为该词全部条目的
/// **最大值**。这恰好覆盖了合并步骤所做的一切（按 code 聚合、同 text 保留更高权重）——
/// 于是「喂各库条目的原始并集」与「喂合并后的结果」产出的索引**逐位相同**。
///
/// 真机实测（feihuzj2，11 库 253 万条）：两条路径都得到 2519693 词、95.4 MB，
/// 而直接构建 1.85 秒、经 `combined.wdat` 需 30 秒建文件 + 3.2 秒建索引。
///
/// 各库的 wdat 由 [`crate::reader_pool`] 按路径共享，活着的引擎通常已持有同一批 reader，
/// 故这里的「逐库打开」几乎零成本：不重新解析、不新增映射。
pub fn build_reverse_index_from(dicts: &[CachedDict]) -> ReverseIndex {
    ReverseIndex::from_bytes(serialize_reverse_index_from(dicts))
}

/// 同 [`build_reverse_index_from`]，但停在**序列化镜像**这一步。
///
/// 分出这个入口是为了让调用方能「先落盘、再从盘上打开」——那条路径能把索引字节
/// 从进程私有内存里彻底移出去（mmap），而 [`build_reverse_index_from`] 得到的
/// 索引恒常驻。两者产出的字节**完全相同**，故行为无差。
pub fn serialize_reverse_index_from(dicts: &[CachedDict]) -> Vec<u8> {
    let mut pairs: Vec<(String, String, i32)> = Vec::new();
    for d in dicts {
        d.for_each_entry(&mut |code, text, weight| {
            pairs.push((text.to_string(), code.to_string(), weight));
        });
    }
    crate::reverseidx::serialize(pairs)
}

/// 从**多个词库**构建单字全码表。判据与单库版完全一致（见
/// [`CachedDict::build_single_char_full_codes`] 的四条），只是候选来自各库并集。
///
/// 与反查索引同理，判据「码长 → 权重 → 码字典序」对条目到达顺序不敏感，
/// 故并集与合并结果等价——这也是它能一起摆脱 `combined.wdat` 的原因。
pub fn build_single_char_full_codes_from(
    dicts: &[CachedDict],
    max_code_length: usize,
) -> std::collections::HashMap<char, String> {
    use std::collections::HashMap;
    // 值存 (code, weight)；weight 仅用于比较，不外传。
    let mut best: HashMap<char, (String, i32)> = HashMap::new();
    for d in dicts {
        d.for_each_entry(&mut |code, text, weight| {
            let mut it = text.chars();
            let (Some(ch), None) = (it.next(), it.next()) else {
                return; // 只收单字条目
            };
            let len = code.chars().count();
            if len == 0 || (max_code_length > 0 && len > max_code_length) {
                return;
            }
            match best.get(&ch) {
                Some((cur, cur_w)) => {
                    let cur_len = cur.chars().count();
                    let better = len > cur_len
                        || (len == cur_len
                            && (weight > *cur_w || (weight == *cur_w && code < cur.as_str())));
                    if better {
                        best.insert(ch, (code.to_string(), weight));
                    }
                }
                None => {
                    best.insert(ch, (code.to_string(), weight));
                }
            }
        });
    }
    best.into_iter().map(|(k, (code, _))| (k, code)).collect()
}

/// [`for_each_code_group`] 交出的一条：文本借自词库，另带排序与音节所需的原始字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupEntry<'a> {
    /// 在 `dicts` 里的下标（[`CachedDict::Layered`] 的子层都记外层下标）。
    pub layer: usize,
    pub text: &'a str,
    /// 词库里的原始权重（未经 `default_weight` / `weight_spec` 换算）。
    pub weight: i32,
    /// 写入时的自然序（同一词库内）。
    pub order: i32,
    /// 音节起始位 bitmask，见 [`crate::binformat::DictEntry::boundary`]；0 = 无边界信息。
    pub boundary: u64,
}

/// 一个词库按码升序逐组吐条目的拉式源。
enum GroupSource<'a> {
    Mmap(&'a WdatReader, crate::datformat::CodeCursor<'a>),
    Memory(Box<dyn Iterator<Item = (&'a str, &'a [crate::codetable::CodetableEntry])> + 'a>),
}

impl<'a> GroupSource<'a> {
    /// 取下一组：条目写进 `out`（先清空），返回码；源耗尽返回 `None`。
    fn next_group(&mut self, layer: usize, out: &mut Vec<GroupEntry<'a>>) -> Option<String> {
        out.clear();
        match self {
            Self::Mmap(r, cur) => {
                let (code, leaf) = cur.next()?;
                let r: &'a WdatReader = r;
                r.for_each_leaf_entry(leaf, &mut |text, weight, order, boundary| {
                    out.push(GroupEntry {
                        layer,
                        text,
                        weight,
                        order: order as i32,
                        boundary,
                    })
                });
                Some(code)
            }
            Self::Memory(it) => {
                let (code, es) = it.next()?;
                out.extend(es.iter().map(|e| GroupEntry {
                    layer,
                    text: &e.text,
                    weight: e.weight,
                    order: e.order,
                    boundary: e.boundary,
                }));
                Some(code.to_string())
            }
        }
    }
}

fn push_group_sources<'a>(
    layer: usize,
    d: &'a CachedDict,
    out: &mut Vec<(usize, GroupSource<'a>)>,
) {
    match d {
        CachedDict::Mmap(r) => out.push((layer, GroupSource::Mmap(r, r.codes()))),
        CachedDict::Memory(m) => out.push((layer, GroupSource::Memory(Box::new(m.groups())))),
        CachedDict::Layered(ls) => ls.iter().for_each(|l| push_group_sources(layer, l, out)),
    }
}

/// 多个词库**按码归并**遍历：同一编码在各库里的条目合成一组交给 `f`（库序在前者在前，
/// 库内保持词库原序），码按字节序升序。
///
/// 供词库统计这类「要看整组」的全表扫描用：重码、同码内序位都得先把一组收齐，而
/// [`CachedDict::for_each_entry`] 是逐条推、还丢了 `order` / `boundary`。各库本身就按码有序
/// （wdat DFS 与 `CodetableDict` 的 BTreeMap 都是字节序），这里只做 k 路归并，任意时刻只
/// 持有每库一组，不物化全表。
///
/// ⚠️ O(全表)，绝不能进按键链路。
pub fn for_each_code_group<'a>(
    dicts: &[&'a CachedDict],
    f: &mut dyn FnMut(&str, &[GroupEntry<'a>]),
) {
    let mut srcs: Vec<(usize, GroupSource<'a>)> = Vec::new();
    for (i, d) in dicts.iter().enumerate() {
        push_group_sources(i, d, &mut srcs);
    }
    // 每个源的当前组：(码, 条目)；码为 None 即该源已耗尽。条目 Vec 逐组复用。
    let mut heads: Vec<(Option<String>, Vec<GroupEntry<'a>>)> = srcs
        .iter_mut()
        .map(|(layer, s)| {
            let mut es = Vec::new();
            (s.next_group(*layer, &mut es), es)
        })
        .collect();
    if let [(code, es)] = heads.as_mut_slice() {
        // 单库（拼音合并主表的常态）：不必归并。
        let (layer, s) = &mut srcs[0];
        while let Some(c) = code.take() {
            f(&c, es);
            *code = s.next_group(*layer, es);
        }
        return;
    }
    let mut buf: Vec<GroupEntry<'a>> = Vec::new();
    while let Some(min_i) = heads
        .iter()
        .enumerate()
        .filter_map(|(i, (c, _))| c.as_deref().map(|c| (i, c)))
        .min_by(|a, b| a.1.cmp(b.1))
        .map(|(i, _)| i)
    {
        let Some(min) = heads[min_i].0.take() else {
            break;
        };
        buf.clear();
        for (i, (c, es)) in heads.iter_mut().enumerate() {
            if i != min_i && c.as_deref() != Some(min.as_str()) {
                continue;
            }
            buf.extend_from_slice(es);
            let (layer, s) = &mut srcs[i];
            *c = s.next_group(*layer, es);
        }
        f(&min, &buf);
    }
}

#[cfg(test)]
#[path = "cached_tests.rs"]
mod tests;
