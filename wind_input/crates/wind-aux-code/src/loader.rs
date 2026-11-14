//! 辅助码 txt 文件 / 文本加载
//!
//! 行格式与 rime-lua-aux-code `aux_code` 目录一致用 **`=` 分隔**（UTF-8，每行一条
//! `字=码`，同一汉字多编码分列多行，如 `阿=ek` / `厑=ib` / `厑=ii`）；空行与 `#`
//! 注释行跳过。`=` 右侧也可**按空白写多个码**（`七=a p`，手心表的写法），与分列多行等价。
//! 文件必须是 UTF-8：非 UTF-8（如 GBK）整张忽略并告警「请转存为 UTF-8」。
//!
//! ## 名称（唯一解析的元数据）
//! 只从**文件第 1 行**读取方法名（`# name: 笔画` 或 `#name: 笔画`，`#` 后空格可有可无、
//! value 两侧去空白）。第 1 行不是名字就整体不解析元数据，`load_from_file` 回落文件
//! 主干名（`stroke.txt` → `stroke`）。version / source 等一律当普通注释，程序不读——
//! 它们留给人类，未来若支持表自动更新再提为解析字段。
//!
//! 本模块只负责把外部文本 / 文件变成 [`AuxCodeTable`]，不接触表的内存布局——
//! 表的三段式存储、去重、合并语义全在 [`crate::table`]。

use crate::table::AuxCodeTable;

/// 从辅助码 txt 文件构建**单张**码表。
///
/// 路径由调用方解析（用户目录同名文件优先），本 crate 不负责定位——与 `wind-reverse`
/// 同一约定。文件读不出来（路径错、无权限、非 UTF-8）返回空表并告警，不 panic。
///
/// 辅助码表普遍很小，直接整体读入即可，无需缓存 / mmap；懒加载（首次输入辅助码时才
/// 读取）由调用方控制触发时机，本函数是一次性构造。
pub fn load_from_file(path: &std::path::Path) -> AuxCodeTable {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let mut t = parse_str(&content);
            if t.name.is_empty() {
                t.name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            t
        }
        // 非 UTF-8（手心等表常见 GBK）：全仓不带 GBK 解码依赖，不为一张表引入 `encoding_rs`，
        // 明确告诉用户怎么办，产出空表（筛选随之 passthrough）。
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            tracing::warn!(
                "辅助码文件不是 UTF-8 编码，已忽略，请转存为 UTF-8: {}",
                path.display()
            );
            AuxCodeTable::new()
        }
        Err(e) => {
            tracing::warn!("读取辅助码文件失败 {}: {}", path.display(), e);
            AuxCodeTable::new()
        }
    }
}

/// **合并加载多张码表**：`merge(paths.iter().map(load_from_file))`（先出现 = 高优）。
///
/// 供协调器「首次辅助码输入时，把每段相邻的文件来源一次性坍缩成一张表」的懒加载调用
/// （`aux_code_source.rs`；方案来源 `schema:<id>` 不经此处，见 [`crate::AuxCodeLookup`]）。
/// 路径由调用方解析后整体传入，本函数只负责「逐张读入 + 跨表去重/优先级坍缩」，
/// 空文件（读失败/空内容）经 `merge` 自动跳过。
pub fn load_merged(paths: &[std::path::PathBuf]) -> AuxCodeTable {
    AuxCodeTable::merge(paths.iter().map(|p| load_from_file(p)))
}

/// 只读首行取码表名（`# name: 小鹤`）。设置页列可选码表时用：为了一个名字整张读入四万行不值。
pub fn read_name(path: &std::path::Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(f).read_line(&mut line).ok()?;
    parse_name_from_first_line(line.trim_start_matches('\u{feff}').trim_end())
}

/// 从首行提取 `# name: X` / `#name: X`（`#` 后空格可有可无，value 两侧去空白）。
fn parse_name_from_first_line(first_line: &str) -> Option<String> {
    let rest = first_line.trim_start().strip_prefix('#')?.trim_start();
    let (key, value) = rest.split_once(':')?;
    if key.trim().eq_ignore_ascii_case("name") {
        let v = value.trim();
        (!v.is_empty()).then(|| v.to_string())
    } else {
        None
    }
}

