//! 真实词典端到端测试
//!
//! 加载仓库内的真实五笔/拼音词典，验证 CachedDict → DictWriter → DictReader (mmap)
//! 整条查询管道正确，守住 binformat `entry_off` 字节偏移这类静默错位的 bug。
//!
//! 测试会在词典文件存在时运行；缺失时自动跳过（CI 无数据环境）。

use std::path::PathBuf;
use wind_dict::cached::CachedDict;

/// 仓库内 build_dev 数据目录（仓库根，三级：crates/wind-dict → crates → wind_input → 根）。
/// 路径写错时两个用例会静默走「跳过」分支、计数照常绿（判据只有耗时 0.00s）。
/// 两处都试，取真有数据的。
fn data_schemas() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = base.join("../../../build_dev/data/schemas");
    [root.clone(), base.join("../../build_dev/data/schemas")]
        .into_iter()
        .find(|d| d.is_dir())
        .unwrap_or(root)
}

/// 把词典源拷进独占的临时目录再返回新路径；源不存在时返回 None（调用方跳过）。
///
/// 不能原地加载：`CachedDict::load` 在源文件旁重建 `.wdat`，而 worktree 里的 `build_dev/`
/// 是指向主仓的链接，原地跑会改写主仓的构建产物，与并发构建、正在 mmap 它们的服务互踩。
/// 目录名带 pid + 词典名，多 worktree / 多会话并行互不干扰。只拷 yaml、不拷 wdat：
/// 本测试要的就是现建缓存。
fn stage(rel: &str) -> Option<PathBuf> {
    let src = data_schemas().join(rel);
    if !src.is_file() {
        eprintln!("跳过：词典不存在 {}", src.display());
        return None;
    }
    let name = src.file_name()?.to_string_lossy().into_owned();
    let stem = name.split('.').next().unwrap_or("dict").to_string();
    let dir = std::env::temp_dir().join(format!("wind-real-dict-{}-{stem}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join(&name);
    std::fs::copy(&src, &dst).ok()?;
    Some(dst)
}

#[test]
fn test_real_wubi_candidates() {
    let Some(path) = stage("wubi86/wubi86_jidian.dict.yaml") else {
        return;
    };

    let dict = CachedDict::load(&path).expect("加载五笔词典");
    assert!(!dict.is_empty(), "五笔词典应非空");
    // 缓存必须是这次建出来的：拷贝过来的只有 yaml，wdat 出现即证明走完了
    // yaml → wdat → mmap 全路径。
    assert!(
        dict.source_file()
            .is_some_and(|w| w.is_file() && w.parent() == path.parent()),
        "应在临时目录里新建 wdat 缓存：{}",
        path.display()
    );

    // 精确查找：a → 工/戈
    let a = dict.search("a");
    assert!(!a.is_empty(), "'a' 应有候选（mmap entry_off 回归）");
    assert!(a.iter().any(|(t, _, _)| t == "工"), "'a' 应包含 工");

    // 非首 key（验证 entry_off 字节偏移修复）
    let aaaa = dict.search("aaaa");
    assert!(!aaaa.is_empty(), "'aaaa' 应有候选");
    assert!(
        aaaa.iter().any(|(t, _, _)| t == "恭恭敬敬"),
        "'aaaa' 应包含 恭恭敬敬"
    );

    // 前缀查找
    let prefix = dict.search_prefix("aa", 20);
    assert!(!prefix.is_empty(), "'aa' 前缀应有候选");

    drop(dict); // 先解除 mmap，Windows 上映射未解除时删不掉文件
    let _ = std::fs::remove_dir_all(path.parent().expect("临时目录"));
}

#[test]
fn test_real_pinyin_candidates() {
    let Some(path) = stage("pinyin/cn_dicts/base.dict.yaml") else {
        return;
    };

    let dict = CachedDict::load(&path).expect("加载拼音词典");
    assert!(!dict.is_empty(), "拼音词典应非空");
    assert!(
        dict.source_file()
            .is_some_and(|w| w.is_file() && w.parent() == path.parent()),
        "应在临时目录里新建 wdat 缓存：{}",
        path.display()
    );

    // 拼音 key 加载时去空格："a ba" → "aba"
    let aba = dict.search("aba");
    assert!(!aba.is_empty(), "'aba' 应有候选（去空格 key）");
    assert!(
        aba.iter().any(|(t, _, _)| t == "阿爸" || t == "阿巴"),
        "'aba' 应包含 阿爸/阿巴，实际: {:?}",
        aba.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>()
    );

    drop(dict); // 同上：解除 mmap 再删临时目录
    let _ = std::fs::remove_dir_all(path.parent().expect("临时目录"));
}

/// 蒙古文词条的尾随空格必须活着走完 yaml → wdat → mmap 全路径。
///
/// 蒙文以空格作词间分隔，toli 词库几乎全部词条以空格收尾，且声明三列、正文两列，text 落在
/// 末列，整行 `trim_end` 会让连续上屏的词粘在一起。这里照其形态自造（同样的 `columns:`
/// 声明、两列、PUA 码位 + 内部空格 + 尾随空格）。必须走 CachedDict：缓存层吃掉空格的话
/// 症状只在命中缓存时出现。
#[test]
fn test_mongolian_trailing_space_survives_cache_pipeline() {
    let dir = std::env::temp_dir().join(format!("wind-toli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("toli.dict.yaml");
    std::fs::write(
        &path,
        "---\nname: toli\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
         a\t\u{e249} \n\
         aabn\t\u{e226}\u{e2f4} \u{e341}\u{e2ca} \n",
    )
    .expect("写词库");

    let dict = CachedDict::load(&path).expect("加载蒙古文词典");
    assert!(
        dict.source_file()
            .is_some_and(|w| w.is_file() && w.parent() == path.parent()),
        "应新建 wdat 缓存，本用例要的就是过缓存这一遭"
    );

    let a = dict.search("a");
    assert_eq!(
        a.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(),
        vec!["\u{e249} "],
        "单字词条的尾随空格须原样穿过整条管道"
    );

    let aabn = dict.search("aabn");
    assert_eq!(
        aabn.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(),
        vec!["\u{e226}\u{e2f4} \u{e341}\u{e2ca} "],
        "词组的内部空格与尾随空格都是词间分隔，须整串保留"
    );

    drop(dict); // 解除 mmap 再删临时目录（Windows 上映射未解除时删不掉）
    let _ = std::fs::remove_dir_all(&dir);
}
