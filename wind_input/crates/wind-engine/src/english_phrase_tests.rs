use super::*;

/// 夹具走生产同一条 `push` + `finish`，不自己复制「≥2 个词才进索引」的判据或补排序，
/// 免得副本与 `build` 漂移。
fn idx(pairs: &[(&str, &str, i32)]) -> PhraseSegIndex {
    let mut b = PhraseSegBuilder::default();
    for (text, code, w) in pairs {
        b.push(code, text, *w);
    }
    PhraseSegIndex::from_image(b.finish())
}

impl PhraseSegIndex {
    /// 全部条目（按镜像序）。只给测试 / 基准里的「朴素全表扫」对照用。
    fn entries(&self) -> impl Iterator<Item = PhraseEntry> + '_ {
        (0..self.entry_count).map(|i| self.entry(i))
    }
}

fn texts(i: &PhraseSegIndex, input: &str) -> Vec<String> {
    i.search(&split_segments(input, '\''), 20)
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 真机量级（18 万条）下二分窗口与全表扫的耗时对比。手动跑：
/// `cargo test -p wind-engine --release --lib -- --ignored --nocapture bench_`
///
/// 窗口在个位微秒、全表扫约 2 ms（量级见 docs/architecture/engine-candidate-pipeline.md 的
/// 「6.1 词组分词」）。全表扫落在按键链路上，每多打一个字母就再付一次，这才是做二分的理由。
///
/// `#[ignore]`：它是基准不是判据，换机器数字就变。正确性由
/// `the_binary_search_window_returns_exactly_what_a_full_scan_would` 守，「有没有真的少扫」由
/// `the_first_word_window_is_exactly_the_prefix_block` 守。
#[test]
#[ignore = "基准，不参与常规回归"]
fn bench_window_vs_full_scan() {
    let mut b = PhraseSegBuilder::default();
    for i in 0..180_000u32 {
        let text = format!("word{i:06} beta gamma{i:04}");
        b.push(&format!("w{i}"), &text, (i % 1000) as i32);
    }
    let me = PhraseSegIndex::from_image(b.finish());
    let segs = split_segments("word0123'gam", PHRASE_SEPARATOR);
    let t0 = std::time::Instant::now();
    for _ in 0..200 {
        std::hint::black_box(me.search(&segs, 20));
    }
    let windowed = t0.elapsed() / 200;
    let t1 = std::time::Instant::now();
    for _ in 0..200 {
        let mut hits = 0usize;
        for e in me.entries() {
            if me.match_entry(&e, &segs).is_some() {
                hits += 1;
            }
        }
        std::hint::black_box(hits);
    }
    let full = t1.elapsed() / 200;
    println!(
        "窗口 {windowed:?} / 全表 {full:?}  窗口条目数={}",
        me.first_word_range(&segs[0]).len()
    );
}

/// 构建期间被 `invalidate` 的那份结果不许写回。
///
/// `get` 刻意在锁外构建（全表扫，持锁会堵住按键线路），「开始构建」到「写回」之间有一段长窗口；
/// 热摘词库落在窗口里时，按旧词库建好的索引若被写回，用户看到的就是「关了词库没反应」。
/// 引擎构造时 `prewarm` 后台建索引，这个窗口每次启动都存在。
///
/// 时序是摆明的：自定义一个遍历中途停下等信号的层，把「已读过旧词库、尚未写回」的瞬间固定住
/// （在调 `get` 之前同步是错的，构建根本没跨过失效点）。层内容用标志翻转模拟热摘而不真去
/// `unregister_layer`：`for_each_entry` 正持着 composite 的锁，主线程摘层会死锁。
///
/// 变异验证：删掉 `get` 里 `generation != started_at` 那段比对即红。
#[test]
fn a_build_that_started_before_invalidate_must_not_win() {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::{Mutex, mpsc};
    use wind_dict::{DictLayer, DictManager, LayerType};

    /// 测试层的共享状态：测试线程与层各持一份 `Arc`。
    struct Gate {
        entered: mpsc::Sender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
        /// 只让第一趟遍历停下来；重建那趟要直接走完。
        stopped_once: AtomicBool,
        /// 还吐不吐 `zzz zzz`。主线程在放行前翻成 false = 那本词库被热摘了。
        yields_zzz: AtomicBool,
    }

    struct FlakyLayer(std::sync::Arc<Gate>);

    impl DictLayer for FlakyLayer {
        fn name(&self) -> &str {
            "flaky"
        }
        fn layer_type(&self) -> LayerType {
            LayerType::System
        }
        fn search(&self, _code: &str, _limit: usize) -> Vec<Candidate> {
            Vec::new()
        }
        fn search_prefix(&self, _p: &str, _limit: usize) -> Vec<Candidate> {
            Vec::new()
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            // 先吐词条再停：反过来的话放行后读到的已是翻转后的标志，两种实现都绿。
            // 要照出缺陷，构建线程手上必须是旧视图。
            if self.0.yields_zzz.load(Relaxed) {
                f("zzz", "zzz zzz", 100);
            }
            f("aaa", "aaa bbb", 100);
            if !self.0.stopped_once.swap(true, Relaxed) {
                // 第一趟：告诉主线程「旧视图我已经读完了」，然后等它失效完再返回。
                self.0.entered.send(()).unwrap();
                self.0.resume.lock().unwrap().recv().unwrap();
            }
        }
    }

    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let gate = std::sync::Arc::new(Gate {
        entered: entered_tx,
        resume: Mutex::new(resume_rx),
        stopped_once: AtomicBool::new(false),
        yields_zzz: AtomicBool::new(true),
    });

    let dm = std::sync::Arc::new(DictManager::new());
    dm.register_layer(Box::new(FlakyLayer(std::sync::Arc::clone(&gate))));

    let lazy = std::sync::Arc::new(LazyPhraseIndex::new());
    let builder = {
        let (lazy, dm) = (std::sync::Arc::clone(&lazy), std::sync::Arc::clone(&dm));
        std::thread::spawn(move || lazy.get(&dm))
    };

    // 构建线程此刻停在 `for_each_entry` 的末尾，手上已是含 zzz 的旧视图。
    entered_rx.recv().expect("构建应已读完旧视图");
    gate.yields_zzz.store(false, Relaxed); // = 热摘掉那本词库
    lazy.invalidate();
    resume_tx.send(()).unwrap();

    builder.join().expect("构建线程");

    let zzz = |i: &PhraseSegIndex| -> Vec<String> {
        i.search(&split_segments("zzz", PHRASE_SEPARATOR), 10)
            .into_iter()
            .map(|c| c.text)
            .collect()
    };
    // 要紧的是别人读到的那份：缓存里不许躺着按旧词库建的索引。
    let after = zzz(&lazy.get(&dm));
    assert!(after.is_empty(), "缓存里躺着按旧词库建的索引：{after:?}");
    // 反面：主库那条必须还在，证明作废的是过时的那份，不是把功能连坐关掉了。
    assert_eq!(
        lazy.get(&dm)
            .search(&split_segments("aaa", PHRASE_SEPARATOR), 10)
            .len(),
        1,
        "主库词组不该受牵连"
    );
}

/// 索引侧与查询侧的小写化必须是同一个函数。
///
/// 判据非用希腊词尾 Σ 不可：`str::to_lowercase` 有上下文特例（`"ΟΔΟΣ"` → `"οδος"`），
/// 逐字符 `char::to_lowercase` 恒给 σ（`"οδοσ"`）；Café / Über 两种写法结果相同，照不出分叉。
///
/// 变异验证：把 `split_segments` 的 `.map(lower)` 换成 `.map(str::to_lowercase)`，本用例即红。
#[test]
fn both_sides_lowercase_the_same_way() {
    // 首词与非首词各放一个词尾 Σ，两条匹配规则都要覆盖。
    let i = idx(&[("ΟΔΟΣ ΑΘΗΝΑΣ", "odos", 100)]);
    assert_eq!(
        texts(&i, "οδο'αθη"),
        vec!["ΟΔΟΣ ΑΘΗΝΑΣ"],
        "首段与非首段都得能配上"
    );
    // 用户打的是大写，查询侧也得归一到同一形态。
    assert_eq!(texts(&i, "ΟΔΟ'ΑΘΗ"), vec!["ΟΔΟΣ ΑΘΗΝΑΣ"]);
    // 整词（词尾 Σ 就在段末）——这是两种写法真正分叉的位置。
    assert_eq!(texts(&i, "οδος'αθη"), vec!["ΟΔΟΣ ΑΘΗΝΑΣ"]);
}

/// 首词有公共前缀的一族 —— 二分边界最容易错的地方（`ip` 的区间必须刚好收住
/// `ipad`/`iphone`/`ipod`，既不漏 `ipod` 也不吃进 `internet`）。
fn prefix_family() -> PhraseSegIndex {
    idx(&[
        ("iPad Pro", "ipadpro", 100),
        ("iPhone 15 Pro Max", "iphone", 100),
        ("iPhone 15 Pro", "iphone", 90),
        ("iPod Touch", "ipod", 80),
        ("Internet Explorer", "ie", 70),
        ("Buenos Aires", "buenosaires", 60),
        ("Mac OS X", "macosx", 50),
        ("Zulu Time", "zulu", 40),
        ("北京 大学", "bjdx", 30),
    ])
}

/// 朴素全表扫，只回答「命中哪些」。
///
/// 刻意不复制排序逻辑（顺序由 `weight_outranks_span` 那几条守），用集合比对测「二分有没有漏/多」。
fn linear_hits(i: &PhraseSegIndex, input: &str) -> std::collections::BTreeSet<String> {
    let segs = split_segments(input, PHRASE_SEPARATOR);
    if segs.is_empty() {
        return Default::default();
    }
    i.entries()
        .filter(|e| i.match_entry(e, &segs).is_some())
        .map(|e| i.text(&e).unwrap().to_string())
        .collect()
}

fn search_hits(i: &PhraseSegIndex, input: &str) -> std::collections::BTreeSet<String> {
    i.search(&split_segments(input, PHRASE_SEPARATOR), 100)
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 二分窗口必须与全表扫召回同一批条目。
///
/// 变异验证：删掉 `finish()` 里的 `sort_by`（并去掉其末尾有序断言），本用例红在 `ip'pro` 上。
/// 有效性来自覆盖一整族前缀：`i'pro` 在未排序时窗口恰好仍然正确，只留它就成了假护栏。
#[test]
fn the_binary_search_window_returns_exactly_what_a_full_scan_would() {
    let i = prefix_family();
    for input in [
        "i'pro",     // 区间跨 ipad/iphone/ipod 三族
        "ip'pro",    //
        "ipa'pro",   // 只剩 iPad
        "iph'max",   // 只剩一条
        "ipo'touch", // 区间**右端**那条，最容易被上界切掉
        "int'exp",   // 区间左邻，不得被吃进来
        "b'air",     // 全表最前
        "z'time",    // 全表最后（ASCII 段）
        "北'大",     // 多字节首词，排在全部 ASCII 之后
        "zz'x",      // 首词无人匹配 ⇒ 空区间
        "'",         // 空段全被剔除 ⇒ 空结果
    ] {
        assert_eq!(
            search_hits(&i, input),
            linear_hits(&i, input),
            "输入 {input:?} 上二分窗口与全表扫不一致"
        );
    }
}

/// 窗口本身的边界：`ip` 收住三族、不吃 `internet`。
///
/// 上一条测「结果对不对」，这条测「少看了多少」：窗口退化成全表时结果照样正确，但本功能白做了。
#[test]
fn the_first_word_window_is_exactly_the_prefix_block() {
    let i = prefix_family();
    let win: std::collections::BTreeSet<String> = i
        .first_word_range("ip")
        .map(|k| i.text(&i.entry(k)).unwrap().to_string())
        .collect();
    assert_eq!(
        win,
        [
            "iPad Pro",
            "iPhone 15 Pro",
            "iPhone 15 Pro Max",
            "iPod Touch"
        ]
        .into_iter()
        .map(String::from)
        .collect::<std::collections::BTreeSet<_>>()
    );
    assert!(
        i.first_word_range("zz").is_empty(),
        "无人匹配的首词该给出空窗口"
    );
    // 空前缀 = 全表。这不是按键路径（`split_segments` 已滤掉空段，`segs[0]` 永不为空），
    // 留着是钉住 `partition_point` 在空前缀上的边界行为，免得被「优化」成返回空切片。
    assert_eq!(
        i.first_word_range("").len(),
        i.len(),
        "空前缀该给出全表窗口"
    );
}

/// 两种编码方案都靠 text 命中——这是整个设计的立足点。
#[test]
fn matches_both_encoding_schemes() {
    let i = idx(&[
        ("Buenos Aires", "buenosaires", 100), // 拼接式
        ("iPhone 15 Pro", "iphone", 100),     // 共同前缀式：code 里没有 15/Pro
    ]);
    assert_eq!(texts(&i, "bue'air"), vec!["Buenos Aires"]);
    assert_eq!(texts(&i, "ip'15"), vec!["iPhone 15 Pro"]);
}

/// 跳词：`ip'pro` 越过 `15` 命中 `Pro`（优先保效果）。
#[test]
fn skips_intervening_words() {
    let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
    assert_eq!(texts(&i, "ip'pro"), vec!["iPhone 15 Pro"]);
}

/// 跳词不等于乱序：段序必须与词序一致。
#[test]
fn skipping_still_requires_order() {
    let i = idx(&[("Mac OS X Snow Leopard", "macosx", 100)]);
    assert_eq!(texts(&i, "mac'snow"), vec!["Mac OS X Snow Leopard"]);
    // `leopard` 在 `snow` 之后，反过来打就不该命中。
    assert!(
        texts(&i, "mac'leo'snow").is_empty(),
        "段序与词序相反时不得命中"
    );
}

/// 首段锚定第一个词：中段词不能当入口。
#[test]
fn first_segment_must_anchor_the_first_word() {
    let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
    assert!(
        texts(&i, "pro'").is_empty(),
        "`pro` 不是首词前缀，不得从中段进入"
    );
}

/// 跨度是次级键：同权重时跳得少的排前面。
/// 两条 weight 必须相等才测得到跨度，否则主键就分出了胜负。
#[test]
fn tighter_span_breaks_ties_within_the_same_weight() {
    let i = idx(&[
        ("iPhone 15 Pro Max", "iphone", 100),
        ("iPhone Pro", "iphone", 100),
    ]);
    assert_eq!(
        texts(&i, "ip'pro"),
        vec!["iPhone Pro", "iPhone 15 Pro Max"],
        "同权重下跳 0 个词的应排在跳 1 个词的前面"
    );
}

/// 而 weight 是主键：权重更高的排前面，哪怕它跨度更大。
///
/// 协调器会按 weight 统一重排（AGENTS.md 硬约定），引擎内序若以跨度为主键，用户看到的就是另一个
/// 顺序。与上一条构成对照：缺了它，「跨度优先」的实现同样能过上一条。
#[test]
fn weight_outranks_span() {
    let i = idx(&[
        ("iPhone 15 Pro Max", "iphone", 900),
        ("iPhone Pro", "iphone", 10),
    ]);
    assert_eq!(
        texts(&i, "ip'pro"),
        vec!["iPhone 15 Pro Max", "iPhone Pro"],
        "weight 是主键：高权重的排前面，跨度只在同权重时才说话"
    );
}

/// weight 主键在同跨度的条目之间同样生效（与 `weight_outranks_span` 互补：那条跨度
/// 不同、这条跨度相同）。
#[test]
fn weight_orders_entries_of_equal_span() {
    let i = idx(&[
        ("iPhone 15 Pro", "iphone", 10),
        ("iPhone 16 Pro", "iphone", 900),
    ]);
    assert_eq!(
        texts(&i, "ip'pro"),
        vec!["iPhone 16 Pro", "iPhone 15 Pro"],
        "同跨度下按 weight 降序"
    );
}

/// 末尾空段被剔除：打到 `ip'` 的那一刻候选不该整片消失。
#[test]
fn trailing_empty_segment_is_dropped() {
    let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
    assert_eq!(split_segments("ip'", '\''), vec!["ip".to_string()]);
    assert_eq!(texts(&i, "ip'"), vec!["iPhone 15 Pro"]);
}

/// 单词条目不进索引——它们走 Trie 前缀匹配，不该在这里被重复召回。
#[test]
fn single_word_entries_are_not_indexed() {
    let i = idx(&[
        ("hello", "hello", 100),
        ("Buenos Aires", "buenosaires", 100),
    ]);
    assert_eq!(i.len(), 1);
}

/// 每条词组的堆开销上界，本模块唯一的内存护栏。
///
/// 用户自备词库可达 18 万条，且建两份（english 方案 + 混输的 english 子引擎），逐条独立分配
/// 时常驻数十 MB。上界取 120 字节/条：arena 版约 78，挡得住退回 `Box<str>`（`Vec` + 两个 `Box`
/// 的头部就 56 字节，加上每词一次分配的开销必然超线）。
///
/// 样本必须足够多且带多词条目：条数太少时 arena 扩容尾巴摊到分母上，测的就是扩容策略。
#[test]
fn heap_cost_per_phrase_stays_within_budget() {
    let pairs: Vec<(String, String, i32)> = (0..2000)
        .map(|i| {
            (
                format!("iPhone {i} Pro Max Ultra"),
                format!("iphone{i}"),
                100,
            )
        })
        .collect();
    let mut b = PhraseSegBuilder::default();
    for (text, code, w) in &pairs {
        b.push(code, text, *w);
    }
    // 走生产同一条收尾（`finish` → 镜像），别手抄，否则改布局时这里会静默地测一个不存在的形态。
    let me = PhraseSegIndex::from_image(b.finish());

    // 堵上面那条护栏的漏网口：`heap_bytes` 只统计镜像，往 `PhraseEntry` 里加回 `Box<str>` /
    // `Vec<Box<str>>` 不会被统计到。用 `size_of` 钉住「条目里不许出现指针」（编译期事实），
    // 也与镜像里一条记录的定长一致。
    assert!(
        size_of::<PhraseEntry>() <= ENTRY_SIZE,
        "PhraseEntry 涨到 {} 字节——加了指针字段？条目必须只存定长偏移",
        size_of::<PhraseEntry>()
    );
    assert_eq!(me.len(), 2000, "全部应进索引（每条 5 个词）");
    let per = me.heap_bytes() / me.len();
    // 预算按本夹具定：每条 5 个词（per≈102），生产表平均 2~3 词（per≈74）。卡 120 只剩 15% 余量，
    // 夹具字符串一改长就会假红，故留到 140；真正钉住结构的是上面的 `size_of`，本条只是粗筛。
    assert!(
        per < 140,
        "每条词组堆开销 {per} 字节，超出预算 140——退回 per-entry 堆分配了？\n             （总计 {} KB / {} 条，本夹具每条 5 个词）",
        me.heap_bytes() / 1024,
        me.len()
    );
}

/// 多字节字符不能把偏移算错：arena 存的是字节偏移，词首尾相接后切片边界必须落在字符边界上。
#[test]
fn multibyte_words_slice_on_char_boundaries() {
    let i = idx(&[("Café Noir Über", "cafe", 100)]);
    assert_eq!(texts(&i, "caf'noir"), vec!["Café Noir Über"]);
    // 小写化后 Ü → ü，段用小写打
    assert_eq!(texts(&i, "caf'üb"), vec!["Café Noir Über"]);
}

/// 段比词多时不命中——别让 `a'b'c` 匹配上只有两个词的条目。
#[test]
fn more_segments_than_words_never_matches() {
    let i = idx(&[("Buenos Aires", "buenosaires", 100)]);
    assert!(texts(&i, "bue'air'x").is_empty());
}

// ───────────────────────── 落盘 / mmap ─────────────────────────

/// 每个用例一个干净的临时目录（`TMPDIR` 下，进程号 + 用例名区分，可并行）。
fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_wphr_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 一次查询的完整可观测结果：顺序、text、code、weight、natural_order。
/// 对拍比的是这个而不是集合——堆版与 mmap 版连顺序都必须一致。
fn full(i: &PhraseSegIndex, input: &str) -> Vec<(String, String, i32, i32)> {
    i.search(&split_segments(input, PHRASE_SEPARATOR), 50)
        .into_iter()
        .map(|c| (c.text, c.code, c.weight, c.natural_order))
        .collect()
}

/// 对拍用的查询：共同前缀式与拼接式、多段、跳词、大小写、多字节、空段、边界。
const PROBES: &[&str] = &[
    "i'pro",
    "ip'pro",
    "IP'PRO",
    "ipa'pro",
    "iph'max",
    "iph'15'max",
    "ipo'touch",
    "int'exp",
    "b'air",
    "bue'air",
    "z'time",
    "北'大",
    "οδο'αθη",
    "οδος'αθη",
    "caf'üb",
    "zz'x",
    "'",
    "ip'",
    "ip''pro",
    "mac'snow",
    "mac'leo'snow",
    "a'b'c'd'e'f",
    "word0123'gam",
    "word1'b'g",
];

/// 对拍夹具：`prefix_family` 那一族 + 多字节 / 希腊词尾 Σ / 跳词。
fn probe_pairs() -> Vec<(String, String, i32)> {
    let mut v: Vec<(String, String, i32)> = [
        ("iPad Pro", "ipadpro", 100),
        ("iPhone 15 Pro Max", "iphone", 100),
        ("iPhone 15 Pro", "iphone", 90),
        ("iPhone Pro", "iphone", 90),
        ("iPod Touch", "ipod", 80),
        ("Internet Explorer", "ie", 70),
        ("Buenos Aires", "buenosaires", 60),
        ("Mac OS X Snow Leopard", "macosx", 50),
        ("Zulu Time", "zulu", 40),
        ("北京 大学", "bjdx", 30),
        ("ΟΔΟΣ ΑΘΗΝΑΣ", "odos", 20),
        ("Café Noir Über", "cafe", 10),
        ("hello", "hello", 100), // 单词，不进索引
    ]
    .iter()
    .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
    .collect();
    for i in 0..300u32 {
        v.push((
            format!("Word{i:04} Beta Gamma{i:03}"),
            format!("w{i}"),
            (i % 7) as i32,
        ));
    }
    v
}

/// `fp` 为空 = 进程内镜像；非空 = 落盘形态（头里带指纹）。
fn build_image(pairs: &[(String, String, i32)], fp: &str) -> Vec<u8> {
    let mut b = PhraseSegBuilder::default();
    for (t, c, w) in pairs {
        b.push(c, t, *w);
    }
    b.sort();
    b.into_image(fp)
}

const FP: &str = "0123456789abcdef";

/// 镜像写盘，再以「常驻读入」「mmap」两种方式打开。
/// 走生产的落盘写法（`write_image` 流式写文件），而不是把内存镜像原样倒进去——
/// 后者测不到「流式写出的字节与内存镜像一致」这一条。
fn open_both(pairs: &[(String, String, i32)], dir: &Path) -> (PhraseSegIndex, PhraseSegIndex) {
    let mut b = PhraseSegBuilder::default();
    for (t, c, w) in pairs {
        b.push(c, t, *w);
    }
    b.sort();
    let p = dir.join("t.wphr");
    write_atomically(&p, |w| b.write_image(FP, w)).unwrap();
    let resident = PhraseSegIndex::open(&p, FP, usize::MAX).unwrap();
    let mapped = PhraseSegIndex::open(&p, FP, 0).unwrap();
    (resident, mapped)
}

/// 堆版、常驻读入版、mmap 版对同一批查询逐条相同（含顺序、code、weight）。
///
/// 三者共用同一套查找代码，这条测「落盘—读回」链路没有走样（指纹位置、写盘、读回）。
/// 照不出对称的解码错误（`entry()` 读错字段时三版错得一样），那一类由
/// `matches_both_encoding_schemes` 等语义用例兜。
///
/// 变异验证：让 `write_atomically` 落盘前把首条记录的 `raw_start` 翻一位，本条红在 `b'air` 上。
#[test]
fn heap_resident_and_mapped_views_agree_on_every_probe() {
    let dir = tmp_dir("agree");
    let image = build_image(&probe_pairs(), "");
    let heap = PhraseSegIndex::from_image(image.clone());
    let (resident, mapped) = open_both(&probe_pairs(), &dir);

    assert!(
        mapped.mapped_bytes() > 0 && mapped.heap_bytes() == 0,
        "mmap 不该计入堆"
    );
    assert!(resident.mapped_bytes() == 0 && resident.heap_bytes() >= image.len());
    assert_eq!(heap.len(), 312, "300 条合成 + 12 条词组，单词 hello 不进");
    let mut nonempty = 0;
    for q in PROBES {
        let want = full(&heap, q);
        nonempty += usize::from(!want.is_empty());
        assert_eq!(
            full(&resident, q),
            want,
            "常驻读入版在 {q:?} 上与堆版不一致"
        );
        assert_eq!(full(&mapped, q), want, "mmap 版在 {q:?} 上与堆版不一致");
    }
    // 防假绿：探针得真的命中东西，全空的对拍什么也证明不了。
    assert!(nonempty >= 15, "探针命中太少（{nonempty}），对拍失去意义");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 真机量级（18 万条）下堆版与 mmap 版对拍。
///
/// 合成词库模仿真实词库的两个特征：两种编码方案并存（拼接式 / 共同前缀式），以及首词大量共享前缀
/// （二分窗口里有成百上千条，跨度与 weight 的排序真正起作用）。
#[test]
fn heap_and_mapped_agree_on_a_180k_synthetic_dictionary() {
    let dir = tmp_dir("agree180k");
    let pairs = synthetic_pairs(180_000);
    let image = build_image(&pairs, "");
    let heap = PhraseSegIndex::from_image(image.clone());
    let (_, mapped) = open_both(&pairs, &dir);
    assert_eq!(heap.len(), 180_000);
    assert!(mapped.heap_bytes() == 0 && mapped.mapped_bytes() == image.len());
    for q in [
        "alpha'be",
        "alp'b'g",
        "Alpha0'Be",
        "be'ga",
        "gam'del",
        "zeta'e",
        "alpha1234'beta",
        "omega9'x",
        "alpha'zz",
        "q'q",
        "delta'",
    ] {
        assert_eq!(full(&mapped, q), full(&heap, q), "18 万条下 {q:?} 不一致");
    }
    assert!(
        !full(&heap, "alp'b'g").is_empty(),
        "防假绿：主力探针必须有命中"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 18 万条合成词组（探针与 `wind-engine` 的内存探针同一个生成器思路）。
fn synthetic_pairs(n: u32) -> Vec<(String, String, i32)> {
    const HEADS: [&str; 8] = [
        "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Omega", "Sigma",
    ];
    (0..n)
        .map(|i| {
            let h = HEADS[(i % 8) as usize];
            let text = match i % 3 {
                0 => format!("{h}{} Beta Gamma{}", i % 5000, i % 97),
                1 => format!("{h}{} Beta", i % 5000),
                _ => format!("{h}{} Delta Epsilon Zeta{}", i % 5000, i),
            };
            // 偶数条拼接式、奇数条共同前缀式（code 只有首词）。
            let code = if i % 2 == 0 {
                text.to_ascii_lowercase().replace(' ', "")
            } else {
                text.split(' ').next().unwrap().to_ascii_lowercase()
            };
            (text, code, (i % 1000) as i32)
        })
        .collect()
}

/// 截断 / magic / 版本 / 指纹不符：`open` 一律 `Err`（调用方据此重建），不 panic。
#[test]
fn open_rejects_truncated_or_foreign_files() {
    let dir = tmp_dir("reject");
    let image = build_image(&probe_pairs(), FP);
    let p = dir.join("x.wphr");
    let try_open = |bytes: &[u8]| {
        std::fs::write(&p, bytes).unwrap();
        PhraseSegIndex::open(&p, FP, 0).map(|_| ())
    };
    assert!(try_open(&image).is_ok(), "前提：完好的文件能打开");
    for cut in [
        0,
        10,
        HEADER_SIZE - 1,
        HEADER_SIZE,
        image.len() / 2,
        image.len() - 1,
    ] {
        assert!(try_open(&image[..cut]).is_err(), "截断到 {cut} 字节应被拒");
    }
    let mut bad = image.clone();
    bad[0] = b'X';
    assert!(try_open(&bad).is_err(), "magic 不符应被拒");
    let mut bad = image.clone();
    bad[4] = 99;
    assert!(try_open(&bad).is_err(), "版本不符应被拒");
    std::fs::write(&p, &image).unwrap();
    assert!(
        PhraseSegIndex::open(&p, "fedcba9876543210", 0).is_err(),
        "指纹不符应被拒（改名 / 拷错的文件不能当命中）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 段内被写坏（头部完好、偏移乱指）：查询不 panic，最多查不中。
/// 头部校验只验各段整体在界内，挡不住这一类；按键链路上的 `convert` 不许 panic。
#[test]
fn scribbled_entries_never_panic() {
    let image = build_image(&probe_pairs(), "");
    // 确定性「随机」：线性同余，每 7 字节翻一次，覆盖 entries / word_ends / arena 全段。
    let mut seed = 0x2545_f491_u32;
    for round in 0..20 {
        let mut bad = image.clone();
        for b in bad[HEADER_SIZE..].iter_mut().step_by(7 + round) {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *b ^= (seed >> 16) as u8;
        }
        let i = PhraseSegIndex::parse(ImageData::Owned(bad)).expect("头部没动，应能解析");
        for q in PROBES {
            let _ = full(&i, q);
        }
    }

    // 写坏的 `word_count` 不许把跳词循环拖成几十亿步：首条词数改成 u32::MAX、段数 2，
    // 无守卫时要空转 ~4e9 次。变异验证：删掉 `match_entry` 里「词表越界」守卫，本条红在耗时上。
    let mut bad = image.clone();
    bad[HEADER_SIZE + 20..HEADER_SIZE + 24].copy_from_slice(&u32::MAX.to_le_bytes());
    let i = PhraseSegIndex::parse(ImageData::Owned(bad)).unwrap();
    let t0 = std::time::Instant::now();
    let first = i.entry(0);
    assert!(
        i.match_entry(&first, &split_segments("b'zzz", '\''))
            .is_none()
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "损坏的词数让匹配空转了 {:?}",
        t0.elapsed()
    );
}

// ── LazyPhraseIndex 的落盘接线 ──────────────────────────────────

/// 测试层：内容、摘要、启用标志都可控；`for_each_entry` 计次。
struct DigestLayer {
    name: String,
    entries: Vec<(String, String, i32)>,
    enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `None` = 说不清摘要（模拟内存词库）。
    has_digest: bool,
}

impl DigestLayer {
    fn new(name: &str, entries: &[(&str, &str, i32)]) -> Self {
        Self {
            name: name.into(),
            entries: entries
                .iter()
                .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
                .collect(),
            enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            has_digest: true,
        }
    }
}

impl wind_dict::DictLayer for DigestLayer {
    fn name(&self) -> &str {
        &self.name
    }
    fn layer_type(&self) -> wind_dict::LayerType {
        wind_dict::LayerType::System
    }
    fn search(&self, _code: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }
    fn search_prefix(&self, _p: &str, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }
    fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        for (t, c, w) in &self.entries {
            f(c, t, *w);
        }
    }
    fn entries_digest(&self) -> Option<String> {
        self.has_digest.then(|| format!("{}|v1", self.name))
    }
    fn enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }
    fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Relaxed);
    }
}

const MAIN: &[(&str, &str, i32)] = &[
    ("Buenos Aires", "buenosaires", 60),
    ("Zulu Time", "zulu", 40),
];
const EXT: &[(&str, &str, i32)] = &[
    ("iPhone 15 Pro Max", "iphone", 100),
    ("iPod Touch", "ipod", 80),
];

fn two_layer_dm() -> DictManager {
    let dm = DictManager::new();
    dm.register_layer(Box::new(DigestLayer::new("main", MAIN)));
    dm.register_layer(Box::new(DigestLayer::new("ext", EXT)));
    dm
}

fn lazy_texts(lazy: &LazyPhraseIndex, dm: &DictManager, input: &str) -> Vec<String> {
    lazy.get(dm)
        .search(&split_segments(input, PHRASE_SEPARATOR), 20)
        .into_iter()
        .map(|c| c.text)
        .collect()
}

fn wphr_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().is_some_and(|x| x == CACHE_EXT));
    v.sort();
    v
}

/// 第二个实例（= 下次启动）命中盘上那份，一次全表扫都不做，结果与首建相同。
/// 变异验证：删掉 `load_or_build` 的 ① 整段（每次都重扫），本条红在 `full_scans() == 0` 上。
#[test]
fn a_second_instance_reuses_the_cache_without_scanning() {
    let dir = tmp_dir("hit");
    let dm = two_layer_dm();
    let first = LazyPhraseIndex::with_cache_dir(Some(dir.clone())).with_resident_max(0);
    let want = lazy_texts(&first, &dm, "ip'max");
    assert_eq!(want, vec!["iPhone 15 Pro Max"]);
    assert_eq!(first.full_scans(), 1);
    assert_eq!(wphr_files(&dir).len(), 1, "首建应落盘一份");
    // 首建之后就是从盘上打开的那份：mmap，堆为 0。
    assert_eq!(first.heap_bytes_if_built(), Some(0));
    assert!(first.mapped_bytes_if_built().unwrap() > 0);

    let second = LazyPhraseIndex::with_cache_dir(Some(dir.clone())).with_resident_max(0);
    assert_eq!(lazy_texts(&second, &dm, "ip'max"), want);
    assert_eq!(second.full_scans(), 0, "命中缓存不该再全表扫");
    assert_eq!(lazy_texts(&second, &dm, "bue'air"), vec!["Buenos Aires"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 小镜像（不超过阈值）读进内存、不 mmap——与 `.wridx` 的 3 MB 分界同一取舍。
#[test]
fn a_small_cache_is_read_into_memory() {
    let dir = tmp_dir("small");
    let dm = two_layer_dm();
    let _ = lazy_texts(
        &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
        &dm,
        "ip",
    );
    let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
    assert_eq!(lazy.full_scans(), 0);
    assert_eq!(
        lazy.mapped_bytes_if_built(),
        Some(0),
        "几百字节的镜像不该 mmap"
    );
    assert!(lazy.heap_bytes_if_built().unwrap() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 热摘词库：按新的启用集合取另一份，不读回旧的；再启用回来直接复用先前那份。
/// 变异验证：删掉 `CompositeDict::entries_digest` 的 `.filter(|l| l.enabled())`，本条红在
/// 「关掉 ext 后不该再召回 iPhone」上。
#[test]
fn toggling_a_dictionary_switches_caches_and_switching_back_reuses() {
    let dir = tmp_dir("toggle");
    let dm = two_layer_dm();
    let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
    assert_eq!(lazy.full_scans(), 1);

    assert!(dm.set_layer_enabled("ext", false));
    lazy.invalidate();
    assert!(
        lazy_texts(&lazy, &dm, "ip'max").is_empty(),
        "关掉 ext 后不该再召回它的词组（读回了旧缓存？）"
    );
    assert_eq!(lazy_texts(&lazy, &dm, "bue'air"), vec!["Buenos Aires"]);
    assert_eq!(lazy.full_scans(), 2, "新集合第一次要扫一遍");
    assert_eq!(wphr_files(&dir).len(), 2, "两个集合各一份");

    assert!(dm.set_layer_enabled("ext", true));
    lazy.invalidate();
    assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
    assert_eq!(lazy.full_scans(), 2, "切回原集合应复用先前那份，不再扫");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 词库内容变了（摘要变）⇒ 换一份重建，不复用旧内容。
///
/// 真实词库的摘要来自 wdat 的 `.fp` 或 `大小:mtime`（`SystemDictLayer::entries_digest`，wind-dict
/// 另有用例钉住）；这里钉「摘要一变，这一层就不再读回旧文件」。
#[test]
fn a_changed_dictionary_is_rebuilt() {
    let dir = tmp_dir("changed");
    let dm = DictManager::new();
    let layer = DigestLayer::new("main", MAIN);
    let version = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1));
    // 版本号放在共享原子里，层注册进去之后仍能从外面改。
    struct Shared(DigestLayer, std::sync::Arc<std::sync::atomic::AtomicU32>);
    impl wind_dict::DictLayer for Shared {
        fn name(&self) -> &str {
            self.0.name()
        }
        fn layer_type(&self) -> wind_dict::LayerType {
            self.0.layer_type()
        }
        fn search(&self, c: &str, l: usize) -> Vec<Candidate> {
            self.0.search(c, l)
        }
        fn search_prefix(&self, p: &str, l: usize) -> Vec<Candidate> {
            self.0.search_prefix(p, l)
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            self.0.for_each_entry(f);
            if self.1.load(Relaxed) >= 2 {
                f("newp", "New Phrase", 1);
            }
        }
        fn entries_digest(&self) -> Option<String> {
            Some(format!("main|v{}", self.1.load(Relaxed)))
        }
    }
    dm.register_layer(Box::new(Shared(layer, std::sync::Arc::clone(&version))));

    let a = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    assert!(lazy_texts(&a, &dm, "new'p").is_empty());

    version.store(2, Relaxed); // 词库被改过
    let b = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    assert_eq!(lazy_texts(&b, &dm, "new'p"), vec!["New Phrase"]);
    assert_eq!(b.full_scans(), 1, "内容变了必须重建");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 盘上那份被截断 / 写坏 ⇒ 重建并覆盖，不 panic；覆盖后下一次又能命中。
#[test]
fn a_corrupt_cache_file_is_rebuilt_and_overwritten() {
    let dir = tmp_dir("corrupt");
    let dm = two_layer_dm();
    let _ = lazy_texts(
        &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
        &dm,
        "ip",
    );
    let files = wphr_files(&dir);
    assert_eq!(files.len(), 1);
    for garbage in [
        &b"WPHR\x01\x00"[..],
        &b"not a phrase index at all, just junk......"[..],
    ] {
        std::fs::write(&files[0], garbage).unwrap();
        let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        assert_eq!(lazy.full_scans(), 1, "坏文件应触发重建");
        let again = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&again, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        assert_eq!(again.full_scans(), 0, "重建应已覆盖坏文件");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 写盘失败（缓存目录不可建）⇒ 退回堆上建，结果照常。
#[test]
fn a_write_failure_falls_back_to_the_heap() {
    let dir = tmp_dir("nowrite");
    let blocker = dir.join("i_am_a_file");
    std::fs::write(&blocker, b"x").unwrap();
    let dm = two_layer_dm();
    // 目录路径实际是个普通文件 ⇒ create_dir_all 失败。
    let lazy = LazyPhraseIndex::with_cache_dir(Some(blocker.join("sub")));
    assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
    assert!(lazy.heap_bytes_if_built().unwrap() > 0, "应退回常驻堆");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 有层说不清摘要（内存词库）⇒ 不落盘：没有稳定产物可比，写下来的缓存无从校验。
#[test]
fn an_undigestable_layer_disables_persistence() {
    let dir = tmp_dir("nodigest");
    let dm = DictManager::new();
    let mut l = DigestLayer::new("mem", MAIN);
    l.has_digest = false;
    dm.register_layer(Box::new(l));
    let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    assert_eq!(lazy_texts(&lazy, &dm, "bue'air"), vec!["Buenos Aires"]);
    assert!(wphr_files(&dir).is_empty(), "说不清摘要时不该落盘");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 扫描与启停交错：扫到的内容对不上扫描前取的指纹 ⇒ 不落盘。
///
/// 否则按新集合扫出的内容会躺进以旧集合命名的文件，跨重启复用后读到错的词组。时序是摆明的：
/// 被扫的层吐完词条后亲手把另一层停掉。
///
/// 变异验证：删掉 `load_or_build` ③ 那段指纹复核，本条红（落了一份文件）。
#[test]
fn a_scan_that_races_a_toggle_is_not_persisted() {
    struct Toggler {
        inner: DigestLayer,
        other: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl wind_dict::DictLayer for Toggler {
        fn name(&self) -> &str {
            self.inner.name()
        }
        fn layer_type(&self) -> wind_dict::LayerType {
            self.inner.layer_type()
        }
        fn search(&self, c: &str, l: usize) -> Vec<Candidate> {
            self.inner.search(c, l)
        }
        fn search_prefix(&self, p: &str, l: usize) -> Vec<Candidate> {
            self.inner.search_prefix(p, l)
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            self.inner.for_each_entry(f);
            self.other.store(false, Relaxed); // 扫描进行中，另一本被停用
        }
        fn entries_digest(&self) -> Option<String> {
            self.inner.entries_digest()
        }
    }
    let dir = tmp_dir("race");
    let dm = DictManager::new();
    let ext = DigestLayer::new("ext", EXT);
    let ext_flag = std::sync::Arc::clone(&ext.enabled);
    dm.register_layer(Box::new(Toggler {
        inner: DigestLayer::new("main", MAIN),
        other: ext_flag,
    }));
    dm.register_layer(Box::new(ext));
    let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    let _ = lazy.get(&dm);
    assert!(
        wphr_files(&dir).is_empty(),
        "扫描期间启用集合变了，这份内容不能以扫描前的指纹落盘"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 同目录只留最近 [`PHRASE_INDEX_CACHE_KEEP`] 份 `phrase-*.wphr`，别的文件不碰。
#[test]
fn old_caches_are_pruned_but_foreign_files_are_left_alone() {
    let dir = tmp_dir("prune");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for k in 0..6 {
        let p = dir.join(format!("phrase-old{k}.{CACHE_EXT}"));
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(old + std::time::Duration::from_secs(k))
            .unwrap();
    }
    std::fs::write(dir.join("english.wdat"), b"keep me").unwrap();
    let dm = two_layer_dm();
    let _ = lazy_texts(
        &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
        &dm,
        "ip",
    );
    let left = wphr_files(&dir);
    assert_eq!(
        left.len(),
        PHRASE_INDEX_CACHE_KEEP,
        "应只留 {PHRASE_INDEX_CACHE_KEEP} 份：{left:?}"
    );
    // 留下的是新建那份 + 最近的三份旧的（old5/old4/old3）。
    assert!(
        left.iter()
            .any(|p| p.ends_with(format!("phrase-old5.{CACHE_EXT}")))
    );
    assert!(
        !left
            .iter()
            .any(|p| p.ends_with(format!("phrase-old0.{CACHE_EXT}")))
    );
    assert!(dir.join("english.wdat").exists(), "别的缓存产物不许碰");
    let _ = std::fs::remove_dir_all(&dir);
}

/// wdat 换了内容、`.fp` 却没跟上（写 `.fp` 失败）：词组索引必须重建，不得召回已没有的词组。
///
/// `SystemDictLayer` 给词组索引的摘要另混入 wdat 自身的 `大小:mtime`；只混这一处，不动
/// `.wridx` 共用的 `cache_digest`（那会让全体用户的反查索引白重建一次）。
///
/// 变异验证：去掉 `entries_digest` 里那段 `大小:mtime`，本条红在 `ip'max` 仍召回 iPhone 上。
#[test]
fn a_rewritten_wdat_with_a_stale_fp_is_rebuilt() {
    use wind_dict::SystemDictLayer;
    use wind_dict::cached::CachedDict;
    let dir = tmp_dir("stalefp");
    // `en.wdat` 是槽位；实际文件名带内容指纹（`cache_fp` S2），建好之后才知道，
    // 用 `source_file()` 取——本用例模拟的是「实际文件被外部原地改写」的现场。
    let (yaml, slot, cache) = (dir.join("en.dict.yaml"), dir.join("en.wdat"), dir.join("c"));
    std::fs::write(
        &yaml,
        "---\nname: en\n...\nBuenos Aires\tbuenosaires\t60\niPhone 15 Pro Max\tiphone\t100\n",
    )
    .unwrap();
    let wdat;
    {
        let d = CachedDict::load_at_with(&yaml, &slot, true).unwrap();
        wdat = d.source_file().expect("应走 mmap 缓存").to_path_buf();
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(d, "en")));
        let lazy = LazyPhraseIndex::with_cache_dir(Some(cache.clone()));
        assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
    }
    let mut fp = wdat.clone().into_os_string();
    fp.push(".fp");
    // wdat 被换成不含 iPhone 的版本，`.fp` 仍是旧的。
    let stale = std::fs::read(&fp).unwrap();
    let mut d = wind_dict::codetable::CodetableDict::empty();
    d.merge_single("buenosaires".into(), "Buenos Aires".into(), 60, 0);
    let mut w = wind_dict::datformat::WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(&wdat).unwrap();
    std::fs::write(&fp, &stale).unwrap();

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Mmap(wind_dict::reader_pool::open_wdat(&wdat).unwrap()),
        "en",
    )));
    let lazy = LazyPhraseIndex::with_cache_dir(Some(cache.clone()));
    assert!(
        lazy_texts(&lazy, &dm, "ip'max").is_empty(),
        "新 wdat 里已没有 iPhone，却从旧索引召回了它"
    );
    assert_eq!(lazy.full_scans(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 崩溃留下的 `phrase-*.tmp`：超过 1 小时的清掉，新鲜的（可能正被另一实例写）不碰，
/// 别人的 `.tmp` 不碰。
#[test]
fn stale_temp_files_from_a_crash_are_cleaned() {
    let dir = tmp_dir("tmpclean");
    let set_age = |p: &Path, secs: u64| {
        std::fs::write(p, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
            .unwrap();
    };
    let old = dir.join(format!("phrase-aaaa.{CACHE_EXT}.999-0.tmp"));
    let fresh = dir.join(format!("phrase-bbbb.{CACHE_EXT}.999-1.tmp"));
    let foreign = dir.join("other.tmp");
    set_age(&old, 2 * 3600);
    set_age(&fresh, 10);
    set_age(&foreign, 2 * 3600);
    let _ = lazy_texts(
        &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
        &two_layer_dm(),
        "ip",
    );
    assert!(!old.exists(), "超过 1 小时的崩溃残留应被清掉");
    assert!(fresh.exists(), "新鲜的临时文件可能正被别的实例写，不许碰");
    assert!(foreign.exists(), "别人的 .tmp 不许碰");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 扫描期间启用集合「关了又开」（ABA）：前后指纹相同但扫到的是中间态，不许落盘——
/// 否则它躺在以完整集合命名的文件里，被关过一瞬的那本词库的词组从此消失。
///
/// 判据靠失效代号：扫描期间每次启停都伴随 `invalidate`，代号一变就不落盘。
/// 变异验证：删掉 `load_or_build` 里落盘前的代号比对，本条红（`ip'max` 召回为空）。
#[test]
fn a_scan_that_sees_an_off_then_on_toggle_is_not_persisted() {
    use std::sync::atomic::AtomicBool;
    struct Hook {
        inner: DigestLayer,
        run: Box<dyn Fn() + Send + Sync>,
    }
    impl wind_dict::DictLayer for Hook {
        fn name(&self) -> &str {
            self.inner.name()
        }
        fn layer_type(&self) -> wind_dict::LayerType {
            self.inner.layer_type()
        }
        fn search(&self, c: &str, l: usize) -> Vec<Candidate> {
            self.inner.search(c, l)
        }
        fn search_prefix(&self, p: &str, l: usize) -> Vec<Candidate> {
            self.inner.search_prefix(p, l)
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            self.inner.for_each_entry(f);
            (self.run)();
        }
        fn entries_digest(&self) -> Option<String> {
            self.inner.entries_digest()
        }
    }
    let dir = tmp_dir("aba");
    let lazy = Arc::new(LazyPhraseIndex::with_cache_dir(Some(dir.clone())));
    let ext = DigestLayer::new("ext", EXT);
    let flag = Arc::clone(&ext.enabled);
    let fired = Arc::new(AtomicBool::new(false));
    let dm = DictManager::new();
    // 层序：off（main 之后关 ext）→ ext（被跳过）→ on（再开回来）。只在第一趟扫描动手。
    let (l1, f1, fl1) = (Arc::clone(&lazy), Arc::clone(&fired), Arc::clone(&flag));
    dm.register_layer(Box::new(Hook {
        inner: DigestLayer::new("main", MAIN),
        run: Box::new(move || {
            if !f1.load(Relaxed) {
                fl1.store(false, Relaxed);
                l1.invalidate();
            }
        }),
    }));
    dm.register_layer(Box::new(ext));
    let (l2, f2, fl2) = (Arc::clone(&lazy), Arc::clone(&fired), Arc::clone(&flag));
    dm.register_layer(Box::new(Hook {
        inner: DigestLayer::new("zz", &[]),
        run: Box::new(move || {
            if !f2.swap(true, Relaxed) {
                fl2.store(true, Relaxed);
                l2.invalidate();
            }
        }),
    }));
    assert_eq!(
        lazy_texts(&lazy, &dm, "ip'max"),
        vec!["iPhone 15 Pro Max"],
        "重试读回了扫描中间态落的盘"
    );
    let again = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
    assert_eq!(lazy_texts(&again, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 单飞：预热线程与按键线程同时 `get`，全表扫只发生一次。
///
/// 两份同指纹的落盘在 Windows 上会让后到的 rename 撞上先到者刚映射的文件，打一条误导性 warn。
/// 时序：第一趟扫描停在层里，放第二条线程进 `get`、等 200 ms 再放行。
/// 变异验证：去掉 `get` 里的单飞锁，本条红在扫描次数 2 上。
#[test]
fn concurrent_gets_scan_only_once() {
    use std::sync::mpsc;
    use std::sync::{Mutex, atomic::AtomicU32};
    struct Gate {
        calls: AtomicU32,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        resume: Mutex<mpsc::Receiver<()>>,
    }
    struct Slow(Arc<Gate>);
    impl wind_dict::DictLayer for Slow {
        fn name(&self) -> &str {
            "slow"
        }
        fn layer_type(&self) -> wind_dict::LayerType {
            wind_dict::LayerType::System
        }
        fn search(&self, _c: &str, _l: usize) -> Vec<Candidate> {
            Vec::new()
        }
        fn search_prefix(&self, _p: &str, _l: usize) -> Vec<Candidate> {
            Vec::new()
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            f("aaa", "aaa bbb", 1);
            if self.0.calls.fetch_add(1, Relaxed) == 0 {
                if let Some(tx) = self.0.entered.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                self.0.resume.lock().unwrap().recv().unwrap();
            }
        }
        fn entries_digest(&self) -> Option<String> {
            Some("slow|v1".into())
        }
    }
    let (etx, erx) = mpsc::channel();
    let (rtx, rrx) = mpsc::channel();
    let gate = Arc::new(Gate {
        calls: AtomicU32::new(0),
        entered: Mutex::new(Some(etx)),
        resume: Mutex::new(rrx),
    });
    let dm = Arc::new(DictManager::new());
    dm.register_layer(Box::new(Slow(Arc::clone(&gate))));
    let dir = tmp_dir("single");
    let lazy = Arc::new(LazyPhraseIndex::with_cache_dir(Some(dir.clone())));
    let spawn = || {
        let (lazy, dm) = (Arc::clone(&lazy), Arc::clone(&dm));
        std::thread::spawn(move || lazy.get(&dm).len())
    };
    let a = spawn();
    erx.recv().unwrap();
    let b = spawn();
    std::thread::sleep(std::time::Duration::from_millis(200));
    rtx.send(()).unwrap();
    assert_eq!((a.join().unwrap(), b.join().unwrap()), (1, 1));
    assert_eq!(gate.calls.load(Relaxed), 1, "两条线程各扫了一遍全表");
    assert_eq!(lazy.full_scans(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
