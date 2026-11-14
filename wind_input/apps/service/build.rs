// 为 wind_input.exe 嵌入 Windows 资源：图标 / 版本信息 / manifest（DPI 感知）。
// 字段值与原 Go 项目 winres/winres.json 基本一致（产品名/版权/描述/图标）。
// 版本号取 docs/VERSION（与 release tag、旧项目单一 VERSION 真值源一致），
// 而非 Cargo 版本（二者当前不同步）。仅 Windows 目标生效，其它目标为空操作。

use std::path::PathBuf;

fn main() {
    // 注入构建时间戳 + git 提交（供服务启动日志确认运行的二进制版本）。
    // 必须在 windows-only 早返回之前，使非 Windows 目标也能解析 env!。
    emit_build_stamp();

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // wind_input/apps/service → 仓库根 WindInput
    let product_root = manifest_dir.join("..").join("..").join("..");
    let version_file = product_root.join("docs/VERSION");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", version_file.display());

    // 产品版本 X.Y.Z[-suffix]：取 docs/VERSION 注入 WIND_APP_VERSION，供 main.rs 启动日志等
    // 运行时使用。env! 对全平台求值，故在 windows-only 早返回之前发出。
    let ver_str = std::fs::read_to_string(&version_file)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rustc-env=WIND_APP_VERSION={ver_str}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // 主线程栈 8 MiB（只是保留地址，按需提交）。默认 1 MiB 在 dev 档（工作区 opt-level=1）下
    // 不够：主题加载链上 serde 访问器每层都是数百 KiB 的帧，曾因此栈溢出、服务循环崩溃。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }

    // 图标资源（仅 Windows）。
    let icon = product_root.join("wind_tsf/res/wind_input.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    // 数值 FIXEDFILEINFO
    let core = ver_str.split('-').next().unwrap_or("0.0.0");
    let mut it = core.split('.').map(|s| s.parse::<u16>().unwrap_or(0));
    let (maj, min, pat) = (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    );
    let ver_u64 = ((maj as u64) << 48) | ((min as u64) << 32) | ((pat as u64) << 16);

    let product_name = "清风输入法";
    let original_filename = "wind_input.exe";

    // manifest：asInvoker + PerMonitorV2 DPI 感知 + win10 + 长路径（对齐旧项目）。
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="com.windinput.service" version="1.0.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#;

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon 路径含非 UTF-8"));
    res.set("ProductName", product_name);
    res.set("CompanyName", "清风输入法");
    res.set("FileDescription", "清风输入法服务");
    res.set("InternalName", "wind_input");
    res.set("OriginalFilename", original_filename);
    res.set("LegalCopyright", "Copyright © 清风输入法");
    res.set("ProductVersion", &ver_str);
    res.set("FileVersion", &ver_str);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, ver_u64);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, ver_u64);
    res.set_manifest(manifest);

    if let Err(e) = res.compile() {
        // 不致命：缺资源不影响功能，仅图标/版本元数据缺失。
        println!("cargo:warning=Windows 资源嵌入失败: {e}");
    }
}

/// 注入编译期版本戳：`WIND_BUILD_TIME`（UTC 构建时刻）+ `WIND_GIT_HASH`（短哈希[-dirty]）。
/// 强制每次构建重跑，使时间戳与实际二进制构建时刻一致（否则未提交改动重建时戳会滞后）。
fn emit_build_stamp() {
    // 引用一个永不存在的路径 → cargo 视为「已变化」→ 每次 `cargo build` 都重跑本脚本。
    println!("cargo:rerun-if-changed=__wind_build_stamp_force_rerun__");

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    println!("cargo:rustc-env=WIND_BUILD_TIME={}", format_utc(secs));
    println!("cargo:rustc-env=WIND_GIT_HASH={}", git_describe());
}

/// git 短哈希；工作树有未提交改动时追加 `-dirty`。git 不可用时返回 `unknown`。
///
/// 判脏忽略未跟踪文件（与 `git describe --dirty` 的口径一致，编辑器按它比对预览引擎版本）。
/// 会影响预览缓存键：只是没那么容易判脏，缓存更少失效。
fn git_describe() -> String {
    use std::process::Command;
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if dirty { format!("{hash}-dirty") } else { hash }
}

/// 把 Unix 时间戳（秒）格式化为 `YYYY-MM-DDTHH:MM:SSZ`（UTC，无第三方依赖）。
fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// 自纪元天数 → (年, 月, 日)，Howard Hinnant `civil_from_days` 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}
