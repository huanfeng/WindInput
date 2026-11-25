//! 方案设置折叠：把方案段里的稀疏补丁叠到类型内置默认上，得到该方案的生效设置。
//!
//! 见 `docs/design/scheme-settings-flatten.md`。补丁的键名与类型结构体字段同名，逐叶子按
//! 注册表校验；不合格的叶子丢弃并记入问题清单，其余照常生效。

use serde::{Serialize, de::DeserializeOwned};

/// 把 `patch` 的合法叶子叠到 `base` 上。
///
/// `registry_prefix` 是这些键在注册表里的前缀（如 `schema.pinyin`），用来查类型与值域；
/// `skip` 里的顶层键不接受方案级设置。
pub(crate) fn fold<T: Serialize + DeserializeOwned + Clone>(
    base: &T,
    patch: &toml::Table,
    registry_prefix: &str,
    skip: &[&str],
) -> (T, Vec<String>) {
    let mut problems = Vec::new();
    if patch.is_empty() {
        return (base.clone(), problems);
    }
    let Ok(toml::Value::Table(mut tree)) = toml::Value::try_from(base) else {
        return (base.clone(), problems);
    };
    let mut touched = false;
    for (path, value) in crate::config_schema::leaf_entries(&toml::Value::Table(patch.clone())) {
        let top = path.split('.').next().unwrap_or("");
        if skip.contains(&top) {
            problems.push(format!("{path}（该项不支持按方案设置）"));
            continue;
        }
        if let Err(e) = crate::config_schema::validate(&format!("{registry_prefix}.{path}"), &value)
        {
            problems.push(format!("{path}（{e}）"));
            continue;
        }
        set_leaf(&mut tree, &path, value);
        touched = true;
    }
    if !touched {
        return (base.clone(), problems);
    }
    match toml::Value::Table(tree).try_into() {
        Ok(v) => (v, problems),
        Err(e) => {
            problems.push(format!("方案设置整体无法解析（{e}）"));
            (base.clone(), problems)
        }
    }
}

fn set_leaf(tree: &mut toml::Table, path: &str, value: toml::Value) {
    match path.split_once('.') {
        None => {
            tree.insert(path.to_string(), value);
        }
        Some((head, rest)) => {
            let child = tree
                .entry(head.to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !child.is_table() {
                *child = toml::Value::Table(toml::Table::new());
            }
            if let toml::Value::Table(t) = child {
                set_leaf(t, rest, value);
            }
        }
    }
}

#[cfg(test)]
#[path = "scheme_fold_tests.rs"]
mod tests;
