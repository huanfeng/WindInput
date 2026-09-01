//! 求解率实测：**层 4 单独决策时**的成功率与正确率。
//!
//! 设计文档 `docs/design/pinyin-entry-boundary-contract.md` §9 第 2 项——
//! 「拿实测求解率决定 §5 的默认选项与文案」，这是整套契约里唯一需要真实数据的决策。
//!
//! # ★ 为什么必须绕开层 2，否则测出来的数字毫无意义
//!
//! 直接跑 `Engine::resolve_boundary` 会得到接近 100% 的漂亮结果，因为层 2 是**词典点查**，
//! 而样本取自词典 ⇒ 每条都能查到自己。那测的是「词典能不能查到自己」，不是我们要问的
//! 「用户导入一批**生词**时能不能解出边界」。
//!
//! 本文件因此直接调 [`generate::boundary_by_char_count`]（层 4）+ 多解时的
//! [`generate::generate_word_pinyin`]（层 3），复刻 `resolve_boundary` 去掉层 2 的部分。
//! ★ 这个替身是**精确**的而非近似：层 4 只用单字读音索引与音节 trie，**从不查这个词本身**，
//! 所以拿词典里的词喂它，与喂一个生词完全等价。
//!
//! # ★★ 正确率比成功率重要
//!
//! 「解出来但解错了」比「解不出来」危险得多：后者会被拒收并告知用户，前者静默入库，
//! 之后简拼索引、双拼校验全按错误切分工作，且**用户永远收不到任何信号**。
//! 所以本文件把「补齐正确率」单独列出，它才是「导入并由程序补充」该不该做默认项的判据。
//!
//! # 跑法
//!
//! ```text
//! cargo test -p wind-engine --test pinyin_boundary_solve_rate -- --ignored --nocapture
//! ```
//!
//! 抽样步长用环境变量 `SOLVE_RATE_STRIDE` 控制（默认 1 = 全量）。

use std::collections::HashMap;
use std::path::PathBuf;

use wind_dict::cached::CachedDict;
use wind_engine::pinyin::generate::{self, CharPinyinIndex};
use wind_engine::pinyin::syllable::SyllableTrie;

