using System;
using System.Threading;
using System.Windows.Forms;

namespace WindPortable
{
    static class Program
    {
        static Mutex _mutex;

        [STAThread]
        static void Main(string[] args)
        {
            PortableConfig cfg = null;
            string detectError = null;

            try
            {
                cfg = PortableConfig.Detect();
            }
            catch (Exception ex)
            {
                detectError = ex.Message;
            }

            // 旧文件清理放后台，不阻塞启动
            if (cfg != null)
            {
                var rootDir = cfg.RootDir;
                System.Threading.Tasks.Task.Run(() => DeployManager.CleanOldFiles(rootDir));
            }

            var opts = ParseCLI(args);

            if (detectError != null && opts.HasAction && !opts.UI)
            {
                Console.Error.WriteLine(detectError);
                Environment.Exit(1);
            }

            ServiceManager manager = null;
            if (cfg != null)
            {
                manager = new ServiceManager(cfg);

                if (opts.HasAction && !opts.UI)
                {
                    try
                    {
                        RunCLI(manager, cfg, opts);
                    }
                    catch (Exception ex)
                    {
                        Console.Error.WriteLine(ex.Message);
                        Environment.Exit(1);
                    }
                    return;
                }
            }

            _mutex = new Mutex(true, BuildVariant.MutexName, out bool createdNew);
            if (!createdNew)
            {
                ActivateExistingWindow();
                return;
            }

            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new MainForm(manager, detectError));
        }

        static void ActivateExistingWindow()
        {
            try
            {
                using (var evt = EventWaitHandle.OpenExisting(BuildVariant.ShowEventName))
                {
                    evt.Set();
                }
            }
            catch { }
        }

        static CliOptions ParseCLI(string[] args)
        {
            var opts = new CliOptions();
            foreach (var arg in args)
            {
                switch (arg.ToLowerInvariant())
                {
                    case "-start": case "--start": opts.Start = true; break;
                    case "-stop": case "--stop": opts.Stop = true; break;
                    case "-status": case "--status": opts.Status = true; break;
                    case "-settings": case "--settings": opts.Settings = true; break;
                    case "-userdata": case "--userdata": opts.Userdata = true; break;
                    case "-ui": case "--ui": opts.UI = true; break;
                    case "-elevate-register": opts.ElevateRegister = true; break;
                    case "-elevate-unregister": opts.ElevateUnregister = true; break;
                }
            }
            return opts;
        }

        static void RunCLI(ServiceManager manager, PortableConfig cfg, CliOptions opts)
        {
            if (opts.ElevateRegister)
            {
                RegistrationManager.RegisterDirect(cfg);
                return;
            }
            if (opts.ElevateUnregister)
            {
                RegistrationManager.UnregisterDirect(cfg);
                return;
            }

            if (opts.Start)
            {
                manager.StartService();
                Console.WriteLine("service started");
            }
            if (opts.Stop)
            {
                bool stopped = manager.StopService();
                Console.WriteLine(stopped ? "service stopped" : "service not running");
            }
            if (opts.Status)
            {
                string service = manager.ServiceRunning() ? "running" : "stopped";
                string reason;
                if (manager.InstalledConflict(out reason))
                {
                    Console.WriteLine($"service={service} mode=conflict reason=\"{reason}\"");
                    return;
                }
                Console.WriteLine($"service={service}");
            }
            if (opts.Settings)
            {
                manager.OpenSettings();
                Console.WriteLine("settings opened");
            }
            if (opts.Userdata)
            {
                manager.OpenUserdataDir();
                Console.WriteLine("userdata opened");
            }
        }

        class CliOptions
        {
            public bool Start, Stop, Status, Settings, Userdata, UI;
            public bool ElevateRegister, ElevateUnregister;
            public bool HasAction => Start || Stop || Status || Settings || Userdata ||
                                     ElevateRegister || ElevateUnregister;
        }
    }
}
