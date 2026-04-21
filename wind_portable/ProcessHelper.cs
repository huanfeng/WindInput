using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace WindPortable
{
    static class ProcessHelper
    {
        const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
        const uint PROCESS_TERMINATE = 0x0001;
        const uint SYNCHRONIZE = 0x00100000;

        [DllImport("kernel32.dll", SetLastError = true)]
        static extern IntPtr OpenProcess(uint dwDesiredAccess, bool bInheritHandle, int dwProcessId);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool TerminateProcess(IntPtr hProcess, uint uExitCode);

        [DllImport("kernel32.dll")]
        static extern uint WaitForSingleObject(IntPtr hHandle, uint dwMilliseconds);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool CloseHandle(IntPtr hObject);

        public static bool TerminateByPath(string targetPath)
        {
            targetPath = Path.GetFullPath(targetPath);
            string targetName = Path.GetFileNameWithoutExtension(targetPath);
            bool stopped = false;

            foreach (var proc in Process.GetProcesses())
            {
                try
                {
                    if (!proc.ProcessName.Equals(targetName, StringComparison.OrdinalIgnoreCase))
                        continue;

                    string procPath = GetProcessPath(proc.Id);
                    if (procPath != null &&
                        string.Equals(Path.GetFullPath(procPath), targetPath, StringComparison.OrdinalIgnoreCase))
                    {
                        TerminatePid(proc.Id);
                        stopped = true;
                    }
                }
                catch { }
                finally { proc.Dispose(); }
            }
            return stopped;
        }

        public static bool ExistsByPath(string targetPath)
        {
            targetPath = Path.GetFullPath(targetPath);
            string targetName = Path.GetFileNameWithoutExtension(targetPath);

            foreach (var proc in Process.GetProcesses())
            {
                try
                {
                    if (!proc.ProcessName.Equals(targetName, StringComparison.OrdinalIgnoreCase))
                        continue;
                    string procPath = GetProcessPath(proc.Id);
                    if (procPath != null &&
                        string.Equals(Path.GetFullPath(procPath), targetPath, StringComparison.OrdinalIgnoreCase))
                        return true;
                }
                catch { }
                finally { proc.Dispose(); }
            }
            return false;
        }

        public static bool ExistsByName(string exeName)
        {
            string name = Path.GetFileNameWithoutExtension(exeName);
            return Process.GetProcessesByName(name).Length > 0;
        }

        static string GetProcessPath(int pid)
        {
            IntPtr handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if (handle == IntPtr.Zero) return null;
            try
            {
                var sb = new StringBuilder(1024);
                uint size = (uint)sb.Capacity;
                if (NativeMethods.QueryFullProcessImageNameW(handle, 0, sb, ref size))
                    return sb.ToString();
                return null;
            }
            finally { CloseHandle(handle); }
        }

        static void TerminatePid(int pid)
        {
            IntPtr handle = OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, false, pid);
            if (handle == IntPtr.Zero) return;
            try
            {
                TerminateProcess(handle, 0);
                WaitForSingleObject(handle, 2000);
            }
            finally { CloseHandle(handle); }
        }
    }
}
