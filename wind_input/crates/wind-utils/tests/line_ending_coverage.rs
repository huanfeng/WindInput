//! 全仓守卫：按行解析用户文件的地方，都得先过一道行尾规整（`normalize_input`）。
//!
//! 判据是「函数体里有 `.lines()`」：会不会被孤立 `\r` 坑只取决于有没有按行解析。按参数名
//! （`text: &str`）匹配既漏又误报；只扫零缩进 `pub fn` 会漏掉 `impl` 块里的关联方法。
//!
//! 两层：文件级白名单先吸收整类不相关的文件，剩下的按函数粒度逐个查。不能只按文件判：
//! 一个文件里有几个互不相干的解析器是常态，一个调了 `normalize_input` 会让其余的隐身。
//!
//! 白名单不是豁免清单，是「这一处看过了」的记录。
//!
//! 已知抓不到：`BufReader::lines()` 那类流式读取。它同样只认 `\n`，但 `normalize_input`
//! 要全文，对「只读头部、正文几百 MB」的场景是内存回归，修法不同——见 `KNOWN_GAPS`。

use std::path::{Path, PathBuf};

/// 不需要 `normalize_input` 的文件，每条都要写清楚为什么。
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
        "apps/wind-tools/src/bin/gen_block_charsets.rs",
        "构建期工具，读 gen_emoji_chars 的产物",
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
    (
        "crates/wind-ui/src/candidate_window/render_golden.rs",
        "整个文件是测试模块（#[cfg(test)] 挂在父模块的 mod 声明上，守卫的 strip_tests 看不到）。\
         .lines() 只用来在 golden 对不上时定位首个不同行、写进失败信息；判等比的是整串，\
         检入的 golden 由本仓自己生成，不是用户文件。",
    ),
    (
        "crates/wind-coordinator/src/process_memory.rs",
        "parse_rss_anon_kb 解析的是内核给的 /proc/self/status，格式固定、只用 \\n 分行，\
         不是用户文件；取不到只是内存统计缺一项，不涉及数据丢失。",
    ),
    (
        "crates/wind-dict/src/cache_fp.rs",
        "st_current_fp 读的是本程序自己写的缓存状态 sidecar（.st），不是用户文件；\
         读不懂只会当缓存不新鲜、重建一次。",
    ),
    (
        "crates/wind-dict/src/datformat.rs",
        "rss_mb 解析内核给的 /proc/self/status（同 process_memory.rs），只用于构建日志。",
    ),
    (
        "crates/wind-coordinator/src/source_scan.rs",
        "仅测试：源码扫描守卫读本 crate 自己的 .rs（rustfmt 排版、include_str!/read_to_string），\
         不碰用户文件；`lines()` 已处理 CRLF，孤立 CR 在源码里不出现。",
    ),
    // ---- 另案：修法与其余几处不同 ----
    (
        "crates/wind-dict/src/codetable.rs",
        "mmap + 字节偏移的零拷贝并行解析（body_lines/rime_body_offset 传的是偏移量）。\
         归一化要复制 9.7MB 的 base.dict.yaml 并打乱 offset 语义，得换修法：\
         在失败路径上探测 CR 报错，或往读文件那层挪。见 .omc A3-9。",
    ),
];

/// 自己不归一化、由调用方保证的函数。
///
/// 它们拿到的已是上游入口规整过的文本。「谁负责」只有人能判断，调用链变了要回来核对这份清单。
const DELEGATED: &[(&str, &str)] = &[
    (
        "charset_def.rs::parse_docs",
        "第一步就是 split_head_body，它归一化",
    ),
    ("charset_def.rs::parse_doc", "同上"),
    // wind-wdict 的 lib.rs
    (
        "lib.rs::parse_word_rows",
        "由 parse_words_wdict / parse_temp_words_wdict 保证",
    ),
    (
        "lib.rs::check_wdict_header",
        "拿到的 header 来自已归一化的全文",
    ),
    ("lib.rs::section_columns_from_header", "同上"),
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
    // 误读表 YAML：入参已由 MisreadTable::parse 归一（孤立 \r 会让 meta 头与
    // 列表体的切分整份算成一行），这里只对归一后的头做逐行检查。
    (
        "misread.rs::head_has_entries",
        "入参来自 MisreadTable::parse 已 normalize_input 的全文",
    ),
];

/// 尚未处理、但已经确认存在的口子，清空它是目标。
///
/// 与 `ALLOWED` 的区别：那边是「看过了，不需要」，这边是「看过了，需要但还没做」。
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
/// 一个文件可以有多个 test mod，只截到第一个 `#[cfg(test)]` 会丢掉其后的生产代码。
///
/// 逐块跳过：行首 `#[cfg(test)]` 进入跳过态，遇到整行恰好是 `}` 结束；条目首行以 `;` 结尾
/// （外移的 `mod tests;`）则只跳这一行。不能只看 `starts_with('}')`：测试里跨行字符串字面量
/// 可能有一行正是 `}"`，会让跳过态提前结束。
fn strip_tests(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut skipping = false;
    let mut at_item_head = false;
    for line in src.split_inclusive('\n') {
        if !skipping {
            if line.starts_with("#[cfg(test)]") {
                skipping = true;
                at_item_head = true;
                continue;
            }
            out.push_str(line);
        } else if at_item_head && line.starts_with("#[") {
            // 条目上连着的其他属性（如 `#[path = "…"]`）。
        } else if at_item_head && line.trim_end().ends_with(';') {
            // 单行条目（`mod tests;`、`use …;`）：只跳这一行，否则会吞掉后面的生产代码。
            skipping = false;
        } else {
            at_item_head = false;
            if line.trim_end() == "}" {
                skipping = false;
            }
        }
    }
    out
}

/// 去掉注释后的代码。
///
/// 必须先剥注释再判：注释里提到 normalize_input 会被当成覆盖的证据，删掉归一化照样绿。
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
            // tests/ 与 target/ 不扫；外移的测试模块（*_tests.rs）同理
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name == "target" || name == "tests" {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && !p.to_string_lossy().ends_with("_tests.rs")
        {
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
