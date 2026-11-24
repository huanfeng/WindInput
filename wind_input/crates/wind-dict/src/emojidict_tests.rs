use super::*;

fn rows(v: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
    v.iter()
        .map(|(t, es)| {
            (
                t.to_string(),
                es.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn write_read(v: &[(&str, &[&str])]) -> (tempdir::Guard, EmojiReader) {
    let g = tempdir::Guard::new("wemj");
    let p = g.path().join("t.wemj");
    write_emoji_wemj(&p, &rows(v)).unwrap();
    let r = EmojiReader::open(&p).unwrap();
    (g, r)
}

/// 写入 → mmap 读回 → 精确点查，三段来回一致。
#[test]
fn roundtrip_lookup() {
    let (_g, r) = write_read(&[("你好", &["😊", "👋"]), ("足球", &["⚽"])]);
    assert_eq!(r.entry_count(), 2);
    assert_eq!(r.lookup("你好"), Some("😊 👋"));
    assert_eq!(r.lookup("足球"), Some("⚽"));
    assert_eq!(r.lookup("没有"), None, "未收录的词返回 None");
}

/// 同键必须合并，不是后者覆盖前者。
///
/// 场景取自两张上游表的同键（`emoji_word.txt` × `emoji_category.txt`）：「奖项」在 word 表只有
/// `🏅`，在 category 表是一整组，覆盖式实现会丢掉一整组。
/// 别拿繁→简撞键来验这条：那些组恰好是异体字指向同一 emoji，覆盖与合并结果相同。
#[test]
fn colliding_keys_merge_not_overwrite() {
    let (_g, r) = write_read(&[("奖项", &["🏅"]), ("奖项", &["🏅", "🎖️", "🥇"])]);
    assert_eq!(r.entry_count(), 1, "同键合并成一条");
    assert_eq!(
        r.lookup("奖项"),
        Some("🏅 🎖️ 🥇"),
        "两侧 emoji 都在、保序、且重复项去掉"
    );
}

/// 合并保序：先到的排前面（与 `sort_by` 的稳定性一同构成「先到先得」）。
#[test]
fn merge_preserves_first_seen_order() {
    let (_g, r) = write_read(&[("x", &["1️⃣", "2️⃣"]), ("x", &["3️⃣", "1️⃣"])]);
    assert_eq!(r.lookup("x"), Some("1️⃣ 2️⃣ 3️⃣"));
}

/// 上游格式：值的首项是键本身，必须按位置丢弃。
#[test]
fn parse_upstream_drops_first_value() {
    let got = parse_upstream("一個人\t一個人 👤\nID\tID 🆔 🪪\n", str::to_string);
    assert_eq!(
        got,
        vec![
            ("一個人".to_string(), vec!["👤".to_string()]),
            ("ID".to_string(), vec!["🆔".to_string(), "🪪".to_string()]),
        ]
    );
}

/// 归一后键与首项不再相等：若把「丢首项」误写成「过滤等于键的项」，繁体首项会被当成
/// emoji 收进去。本例即那个错误实现的指纹。
#[test]
fn parse_upstream_drops_by_position_not_by_equality() {
    // 归一：把「個」换成「个」，于是键 = 一个人，而值首项仍是 一個人。
    let norm = |s: &str| s.replace('個', "个");
    let got = parse_upstream("一個人\t一個人 👤\n", norm);
    assert_eq!(got, vec![("一个人".to_string(), vec!["👤".to_string()])]);
}

/// 空行、注释、无 tab 行、无 emoji 行一律跳过，不产出空条目。
#[test]
fn parse_upstream_skips_malformed() {
    let got = parse_upstream(
        "\n# 注释\n没有制表符\n只有键\t只有键\n好\t好 🙂\n",
        str::to_string,
    );
    assert_eq!(got, vec![("好".to_string(), vec!["🙂".to_string()])]);
}

/// 空表可写可读（功能出厂关闭时的正常状态，不该是错误）。
#[test]
fn empty_table_is_valid() {
    let (_g, r) = write_read(&[]);
    assert!(r.is_empty());
    assert_eq!(r.lookup("任何"), None);
}

/// 截断的缓存文件不 panic：要么开不了，要么查不到，都不许崩。
#[test]
fn truncated_file_does_not_panic() {
    let g = tempdir::Guard::new("wemj-trunc");
    let p = g.path().join("t.wemj");
    write_emoji_wemj(&p, &rows(&[("你好", &["😊"])])).unwrap();
    let full = std::fs::read(&p).unwrap();
    // 砍掉字符串池的一半：头部与索引仍然自洽，但 row() 会越界。
    std::fs::write(&p, &full[..full.len() - 3]).unwrap();
    // 开不了也是可接受的降级；开得了就必须查不崩。
    if let Ok(r) = EmojiReader::open(&p) {
        let _ = r.lookup("你好");
        let _ = r.lookup("");
    }
}

// ── load_or_build（建缓存编排）─────────────────────────────────────────

/// 多张表按顺序合并：精确词表的 emoji 排在分类表之前。
#[test]
fn build_merges_tables_in_order() {
    let g = tempdir::Guard::new("wemj-order");
    let (w, c) = (g.path().join("word.txt"), g.path().join("cat.txt"));
    std::fs::write(&w, "奖项\t奖项 🏅\n").unwrap();
    std::fs::write(&c, "奖项\t奖项 🎖️ 🥇\n").unwrap();
    let r = load_or_build(&[w, c], &[], &g.path().join("e.wemj"), &[], None).expect("应建成");
    assert_eq!(
        r.lookup("奖项"),
        Some("🏅 🎖️ 🥇"),
        "word 表在前，分类表在后"
    );
}

/// 不存在的表跳过（分类表可选），不算失败。
#[test]
fn build_skips_missing_table() {
    let g = tempdir::Guard::new("wemj-missing");
    let w = g.path().join("word.txt");
    std::fs::write(&w, "好\t好 🙂\n").unwrap();
    let r = load_or_build(
        &[w, g.path().join("不存在.txt")],
        &[],
        &g.path().join("e.wemj"),
        &[],
        None,
    )
    .expect("缺可选表不该失败");
    assert_eq!(r.lookup("好"), Some("🙂"));
}

/// 全部表都读不到 ⇒ None，且不留下缓存文件（不能让空表被后续当成新鲜缓存复用）。
#[test]
fn build_returns_none_when_no_source() {
    let g = tempdir::Guard::new("wemj-none");
    let cache = g.path().join("e.wemj");
    assert!(load_or_build(&[g.path().join("无.txt")], &[], &cache, &[], None).is_none());
    assert!(!cache.exists(), "没建成就不该留下缓存文件");
}

/// 归一表必须参与指纹：`fp_extra` 变了就得重建。
///
/// 归一表不是要解析的词表，很容易漏放进 sources；漏了的话 OpenCC 数据升级后会永久复用按
/// 旧归一表建的缓存，表现为换了转换表某些词仍打不出 emoji，而源文件确实没动过。
#[test]
fn rebuild_when_normalizer_source_changes() {
    let g = tempdir::Guard::new("wemj-fp");
    let (w, norm_tbl) = (g.path().join("word.txt"), g.path().join("t2s.bin"));
    let cache = g.path().join("e.wemj");
    std::fs::write(&w, "個\t個 👤\n").unwrap();
    std::fs::write(&norm_tbl, b"v1").unwrap();

    // 第一次：归一为恒等 ⇒ 键是「個」。
    let r = load_or_build(
        std::slice::from_ref(&w),
        std::slice::from_ref(&norm_tbl),
        &cache,
        &[],
        Some(&|s: &str| s.to_string()),
    )
    .unwrap();
    assert_eq!(r.lookup("個"), Some("👤"));
    drop(r); // 释放 mmap，Windows 上 rename 覆盖才不会 Access Denied

    // 只改归一表内容（源词表一个字节没动），并换用真的会归一的闭包。
    std::fs::write(&norm_tbl, b"v2").unwrap();
    let r2 = load_or_build(
        &[w],
        &[norm_tbl],
        &cache,
        &[],
        Some(&|s: &str| s.replace('個', "个")),
    )
    .unwrap();
    assert_eq!(r2.lookup("个"), Some("👤"), "指纹变了 ⇒ 重建 ⇒ 归一生效");
    assert_eq!(r2.lookup("個"), None, "旧键不该还在");
}

/// 一般 OpenCC 表：首项等于键才丢（不带重复首项的用户表全收），`---` 行是注释。
#[test]
fn parse_opencc_drops_first_only_when_it_is_the_key() {
    let got = parse_opencc("----------注释\t----------\n好\t好 👌\n猫猫\t🐱 😺\n空\t空\n");
    assert_eq!(
        got,
        vec![
            ("好".to_string(), vec!["👌".to_string()]),
            ("猫猫".to_string(), vec!["🐱".to_string(), "😺".to_string()]),
        ]
    );
}

/// 按分组取行：标题行不进结果；`（太短了不启用了）` 这类被注释掉的词条不是标题；
/// 第一个标题之前的行不属于任何组。空分组表 = 整表。
#[test]
fn select_sections_keeps_only_named_groups() {
    let src = "前言\t前言 x\n\
                   ----------偏旁部首\t----------\n\
                   提手旁\t提手旁 扌\n\
                   ----------（太短了不启用了）派\t派 pi π\n\
                   土部\t土部 土\n\
                   ----------著名表演艺术家\t----------\n\
                   某某\t某某 x\n\
                   ----------希腊字母\t----------\n\
                   贝塔\t贝塔 beta β\n";
    let got = select_sections(src, &["偏旁部首".into(), "希腊字母".into()]);
    assert_eq!(got, "提手旁\t提手旁 扌\n土部\t土部 土\n贝塔\t贝塔 beta β\n");
    assert_eq!(select_sections(src, &[]), src);
}

/// 同一份源按不同分组建的缓存互不冒认：分组进了指纹 tag。
#[test]
fn sections_are_part_of_cache_fingerprint() {
    let g = tempdir::Guard::new("emoji-sections");
    let src = g.path().join("others.txt");
    std::fs::write(
        &src,
        "----------甲组\t----------\n甲\t甲 🅰\n----------乙组\t----------\n乙\t乙 🅱\n",
    )
    .unwrap();
    let cache = g.path().join("m.wemj");
    let a = load_or_build(
        std::slice::from_ref(&src),
        &[],
        &cache,
        &["甲组".into()],
        None,
    )
    .unwrap();
    assert_eq!(a.lookup("甲"), Some("🅰"));
    assert_eq!(a.lookup("乙"), None);
    drop(a);
    let b = load_or_build(&[src], &[], &cache, &["乙组".into()], None).unwrap();
    assert_eq!(
        b.lookup("乙"),
        Some("🅱"),
        "换了分组必须重建，不能复用甲组的缓存"
    );
    assert_eq!(b.lookup("甲"), None);
}

/// 源与指纹都没变时复用缓存，不重建。
///
/// 重建会写出完全相同的文件，「缓存大小/mtime 没变」对「每次都重建」的实现没有判断力。
/// 这里偷换缓存内容：指纹只按源文件算，缓存仍判新鲜；查到替换后的内容即证明走的是缓存。
#[test]
fn reuses_fresh_cache() {
    let g = tempdir::Guard::new("wemj-fresh");
    let w = g.path().join("word.txt");
    let cache = g.path().join("e.wemj");
    std::fs::write(&w, "好\t好 🙂\n").unwrap();
    let r = load_or_build(std::slice::from_ref(&w), &[], &cache, &[], None).unwrap();
    assert_eq!(r.lookup("好"), Some("🙂"));
    drop(r); // 释放 mmap，Windows 上才能覆盖

    // 换成一份内容不同的合法缓存（源文件与 .fp 都不动）。
    write_emoji_wemj(&cache, &rows(&[("好", &["🎯"])])).unwrap();
    let r2 = load_or_build(&[w], &[], &cache, &[], None).unwrap();
    assert_eq!(r2.lookup("好"), Some("🎯"), "走的是缓存，没有重建");
}

/// 极小的临时目录守卫：测试结束即删，避免污染工作区。
mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    pub struct Guard(PathBuf);

    impl Guard {
        pub fn new(tag: &str) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p =
                std::env::temp_dir().join(format!("windinput-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Guard(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// 行尾不挑食：emoji 表带 `#` 注释头时，孤立 `\r` 会让整份被当成一条注释 → 0 条。
/// 样本在代码里构造，不要改用仓库 fixture（git core.autocrlf 会按平台改行尾）。
#[test]
fn line_endings_do_not_change_the_result() {
    let lf = "# emoji 表\n笑脸\t笑脸 😀\n大笑\t大笑 😂 🤣\n";
    let want = format!("{:?}", parse_upstream(lf, str::to_string));
    for (tag, text) in [
        ("CRLF", lf.replace('\n', "\r\n")),
        ("CR", lf.replace('\n', "\r")),
    ] {
        let got = format!("{:?}", parse_upstream(&text, str::to_string));
        assert_eq!(got, want, "{tag}: 结果与 LF 版不同");
        assert!(got.contains("😀"), "{tag}: 应解析出条目，实际 {got}");
    }
}