/// 解析辅助码文本（**`=` 分隔**，与 rime-lua-aux-code 行格式一致）：
///
/// 每行一条 `字=码`（字符在前、码在后；同一汉字多种编码分列多行），如
/// `阿=ek`、`厑=ib`、`厑=ii`。
///
/// - 空行与 `#` 注释行跳过；无 `=`、左侧非单字或右侧为空码的行整行跳过
/// - 右侧按空白拆成多个码（`七=a p` = `七=a` + `七=p`），行内 `#` 起的注释先剥掉
/// - 开头剥掉 UTF-8 BOM、孤立 `\r` 行尾折成 `\n`（见 [`wind_utils::text::normalize_input`]）：
///   前者避免首行字被当成非单字跳掉，后者避免整份文件被当成一行——带 `# name:` 头的
///   出厂表在那种情况下会被整个当成一条注释，0 条且无任何提示
/// - **第 1 行**提取方法名（`# name: 笔画`，见模块文档）；其余元数据一律当注释
///
/// 内容来源（文件 / 网络 / 内嵌）由调用方决定，本函数只做纯解析，不接触文件系统。
/// crate 内部用（`load_from_file`）；如需对外暴露纯解析入口再提升为 `pub`。
pub(crate) fn parse_str(content: &str) -> AuxCodeTable {
    let normalized = wind_utils::text::normalize_input(content);
    let content = normalized.as_ref();
    let mut name = String::new();
    let mut rows: Vec<(char, &str)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if i == 0
            && let Some(n) = parse_name_from_first_line(line)
        {
            name = n;
        }
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((head, code)) = line.split_once('=') else {
            continue; // 无 =：不是 `字=码` 形态
        };
        let head = head.trim();
        // 行内 `#` 注释先剥掉再拆码（码只有字母，`#` 不会是码的一部分）：否则 `七=a # 注`
        // 按空白一拆，`#`、`注` 都成了码。
        let code = code.split('#').next().unwrap_or_default().trim();
        if code.is_empty() {
            continue; // 空码 = 没码
        }
        let mut chars = head.chars();
        let Some(c) = chars.next() else {
            continue;
        };
        if chars.next().is_some() {
            continue; // 等号左侧必须是单个汉字，多字行跳过
        }
        // 一行多码：`七=a p`（手心表的写法）按空白拆开，与分列多行等价。
        rows.extend(code.split_whitespace().map(|code| (c, code)));
    }
    AuxCodeTable::from_rows(rows).with_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// parse_str：`=` 分隔格式；同字多码分列多行；注释/空行跳过；无 `=` / 非单字 / 空码行不产生条目
    #[test]
    fn parse_str_equals_separated_rows() {
        let t = parse_str("# 注释行\n\n阿=ek\n厑=ib\n厑=ii\n李=mz\n河\n王= \nabc=xy\n");
        assert_eq!(t.first_code('阿'), Some("ek"));
        assert_eq!(t.codes_of('厑').collect::<Vec<_>>(), vec!["ib", "ii"]);
        assert_eq!(t.first_code('李'), Some("mz"));
        assert!(t.first_code('河').is_none(), "无 = 的行跳过");
        assert!(t.first_code('王').is_none(), "空码行跳过");
        assert!(t.first_code('a').is_none(), "非单字行跳过");
        assert_eq!(t.code_count(), 4, "阿1 + 厑2 + 李1 = 4 码");
    }

    /// 一行多码：`=` 右侧按空白拆成多个码（手心表 `七=a p` 的形态）。此前整段 `a p` 被当成
    /// 一个码，`p` 永远匹配不上。与「同字多码分列多行」等价，且两种写法可混用、跨行去重。
    #[test]
    fn parse_str_splits_whitespace_separated_codes() {
        let t = parse_str("七=a p\n八=b\tq  r\n七=p\n七=s\n");
        assert_eq!(t.codes_of('七').collect::<Vec<_>>(), vec!["a", "p", "s"]);
        assert_eq!(t.codes_of('八').collect::<Vec<_>>(), vec!["b", "q", "r"]);
    }

    /// 行内 `#` 注释在拆码之前剥掉：`七=a # 注` 只有码 `a`，不能把 `#`、`注` 也当成码。
    #[test]
    fn parse_str_strips_inline_comment_before_splitting() {
        let t = parse_str("七=a p # 手心\n八=b#注\n九= # 只有注释\n");
        assert_eq!(t.codes_of('七').collect::<Vec<_>>(), vec!["a", "p"]);
        assert_eq!(t.codes_of('八').collect::<Vec<_>>(), vec!["b"]);
        assert!(t.first_code('九').is_none(), "剥完注释是空码：整行跳过");
    }

    /// 非 UTF-8 文件（手心表常见 GBK）：告警「请转存为 UTF-8」并产出空表，不 panic、不乱码入表。
    #[test]
    fn load_from_file_non_utf8_returns_empty() {
        let path =
            std::env::temp_dir().join(format!("wind-aux-code-gbk-test-{}.txt", std::process::id()));
        // 「七=a」的 GBK 编码：C6 DF 3D 61。
        std::fs::write(&path, [0xC6u8, 0xDF, b'=', b'a', b'\n']).unwrap();
        let t = load_from_file(&path);
        let _ = std::fs::remove_file(&path);
        assert!(t.is_empty());
    }

    /// 同字重复码经 from_rows 的 first-seen 去重（load 路径同样适用）
    #[test]
    fn parse_str_dedups_within_char() {
        let t = parse_str("李=mz\n李=mz\n");
        assert_eq!(t.codes_of('李').collect::<Vec<_>>(), vec!["mz"]);
    }

    /// parse_str：UTF-8 BOM（Windows 记事本）剥掉，首行不丢
    #[test]
    fn parse_str_strips_utf8_bom() {
        let t = parse_str("\u{feff}阿=ek\n厑=ib\n");
        assert_eq!(t.first_code('阿'), Some("ek"), "带 BOM 的首行不应被跳过");
        assert_eq!(t.first_code('厑'), Some("ib"));
    }

    /// parse_str：第 1 行提取 `# name: X` / `#name: X`（`#` 后空格可有可无，空格/大小写宽容）
    #[test]
    fn parse_str_extracts_name_from_first_line() {
        let t = parse_str("# name: 笔画\n# version: 1.0\n阿=ek\n");
        assert_eq!(t.name, "笔画");
        assert_eq!(t.first_code('阿'), Some("ek"), "数据行照常解析");
        let t2 = parse_str("#name: 笔画\n阿=ek\n");
        assert_eq!(t2.name, "笔画", "# 后无空格也应解析");
        let t3 = parse_str("# name :  笔画  \n阿=ek\n");
        assert_eq!(t3.name, "笔画", "冒号/值两侧空白应 trim");
        let t4 = parse_str("# NAME: 笔画\n阿=ek\n");
        assert_eq!(t4.name, "笔画", "key 大小写不敏感");
    }

    /// parse_str：第 1 行不是名字 → 不解析元数据，name 留空
    #[test]
    fn parse_str_name_empty_when_first_line_not_name() {
        let t = parse_str("# 标题注释\n# version: 1.0\n阿=ek\n");
        assert_eq!(
            t.name, "",
            "第 1 行是普通注释 → 不读后面的 version，name 留空"
        );
        let t2 = parse_str("阿=ek\n");
        assert_eq!(t2.name, "", "第 1 行直接是数据 → name 留空");
    }

    /// load_from_file：第 1 行没写名字时回落文件主干名
    #[test]
    fn load_from_file_name_falls_back_to_file_stem() {
        let dir =
            std::env::temp_dir().join(format!("wind-aux-code-name-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let named = dir.join("stroke.txt");
        std::fs::write(&named, "# name: 笔画\n阿=ek\n").unwrap();
        let unnamed = dir.join("flypy_full.txt");
        std::fs::write(&unnamed, "阿=ek\n").unwrap();
        let t1 = load_from_file(&named);
        let t2 = load_from_file(&unnamed);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(t1.name, "笔画", "文件头名字优先");
        assert_eq!(
            t2.name, "flypy_full",
            "无名字回落文件主干名（含下划线原样）"
        );
    }

    /// 随仓库分发的首右形码表：能读出名字，且常用字有码。
    #[test]
    fn bundled_shouyou_tables_load() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/aux_code");
        for (file, name, ch, code) in [
            ("shouyou.txt", "首右 2.0", '啊', "kv"),
            ("shouyou_plus.txt", "首右 Plus", '的', "pb"),
        ] {
            let t = load_from_file(&dir.join(file));
            assert_eq!(t.name, name, "{file}");
            assert!(
                t.any_code_starts_with(ch, code),
                "{file}: {ch} 应有码 {code}"
            );
            assert!(t.char_count() > 5000, "{file} 字数");
        }
    }

    /// load_from_file：读取 txt 文件（`=` 格式，注释行跳过）
    #[test]
    fn load_from_file_reads_txt() {
        let path = std::env::temp_dir().join(format!(
            "wind-aux-code-load-test-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "阿=ek\n厑=ib\n# 注释\n").unwrap();
        let t = load_from_file(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(t.first_code('阿'), Some("ek"));
        assert_eq!(t.first_code('厑'), Some("ib"));
    }

    /// load_from_file：文件不存在 → 告警 + 空表（不 panic）
    #[test]
    fn load_from_file_missing_returns_empty() {
        let t = load_from_file(std::path::Path::new("C:/definitely_not_exist_aux_code.txt"));
        assert!(t.is_empty());
    }

    /// load_merged：多路径按序合并（先出现 = 高优）、跨文件同码去重、缺失文件跳过。
    /// 即协调器 `ensure_aux_code_runtime`（`aux_code_source.rs`）合并相邻文件来源的路径。
    #[test]
    fn load_merged_combines_files() {
        let dir =
            std::env::temp_dir().join(format!("wind-aux-code-merged-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hi = dir.join("high.txt");
        let lo = dir.join("low.txt");
        std::fs::write(&hi, "# name: 拆分\n李=mz\n河=sk\n").unwrap();
        std::fs::write(&lo, "李=mz\n河=dk\n樱=mn\n").unwrap();
        let missing = dir.join("missing.txt");
        let t = load_merged(&[hi.clone(), lo.clone(), missing]);
        for f in [&hi, &lo] {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_dir(&dir);
        assert_eq!(t.name, "拆分", "名称取首个非空（高优表）");
        assert_eq!(t.codes_of('李').collect::<Vec<_>>(), vec!["mz"]);
        assert_eq!(t.codes_of('河').collect::<Vec<_>>(), vec!["sk", "dk"]);
        assert_eq!(t.codes_of('樱').collect::<Vec<_>>(), vec!["mn"]);
    }

    #[test]
    fn read_name_reads_only_first_line() {
        let dir = std::env::temp_dir().join(format!("wind-aux-read-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, "# name: 小鹤\n# version: 1\n阿=ek\n").unwrap();
        let b = dir.join("b.txt");
        std::fs::write(&b, "阿=ek\n# name: 不算\n").unwrap();
        assert_eq!(read_name(&a).as_deref(), Some("小鹤"));
        assert_eq!(read_name(&b), None);
        assert_eq!(read_name(&dir.join("missing.txt")), None);
    }

    /// 行尾不挑食：同一份表无论 LF / CRLF / 孤立 CR，解析结果必须一致。
    ///
    /// 修之前带 `# name:` 头的 CR 文件整份被当成一条注释 → **0 字、无任何提示**；
    /// 不带头的则只认出 1 个字，且它的码是 `mz\r樱=my\r河=sk` 这种含 CR 的垃圾串。
    /// 出厂的 flypy_full.txt / stroke.txt 都带 `# name:` 头，所以前一种才是常态。
    ///
    /// ⚠️ 样本在代码里构造，不要改用仓库里的 fixture：git 的 core.autocrlf 会按平台
    /// 改写文件行尾，那样这条测的就是 git 配置而不是解析器。
    #[test]
    fn line_endings_do_not_change_the_result() {
        for lf in [
            "李=mz\n樱=my\n河=sk\n",
            "# name: 飞键\n李=mz\n樱=my\n河=sk\n",
        ] {
            let want = parse_str(lf);
            for (tag, text) in [
                ("CRLF", lf.replace('\n', "\r\n")),
                ("CR", lf.replace('\n', "\r")),
            ] {
                let got = parse_str(&text);
                assert_eq!(got.name, want.name, "{tag}: 方法名");
                for ch in ['李', '樱', '河'] {
                    assert_eq!(
                        got.codes_of(ch).collect::<Vec<_>>(),
                        want.codes_of(ch).collect::<Vec<_>>(),
                        "{tag}: {ch} 的码与 LF 版不同"
                    );
                }
            }
        }
    }
}
