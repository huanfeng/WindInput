//! 启动阶段轨迹：绕开 tracing，每次调用都同步落盘。
//!
//! 存在的理由是主日志本身可能失效。`tracing_appender::non_blocking` 把写入交给一个
//! worker 线程，该线程一旦出事，channel 断开后 lossy 模式会**静默丢弃**其后的每一条
//! 日志——`wind_input.1.log` 就永久停在某一行，而进程仍在正常运行。这种日志外观极易被
//! 误读成「进程卡在那一行」，实际进度可能远在其后。
//!
//! 本模块每次调用 open→write→flush→close，不经 tracing、不经缓冲、不常驻句柄，
//! 因此在主日志已死的场景下依然留痕。它只在启动路径与故障分支上被调用寥寥数次，
//! **不得进入按键热路径**。
//!
//! 每行都带 pid，因为「究竟起了几个进程」是这类故障的关键判据，而单看主日志答不了
//! ——被顶掉序号的日志文件会让多进程看起来像一次运行。
//!
//! 放在 wind-config 而非服务 crate，是为了让 wind-ui 等下层也能打点：UI 线程
//! 自己挂掉时，主线程与主日志都可能毫无察觉。

use std::io::Write;
use std::sync::OnceLock;

/// 日志时间戳格式。与 `wind_tsf` 的 `FileLogger`(`_FormatTimestamp`) 逐字符一致，
/// 三份日志可直接归并排序。主日志的 timer 也应复用它，避免两处各写一份而漂移。
pub const LOG_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// 轨迹文件大小上限，超过则清空重来。
///
/// 上限存在的目的**不是**控制体积——一次启动约 450 字节，一年 365 次开机也才 150KB。
/// 它防的是崩溃重启循环：服务若每秒重启数次，无限增长会失控。
///
/// 取值要足够大：故障是客户侧偶发的，日志往往隔几天才收集回来，期间的正常开机
/// 不能把那次复现的记录冲掉。1MB ≈ 2300 次启动，正常使用几年都摸不到。
const MAX_BYTES: u64 = 1024 * 1024;

fn trace_path() -> Option<std::path::PathBuf> {
    crate::config::Config::log_dir().map(|d| d.join("startup_stage.log"))
}

/// 最终生效的日志级别：`RUST_LOG` > `debug.log_level` > `"info"`。
///
/// ★ 抽成公开函数是因为它有**两个**消费者：服务主日志的 `EnvFilter`，与本模块的
/// 关闭门控。两边各算一遍优先级链，迟早会漂移成「主日志写着、启动轨迹停了」这种
/// 自相矛盾的状态——而这两份日志正是用来互相印证的。
///
/// 本函数自己加载配置。调用方**手上已经有配置**时改用
/// [`effective_log_level_from`]：`Config::load` 是一次多层合并 + 多个文件读取，
/// 为了一个字段再走一遍不划算，而它正好落在启动路径上。
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
/// 这条区分在加了 `off` 档之后才要紧：`off` 是用户显式选的"别记日志"，空串是
/// 出厂状态。两者混为一谈的话，全新安装会一条日志都不写。
///
/// 单独抽出来是为了可测：`RUST_LOG` 那一层要动进程环境变量，测试并行时会互相污染，
/// 而真正需要守住的判据在这三行里。
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
/// ★ 「关闭」必须是**真的一个文件都不产生**，否则这个选项对用户没有意义：他要的是
/// 「这台机器上别留输入法的痕迹」，而 `startup_stage.log` 同样带时间戳与 pid。
///
/// ⚠️ 判据读不到时**照写**。诊断设施的默认方向是留痕；而「关掉日志」是用户的显式
/// 选择，在读不到那个选择的时候不该替他做主。配置只读一次（`OnceLock`）：本模块
/// 在启动路径上只被调用寥寥数次，但它明确禁止进入按键热路径。
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

    // 必须先拼成整行再一次 write_all：`writeln!` 走 `Write::write_fmt`，会按格式片段
    // 逐段调 write（时间戳一次、" pid=" 一次、pid 一次……）。append 模式下单次 write 是
    // 原子的，但**片段之间**会被其它进程插入，行就被撕成乱码。而多进程齐发（开机、升级后
    // 十余个宿主同时抢着拉服务）恰恰是本文件唯一的证据来源——2026-07-23 客户升级日志里
    // 就有整片撕裂行无法判读。
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
