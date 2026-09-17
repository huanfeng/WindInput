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
//! 两层：**文件**级白名单先吸收整类不相关的文件（构建工具、另案…），剩下的按
//! **函数**粒度逐个查。
//!
//! ⚠️ 文件粒度单独用是不够的——2026-09-17 的第二轮审查实锤：`freq.rs` 里
//! `import_freq_jsonl` 调了 `normalize_input`，整个文件就被判成「已覆盖」，同文件
//! 另一个独立的 `FreqTracker::load_from_file` 就此隐身。顺着这条线还查出
//! `wind-reverse/src/lib.rs` 的 `load_chaizi` / `load_pinyin` 同样被放过——
//! **一个文件里有几个互不相干的解析器是常态**。
//!
//! 白名单不是豁免清单，是**「这一处我们看过了」的记录**——它的长度就是全仓审视过的范围。
//!
//! 已知抓不到的：`BufReader::lines()` 那一类**流式**读取。它同样只认 `\n`，但
//! `normalize_input` 要全文，对「只读头部、正文几百 MB」的场景是内存回归，修法
//! 不同——见 `KNOWN_GAPS` 里的 `manager.rs`。

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
    (
        "apps/service/src/config_cli/custom_check.rs",
        "它的 .lines() 是 print_block——把多行文案按行打出来做缩进对齐，不是文件解析；\
         读文件那条是 read_toml，同 config.rs 的理由。\
         （2026-09-17 审查曾把它列为待修，是判错了，核对代码后移到这里。）",
    ),
    // ---- 另案：修法与其余几处不同 ----
    (
        "crates/wind-dict/src/codetable.rs",
        "mmap + 字节偏移的零拷贝并行解析（body_lines/rime_body_offset 传的是偏移量）。\
         归一化要复制 9.7MB 的 base.dict.yaml 并打乱 offset 语义，得换修法：\
         在失败路径上探测 CR 报错，或往读文件那层挪。见 .omc A3-9。",
    ),
];

/// 自己不归一化、由**调用方**保证的函数。
///
/// 它们拿到的已经是规整过的文本（上游入口调了 `normalize_input` 再传进来），
/// 自己再调一次是多余的扫描。列在这里而不是让守卫猜，是因为「谁负责」这件事
/// 只有人能判断——而一旦调用链变了，这份清单就是该回来核对的地方。
const DELEGATED: &[(&str, &str)] = &[
    (
        "charset_def.rs::parse_docs",
        "第一步就是 split_head_body，它归一化",
    ),
    ("charset_def.rs::parse_doc", "同上"),
    (
        "wdict.rs::parse_word_rows",
        "由 parse_words_wdict / parse_temp_words_wdict 保证",
    ),
    (
        "wdict.rs::check_wdict_header",
        "拿到的 header 来自已归一化的全文",
    ),
    ("wdict.rs::section_columns_from_header", "同上"),
    (
        "phrase_text.rs::first_content_line",
        "由 is_phrase_text / parse_phrase_text 保证",
    ),
    (
        "handle_common_chars.rs::parse_common_chars_jsonl",
        "由 parse_common_chars_file 保证",
    ),
    (
        "lib.rs::columns_names",
        "拿到的 header 来自 parse_comment_dict 已归一化的全文",
    ),
    ("lib.rs::declares_comment_column", "同上"),
];

/// 尚未处理、但**已经确认存在**的口子。清空它是目标，不是把它当摆设。
///
/// 与 `ALLOWED` 的区别：那边是「看过了，不需要」，这边是「看过了，需要但还没做」。
/// 留在这里是为了让它可见且可数，而不是散在某个 TODO 注释里。
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "crates/wind-webdata/src/lib.rs",
        "dict_yaml_name：BufReader::lines().take(200) 读 .dict.yaml 取方案名。与 \
         manager.rs::read_dict_head 同类、读的也是同一种文件 ⇒ 同一个案子。\
         ★ 它是**改好 strip_tests 之后才浮出来的**：早先守卫截到第一个 #[cfg(test)] \
         就停，而这个函数在第二个 test mod 之后，整段被丢掉了。",
    ),
    (
        "crates/wind-engine/src/manager.rs",
        "read_dict_head：读 .dict.yaml 的**头部**。normalize_input 要全文，而这里\
         「主词库正文动辄几百 MB，本函数每次启动对每张表都要跑一遍」，还有 \
         DICT_HEAD_SCAN_LIMIT 卡着——换成 read_to_string 是明确的内存与启动耗时回归。\
         要修得换读取策略（先读有上限的一段字节再在内存里分行）。\
         ⇒ 它读的就是 .dict.yaml，与 codetable.rs 是**同一种文件的另一条读取路径**，\
         归入同一个案子一起处理，别分开改。见 .omc A3-9。",
    ),
];

