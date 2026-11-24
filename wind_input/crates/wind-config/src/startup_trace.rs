//! 启动阶段轨迹：绕开 tracing，每次调用都同步落盘。
//!
//! 主日志可能失效：`tracing_appender::non_blocking` 的 worker 线程出事后，lossy 模式会
//! 静默丢弃其后的日志，主日志停在某一行而进程仍在运行，易被误读成「卡在那一行」。
//!
//! 本模块每次调用 open→write→flush→close，不经 tracing、不常驻句柄。只在启动路径与
//! 故障分支上调用，不得进入按键热路径。每行带 pid，用于判断究竟起了几个进程。
//! 放在 wind-config 是为了让 wind-ui 等下层也能打点。

use std::io::Write;
use std::sync::OnceLock;

/// 日志时间戳格式。与 `wind_tsf` 的 `FileLogger`(`_FormatTimestamp`) 逐字符一致，
/// 三份日志可直接归并排序。主日志的 timer 也应复用它，避免两处各写一份而漂移。
pub const LOG_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// 轨迹文件大小上限，超过则清空重来。
///
/// 上限防的是崩溃重启循环，不是控制体积。取值要大到正常开机几年都摸不到（1MB 约
/// 2300 次启动），以免隔几天才收集的现场被冲掉。
const MAX_BYTES: u64 = 1024 * 1024;

fn trace_path() -> Option<std::path::PathBuf> {
    crate::config::Config::log_dir().map(|d| d.join("startup_stage.log"))
}

/// 最终生效的日志级别：`RUST_LOG` > `debug.log_level` > `"info"`。
///
/// 服务主日志的 `EnvFilter` 与本模块的关闭门控共用它，免得两边优先级链漂移。
///
/// 本函数自己加载配置；调用方手上已有配置时改用 [`effective_log_level_from`]。
pub fn effective_log_level() -> String {
    let configured = crate::config::Config::load(crate::config::Config::data_dir().as_deref())
        .map(|c| c.debug.log_level)
        .unwrap_or_default();
    effective_log_level_from(&configured)
}

/// [`effective_log_level`] 的「配置已在手上」版本。**优先级链的唯一实现**。
pub fn effective_log_level_from(configured: &str) -> String {
    match std::env::var("RUST_LOG") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => level_or_default(configured),
    }
}

/// 配置值 → 生效级别。**空串 = 没选过 ⇒ `info`**，不是「选了空」。
///
/// `off` 是用户显式选的「别记日志」，空串是出厂状态；混为一谈会让全新安装一条日志都不写。
/// 单独抽出是为了可测（`RUST_LOG` 那层要动进程环境变量）。
fn level_or_default(configured: &str) -> String {
    let l = configured.trim();
    if l.is_empty() {
        "info".to_string()
    } else {
        l.to_string()
    }
}

/// 日志是否被用户整个关掉（级别为 `off`）。关掉时连启动轨迹也不写。
///
/// 「关闭」必须真的一个文件都不产生。判据读不到时照写（诊断设施默认留痕）。
/// 配置只读一次（`OnceLock`）。
fn disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| effective_log_level().eq_ignore_ascii_case("off"))
}

/// 记录一个启动/故障阶段。失败一律静默——诊断设施绝不能反过来影响启动。
pub fn stage(name: &str) {
    if disabled() {
        return;
    }
    let Some(path) = trace_path() else { return };

    if std::fs::metadata(&path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&path);
    }

    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    // 先拼成整行再一次 write_all：`writeln!` 会按格式片段逐段 write，append 模式下片段之间
    // 会被其它进程插入，多进程齐发时行被撕碎，而那正是本文件要记录的场景。
    let line = format!(
        "{} pid={} {}\n",
        chrono::Local::now().format(LOG_TIME_FORMAT),
        std::process::id(),
        name
    );
    let _ = f.write_all(line.as_bytes());
    let _ = f.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_means_never_chosen_not_off() {
        assert_eq!(level_or_default(""), "info");
        assert_eq!(level_or_default("   "), "info", "空白等同未设置");
        // `off` 是用户显式选的，必须原样传下去——落回 info 会让「关闭日志」这个
        // 选项完全失效，而且用户看不出任何异常。
        assert_eq!(level_or_default("off"), "off");
        assert_eq!(level_or_default(" debug "), "debug", "两侧空白要修掉");
    }
}
