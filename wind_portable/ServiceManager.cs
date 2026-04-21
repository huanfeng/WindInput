using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
using Microsoft.Win32;

namespace WindPortable
{
    class ServiceManager
    {
        public PortableConfig Config { get; }
        readonly RpcClient _rpc;

        // 注册表自启动键名（与安装版一致）
        static string RunKeyName
        {
            get { return BuildVariant.IsDebug ? "WindInputDebug" : "WindInput"; }
        }

        public ServiceManager(PortableConfig config)
        {
            Config = config;
            _rpc = new RpcClient();
        }

        public bool ServiceRunning() => _rpc.IsAvailable();

        public void StartService()
        {
            EnsureAvailable("启动服务");
            if (ServiceRunning()) return;

            EnsurePortableLayout();
            ClearStoppedFlag();

            // 1. 注册输入法（regsvr32 + InstallLayoutOrTip）
            if (!RegistrationManager.IsRegistered(Config))
                RegistrationManager.Register(Config);

            // 2. 写入开机自启动注册表
            SetAutoStart(true);

            // 3. 启动服务进程
            if (!File.Exists(Config.ServiceExe))
                throw new FileNotFoundException("未找到服务程序: " + Config.ServiceExe);

            var psi = new ProcessStartInfo(Config.ServiceExe)
            {
                WorkingDirectory = Path.GetDirectoryName(Config.ServiceExe),
                WindowStyle = ProcessWindowStyle.Hidden,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            Process.Start(psi);
        }

        public bool StopService()
        {
            EnsureAvailable("停止服务");
            SetStoppedFlag();

            bool wasRunning = ServiceRunning();
            bool wasRegistered = RegistrationManager.IsRegistered(Config);

            // 1. 停止服务进程
            if (wasRunning)
            {
                bool graceful = false;
                try
                {
                    _rpc.Shutdown();
                    for (int i = 0; i < 6; i++)
                    {
                        Thread.Sleep(500);
                        if (!ServiceRunning()) { graceful = true; break; }
                    }
                }
                catch { }

                if (!graceful && ServiceRunning())
                    ProcessHelper.TerminateByPath(Config.ServiceExe);
            }

            // 2. 移除开机自启动注册表
            SetAutoStart(false);

            // 3. 注销输入法
            if (wasRegistered)
                RegistrationManager.Unregister(Config);

            return wasRunning || wasRegistered;
        }

        public void OpenSettings()
        {
            EnsureAvailable("打开设置");
            if (!File.Exists(Config.SettingExe))
                throw new FileNotFoundException("未找到设置程序: " + Config.SettingExe);
            Process.Start(new ProcessStartInfo(Config.SettingExe)
            {
                WorkingDirectory = Path.GetDirectoryName(Config.SettingExe),
            });
        }

        public void OpenUserdataDir()
        {
            EnsureAvailable("打开数据目录");
            string target = Config.AppDataDir;
            if (!Directory.Exists(target))
                target = Config.UserdataDir;
            if (!Directory.Exists(target))
                throw new DirectoryNotFoundException("数据目录尚未创建，请先启动一次服务: " + Config.UserdataDir);
            Process.Start("explorer.exe", target);
        }

        public bool IsRegistered() => RegistrationManager.IsRegistered(Config);

        public bool InstalledConflict(out string reason)
        {
            return RegistrationManager.InstalledConflict(Config, out reason);
        }

        public string InstalledConflictPath()
        {
            return RegistrationManager.InstalledConflictPath(Config);
        }

        public void SetStoppedFlag() => WriteMarkerFile(true);
        public void ClearStoppedFlag() => WriteMarkerFile(false);

        // ── 开机自启动注册表管理 ──

        /// <summary>
        /// 设置或移除开机自启动注册表项（HKCU\Software\Microsoft\Windows\CurrentVersion\Run）。
        /// </summary>
        void SetAutoStart(bool enable)
        {
            const string runKeyPath = @"Software\Microsoft\Windows\CurrentVersion\Run";
            try
            {
                using (var key = Registry.CurrentUser.OpenSubKey(runKeyPath, true))
                {
                    if (key == null) return;
                    if (enable)
                    {
                        key.SetValue(RunKeyName, "\"" + Config.ServiceExe + "\"");
                    }
                    else
                    {
                        key.DeleteValue(RunKeyName, false);
                    }
                }
            }
            catch { }
        }

        // ── 内部方法 ──

        void EnsureAvailable(string action)
        {
            if (InstalledConflict(out string reason))
                throw new InvalidOperationException(action + "失败：" + reason);
        }

        void EnsurePortableLayout()
        {
            var dirs = new[] {
                Config.UserdataDir,
                Config.AppDataDir,
                Path.Combine(Config.AppDataDir, "logs"),
                Path.Combine(Config.AppDataDir, "cache"),
                Path.Combine(Config.AppDataDir, "themes"),
            };
            foreach (var dir in dirs)
                Directory.CreateDirectory(dir);

            if (!File.Exists(Config.PortableMarker))
                File.WriteAllText(Config.PortableMarker, "wind_portable=1\n");
        }

        void WriteMarkerFile(bool stopped)
        {
            string content = "wind_portable=1\n";
            if (stopped) content += "stopped=1\n";
            File.WriteAllText(Config.PortableMarker, content);
        }
    }
}
