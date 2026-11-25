//! 用户目录分类守卫：新增用户目录文件时在此登记分类。

use wind_sync::scope::{Class, classify, in_scope};

#[test]
fn every_known_layout_entry_is_classified() {
    let table: &[(Class, &[&str])] = &[
        (
            Class::Setting,
            &[
                "config.toml",
                "compat.toml",
                "schema_overrides/wubi86.toml",
                "data/pinyin/dicts.toml",
                "wind_setting.toml",
                "wind_setting_hide.toml",
            ],
        ),
        (
            Class::Resource,
            &[
                "schemas/wubi86.schema.toml",
                "schemas/comments/test.dict.yaml",
                "themes/a/theme.toml",
                "extensions/keysound/extension.toml",
                "extensions/keysound/keysound.exe",
                "system.softkeyboard.toml",
            ],
        ),
        (
            Class::Data,
            &[
                "data/pinyin/user.dict.yaml",
                "data/pinyin/shadow.dict.yaml",
                "data/pinyin/dicts/x.dict.yaml",
                "data/phrases.dict.yaml",
                "data/quick_format.toml",
                "data/pinyin_misread.yaml",
                "data/charsets/common_han.yaml",
                "emoji/a.txt",
            ],
        ),
        (
            Class::Excluded,
            &[
                "meta.toml",
                "screenshots/a.png",
                "charsets/pinyin.toml",
                "data/.backup/x",
                "data/phrases.sys_hash",
                "data/pinyin/temp.dict.yaml",
                "data/pinyin/freq.table.yaml",
                "data/toli/assoc.table.yaml",
                "data/toli/draft.dict.yaml",
                "data/stats/2026-11.toml",
                "data/completion.table.yaml",
                "schemas/kf.schema.toml.bak-overlay",
                "config.conflict-a-2026-11-24.toml",
                "themes/a/x.tmp",
            ],
        ),
    ];
    for (want, paths) in table {
        for p in *paths {
            assert_eq!(classify(p), *want, "{p}");
            assert_eq!(in_scope(p), *want != Class::Excluded, "{p}");
        }
    }
}
