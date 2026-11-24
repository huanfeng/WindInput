//! 配置读写动作（对照 Go funcs/config_funcs.go）。`config.get` 为纯函数（依赖配置状态，
//! 非确定），`config.set` / `config.toggle` 为副作用。经
//! [`ConfigService`](crate::services::ConfigService)，key 为 YAML 路径。

use super::action::check_enum;
use super::func_specs;
use super::util::{runtime_err, services};
use crate::context::EvalContext;
use crate::error::{CmdbarError, Result};
use crate::registry::FuncSpec;

/// `config.set` 的 `toast` 取值白名单。没有 `wind.cli` 那样的 `error` 档：
/// 写回失败的提示由 cmdbar 的错误通路负责（与 `toast` 无关地照常弹），这里只管
/// **成功**那条「设置已更新」。
const SET_TOAST_MODES: &[&str] = &["on", "off"];

pub fn specs() -> Vec<FuncSpec> {
    func_specs! {
        "config.get"   : Config (1, 1) pure   => fn_get,    "读取配置项当前值; key 为 YAML 路径 (如 ui.candidate.layout)", "config.get(\"ui.theme.style\")";
        "config.set"   : Config (2, 2) effect => fn_set,    "设置配置项并持久化; key 为 YAML 路径, value 为字符串", "config.set(\"ui.theme.style\", \"dark\")"
            named(fn_set_named,
                  "toast" = "写回并热重载成功后的提示: on(默认,弹「设置已更新」)/off; 一条命令里后面还有自己的提示(如 wind.cli(…, ok=…))时设 off");
        "config.toggle": Config (1, 1) effect => fn_toggle, "枚举循环切换 / bool 翻转, 持久化并返回新值", "config.toggle(\"ui.theme.style\")";
    }
}

fn fn_get(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("config.get", ctx)?;
    let config = s
        .config
        .as_ref()
        .ok_or_else(|| CmdbarError::service("config.get"))?;
    config
        .get(&args[0])
        .map_err(|e| runtime_err("config.get", e))
}

fn fn_set(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    fn_set_named(ctx, args, &[])
}

/// `config.set(key, value, toast="on|off")`。
fn fn_set_named(
    ctx: &dyn EvalContext,
    args: &[String],
    named: &[(String, String)],
) -> Result<String> {
    let s = services("config.set", ctx)?;
    let config = s
        .config
        .as_ref()
        .ok_or_else(|| CmdbarError::service("config.set"))?;
    let toast = named_val(named, "toast");
    check_enum("config.set", "toast", toast, SET_TOAST_MODES)?;
    config
        .set_quiet(&args[0], &args[1], toast == "off")
        .map_err(|e| runtime_err("config.set", e))?;
    Ok(String::new())
}

fn fn_toggle(ctx: &dyn EvalContext, args: &[String]) -> Result<String> {
    let s = services("config.toggle", ctx)?;
    let config = s
        .config
        .as_ref()
        .ok_or_else(|| CmdbarError::service("config.toggle"))?;
    config
        .toggle(&args[0])
        .map_err(|e| runtime_err("config.toggle", e))
}

/// 取具名参数值；名字白名单已在 [`crate::eval`] 的 `call_func` 校验过，此处只取。
fn named_val<'a>(named: &'a [(String, String)], key: &str) -> &'a str {
    named
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