/// 定位方案实际使用的**合并词典** `rime_frost.merged.wdat`。
///
/// ★★ 必须用合并产物，不能用 `cn_dicts/base.dict.wdat`。踩过一次：base 只有词、**没有单字**，
/// 而 [`CharPinyinIndex`] 靠遍历标准音节收集单字读音 ⇒ 索引全空 ⇒ 判据①（每字都要有读音）
/// 全部失败 ⇒ 实测报出 **100% Unresolvable**。那不是实现的结论，是夹具喂错了词典。
/// 单字在 `cn_dicts/8105`、`cn_dicts/41448`，由 `rime_frost.dict.yaml` 的 `import_tables`
/// 合并进来；生产环境 `resolve_boundary` 拿到的正是这份合并 dict，故用它才等价于线上。
///
/// ⛔ **有意不设「找不到就退回 base」的兜底**：那会静默产出一组毫无意义的数字，
/// 而这正是本仓「词库测试假绿」的经典形态。找不到就直接失败。
fn merged_wdat() -> PathBuf {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let candidates = [
        PathBuf::from(&local).join("WindInputDev/cache/pinyin/rime_frost.merged.wdat"),
        PathBuf::from(&local).join("WindInput/cache/pinyin/rime_frost.merged.wdat"),
    ];
    for c in &candidates {
        if c.is_file() {
            return c.clone();
        }
    }
    panic!(
        "找不到合并词典 rime_frost.merged.wdat，试过：\n  {}\n\
         它由服务首次加载拼音方案时生成——先跑一次输入法（或部署 Dev 变体）再来跑本 eval。",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// 求解结果 + **是哪一层给出的**——分层计数要靠它，否则得把层 4 重跑一遍。
#[derive(Clone, Copy)]
enum Outcome {
    /// 层 4 直接唯一解。
    Unique(u64),
    /// 层 4 多解，层 3 确认了其中一条。
    Rescued(u64),
    /// 层 4 多解且层 3 没救回，附带按读音权重选出的首选。
    Ambiguous(u64),
    /// 切分已定、但 text 含无读音字符 ⇒ 读音表验证不了（照常入库）。
    /// 真实词库里这一档应当≈0：出现得多意味着夹具的单字读音表没建起来。
    NoReading(u64),
    /// 层 4 无解 ⇒ 契约判非法。
    Unresolvable,
}

/// 复刻 `PinyinEngine::resolve_boundary` 的**层 4 → 层 3** 部分（不含层 2 词典点查）。
fn resolve_without_layer_two(
    dict: &CachedDict,
    idx: &CharPinyinIndex,
    trie: &SyllableTrie,
    code: &str,
    text: &str,
) -> Outcome {
    let Some(sol) = generate::boundary_by_char_count(idx, trie, code, text) else {
        return Outcome::Unresolvable;
    };
    // 与生产代码同序：no_reading 先于 ambiguous 判（那一档的「已按读音权重择一」
    // 在读音表缺席时讲不通）。
    if sol.no_reading {
        return Outcome::NoReading(sol.mask);
    }
    if !sol.ambiguous {
        return Outcome::Unique(sol.mask);
    }
    // 层 3：仅在多解时出场，与生产代码同序。
    if let Some(spaced) = generate::generate_word_pinyin(dict, idx, text) {
        let (flat, derived) = wind_store::wdict::split_spaced_code(&spaced);
        if flat == code && derived != 0 {
            return Outcome::Rescued(derived);
        }
    }
    Outcome::Ambiguous(sol.mask)
}

#[derive(Default)]
struct Tally {
    /// 层 4 直接给出唯一解。
    unique_ok: usize,
    unique_wrong: usize,
    /// 层 4 多解、层 3 救回。
    rescued_ok: usize,
    rescued_wrong: usize,
    /// 层 4 多解、层 3 也没救回（Ambiguous），统计其首选是否正确。
    ambiguous_ok: usize,
    ambiguous_wrong: usize,
    /// text 含无读音字符 ⇒ 切分已定但验证不了（照常入库，故计入 `filled`）。
    no_reading_ok: usize,
    no_reading_wrong: usize,
    /// 层 4 无解 ⇒ 契约判非法、拒收。
    unresolvable: usize,
}

impl Tally {
    fn total(&self) -> usize {
        self.unique_ok
            + self.unique_wrong
            + self.rescued_ok
            + self.rescued_wrong
            + self.ambiguous_ok
            + self.ambiguous_wrong
            + self.no_reading_ok
            + self.no_reading_wrong
            + self.unresolvable
    }
    /// 「导入并由程序补充」实际能覆盖的面：层 4 唯一解 + 层 3 救回 + 读音验证缺席档。
    /// ⚠️ 不含 Ambiguous——那一档按契约不落库为确定值。
    fn filled(&self) -> usize {
        self.unique_ok
            + self.unique_wrong
            + self.rescued_ok
            + self.rescued_wrong
            + self.no_reading_ok
            + self.no_reading_wrong
    }
    fn filled_ok(&self) -> usize {
        self.unique_ok + self.rescued_ok + self.no_reading_ok
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 * 100.0 / d as f64
    }
}

#[test]
#[ignore = "eval：需本机已生成的 rime_frost.merged.wdat，手动跑 -- --ignored --nocapture"]
fn measure_layer_four_solve_rate() {
    // ⚠️ 缺数据当场 panic（见 [`merged_wdat`]），绝不静默跳过：本仓「词库测试静默跳过、
    // 计数照绿」已经骗过人一次，而 eval 是被显式调用的，跳过等于白跑一轮还留个绿。
    let wdat = merged_wdat();
    let reader = wind_dict::reader_pool::open_wdat(&wdat).expect("挂载合并词典失败");
    let dict = CachedDict::Mmap(reader);
    let idx = CharPinyinIndex::build(&dict);
    let trie = SyllableTrie::new();

    // ★ 前置断言：索引里必须真有单字读音。没有这条，喂错词典时实测会安静地报出
    // 「100% 无解」——一个看起来像结论、实则是夹具故障的数字。
    assert!(
        !dict.search("ai").is_empty() && !dict.search("hao").is_empty(),
        "词典 {} 查不到单音节单字条目，CharPinyinIndex 会是空的，实测结果无意义",
        wdat.display()
    );

    // 收集样本：code → 该 code 下的全部 text。一个 code 只点查一次真值。
    let mut by_code: HashMap<String, Vec<String>> = HashMap::new();
    dict.for_each_entry(&mut |code, text, _w| {
        // 只要多字词：单音节词在词库里 boundary 恒为 0（无空格可切），没有真值可比。
        if text.chars().count() < 2 || code.len() > 64 {
            return;
        }
        by_code
            .entry(code.to_string())
            .or_default()
            .push(text.to_string());
    });

    let stride: usize = std::env::var("SOLVE_RATE_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(1);

    let mut t = Tally::default();
    let mut no_truth = 0usize;
    let mut wrong_samples: Vec<(String, String, u64, u64)> = Vec::new();
    let mut unresolvable_samples: Vec<(String, String)> = Vec::new();

    let mut codes: Vec<&String> = by_code.keys().collect();
    codes.sort_unstable(); // 确定性：HashMap 顺序不稳，抽样必须可复现
    let started = std::time::Instant::now();

    for code in codes.into_iter().step_by(stride) {
        // 真值：词典里这条 code 的全部命中，按 text 取 boundary。
        let truth: HashMap<String, u64> = dict
            .search_with_boundary(code)
            .into_iter()
            .map(|h| (h.text, h.boundary))
            .collect();

        for text in &by_code[code] {
            let Some(&want) = truth.get(text) else {
                continue; // 该 text 未在精确查询里出现（简码/前缀条目），无真值
            };
            if want == 0 {
                no_truth += 1;
                continue; // 词库自己就没有边界信息，不能当 ground truth
            }
            let outcome = resolve_without_layer_two(&dict, &idx, &trie, code, text);
            let got = match outcome {
                Outcome::Unresolvable => {
                    t.unresolvable += 1;
                    if unresolvable_samples.len() < 30 {
                        unresolvable_samples.push((code.clone(), text.clone()));
                    }
                    continue;
                }
                Outcome::Unique(m) => {
                    if m == want {
                        t.unique_ok += 1
                    } else {
                        t.unique_wrong += 1
                    }
                    m
                }
                Outcome::Rescued(m) => {
                    if m == want {
                        t.rescued_ok += 1
                    } else {
                        t.rescued_wrong += 1
                    }
                    m
                }
                Outcome::Ambiguous(m) => {
                    if m == want {
                        t.ambiguous_ok += 1
                    } else {
                        t.ambiguous_wrong += 1
                    }
                    m
                }
                Outcome::NoReading(m) => {
                    if m == want {
                        t.no_reading_ok += 1
                    } else {
                        t.no_reading_wrong += 1
                    }
                    m
                }
            };
            // ★ 只收「真会被写进库」的错解：Ambiguous 一档按契约不落库为确定值，
            // 把它算进「补错」会高估危害面。
            let written = matches!(
                outcome,
                Outcome::Unique(_) | Outcome::Rescued(_) | Outcome::NoReading(_)
            );
            if written && got != want && wrong_samples.len() < 30 {
                wrong_samples.push((code.clone(), text.clone(), want, got));
            }
        }
    }

    let n = t.total();
    println!("\n===== 拼音词条边界求解率实测（层 4 单独决策）=====");
    println!(
        "步长 {stride}，耗时 {:.1}s",
        started.elapsed().as_secs_f64()
    );
    println!("样本（多字词 + 词库带真值 boundary）：{n}");
    println!("（跳过：词库自身 boundary==0 的多字词 {no_truth} 条，无真值可比）\n");

    println!(
        "层4 唯一解        {:>8}  ({:>5.2}%)  正确 {:>8} / 错误 {:>6}",
        t.unique_ok + t.unique_wrong,
        pct(t.unique_ok + t.unique_wrong, n),
        t.unique_ok,
        t.unique_wrong
    );
    println!(
        "层4 多解→层3 救回 {:>8}  ({:>5.2}%)  正确 {:>8} / 错误 {:>6}",
        t.rescued_ok + t.rescued_wrong,
        pct(t.rescued_ok + t.rescued_wrong, n),
        t.rescued_ok,
        t.rescued_wrong
    );
    println!(
        "仍多解 Ambiguous  {:>8}  ({:>5.2}%)  首选正确 {:>6} / 错误 {:>6}",
        t.ambiguous_ok + t.ambiguous_wrong,
        pct(t.ambiguous_ok + t.ambiguous_wrong, n),
        t.ambiguous_ok,
        t.ambiguous_wrong
    );
    println!(
        "读音验证缺席      {:>8}  ({:>5.2}%)  正确 {:>8} / 错误 {:>6}  ← 应≈0，多了说明夹具读音表没建起来",
        t.no_reading_ok + t.no_reading_wrong,
        pct(t.no_reading_ok + t.no_reading_wrong, n),
        t.no_reading_ok,
        t.no_reading_wrong
    );
    println!(
        "无解 Unresolvable {:>8}  ({:>5.2}%)  ← 契约按非法拒收",
        t.unresolvable,
        pct(t.unresolvable, n)
    );

    println!("\n--- 决策用的两个数 ---");
    println!(
        "可自动补齐率 = {:.2}%   （层4唯一解 + 层3救回）/ 样本",
        pct(t.filled(), n)
    );
    println!(
        "补齐正确率   = {:.4}%  正确 / 已补齐   ★ 这个数决定默认选项",
        pct(t.filled_ok(), t.filled())
    );

    if !wrong_samples.is_empty() {
        println!("\n--- 补错的样本（前 {}）---", wrong_samples.len());
        for (c, x, want, got) in &wrong_samples {
            println!("  {c:20} {x:12} 真值 {want:#014b}  解出 {got:#014b}");
        }
    }
    if !unresolvable_samples.is_empty() {
        println!(
            "\n--- 被判非法的样本（前 {}）★ 若这里出现正常词，说明判据过严 ---",
            unresolvable_samples.len()
        );
        for (c, x) in &unresolvable_samples {
            println!("  {c:20} {x}");
        }
    }
    println!("\n=====================================================\n");

    assert!(n > 0, "样本为 0，实测没有真正跑起来");
}
