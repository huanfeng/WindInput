#pragma once

#include <windows.h>

// ============================================================================
// 安装根目录与便携形态判定 —— 本 DLL 内的**唯一真相源**
//
// 抽成独立单元的理由：这两件事此前只存在于 `IPCClient.cpp` 的一个 static 函数里，
// 而 `FileLogger` 也需要它们（便携部署的日志必须落在便携目录内，不能写
// `%LOCALAPPDATA%`）。各写一份的后果是「服务从便携目录拉起、日志却写进
// %LOCALAPPDATA%」这种半便携形态——用户拔盘走人，机器上仍留着输入痕迹。
//
// ⚠️ 两个函数都可能在 `DllMain(DLL_PROCESS_ATTACH)` 的 loader lock 下被调用
// （`CFileLogger::Init` 就在那里）。故实现里**只允许**注册表读取与文件打开这类
// 内核对象操作：不加载别的 DLL、不建线程、不等同步对象。
// ============================================================================

// 解析应用安装目录（写入 outDir，末尾不带反斜杠）。
//
// ★ 优先取 `HKLM\Software\WindInput[Dev]\InstallDir`，**不能**由模块路径推导：本 DLL 被
// 部署到系统目录（System32\IME\<app>\）后 GetModuleFileName 取到的是系统副本路径，
// 其同级目录既没有服务 exe 也没有便携标记。三个部署方（wind-installer /
// scripts\dev.ps1 / wind-portable）在注册 COM 前都会写该值。
//
// 键缺失时回退到 DLL 自身目录——兼容「就地注册」的存量部署与未走部署脚本的开发构建。
// 回退不是可有可无的兜底：漏掉它会让所有旧安装在升级前起不了服务。
BOOL WindResolveInstallRoot(WCHAR* outDir, DWORD cchOutDir);

// 该目录下是否有便携标记 ⇒ 本次部署是便携形态。
//
// 判据与 Rust 侧 `wind_config::variant::has_portable_marker_in` 逐条对齐：新名
// `portable_mode` 优先，旧名 `wind_portable_mode` 仅为存量便携包保留读取兼容。
// 名字还须与安装器清单 `config/app.toml` 的 `[app] portable_marker` 一致——
// **四处同名，无编译期约束**，改名时一起改。
BOOL WindIsPortableRoot(const WCHAR* root);
