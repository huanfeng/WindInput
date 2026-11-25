//! 单个方案的完整设置表单（手机设置页「方案设置」）：与电脑版 RPC `schema.getSettings` /
//! `schema.setSettings` 同口径，键为旧全局键名 `schema.<类型>.<项>`。
//!
//! 只包 `EngineManager::scheme_settings_snapshot` / `scheme_settings_apply`，不另起合并逻辑。
//! 方案属性类的项（双拼布局、辅助码来源、标点、命令短语、整句输入）仍走 [`crate::schema_settings`]。
//! 两边写同一个 `schema_overrides/<id>.toml`，宿主用同一把锁串行。

use wind_coordinator::web_host::WebDataHost;

use crate::MobileCore;
use crate::config_api::ConfigValue;

/// 一个方案的设置表单：三张扁平表，序列化成 TOML 文本（顶层键是完整点分键，
/// 如 `"schema.pinyin.sentence_count" = 3`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SchemeForm {
    /// 现在按什么跑。
    pub effective: String,
    /// 方案默认值：恢复默认后的值。
    pub default: String,
    /// 方案文件改过的项在类型内置默认里的值（只含与方案默认不同的项）。
    pub builtin: String,
    /// 混输方案的成员：`[主成员（码表）, 次成员（拼音）]`；其它方案为空。
    pub members: Vec<String>,
}

impl MobileCore {
    /// 方案的完整设置表单。未知方案或没有可设置项的方案 → Err。
    pub fn scheme_form(&self, schema_id: &str) -> Result<SchemeForm, String> {
        let mgr = self.coord.engine_mgr();
        let snap = mgr
            .scheme_settings_snapshot(schema_id)
            .ok_or_else(|| format!("方案不存在或没有可设置的项: {schema_id}"))?
            .map_err(|e| format!("读取方案 {schema_id} 的设置失败: {e}"))?;
        let text = |t: &toml::Table| toml::to_string(t).map_err(|e| format!("序列化失败: {e}"));
        let members = mgr
            .schema_merged(schema_id)
            .map(|s| {
                let m = &s.engine.mixed;
                [&m.primary_schema, &m.secondary_schema]
                    .into_iter()
                    .filter(|id| !id.is_empty())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Ok(SchemeForm {
            effective: text(&snap.effective)?,
            default: text(&snap.default)?,
            builtin: text(&snap.builtin)?,
            members,
        })
    }

    /// 改方案的设置：`sets` 逐项写入、`resets` 逐项恢复方案默认。值按注册表校验，有一项不合格
    /// 整批不写。生效要另发 [`crate::MobileCommand::ReloadConfig`]。
    pub fn scheme_form_apply(
        &self,
        schema_id: &str,
        sets: Vec<(String, ConfigValue)>,
        resets: Vec<String>,
    ) -> Result<(), String> {
        self.ensure_writable()?;
        let sets: Vec<(String, toml::Value)> =
            sets.into_iter().map(|(k, v)| (k, v.into_toml())).collect();
        for (key, value) in &sets {
            wind_config::config_schema::validate(key, value).map_err(|e| format!("{key}: {e}"))?;
        }
        self.coord
            .engine_mgr()
            .scheme_settings_apply(schema_id, sets, &resets)
            .map_err(|e| e.to_string())?;
        self.coord.refresh_schema_derived_config();
        Ok(())
    }
}

/// 表单读写门面：全拼 / 双拼各一份、恢复不动覆盖层别的内容、方案预设、停用方案、坏文件拒写、
/// 类型不符拒写、混输成员。取值折叠本身在 wind-engine 的 manager 测试里验。
#[cfg(test)]
#[path = "scheme_form_tests.rs"]
mod tests;
