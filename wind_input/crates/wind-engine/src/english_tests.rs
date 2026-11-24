use super::*;
use crate::codetable::CommitOptions;
use crate::engine::Engine;
use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;
use wind_dict::{DictManager, SystemDictLayer};

/// 内存英文引擎，开着词组分词。
fn engine(entries: &[(&str, &str, i32)]) -> EnglishEngine {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "en")));
    let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm));
    EnglishEngine::new(ct).with_phrase_seg(Some(crate::english_phrase::PHRASE_SEPARATOR))
}

fn texts(e: &EnglishEngine, input: &str) -> Vec<String> {
    e.convert(input, 20)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 撇号词的编码在词库之间不统一，两种方案都得能打出来。
///
/// 出厂英文词库保留撇号（`o'clock` 的 code 就是 `o'clock`），用户自制/第三方词库常把它去掉
/// （`o'clock → oclock`）；只查原串的话，去撇号的词库下从 `o'cl` 起就是零候选。
/// 出厂词库表达不了这个场景，所以必须用自造词库测。
#[test]
fn apostrophe_word_found_under_both_encoding_schemes() {
    // 方案一：code 保留撇号（出厂英文词库的形态）
    let keep = engine(&[("o'clock", "o'clock", 1000)]);
    assert_eq!(texts(&keep, "o'cl"), vec!["o'clock"], "保留撇号的词库");

    // 方案二：code 去掉撇号（用户自制词库的常见形态）
    let strip = engine(&[("oclock", "o'clock", 1000)]);
    assert_eq!(texts(&strip, "o'cl"), vec!["o'clock"], "去撇号的词库");

    // 两种方案下打完整串同样命中。
    assert_eq!(texts(&keep, "o'clock"), vec!["o'clock"]);
    assert_eq!(texts(&strip, "o'clock"), vec!["o'clock"]);
}

/// 剥离查询不得产生重复：两种 code 同时存在时，同一个 text 只出一条。
#[test]
fn stripped_query_does_not_duplicate() {
    let e = engine(&[("o'clock", "o'clock", 1000), ("oclock", "o'clock", 900)]);
    assert_eq!(
        texts(&e, "o'cl"),
        vec!["o'clock"],
        "同 text 的两条编码只该出一条"
    );
}

/// 剥离查询不得把普通单词的前缀补全灌进来。
///
/// 打 `p'o` 时剥离串 `po` 的前缀匹配会召回 pocket / pod / poem 一类单词，按权重铺满
/// `max_candidates`，挤掉真正被请求的词组；这些词不打分词符本来就查得到。
#[test]
fn stripped_query_drops_plain_word_completions() {
    let e = engine(&[
        ("pocket", "pocket", 100),
        ("pod", "pod", 100),
        ("poem", "poem", 100),
        ("pocketpc", "Pocket PC", 50),
    ]);
    assert_eq!(
        texts(&e, "p'o"),
        vec!["Pocket PC"],
        "剥离命中里只有多词条目该留下"
    );
    // 反向对照：不打分词符时这些单词照常出——闸门关的是「打了分词符还出它们」。
    let plain = texts(&e, "po");
    assert!(plain.contains(&"pocket".to_string()) && plain.len() == 4);
}

/// 但剥离命中的多词条目必须留下：分段路径够不着它们。
///
/// `macOS Tahoe` 的 text 里 `macOS` 是一个词，`mac'os` 的第二段 `os` 无词可配，分段路径恒空，
/// 只能靠剥离串 `macos` 前缀命中拼接式编码。
#[test]
fn stripped_query_keeps_multi_word_hits() {
    let e = engine(&[("macos", "macOS Tahoe", 9)]);
    assert_eq!(texts(&e, "mac'os"), vec!["macOS Tahoe"]);
}

/// 分段路径与剥离路径撞同一条 text 时只出一条。
///
/// `mac'os`：`Mac OS X`（code `macosx`）既被分段路径召回，又被剥离串 `macos` 前缀命中。
/// 判据只能落在引擎层：协调器按 text 的通用去重会在用户端接住重复，端到端断言恒绿；
/// 这里挡的是重复白占 `truncate(max_candidates)` 的名额，被挤掉的词组协调器去重也变不回来。
#[test]
fn phrase_and_stripped_hits_are_deduped() {
    let e = engine(&[("macosx", "Mac OS X", 999)]);
    assert_eq!(texts(&e, "mac'os"), vec!["Mac OS X"]);
}

/// 同权重时分段候选排在剥离候选之前。
///
/// 按下分词符就是在表达「我要按段找词组」。词库里同 weight 条目成片存在（出厂词组大量 weight = 0），
/// 这个次序在实际候选窗里说了算。两条 weight 必须相等才测得到顺序本身。
#[test]
fn phrase_candidates_come_before_stripped_ones() {
    let e = engine(&[
        // 只有分段路径能命中（code `macx` 接不上剥离串 `macos`）。
        ("macx", "Mac OS X", 100),
        // 只有剥离路径能命中（`Tahoe` 不以 `os` 开头，分段路径配不上第二段）。
        ("macos", "macOS Tahoe", 100),
    ]);
    assert_eq!(texts(&e, "mac'os"), vec!["Mac OS X", "macOS Tahoe"]);
}

/// 反向对照：不含分词符时不做剥离查询，行为逐字节不变。
/// 没有这条，「恒查两次」与「只在含分词符时查两次」都能过上面几条。
#[test]
fn plain_input_does_not_trigger_stripped_query() {
    let e = engine(&[("oclock", "o'clock", 1000), ("ocl", "OCL", 900)]);
    // 打 `ocl`（不含分词符）：只有前缀匹配的结果，不会因为剥离而多出什么。
    let got = texts(&e, "ocl");
    assert!(got.contains(&"OCL".to_string()));
    assert!(got.contains(&"o'clock".to_string()));
    assert_eq!(got.len(), 2, "不含分词符时不该有额外查询，实际: {got:?}");
}

/// 词组分词关着时，那张索引一个字节都不许建（大词库下它是进程 Private 的最大单块）。
///
/// 它存不存在取决于 `schema.english.phrase_seg || input.temp_english.phrase_seg`，两处都关才不建。
/// 三个层次各断言一次：
///
/// 1. 装配期：`with_phrase_seg(None)` 之后索引不存在。
/// 2. `convert` 整条路不把它勾出来——用户真正走的入口。
/// 3. `phrase_candidates` 自己早退。
///
/// 第 3 条不能省：`convert` 外面还有一道 `if let Some(sep) = self.seg_sep`，只测 `convert` 时
/// 第二道闸破了照样绿；而 `LazyPhraseIndex::get` 自带懒建，在早退之前多调一次索引就静默回来了。
///
/// 第 1 条守不住 `with_phrase_seg` 里那道 `if sep.is_some()`：改成无条件 `prewarm` 仍会绿
/// （后台线程，断言时多半还没建完）。要守住得让 `prewarm` 可 join，收益不相称。
#[test]
fn a_disabled_feature_never_builds_the_index() {
    let mut d = CodetableDict::empty();
    d.merge_single("macx".into(), "Mac OS X".into(), 100, 0);
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "en")));
    let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm));
    // 走生产同一条装配：`phrase_seg_anywhere` 为 false 时 `build_engine` 传的就是
    // `None`（`manager.rs` 的 `phrase_seg_anywhere.then_some(...)`）。此前这里写的是
    // 裸 `EnglishEngine::new(ct)`，那连 `with_phrase_seg` 都没经过，名实不符。
    let e = EnglishEngine::new(ct).with_phrase_seg(None);
    assert!(!e.phrase.is_built(), "装配完成的那一刻索引不该在");

    // 带分词符的输入走完整条 convert，也不许把它勾出来。
    let _ = e.convert("mac'os", 20).unwrap();
    assert!(!e.phrase.is_built(), "convert 把索引勾出来了");

    // 越过 convert 的外层闸，直接打真正那道：`seg_sep` 为 None 就该在够着
    // `LazyPhraseIndex::get` 之前返回。
    assert!(e.phrase_candidates("mac'os", 20).is_empty());
    assert!(
        !e.phrase.is_built(),
        "关着词组分词却把 12.7 MB 的索引建了出来 —— 有人在 seg_sep 早退之前调了 get"
    );
}

