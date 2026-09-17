//! 全仓守卫：**按行解析用户文件的地方，都得先过一道行尾规整**。
//!
//! # 为什么判据是「函数体里有 `.lines()`」而不是「参数叫 text」
//!
//! 第一版守卫（在 wind-store 里）扫的是 `pub fn xxx(text: &str`，两个毛病都在
//! 2026-09-17 的独立审查里被抓了出来：
//!
//! - 它用 `match_indices("\npub fn ")` 定位，**只能扫到零缩进的模块级自由函数**。
//!   写在 `impl` 块里的关联方法天生在扫描范围之外——整机备份恢复的四个 JSONL 入口
//!   （`import_{common_chars,freq,shadow,stats}_jsonl`）就是这么被放过的。
//! - 它按**参数名**匹配，那既会漏（`parse_common_chars_file(content: &str)`），
//!   又会误报（`record_freq(.., text: &str)` 里的 `text` 只是一个词，不该归一化）。
//!
//! 「函数体里按行切」才是本质判据：会不会被孤立 `\r` 坑，取决于有没有按行解析，
//! 与参数怎么命名、函数写在哪一层都无关。
//!
//! # 粒度与边界
//!
//! 按**文件**粒度。一个文件里只要有按行解析用户文本的地方，它就该出现
//! `normalize_input`；确实不该有的，进下面的白名单并写清楚理由。白名单不是豁免
//! 清单，是**「这一处我们看过了」的记录**——它的长度就是全仓审视过的范围。
//!
//! 已知抓不到的：`BufReader::lines()` 那一类**流式**读取（`wind-reverse`、
//! `wind-webdata`、`wind-engine/manager.rs` 各有一处）。它们同样只认 `\n`，但
//! `normalize_input` 需要全文，修法不同，得单独处理——见白名单里的 `STREAMING`。

use std::path::{Path, PathBuf};

/// 不需要 `normalize_input` 的文件，**每条都要写清楚为什么**。
///
/// 加条目之前先问：这个文件按行解析的是不是用户能拿到、能编辑、能从别的系统
/// 搬过来的文本？是的话就该修，不是加白名单。
const ALLOWED: &[(&str, &str)] = &[
    // ---- 构建期工具：读的是我们从上游拉下来的数据，不是用户文件 ----
    // 上游（rime / opencc / unicode 数据）用 LF 或 CRLF，不会是孤立 \r；
    // 且构建期解析异常会当场让构建失败或产出明显异常，不存在「运行时静默」。
    (
        "apps/wind-tools/src/bin/gen_aux_code.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_emoji_chars.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_emoji_names.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_opencc.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_pinyin.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_unigram.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_dict/boost.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_dict/extra.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_dict/parse.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_dict/reverse.rs",
        "构建期工具，读上游数据",
    ),
    (
        "apps/wind-tools/src/bin/gen_dict/weight.rs",
        "构建期工具，读上游数据",
    ),
    // ---- 按行处理的不是「用户的文本文件」----
    (
        "crates/wind-config/src/value_domain_guard.rs",
        "扫的是我们自己仓里的 .rs 源码（找 AppCompatRule 的字段定义），不是用户文件",
    ),
    (
        "crates/wind-coordinator/src/handle_cmdbar.rs",
        "取的是子进程输出的首/末非空行做 toast 文案，不是文件解析；\
         最坏情况是提示里多个字符，不涉及数据丢失",
    ),
    (
        "crates/wind-config/src/config.rs",
        "TOML 宽容解析的错误恢复路径（逐行剔除坏行重试）。TOML 规范本身不认孤立 \r，\
         toml::from_str 会**明确报错**而不是静默失效——这个守卫要防的是「不报错的失效」。",
    ),
    // ---- 另案：修法与其余几处不同 ----
    (
        "crates/wind-dict/src/codetable.rs",
        "mmap + 字节偏移的零拷贝并行解析（body_lines/rime_body_offset 传的是偏移量）。\
         归一化要复制 9.7MB 的 base.dict.yaml 并打乱 offset 语义，得换修法：\
         在失败路径上探测 CR 报错，或往读文件那层挪。见 .omc A3-9。",
    ),
];