/// 去掉所有 `#[cfg(test)]` 块——测试里的 `.lines()` 不算。
///
/// ⚠️ 早先这里是「截到**第一个** `#[cfg(test)]` 为止」，那会把它后面的**生产代码**
/// 一并丢掉。一个文件可以有多个 test mod（本仓有 8 个以上这样的文件），
/// `wind-webdata/src/lib.rs` 就是这么漏掉 `dict_yaml_name` 的：它在第二个 test mod
/// 之后，行首无缩进，是实打实的生产代码。
///
/// 现在逐块跳过：行首 `#[cfg(test)]` 进入跳过态，遇到**整行恰好是** `}` 结束。
///
/// 判据不能只看 `starts_with('}')`：`app_compat.rs` 的测试里有个跨行字符串字面量，
/// 其中一行正是 `}"`，那会让跳过态提前结束、把后面的测试代码当成生产代码报出来。
fn strip_tests(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut skipping = false;
    for line in src.split_inclusive('\n') {
        if !skipping {
            if line.starts_with("#[cfg(test)]") {
                skipping = true;
                continue;
            }
            out.push_str(line);
        } else if line.trim_end() == "}" {
            skipping = false;
        }
    }
    out
}

/// 去掉注释后的代码。
///
/// ⚠️ 必须先剥注释再判：`freq.rs` 的注释里写了「…调了 normalize_input…」这句话，
/// 结果把归一化**删掉**之后守卫照样绿——它把注释当成了覆盖的证据。这是变异测试
/// 抓出来的，不是推想。
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 真的**调用**了归一化，而不是只在文字里提到它。
fn calls_normalize(code: &str) -> bool {
    code.contains("normalize_input(")
}

/// 按行解析的特征。`.lines()` 之外，`split('\n')` 一类同样算。
fn is_line_based(src: &str) -> bool {
    src.contains(".lines()") || src.contains("split('\n')") || src.contains("split_inclusive('\n')")
}

/// 把源码按 `fn` 切成 (函数名, 函数体) —— 段的结尾就是下一个 `fn` 的开头。
///
/// 不做花括号配对：嵌套函数会各自成段，对本守卫无害（照样逐个查）。
fn split_fns(src: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut name: Option<String> = None;
    for line in src.lines() {
        if let Some(n) = fn_name_of(line) {
            if let Some(prev) = name.take() {
                out.push((prev, cur.join("\n")));
            }
            name = Some(n);
            cur = vec![line];
        } else {
            cur.push(line);
        }
    }
    if let Some(prev) = name {
        out.push((prev, cur.join("\n")));
    }
    out
}

/// 这一行是不是函数定义的开头，是则给出函数名。
fn fn_name_of(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t.strip_prefix("pub ").unwrap_or(t);
    // pub(crate) / pub(super) …
    let t = match t.strip_prefix("pub(") {
        Some(rest) => rest.split_once(')').map_or(t, |(_, r)| r.trim_start()),
        None => t,
    };
    let t = t.strip_prefix("async ").unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    let n: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!n.is_empty()).then_some(n)
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
        if r.starts_with("crates/wind-utils/") {
            continue;
        }
        let prod = strip_comments(&strip_tests(&src));
        let listed =
            ALLOWED.iter().any(|(f, _)| *f == r) || KNOWN_GAPS.iter().any(|(f, _)| *f == r);
        if !is_line_based(&prod) {
            if listed {
                stale_allow.push(r);
            }
            continue;
        }
        // 文件级白名单先吸收（构建工具、另案、非文件解析…）
        if listed {
            if KNOWN_GAPS.iter().any(|(f, _)| *f == r) && !calls_normalize(&prod) {
                // 仍是缺口，符合预期
            }
            continue;
        }
        // 再按函数粒度逐个查——一个文件里有几个互不相干的解析器是常态
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut any = false;
        for (name, body) in split_fns(&prod) {
            if !is_line_based(&body) {
                continue;
            }
            if calls_normalize(&body) {
                any = true;
                continue;
            }
            let key = format!("{file_name}::{name}");
            if DELEGATED.iter().any(|(f, _)| *f == key) {
                continue;
            }
            offenders.push(format!("{r}::{name}"));
        }
        if any {
            covered += 1;
        }
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
