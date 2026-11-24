//! 改动方案文件 / 方案 override 的每个 RPC 落盘后都必须重建吃键集。
//!
//! 吃键集只在 `ConfigBundle` 构建时算一次，其跨方案并集读的正是方案文件与 override；这几个 RPC
//! 不经 `config.setItems`，漏接 `refresh_schema_derived_config` 就是「改了要重启才生效」且零日志。
//! 用源码守卫而非端到端：这些 RPC 写真实用户目录（测试进程改不了）。重建本身的正确性由
//! wind-coordinator 的 `schema_punct_custom_mixed.rs` 覆盖，这里只钉「每个入口都接上了」。

/// 取 `fn <name>(` 起到其首个 4 格缩进的闭合 `}` 为止的函数体（trait 默认方法都在 4 格
/// 缩进上闭合）。只扫文件中第一个 `#[cfg(test)]` 之前的部分，函数挪到那之后会报「找不到」而非假绿。
fn fn_body(name: &str) -> &'static str {
    let src = include_str!("lib.rs");
    let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
    let head = format!("fn {name}(");
    let start = prod.find(&head).unwrap_or_else(|| {
        panic!("找不到 {head}——改名了，或挪到了第一个 #[cfg(test)] 之后？同步更新本守卫")
    });
    let rest = &prod[start + head.len()..];
    let end = rest
        .find("\n    }\n")
        .unwrap_or_else(|| panic!("{head} 找不到函数体的闭合行"));
    &rest[..end]
}

#[test]
fn schema_mutating_rpcs_refresh_key_gate() {
    for name in [
        "web_schema_save_config",
        "web_schema_reset_config",
        "web_schema_delete",
        "web_scheme_import_package",
        "web_scheme_import_text",
        "web_backup_restore",
    ] {
        assert!(
            fn_body(name).contains("refresh_schema_derived_config()"),
            "{name} 改了方案文件 / override，却没重建吃键集（GH#144 同根）"
        );
    }
}

/// CLI `schema set/reset` 写完 override 后调的 `schema.invalidate` 在分派表里就地实现。
#[test]
fn schema_invalidate_arm_refreshes_key_gate() {
    let src = include_str!("lib.rs");
    let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
    let start = prod
        .find("\"schema.invalidate\" =>")
        .expect("分派表里找不到 schema.invalidate");
    let arm_len = prod[start..]
        .find("\n            }")
        .expect("schema.invalidate 臂不再是块形式？同步更新本守卫");
    let arm = &prod[start..start + arm_len];
    assert!(arm.contains("refresh_schema_derived_config()"));
}
