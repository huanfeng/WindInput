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

/// 前缀查询中该条目是否该保留：词的音节数不超过 `max_syllables`。
///
/// 拼音「短输入音节数匹配」判据的词库层半边（策略与阈值在 `wind-engine` 的
/// `STRICT_SYLLABLE_MATCH_MAX`）。`max_syllables` 必须是输入自身的音节数，不能是「输入在这条
/// 候选切分下占几个音节」，否则 `xia` 会因 `xi|an` 被当成 2 音节放行整批词组。
///
/// `boundary == 0`（无边界信息）一律放行，交引擎层用 DAG 现切兜底（`effective_boundary`）；
/// 词库层只提前丢弃确定不合格的条目，免得它们白占 top-N 配额。
///
/// 刻意 `pub(crate)`：外部一律用 [`prefix_entry_keep`]。单用本函数分不开 `shen` 与 `sheng`
/// （音节数同为 1），关掉模糊音也照样出 `sheng` 的字。
pub(crate) fn prefix_syllable_keep(boundary: u64, max_syllables: u32) -> bool {
    boundary == 0 || boundary.count_ones() <= max_syllables
}

/// 前缀查询中该条目是否音节边界对齐：输入已完成的音节部分必须正好停在该条目的某个音节起点上，
/// 即补全只能从下一个音节开始，不能把当前音节继续拉长。
///
/// wdat 的 key 是扁平码，前缀匹配发生在字符域，`shen` 会召回 `sheng` 的字，绕过模糊音开关。
/// 判据取 `completed_len`（输入中已成完整音节的字节长度）而非击键长度，残码位因此自动放行：
///
/// | 输入 | `completed_len` | `shenme`(什么) | `sheng`(生) |
/// |---|---|---|---|
/// | `shen` | 4 | `0b10001` 保留 | `0b1` 丢弃 |
/// | `shenm` | 4 | 残码补全照常 | 不是前缀 |
/// | `fe`（不成音节）| 0 | 位 0 恒置位，全放行 | 放行 |
///
/// 代价：`yingu`→「因果」、`shaixu`→「筛选」同样被挡，须打完全拼。这是取舍不是算法缺陷，
/// 不要再找「只挡单字、放行词」的判据。依据见 docs/architecture/candidate-sorting-rules.md
/// 的「4.2 拼音引擎」词库层前缀判据一条。
///
/// 两处放行：`boundary == 0`（无边界信息，同 [`prefix_syllable_keep`] 降级给引擎层）；
/// `code_len <= completed_len`（不是补全，本判据不适用）。
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

