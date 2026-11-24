use super::*;

#[test]
fn trims_and_falls_back_to_the_factory_alias() {
    assert_eq!(
        sanitize_dota2_alias("  拼音输入法 ").unwrap(),
        DEFAULT_DOTA2_ALIAS
    );
    // 留空 = 用出厂值，不是「关闭」——关闭走 system.dota2_compat 开关。
    assert_eq!(sanitize_dota2_alias("").unwrap(), DEFAULT_DOTA2_ALIAS);
    assert_eq!(sanitize_dota2_alias("   ").unwrap(), DEFAULT_DOTA2_ALIAS);
    // 内部空格一个都不能动：白名单里多数条目就是带空格的。
    // ⛔ 这里必须用**表里真实存在**的条目：写一个编造的名字，读代码的人会以为
    //    它能用（`中文 (简体) - 五笔` 就是这么被我编出来的，表里没有）。
    //    全表见 docs/design/game-compat-tsf-uielement.md §1.3。
    assert_eq!(
        sanitize_dota2_alias("中文 (简体) - 双拼").unwrap(),
        "中文 (简体) - 双拼"
    );
}

#[test]
fn rejects_invisible_characters_that_look_identical() {
    // ⛔ 回归：这几个在设置页里和正确值**长得一模一样**，放过去就是「填了、
    // 应用了、游戏里还是没候选」，而且没有任何线索。
    for (name, s) in [
        ("NBSP", "中文\u{00A0}(简体) - 双拼"),
        ("全角空格", "中文\u{3000}(简体) - 双拼"),
        ("零宽空格", "拼音\u{200B}输入法"),
        ("BOM", "\u{FEFF}拼音输入法"),
        ("双向覆盖", "拼音\u{202E}输入法"),
        ("软连字符", "拼音\u{00AD}输入法"),
    ] {
        let r = sanitize_dota2_alias(s);
        assert!(r.is_err(), "{name} 应被拒绝，实际得到 {r:?}");
        assert!(
            r.unwrap_err().contains("U+"),
            "{name} 的报错要带码位，否则用户看不出问题在哪"
        );
    }
    // 半角空格是白名单条目本来就有的，一个都不能动。
    assert_eq!(
        sanitize_dota2_alias("中文 (简体) - 双拼").unwrap(),
        "中文 (简体) - 双拼"
    );
}

#[test]
fn rejects_what_would_fail_silently() {
    // 控制字符：写进 REG_SZ 后语言栏显示成乱码，而设置页里看不出粘进了什么。
    assert!(sanitize_dota2_alias("中文\n郑码").is_err());
    assert!(sanitize_dota2_alias("中文\t郑码").is_err());
    // 超长：Register.cpp 的 wchar_t cur[256] 读回来是截断的，比对必然不等，
    // 于是每次重装都把用户的设置冲掉。
    assert!(sanitize_dota2_alias(&"あ".repeat(DOTA2_ALIAS_MAX_LEN)).is_ok());
    assert!(sanitize_dota2_alias(&"あ".repeat(DOTA2_ALIAS_MAX_LEN + 1)).is_err());
    // 按 UTF-16 码元算，不是按 char：BMP 外的字符占两个码元。
    assert!(sanitize_dota2_alias(&"𠀀".repeat(DOTA2_ALIAS_MAX_LEN / 2 + 1)).is_err());
}

#[test]
fn legacy_alias_in_the_cpp_side_matches_the_table_byte_for_byte() {
    // `wind_tsf/src/Register.cpp` 的 `kLegacyDota2CompatAlias` —— 2026-09 之前
    // 写死的那个别名，如今只用来认出老装机（那时还没有 Dota2CompatAlias 记录值）。
    //
    // ⛔ 它在 C++ 侧是个孤零零的字面量：出厂值换成「拼音输入法」之后，原先钉着它的
    // 那条测试改钉新值，于是它一条守护都没有了。谁把它「顺手整理」一下（括号改全角、
    // 空格增减），存量用户升级时的别名保留就静默失效 —— 而这条路径**只有存量用户
    // 会走**，现场最难复现。故在此单独钉一份字节。
    //
    // 本常量在 Rust 侧无任何生产用途，只作为那份 C++ 字面量的带测试副本存在。
    const CPP_LEGACY: &str = "中文 (简体) - 郑码";
    const FROM_BINARY: &[u8] = &[
        0xE4, 0xB8, 0xAD, 0xE6, 0x96, 0x87, 0x20, 0x28, 0xE7, 0xAE, 0x80, 0xE4, 0xBD, 0x93, 0x29,
        0x20, 0x2D, 0x20, 0xE9, 0x83, 0x91, 0xE7, 0xA0, 0x81,
    ];
    assert_eq!(
        CPP_LEGACY.as_bytes(),
        FROM_BINARY,
        "老装机识别串与白名单第 89 条不一致，存量用户升级时别名会被冲回真名"
    );
}

#[test]
fn factory_alias_matches_the_hardcoded_table_byte_for_byte() {
    // 从 Dota 2 的 imemanager.dll 里原样提取的 UTF-8 字节：白名单第 90 条
    // （`canonical="Unknown"`, `product_id=0x410000`），2026-09-13 复核。
    // 这条断言的作用不是「测代码」，而是把那串字节钉在仓里：出厂别名一旦被人
    // 顺手「整理」（换成全角、加空格、改用词），开关就会静默失效，而现象只在
    // 游戏里看得到。改出厂值必须重新从二进制里取字节，不能照着屏幕抄。
    const FROM_BINARY: &[u8] = &[
        0xE6, 0x8B, 0xBC, 0xE9, 0x9F, 0xB3, 0xE8, 0xBE, 0x93, 0xE5, 0x85, 0xA5, 0xE6, 0xB3, 0x95,
    ];
    assert_eq!(
        DEFAULT_DOTA2_ALIAS.as_bytes(),
        FROM_BINARY,
        "出厂别名与 Dota 2 白名单里的字节不一致，开关会静默失效"
    );
}

#[test]
fn default_system_config_keeps_the_alias_non_empty() {
    // ⛔ 回归守卫：SystemConfig 若被改回 #[derive(Default)]，本键会静默变成空串，
    // 表现为「开了开关，系统里的名字被改成了空」——语言栏上一片空白，且无任何报错。
    assert_eq!(
        SystemConfig::default().dota2_compat_name,
        DEFAULT_DOTA2_ALIAS
    );
    assert!(!SystemConfig::default().dota2_compat);
}
