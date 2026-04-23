using System;
using System.Diagnostics;
using System.IO;
using System.Security.AccessControl;
using System.Security.Principal;
using Microsoft.Win32;

namespace WindPortable
{
    static class RegistrationManager
    {
        public static void Register(PortableConfig cfg)
        {
            if (string.IsNullOrEmpty(cfg.TsfDll))
                throw new FileNotFoundException($"未找到 TSF DLL，请先构建 {BuildVariant.DllName}");

            if (IsAdministrator())
                RegisterDirect(cfg);
            else
                RunElevated("-elevate-register");
        }

        public static void Unregister(PortableConfig cfg)
        {
            if (IsAdministrator())
                UnregisterDirect(cfg);
            else
                RunElevated("-elevate-unregister");
        }

        public static void RegisterDirect(PortableConfig cfg)
        {
            if (string.IsNullOrEmpty(cfg.TsfDll))
                throw new FileNotFoundException($"未找到 TSF DLL，请先构建 {BuildVariant.DllName}");

            GrantAppPackagesAccess(cfg.TsfDll);
            Regsvr32Register(cfg.TsfDll, false);

            if (!string.IsNullOrEmpty(cfg.TsfDllX86))
            {
                try { GrantAppPackagesAccess(cfg.TsfDllX86); } catch { }
                try { Regsvr32Register(cfg.TsfDllX86, true); } catch { }
            }

            if (!NativeMethods.InstallLayoutOrTip(BuildVariant.ProfileStr, 0))
                throw new Exception("InstallLayoutOrTip 失败");
        }

        public static void UnregisterDirect(PortableConfig cfg)
        {
            if (!string.IsNullOrEmpty(BuildVariant.ProfileStr))
                NativeMethods.InstallLayoutOrTip(BuildVariant.ProfileStr, NativeMethods.ILOT_UNINSTALL);

            if (!string.IsNullOrEmpty(cfg.TsfDllX86))
                try { Regsvr32Unregister(cfg.TsfDllX86, true); } catch { }

            if (!string.IsNullOrEmpty(cfg.TsfDll))
                try { Regsvr32Unregister(cfg.TsfDll, false); } catch { }
        }

        public static bool IsRegistered(PortableConfig cfg)
        {
            string path = RegisteredDllPath();
            return !string.IsNullOrEmpty(path) && !string.IsNullOrEmpty(cfg.TsfDll) &&
                   string.Equals(Path.GetFullPath(path), Path.GetFullPath(cfg.TsfDll),
                       StringComparison.OrdinalIgnoreCase);
        }

        public static bool InstalledConflict(PortableConfig cfg, bool serviceRunning, out string reason)
        {
            reason = null;

            // 检查 1：当前目录是否为安装版目录（注册表 + uninstall.exe 双重检测）
            if (IsInstalledDirectory(cfg.RootDir))
            {
                reason = "当前位于已安装目录，便携模式不可用。如需使用便携模式，请将文件复制到其他目录运行。";
                return true;
            }

            // 检查 2：是否有其他位置的 DLL 注册
            string regPath = RegisteredDllPath();
            if (string.IsNullOrEmpty(regPath)) return false;
            if (string.IsNullOrEmpty(cfg.TsfDll)) return false;
            if (string.Equals(Path.GetFullPath(regPath), Path.GetFullPath(cfg.TsfDll),
                    StringComparison.OrdinalIgnoreCase))
                return false;

            // 注册的 DLL 文件已不存在，属于残留注册，可安全接管
            if (!File.Exists(regPath))
                return false;

            // 不同位置的 DLL 已注册，判断来源
            if (HasPortableMarker(regPath))
            {
                // 来自另一个便携版实例
                if (!serviceRunning)
                    return false; // 残留注册，服务未运行，可安全接管

                reason = "检测到另一个便携版实例正在运行，请先停止该实例后再启动。";
                return true;
            }

            // 来自安装版的注册
            reason = "系统已注册其他位置的清风输入法，为避免覆盖现有注册信息，便携模式已禁用。";
            return true;
        }

        public static string InstalledConflictPath(PortableConfig cfg)
        {
            if (IsInstalledDirectory(cfg.RootDir))
                return NsisInstallLocation() ?? cfg.RootDir;
            return RegisteredDllPath();
        }

