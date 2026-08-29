//! 真实词典端到端测试
//!
//! 加载仓库内的真实五笔/拼音词典，验证 CachedDict → DictWriter → DictReader (mmap)
//! 整条查询管道正确。这是 binformat entry_off 字节偏移 bug 的真实数据回归保护。
//!
//! 测试会在词典文件存在时运行；缺失时自动跳过（CI 无数据环境）。

use std::path::PathBuf;
use wind_dict::cached::CachedDict;

/// 仓库内 build_dev 数据目录。
///
/// ⚠️ 这里原本只写**两级** `../../build_dev/...`，解析到 `wind_input/build_dev/`——那个目录
/// 不存在，于是本文件两个用例长期静默走「跳过」分支、计数照常绿（判据只有耗时 0.00s），
/// 而它们守的正是 binformat `entry_off` 字节偏移那类最容易静默错位的 bug。仓库根才是
/// `build_dev` 的位置（三级：crates/wind-dict → crates → wind_input → 仓库根）。
/// 同款坑见 `wind-engine/tests/engine_manager.rs`，那边早已修正。两处都试，取真有数据的。
fn data_schemas() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = base.join("../../../build_dev/data/schemas");
    [root.clone(), base.join("../../build_dev/data/schemas")]
        .into_iter()
        .find(|d| d.is_dir())
        .unwrap_or(root)
}

/// 把词典源**拷进独占的临时目录**再返回新路径；源不存在时返回 None（调用方跳过）。
///
/// ⚠️ 不能原地加载：`CachedDict::load` 在**源文件旁**重建 `.wdat` 缓存，而源在
/// `build_dev/` 下——本仓的 worktree 里那是指向主仓的 junction。原地跑等于每次
/// `cargo test` 都改写主仓的构建产物（这两份是 5 MB / 31 MB），与并发构建、以及正在
/// mmap 这些文件的运行中服务互踩。本文件此前恰好因为路径写错而一直跳过，所以没炸过。
///
/// 目录名带 pid + 词典名：本仓常态是多 worktree / 多会话并行跑测试，同进程内两个用例
/// 也各占一份，互不干扰。**刻意只拷 yaml、不拷 wdat**——本测试要的就是
/// 「yaml → wdat → mmap 全路径」，缓存必须是现建的。
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
    // 缓存必须是**这次**建出来的：拷贝过来的只有 yaml，wdat 出现即证明走完了
    // yaml → wdat → mmap 全路径（本文件守的 entry_off 回归只在这条路上成立）。
    assert!(
        path.with_extension("wdat").is_file(),
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
        path.with_extension("wdat").is_file(),
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