/// 端到端：真 wdat 词库 + 缓存目录 + 真热摘（`set_dict_enabled` → `unregister_layer`）。
/// 摘掉扩展库后换用另一份落盘文件，且不再召回被摘库的词组。
///
/// `tests/english_phrase_index.rs` 走 `EngineManager`，缓存根是进程级 `CACHE_DIR`、断言不了
/// 「换了哪份文件」；这里用引擎的缓存目录注入口补上这一半。
#[test]
fn hot_unplugging_a_dictionary_switches_to_another_cache_file() {
    let dir = std::env::temp_dir().join(format!("wind_en_unplug_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let load = |name: &str, body: &str| {
        let yaml = dir.join(format!("{name}.dict.yaml"));
        std::fs::write(&yaml, format!("---\nname: {name}\n...\n{body}")).unwrap();
        CachedDict::load_at_with(&yaml, &dir.join(format!("{name}.wdat")), true).unwrap()
    };
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        load("en", "Buenos Aires\tbuenosaires\t60\n"),
        "en",
    )));
    dm.register_layer(Box::new(SystemDictLayer::new(
        load("en_ext", "iPhone 15 Pro Max\tiphone\t100\n"),
        "codetable-extra-en_ext",
    )));
    let cache = dir.join("cache");
    let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm))
        .with_own_extra_dicts(["en_ext".to_string()]);
    let e = EnglishEngine::new(ct)
        .with_phrase_cache_dir(Some(cache.clone()))
        .with_phrase_seg(Some(crate::english_phrase::PHRASE_SEPARATOR));
    let files = || -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&cache)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".wphr"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };

    assert!(texts(&e, "ip'max").contains(&"iPhone 15 Pro Max".to_string()));
    let before = files();
    assert_eq!(before.len(), 1, "开着两本库时应落盘一份：{before:?}");

    assert!(
        e.set_dict_enabled("en_ext", false),
        "前提：热摘应被本引擎认领"
    );
    assert!(
        !texts(&e, "ip'max").contains(&"iPhone 15 Pro Max".to_string()),
        "热摘后不该再召回被摘库的词组"
    );
    assert!(texts(&e, "bue'air").contains(&"Buenos Aires".to_string()));
    let after = files();
    assert_eq!(after.len(), 2, "热摘后应按新集合另落一份：{after:?}");
    assert!(
        after.contains(&before[0]),
        "原集合那份应保留（再启用时复用）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
