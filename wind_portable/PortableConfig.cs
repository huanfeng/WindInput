using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace WindPortable
{
    class PortableConfig
    {
        public string RootDir { get; set; }
        public string UserdataDir { get; set; }
        public string AppDataDir { get; set; }
        public string PortableMarker { get; set; }
        public string IconPath { get; set; }
        public string ServiceExe { get; set; }
        public string SettingExe { get; set; }
        public string TsfDll { get; set; }
        public string TsfDllX86 { get; set; }

        public static PortableConfig Detect()
        {
            string exePath = System.Reflection.Assembly.GetExecutingAssembly().Location;
            string exeDir = Path.GetDirectoryName(exePath);

            if (IsProtectedDir(exeDir))
                throw new InvalidOperationException(
                    $"当前位于系统保护目录({exeDir})，不支持便携模式。\n请将便携包复制到其他目录运行。");

            string wd = Directory.GetCurrentDirectory();
            var candidates = UniquePaths(new[] {
                exeDir,
                Path.GetDirectoryName(exeDir),
                wd,
                Path.GetDirectoryName(wd)
            });

            foreach (var root in candidates)
            {
                string svcExe = FirstExisting(new[] {
                    Path.Combine(root, BuildVariant.ServiceName),
                    Path.Combine(root, "build", BuildVariant.ServiceName),
                    Path.Combine(root, "build_debug", BuildVariant.ServiceName),
                });
                if (svcExe == null) continue;

                string setExe = FirstExisting(new[] {
                    Path.Combine(root, BuildVariant.SettingName),
                    Path.Combine(root, "build", BuildVariant.SettingName),
                    Path.Combine(root, "build_debug", BuildVariant.SettingName),
                }) ?? Path.Combine(root, BuildVariant.SettingName);

                string tsfDll = FirstExisting(new[] {
                    Path.Combine(root, BuildVariant.DllName),
                    Path.Combine(root, "build", BuildVariant.DllName),
                    Path.Combine(root, "build_debug", BuildVariant.DllName),
                });
                string tsfDllX86 = FirstExisting(new[] {
                    Path.Combine(root, BuildVariant.DllNameX86),
                    Path.Combine(root, "build", BuildVariant.DllNameX86),
                    Path.Combine(root, "build_debug", BuildVariant.DllNameX86),
                });

                string userdataDir = Path.Combine(root, BuildVariant.PortableDataDir);
                return new PortableConfig
                {
                    RootDir = root,
                    UserdataDir = userdataDir,
                    AppDataDir = userdataDir,
                    PortableMarker = Path.Combine(root, BuildVariant.PortableMarkerName),
                    IconPath = FirstExisting(new[] {
                        Path.Combine(root, "wind_portable", "res", "wind_input_portable.ico"),
                        Path.Combine(root, "res", "wind_input_portable.ico"),
                        Path.Combine(root, "wind_tsf", "res", "wind_input.ico"),
                    }),
                    ServiceExe = svcExe,
                    SettingExe = setExe,
                    TsfDll = tsfDll,
                    TsfDllX86 = tsfDllX86,
                };
            }

            throw new FileNotFoundException(
                $"未找到 {BuildVariant.ServiceName}，请先构建主服务或将 launcher 放到打包目录中");
        }

        public static string FindDeploySourceDir()
        {
            try
            {
                string exeDir = Path.GetDirectoryName(
                    System.Reflection.Assembly.GetExecutingAssembly().Location);
                string wd = Directory.GetCurrentDirectory();
                foreach (var root in UniquePaths(new[] { exeDir, Path.GetDirectoryName(exeDir), wd, Path.GetDirectoryName(wd) }))
                {
                    if (FirstExisting(new[] {
                        Path.Combine(root, BuildVariant.ServiceName),
                        Path.Combine(root, "build", BuildVariant.ServiceName),
                        Path.Combine(root, "build_debug", BuildVariant.ServiceName),
                    }) != null)
                        return root;
                }
            }
            catch { }
            return null;
        }

        public static bool IsProtectedDir(string dir)
        {
            string lower = Path.GetFullPath(dir).ToLowerInvariant();
            var prefixes = new[] {
                Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles),
                Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86),
                Environment.GetEnvironmentVariable("ProgramW6432"),
                Environment.GetEnvironmentVariable("SystemRoot"),
            };
            return prefixes
                .Where(p => !string.IsNullOrEmpty(p))
                .Any(p => lower.StartsWith(p.ToLowerInvariant() + @"\"));
        }

        static string FirstExisting(string[] paths)
        {
            return paths.FirstOrDefault(p => !string.IsNullOrEmpty(p) && File.Exists(p));
        }

        static List<string> UniquePaths(string[] paths)
        {
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var result = new List<string>();
            foreach (var p in paths)
            {
                if (string.IsNullOrEmpty(p)) continue;
                string clean = Path.GetFullPath(p);
                if (seen.Add(clean))
                    result.Add(clean);
            }
            return result;
        }
    }
}