/// 尚未处理、但**已经确认存在**的口子。清空它是目标，不是把它当摆设。
///
/// 与 `ALLOWED` 的区别：那边是「看过了，不需要」，这边是「看过了，需要但还没做」。
/// 留在这里是为了让它可见且可数，而不是散在某个 TODO 注释里。
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "crates/wind-config/src/charset_def.rs",
        "用户自定义字符类 charsets/*.yaml，按行切 head/body",
    ),
    (
        "crates/wind-reverse/src/lib.rs",
        "注释词库（rime .dict.yaml 形态）。它刻意不复用 wind_dict::codetable\
         （怕连累缓存失效判定），但自己是普通 read_to_string + lines()，\
         codetable 那条「零拷贝 offset」的豁免理由在这里不适用。",
    ),
    (
        "crates/wind-dict/src/emojidict.rs",
        "运行时加载 emoji_word.txt / emoji_category.txt",
    ),
    (
        "apps/service/src/config_cli/custom_check.rs",
        "配置体检 CLI，按行读用户的自定义方案文件",
    ),
    (
        "crates/wind-engine/src/manager.rs",
        "STREAMING：BufReader::lines() 流式读整句词频词库。同样只认 \n，但 normalize_input \
         要全文，修法不同（要么先 read_to_string，要么换个按行读取器）。",
    ),
];

/// 截掉第一个行首 `#[cfg(test)]` 之后的内容——测试里的 `.lines()` 不算。
fn strip_tests(src: &str) -> &str {
    let mut at = 0usize;
    for (i, line) in src.split_inclusive('\n').enumerate() {
        if line.starts_with("#[cfg(test)]") {
            return &src[..at];
        }
        let _ = i;
        at += line.len();
    }
    src
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            // tests/ 与 target/ 不扫
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name == "target" || name == "tests" {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn every_line_based_parser_normalizes() {
    // wind-utils/ → wind_input/
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("上溯到 workspace 根")
        .to_path_buf();
    assert!(
        root.join("Cargo.toml").is_file(),
        "没找到 workspace 根：{root:?}"
    );

    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("apps"), &mut files);
    files.sort();
    assert!(
        files.len() > 100,
        "只扫到 {} 个 .rs，守卫会假通过",
        files.len()
    );

    let rel = |p: &Path| {
        p.strip_prefix(&root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };

    let mut offenders = Vec::new();
    let mut covered = 0usize;
    let mut stale_allow: Vec<String> = Vec::new();

    for path in &files {
        let r = rel(path);
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let prod = strip_tests(&src);
        // 守卫自己和 normalize_input 的定义处不算
        if r.starts_with("crates/wind-utils/") {
            continue;
        }
        if !prod.contains(".lines()") {
            if ALLOWED.iter().any(|(f, _)| *f == r) || KNOWN_GAPS.iter().any(|(f, _)| *f == r) {
                stale_allow.push(r);
            }
            continue;
        }
        if prod.contains("normalize_input") {
            covered += 1;
            if let Some((f, _)) = KNOWN_GAPS.iter().find(|(f, _)| *f == r) {
                stale_allow.push(format!("{f}（已修好，该从 KNOWN_GAPS 移除）"));
            }
            continue;
        }
        if ALLOWED.iter().any(|(f, _)| *f == r) || KNOWN_GAPS.iter().any(|(f, _)| *f == r) {
            continue;
        }
        offenders.push(r);
    }

    assert!(
        covered >= 6,
        "已覆盖的文件只剩 {covered} 个（当前应有 8 个），比预期少——是不是有地方把 normalize_input 去掉了？"
    );

    assert!(
        stale_allow.is_empty(),
        "白名单/待办清单里这些条目已经不成立了，请删掉，别让清单变成摆设：\n{stale_allow:#?}"
    );

    assert!(
        offenders.is_empty(),
        "这些文件按行解析却没有过 wind_utils::text::normalize_input。\n\
         孤立 \\r 会让整份文件算成一行——带 `#` 注释头的文件会被整个当成注释，\n\
         症状是「0 条 / 空表 / 导入成功但只进 1 条」，且全程无报错。\n\n\
         {offenders:#?}\n\n\
         要么在入口加：\n  \
         let normalized = wind_utils::text::normalize_input(text);\n  \
         let text = normalized.as_ref();\n\
         要么加进 ALLOWED 并写清楚为什么不需要（不是随手豁免，是留下「看过了」的记录）。"
    );
}
