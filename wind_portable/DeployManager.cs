using System;
using System.IO;
using System.IO.Compression;
using System.Linq;

namespace WindPortable
{
    static class DeployManager
    {
        public static void ValidateZip(string zipPath)
        {
            using (var archive = ZipFile.OpenRead(zipPath))
            {
                var required = new[] { BuildVariant.ServiceName, BuildVariant.DllName };
                var fileNames = archive.Entries
                    .Select(e => Path.GetFileName(e.FullName))
                    .ToList();

                var missing = required.Where(r =>
                    !fileNames.Any(f => string.Equals(f, r, StringComparison.OrdinalIgnoreCase)))
                    .ToList();
                if (missing.Count > 0)
                    throw new Exception($"ZIP 缺少必要文件: {string.Join(", ", missing)}");
            }
        }

        public static bool DeployFromZip(string zipPath, string targetDir)
        {
            string selfExe = System.Reflection.Assembly.GetExecutingAssembly().Location;
            bool needsRestart = false;
            var rng = new Random();

            using (var archive = ZipFile.OpenRead(zipPath))
            {
                foreach (var entry in archive.Entries)
                {
                    if (string.IsNullOrEmpty(entry.Name)) continue;

                    string dstPath = Path.Combine(targetDir, entry.FullName.Replace('/', '\\'));
                    Directory.CreateDirectory(Path.GetDirectoryName(dstPath));

                    if (string.Equals(Path.GetFullPath(dstPath), Path.GetFullPath(selfExe),
                            StringComparison.OrdinalIgnoreCase))
                    {
                        string oldPath = $"{selfExe}.old_{rng.Next(100000)}";
                        File.Move(selfExe, oldPath);
                        needsRestart = true;
                    }

                    try
                    {
                        entry.ExtractToFile(dstPath, true);
                    }
                    catch (IOException)
                    {
                        string oldPath = $"{dstPath}.old_{rng.Next(100000)}";
                        try { File.Move(dstPath, oldPath); } catch { throw; }
                        entry.ExtractToFile(dstPath, true);
                    }
                }
            }
            return needsRestart;
        }

        public static void DeployFromDirectory(string srcDir, string targetDir)
        {
            srcDir = Path.GetFullPath(srcDir);
            targetDir = Path.GetFullPath(targetDir);

            if (string.Equals(srcDir, targetDir, StringComparison.OrdinalIgnoreCase))
                throw new Exception("源目录与目标目录相同");

            foreach (string srcPath in Directory.GetFiles(srcDir, "*", SearchOption.AllDirectories))
            {
                string relPath = srcPath.Substring(srcDir.Length).TrimStart('\\', '/');
                if (relPath.StartsWith("userdata\\", StringComparison.OrdinalIgnoreCase) ||
                    relPath.Equals("userdata", StringComparison.OrdinalIgnoreCase))
                    continue;

                string dstPath = Path.Combine(targetDir, relPath);
                Directory.CreateDirectory(Path.GetDirectoryName(dstPath));
                File.Copy(srcPath, dstPath, true);
            }
        }

        public static void CleanOldFiles(string dir)
        {
            if (!Directory.Exists(dir)) return;
            try
            {
                foreach (var f in Directory.GetFiles(dir, "*.old_*"))
                    try { File.Delete(f); } catch { }
                foreach (var f in Directory.GetFiles(dir, "*.bak"))
                    try { File.Delete(f); } catch { }
            }
            catch { }
        }
    }
}
