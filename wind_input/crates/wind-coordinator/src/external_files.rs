//! 外部改了用户目录文件之后让核心重读（数据同步、手改）。
//!
//! 桌面设置页 RPC（`wind-webdata` 的 `dict.reload` / `data.beginEdit` 等）与移动端同步门面共用
//! 这一份，行为不会分叉。入参是 [`WebDataHost`] 窄面，不碰输入态。

use std::time::Duration;

use anyhow::{Result, anyhow};
use wind_store::external_edit::{EditResult, EditSession};

use crate::web_host::WebDataHost;

/// 用户词文本手改后重新载入：带 `paths` 只同步这几个文件，缺省全量扫描。返回变了的项。
pub fn reload_dict_files<H: WebDataHost + ?Sized>(
    h: &H,
    paths: Option<&[String]>,
) -> Result<Vec<String>> {
    let store = h.user_store().ok_or_else(|| anyhow!("无持久化存储"))?;
    let reloaded = match paths {
        Some(paths) => store.reload_user_text_paths(paths)?,
        None => {
            let mut r = store.sync_user_texts();
            // 用户分库：比对已建引擎记下的指纹，变了才失效
            r.extend(
                h.engine_mgr()
                    .user_dicts_changed()
                    .into_iter()
                    .map(|ds| format!("{}{ds}", wind_store::user_words::USER_DICTS_CHANGED)),
            );
            r
        }
    };
    // 变了的项重建对应的内存镜像（短语层、快捷格式、常用字、字符集）
    h.apply_user_text_changes(&reloaded);
    Ok(reloaded)
}

/// 开始外部编辑 `path`（相对数据目录）：暂停该文件写回。只读模式拒绝。
pub fn begin_edit<H: WebDataHost + ?Sized>(h: &H, path: &str) -> Result<EditSession> {
    if let Some(r) = h.storage_readonly_reason() {
        anyhow::bail!("{r}");
    }
    let store = h.user_store().ok_or_else(|| anyhow!("无持久化存储"))?;
    store.begin_edit(path)
}

/// 结束外部编辑：按文件载入、恢复写回，并重建变了的内存镜像。
pub fn end_edit<H: WebDataHost + ?Sized>(h: &H, path: &str) -> Result<EditResult> {
    let store = h.user_store().ok_or_else(|| anyhow!("无持久化存储"))?;
    let r = store.end_edit(path)?;
    h.apply_user_text_changes(&r.reloaded);
    Ok(r)
}

/// 进行中的编辑会话：(相对数据目录的文件, 距开始多久)。
pub fn edit_sessions<H: WebDataHost + ?Sized>(h: &H) -> Vec<(String, Duration)> {
    h.user_store()
        .map(|s| s.edit_sessions())
        .unwrap_or_default()
}

/// 方案文件被外部改了：失效这些方案（缺省 = 全部已安装）的引擎与解析缓存，返回方案 id。
pub fn reload_schema_files<H: WebDataHost + ?Sized>(
    h: &H,
    ids: Option<Vec<String>>,
) -> Vec<String> {
    let ids = ids.unwrap_or_else(|| h.engine_mgr().installed_schemas());
    for id in &ids {
        h.engine_mgr().invalidate_schema(id);
    }
    h.refresh_schema_derived_config();
    ids
}

/// 主题文件被外部改了：对每个 id 调 `forget`（丢预览图缓存等宿主私有缓存）；
/// 当前生效主题在其中（或缺省 `ids`）时重读并下发。返回 `(ids, 是否重新下发)`。
pub fn reload_theme_files<H: WebDataHost + ?Sized>(
    h: &H,
    ids: Option<Vec<String>>,
    forget: &dyn Fn(&str),
) -> Result<(Vec<String>, bool)> {
    let current = h.current_theme_name();
    let (ids, all) = match ids {
        Some(ids) => {
            if let Some(bad) = ids.iter().find(|i| !wind_theme::is_plain_slug(i)) {
                anyhow::bail!("非法主题 id: {bad}");
            }
            (ids, false)
        }
        None => (
            h.list_themes_full().into_iter().map(|t| t.0).collect(),
            true,
        ),
    };
    for id in &ids {
        forget(id);
    }
    let repush = all || ids.contains(&current);
    if repush {
        h.push_theme(&current, h.current_theme_is_dark());
    }
    Ok((ids, repush))
}
