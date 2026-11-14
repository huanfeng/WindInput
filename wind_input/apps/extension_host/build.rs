// 为 wind_input_ext.exe 嵌入 Windows 资源：图标 / 版本信息 / manifest，任务管理器里显示
// 「清风输入法扩展服务」而不是光秃的文件名。版本号与核心同取 docs/VERSION（见 service/build.rs）。
// 仅 Windows 目标生效，其它目标为空操作。

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // wind_input/apps/extension_host → 仓库根 WindInput
    let product_root = manifest_dir.join("..").join("..").join("..");
    let version_file = product_root.join("docs/VERSION");
    let icon = product_root.join("wind_tsf/res/wind_input.ico");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", version_file.display());
    println!("cargo:rerun-if-changed={}", icon.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let ver_str = std::fs::read_to_string(&version_file)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "0.0.0".to_string());
    let core = ver_str.split('-').next().unwrap_or("0.0.0");
    let mut it = core.split('.').map(|s| s.parse::<u16>().unwrap_or(0));
    let (maj, min, pat) = (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    );
    let ver_u64 = ((maj as u64) << 48) | ((min as u64) << 32) | ((pat as u64) << 16);

    // 无界面的后台进程：只要 asInvoker 与 win10 兼容声明，不需要 DPI 感知。
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="com.windinput.ext" version="1.0.0.0"/>
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
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#;

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon 路径含非 UTF-8"));
    res.set("ProductName", "清风输入法");
    res.set("CompanyName", "清风输入法");
    res.set("FileDescription", "清风输入法扩展服务");
    res.set("InternalName", "wind_input_ext");
    res.set("OriginalFilename", "wind_input_ext.exe");
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