/// 前缀查询条目的完整保留判据：音节数不超限（[`prefix_syllable_keep`]）且音节边界
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
/// 不能用 `Path::with_extension("wdat")`，那只会得到 `bar.dict.wdat`。本约定与 Go 版
/// `wdbOnlyInDir` 一致。供 wdat-only 分发模式定位用户投放的二进制词库，与缓存路径
/// （`EngineManager::cache_path`）无关。非 `.dict.yaml` / `.yaml` 结尾返回 `None`。
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
    /// reader 经 [`crate::reader_pool`] 按文件路径共享；用 `Arc` 而非独占，是为了让最后一个
    /// 持有者释放时 mmap 随之解除。
    Mmap(Arc<WdatReader>),
    /// 内存模式（首次加载或缓存写入失败）
    Memory(CodetableDict),
    /// 分层：首层是主库，其余是扩展库（拼音细胞词库），每层各有自己的缓存文件。
    ///
    /// 查询时逐层查、按单库的排序语义合并（见 [`merge_ranked`] / [`merge_exact`]），开关一张
    /// 扩展库不必重建主库那份几十 MB 的合并缓存。只由拼音引擎构建（码表的扩展库另有
    /// CompositeDict 分层）。
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
/// 不跨层去重：合并缓存同样保留重复项（下游引擎会去重），保留才能逐项一致，也免得把主库
/// 自身层内的重复项一并吞掉。
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
/// `order` 是「该词在自己 code 下的名次」，不能跨层直接比。合并缓存里的名次 = 层内名次 +
/// 其他层同 code 下排在它前面的条目数，后者必须用各层的精确查询去数：按音节截断的查询会滤掉
/// 同 code 的高权词，它们却照样占着合并缓存里的名次。
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
    /// 加载词典，自动使用 .wdat 缓存：缓存与源一致（内容指纹，见 `cache_fp`）时直接 mmap，
    /// 否则解析 yaml、写缓存后再 mmap。
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
    /// 名次表作为 wdat 的第二个源参与指纹；不在时按「缺失」算指纹、不赋权。
    ///
    /// 同一份英文词库的每个读盘点都要走这里，否则同一槽位有两种指纹，两份 wdat 来回重建。
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
        // wdat-only：用户只投放编译好的二进制词库、不带 yaml 源。必须抢在 cache_is_valid 之前
        // （无源可比，且指纹把「源缺失」当稳定事实，不会判失效），也必须抢在
        // `CodetableDict::load(yaml_path)?` 这个硬失败点之前。
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

        // `wdat_path` 是槽位，实际文件是 `<stem>-<指纹>.wdat`（内容寻址）。按槽位 single-flight：
        // 同一槽位常被多个方案引用，而混输子引擎的构建不经 ensure_loaded、不受 build_locks 保护，
        // 故须在此自锁。
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
        // 指纹是解析之前算的；解析期间源被保存，按旧指纹命名的文件就会装着新内容。
        if !crate::cache_fp::stamps_of(&sources).unchanged_since(&stamps_before) {
            warn!(
                "源 {} 在解析期间被改写，本次不落盘（内存模式），下次启动重来。",
                yaml_path.display()
            );
            return Ok(Self::Memory(dict));
        }

        // 确保缓存目录存在后写入 .wdat 缓存。目标是新文件名，不覆盖任何正被 mmap 的旧文件。
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

    /// 精确查找，并返回音节边界（[`DictHit::boundary`]）。
    ///
    /// 与 [`Self::search`] 并存：边界只对拼音有意义，`search` 的大量消费方无音节概念。
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

    /// 前缀查找，并返回音节边界。与 [`Self::search_prefix`] 并存，理由同
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

    /// 前缀查找（带边界），只取音节数不超过 `max_syllables` 且在 `completed_len` 处音节边界
    /// 对齐的条目。
    ///
    /// 拼音短输入严格档专用：两道过滤下推到词库层，top-N 直接是 N 条合格条目，而不是让注定被
    /// 丢弃的条目吃光配额。判据见 [`prefix_entry_keep`]，两个后端共用。
    /// `completed_len` 传 0 即退化为纯字符前缀匹配（见 [`prefix_syllable_aligned`]）。
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

    /// 是否存在严格长于 `prefix` 的编码（存在性判据，不物化候选）。
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
            // ⚠ 仅供拼音：拼音不走通配，这里只求不丢条目，排序按前缀查询的键，不复刻
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

    /// 简拼查找（声母缩写，如 `nh`）：返回该简拼对应的全拼码列表，已按权重降序、截断。
    /// 仅 wdat(Mmap) 的独立 AbbrevSection 支持；内存回退返回空。
    ///
    /// 返回码而不是词：调用方拿码去主表装配候选，得到真实的 code 与 boundary，词频才不会与
    /// 全拼输入分裂成两份计数。
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

    /// 构建反查索引:汉字/词 → 词库中的全部编码,按「码长升序→字典序升序」排列并去重。
    /// 供悬停 [编码] 段显示完整打法列表(如 `a/ab/abc`)与拼音编码提示(取末位=最长码,
    /// 全码最稳——简码可能被一级简码等占用)。取词库实际码,避免按字生成码却打不出的错配。
    pub fn build_reverse_index(&self) -> ReverseIndex {
        build_reverse_index_from(std::slice::from_ref(self))
    }

    /// 构建单字全码表：汉字 → 该字在本词库中的全码。供码表造词按 `[[encoder.rules]]`
    /// 公式组装词组编码（`wind_engine::encoder`）。
    ///
    /// 全码判据（按序）：
    /// 1. 滤掉码长 > `max_code_length` 的编码（`0` = 不设闸），否则扩展库的怪长码会成为「最长码」；
    /// 2. 最长码长（简码位数不够公式取码）；
    /// 3. 权重降序；
    /// 4. 码字典序升序。不能用「首次出现」：内存模式遍历 `HashMap`，顺序不确定。
    pub fn build_single_char_full_codes(
        &self,
        max_code_length: usize,
    ) -> std::collections::HashMap<char, String> {
        build_single_char_full_codes_from(std::slice::from_ref(self), max_code_length)
    }

    /// 本词库实际读取的文件；内存模式无文件 → `None`。
    ///
    /// 供二级缓存（`.wridx`）判定派生源。取 reader 自报的路径，不在别处重新推导 wdat 路径
    /// （`load_dicts_individually` 里分三个分支，照抄必然分叉）。`None` 意味着没有稳定的磁盘
    /// 产物，以它为源的二级缓存不该落盘（见 `EngineManager::build_reverse_index_for`）。
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

/// 从多个词库构建反查索引，等价于「先合并成一份再构建」，但不产出中间文件。
///
/// 等价性来自 [`ReverseIndex::build`] 自己按 `(text, code)` 去重并取最大权重，恰好覆盖合并
/// 步骤所做的一切，这是不再需要 `combined.wdat` 的依据。各库 wdat 经 [`crate::reader_pool`]
/// 共享，逐库打开几乎零成本。
pub fn build_reverse_index_from(dicts: &[CachedDict]) -> ReverseIndex {
    ReverseIndex::from_bytes(serialize_reverse_index_from(dicts))
}

/// 同 [`build_reverse_index_from`]，但停在序列化镜像这一步，让调用方能先落盘再 mmap 打开，
/// 把索引字节移出私有内存。两者产出的字节完全相同。
pub fn serialize_reverse_index_from(dicts: &[CachedDict]) -> Vec<u8> {
    let mut pairs: Vec<(String, String, i32)> = Vec::new();
    for d in dicts {
        d.for_each_entry(&mut |code, text, weight| {
            pairs.push((text.to_string(), code.to_string(), weight));
        });
    }
    crate::reverseidx::serialize(pairs)
}

/// 从多个词库构建单字全码表。判据与单库版一致（见 [`CachedDict::build_single_char_full_codes`]），
/// 且对条目到达顺序不敏感，故并集与合并结果等价。
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

/// 多个词库按码归并遍历：同一编码在各库里的条目合成一组交给 `f`（库序在前者在前，库内保持
/// 原序），码按字节序升序。
///
/// 供词库统计这类要看整组的全表扫描用（[`CachedDict::for_each_entry`] 逐条推且丢了
/// `order` / `boundary`）。各库本身按码有序，这里只做 k 路归并，不物化全表。
/// O(全表)，绝不能进按键链路。
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