        static void GrantAppPackagesAccess(string filePath)
        {
            var security = File.GetAccessControl(filePath);
            var sid = new SecurityIdentifier("S-1-15-2-1");
            security.AddAccessRule(new FileSystemAccessRule(
                sid,
                FileSystemRights.ReadAndExecute,
                AccessControlType.Allow));
            File.SetAccessControl(filePath, security);
        }

        static void Regsvr32Register(string dllPath, bool x86) => RunRegsvr32(dllPath, x86, false);
        static void Regsvr32Unregister(string dllPath, bool x86) => RunRegsvr32(dllPath, x86, true);

        static void RunRegsvr32(string dllPath, bool x86, bool unregister)
        {
            if (string.IsNullOrEmpty(dllPath)) return;
            if (!File.Exists(dllPath))
                throw new FileNotFoundException($"未找到 DLL: {dllPath}");

            string regsvr32 = x86
                ? Path.Combine(Environment.GetEnvironmentVariable("SystemRoot"), "SysWOW64", "regsvr32.exe")
                : "regsvr32.exe";

            string args = unregister ? $"/u /s \"{dllPath}\"" : $"/s \"{dllPath}\"";
            var psi = new ProcessStartInfo(regsvr32, args)
            {
                WindowStyle = ProcessWindowStyle.Hidden,
                CreateNoWindow = true,
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };

            var proc = Process.Start(psi);
            proc.WaitForExit(10000);
            if (proc.ExitCode != 0)
                throw new Exception($"regsvr32 执行失败 ({Path.GetFileName(dllPath)}): 退出码 {proc.ExitCode}");
        }

        static void RunElevated(string args)
        {
            string exePath = System.Reflection.Assembly.GetExecutingAssembly().Location;
            var psi = new ProcessStartInfo(exePath, args)
            {
                Verb = "runas",
                UseShellExecute = true,
            };
            try
            {
                var proc = Process.Start(psi);
                proc?.WaitForExit(30000);
            }
            catch (System.ComponentModel.Win32Exception)
            {
                throw new Exception("请求管理员权限失败或被取消");
            }
        }

        /// <summary>
        /// 检测目录是否为安装版目录：NSIS 注册表匹配或存在卸载程序。
        /// </summary>
        static bool IsInstalledDirectory(string rootDir)
        {
            string installDir = NsisInstallLocation();
            if (!string.IsNullOrEmpty(installDir) &&
                string.Equals(Path.GetFullPath(rootDir), Path.GetFullPath(installDir),
                    StringComparison.OrdinalIgnoreCase))
                return true;

            if (File.Exists(Path.Combine(rootDir, "uninstall.exe")))
                return true;

            return false;
        }

        /// <summary>
        /// 检查 DLL 所在目录是否存在便携模式标记文件（必须同级，不向上遍历）。
        /// </summary>
        static bool HasPortableMarker(string dllPath)
        {
            try
            {
                string dir = Path.GetDirectoryName(Path.GetFullPath(dllPath));
                if (!string.IsNullOrEmpty(dir))
                    return File.Exists(Path.Combine(dir, BuildVariant.PortableMarkerName));
            }
            catch { }
            return false;
        }

        static string RegisteredDllPath()
        {
            string clsid = BuildVariant.Clsid;
            var candidates = new (RegistryKey Key, string Path)[] {
                (Registry.CurrentUser, $@"Software\Classes\CLSID\{clsid}\InprocServer32"),
                (Registry.LocalMachine, $@"Software\Classes\CLSID\{clsid}\InprocServer32"),
                (Registry.ClassesRoot, $@"CLSID\{clsid}\InprocServer32"),
            };
            foreach (var c in candidates)
            {
                try
                {
                    using (var key = c.Key.OpenSubKey(c.Path))
                    {
                        string val = key?.GetValue("")?.ToString()?.Trim();
                        if (!string.IsNullOrEmpty(val)) return val;
                    }
                }
                catch { }
            }
            return null;
        }

        static string NsisInstallLocation()
        {
            string displayName = BuildVariant.DisplayName;
            string path = $@"Software\Microsoft\Windows\CurrentVersion\Uninstall\{displayName}";
            try
            {
                using (var key = Registry.LocalMachine.OpenSubKey(path))
                {
                    return key?.GetValue("InstallLocation")?.ToString()?.Trim();
                }
            }
            catch { return null; }
        }

        static bool IsAdministrator()
        {
            using (var identity = WindowsIdentity.GetCurrent())
            {
                var principal = new WindowsPrincipal(identity);
                return principal.IsInRole(WindowsBuiltInRole.Administrator);
            }
        }
    }
}
