using System.IO;

namespace WindPortable
{
    /// <summary>
    /// 构建变体：运行时自动检测。
    /// 单一二进制，默认为发布版。如果检测到 wind_input_debug.exe 则切换到开发版逻辑。
    /// </summary>
    static class BuildVariant
    {
        public static bool IsDebug { get; private set; }
        public static string Suffix { get; private set; }
        public static string AppName { get; private set; }
        public static string DisplayName { get; private set; }
        public static string ProfileStr { get; private set; }
        public static string Clsid { get; private set; }

        public static string ServiceName { get; private set; }
        public static string SettingName { get; private set; }
        public static string DllName { get; private set; }
        public static string DllNameX86 { get; private set; }
        public static string RpcPipeName { get; private set; }
        public static string MutexName { get; private set; }
        public static string ShowEventName { get; private set; }

        public const string PortableMarkerName = "wind_portable_mode";
        public const string PortableDataDir = "userdata";

        static BuildVariant()
        {
            // 默认为发布版，如果检测到 debug 服务则切换
            bool debug = DetectDebugVariant();
            Apply(debug);
        }

        /// <summary>
        /// 检测是否应使用开发版变体：扫描 exe 所在目录及上级目录是否存在 wind_input_debug.exe。
        /// </summary>
        static bool DetectDebugVariant()
        {
            string exePath = System.Reflection.Assembly.GetExecutingAssembly().Location;
            string exeDir = Path.GetDirectoryName(exePath);

            var candidates = new[]
            {
                exeDir,
                Path.GetDirectoryName(exeDir),
            };

            foreach (var dir in candidates)
            {
                if (string.IsNullOrEmpty(dir)) continue;
                // 直接查找
                if (File.Exists(Path.Combine(dir, "wind_input_debug.exe"))) return true;
                // build_debug 子目录
                if (File.Exists(Path.Combine(dir, "build_debug", "wind_input_debug.exe"))) return true;
            }
            return false;
        }

        static void Apply(bool debug)
        {
            IsDebug = debug;
            if (debug)
            {
                Suffix = "_debug";
                AppName = "WindInputDebug";
                DisplayName = "清风输入法开发版";
                ProfileStr = "0804:{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}";
                Clsid = "{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}";
            }
            else
            {
                Suffix = "";
                AppName = "WindInput";
                DisplayName = "清风输入法";
                ProfileStr = "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}";
                Clsid = "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}";
            }

            ServiceName = "wind_input" + Suffix + ".exe";
            SettingName = "wind_setting" + Suffix + ".exe";
            DllName = "wind_tsf" + Suffix + ".dll";
            DllNameX86 = "wind_tsf" + Suffix + "_x86.dll";
            RpcPipeName = "wind_input" + Suffix + "_rpc";
            MutexName = @"Local\WindPortable" + Suffix + "Launcher";
            ShowEventName = @"Local\WindPortable" + Suffix + "ShowEvent";
        }
    }
}
