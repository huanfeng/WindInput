using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace WindPortable
{
    partial class MainForm : Form
    {
        readonly ServiceManager _manager;
        readonly string _detectError;
        TrayManager _tray;
        CancellationTokenSource _cts = new CancellationTokenSource();
        DateTime _cooldownUntil;
        readonly ManualResetEvent _pollWakeup = new ManualResetEvent(false);

        public MainForm(ServiceManager manager, string detectError)
        {
            _manager = manager;
            _detectError = detectError;

            InitializeComponent();

            this.Text = BuildVariant.IsDebug
                ? "清风输入法便携启动器 (Debug)"
                : "清风输入法便携启动器";

            try
            {
                this.Icon = System.Drawing.Icon.ExtractAssociatedIcon(Application.ExecutablePath);
            }
            catch { }

            if (_detectError != null)
                tabControl.SelectedTab = tabDeploy;

            if (_manager != null)
                lblRootHint.Text = "目录: " + CompactPath(_manager.Config.RootDir);

            // 初始状态：显示"正在检查"，按钮全禁用
            lblStatus.Text = "正在检查服务状态...";
            lblDetail.Text = "";
            SetButtonsEnabled(false);
        }

        protected override void OnShown(EventArgs e)
        {
            base.OnShown(e);

            // 强制完成首次绘制，确保用户立即看到完整界面
            this.Update();

            // 异步初始化：界面已显示，后台检测后更新状态
            Task.Run(() =>
            {
                // 后台采集所有状态
                bool conflict = false;
                bool running = false;
                bool registered = false;
                string conflictPath = null;
                bool hasDeploySource = false;

                if (_manager != null)
                {
                    conflict = _manager.InstalledConflict(out _);
                    if (!conflict)
                    {
                        running = _manager.ServiceRunning();
                        registered = _manager.IsRegistered();
                    }
                    conflictPath = conflict ? _manager.InstalledConflictPath() : null;
                }
                hasDeploySource = PortableConfig.FindDeploySourceDir() != null;

                try
                {
                    this.BeginInvoke((Action)(() =>
                    {
                        if (_manager != null && !conflict)
                            _tray = new TrayManager(this, _manager);
                        ApplyStatus(running, registered, conflict, conflictPath, hasDeploySource);
                    }));
                }
                catch (ObjectDisposedException) { }
                catch (InvalidOperationException) { }

                if (_detectError == null)
                    PollStatus(_cts.Token);
            });

            if (_detectError == null)
                Task.Run(() => ListenShowEvent(_cts.Token));
        }

        protected override void OnFormClosing(FormClosingEventArgs e)
        {
            if (_detectError != null || _manager == null)
            {
                base.OnFormClosing(e);
                return;
            }
            string r;
            if (_manager.InstalledConflict(out r))
            {
                base.OnFormClosing(e);
                return;
            }
            e.Cancel = true;
            HideToTray();
        }

        protected override void OnResize(EventArgs e)
        {
            base.OnResize(e);
            if (WindowState == FormWindowState.Minimized)
            {
                if (_detectError != null || _manager == null) return;
                string r;
                if (_manager.InstalledConflict(out r)) return;
                HideToTray();
            }
        }

        // ── Button events ──

        void BtnStart_Click(object sender, EventArgs e)
        {
            SetButtonsEnabled(false);
            lblStatus.Text = "正在启动服务...";
            Task.Run(() =>
            {
                Exception err = null;
                try { _manager.StartService(); } catch (Exception ex) { err = ex; }
                this.Invoke((Action)(() =>
                {
                    if (err != null)
                    {
                        ShowError(err);
                        RefreshStatus();
                    }
                    else
                    {
                        _cooldownUntil = DateTime.Now.AddSeconds(2);
                        lblStatus.Text = "服务启动中...";
                        lblDetail.Text = "等待服务就绪";
                    }
                    RequestFastPoll();
                }));
            });
        }

        void BtnStop_Click(object sender, EventArgs e)
        {
            SetButtonsEnabled(false);
            lblStatus.Text = "正在停止服务...";
            Task.Run(() =>
            {
                Exception err = null;
                try { _manager.StopService(); } catch (Exception ex) { err = ex; }
                this.Invoke((Action)(() =>
                {
                    if (err != null) ShowError(err);
                    RefreshStatus();
                    RequestFastPoll();
                }));
            });
        }

        void BtnSetting_Click(object sender, EventArgs e)
        {
            try { _manager.OpenSettings(); }
            catch (Exception ex) { ShowError(ex); }
        }

        void BtnData_Click(object sender, EventArgs e)
        {
            try { _manager.OpenUserdataDir(); }
            catch (Exception ex) { ShowError(ex); }
        }

        void BtnUpdate_Click(object sender, EventArgs e)
        {
            using (var dlg = new OpenFileDialog())
            {
                dlg.Title = "选择便携版更新包";
                dlg.Filter = "ZIP 压缩包 (*.zip)|*.zip|所有文件 (*.*)|*.*";
                if (dlg.ShowDialog(this) != DialogResult.OK) return;
                string zipPath = dlg.FileName;

                try { DeployManager.ValidateZip(zipPath); }
                catch (Exception ex) { ShowError(new Exception("无效的更新包: " + ex.Message)); return; }

                if (MessageBox.Show(this, "确认从以下文件更新便携版？\n\n" + zipPath,
                    "确认更新", MessageBoxButtons.OKCancel, MessageBoxIcon.Question) != DialogResult.OK)
                    return;

                SetButtonsEnabled(false);
                lblStatus.Text = "正在更新...";
                lblDetail.Text = "正在停止服务并准备更新";
                Task.Run(() =>
                {
                    Exception err = null;
                    try { PerformUpdate(zipPath); } catch (Exception ex) { err = ex; }
                    this.Invoke((Action)(() =>
                    {
                        if (err != null) ShowError(err);
                        RefreshStatus();
                    }));
                });
            }
        }

        void BtnDeployCopy_Click(object sender, EventArgs e)
        {
            string sourceDir = PortableConfig.FindDeploySourceDir();
            if (sourceDir == null) { ShowError(new Exception("未找到可复制的源文件")); return; }

            using (var dlg = new FolderBrowserDialog())
            {
                dlg.Description = "选择部署目标目录";
                dlg.ShowNewFolderButton = true;
                if (dlg.ShowDialog(this) != DialogResult.OK) return;
                string targetDir = dlg.SelectedPath;

                if (PortableConfig.IsProtectedDir(targetDir))
                { ShowError(new Exception("不能部署到系统保护目录: " + targetDir)); return; }

                if (MessageBox.Show(this,
                    "确认将当前文件复制到以下目录？\n\n源: " + sourceDir + "\n目标: " + targetDir,
                    "确认部署", MessageBoxButtons.OKCancel, MessageBoxIcon.Question) != DialogResult.OK)
                    return;

                SetButtonsEnabled(false);
                lblStatus.Text = "正在部署...";
                lblDetail.Text = "正在复制文件到目标目录";
                Task.Run(() =>
                {
                    Exception err = null;
                    try { DeployManager.DeployFromDirectory(sourceDir, targetDir); }
                    catch (Exception ex) { err = ex; }
                    this.Invoke((Action)(() =>
                    {
                        if (err != null)
                        {
                            ShowError(new Exception("部署失败: " + err.Message));
                        }
                        else
                        {
                            lblDetail.Text = "已部署到: " + CompactPath(targetDir);
                            MessageBox.Show(this,
                                "便携版已成功部署到:\n\n" + targetDir + "\n\n请到该目录运行 wind_portable.exe 启动。",
                                "部署完成", MessageBoxButtons.OK, MessageBoxIcon.Information);
                        }
                        RefreshStatus();
                    }));
                });
            }
        }

        void BtnDeployZip_Click(object sender, EventArgs e)
        {
            string zipPath;
            using (var dlg = new OpenFileDialog())
            {
                dlg.Title = "选择便携版压缩包";
                dlg.Filter = "ZIP 压缩包 (*.zip)|*.zip|所有文件 (*.*)|*.*";
                if (dlg.ShowDialog(this) != DialogResult.OK) return;
                zipPath = dlg.FileName;
            }

            try { DeployManager.ValidateZip(zipPath); }
            catch (Exception ex) { ShowError(new Exception("无效的压缩包: " + ex.Message)); return; }

            using (var dlg = new FolderBrowserDialog())
            {
                dlg.Description = "选择部署目标目录";
                dlg.ShowNewFolderButton = true;
                if (dlg.ShowDialog(this) != DialogResult.OK) return;
                string targetDir = dlg.SelectedPath;

                if (PortableConfig.IsProtectedDir(targetDir))
                { ShowError(new Exception("不能部署到系统保护目录: " + targetDir)); return; }

                if (MessageBox.Show(this, "确认将 ZIP 包部署到以下目录？\n\n" + targetDir,
                    "确认部署", MessageBoxButtons.OKCancel, MessageBoxIcon.Question) != DialogResult.OK)
                    return;

                SetButtonsEnabled(false);
                lblStatus.Text = "正在部署...";
                lblDetail.Text = "正在解压文件到目标目录";
                Task.Run(() =>
                {
                    Exception err = null;
                    try { DeployManager.DeployFromZip(zipPath, targetDir); }
                    catch (Exception ex) { err = ex; }
                    this.Invoke((Action)(() =>
                    {
                        if (err != null)
                        {
                            ShowError(new Exception("部署失败: " + err.Message));
                        }
                        else
                        {
                            lblDetail.Text = "已部署到: " + CompactPath(targetDir);
                            MessageBox.Show(this,
                                "便携版已成功部署到:\n\n" + targetDir + "\n\n请到该目录运行 wind_portable.exe 启动。",
                                "部署完成", MessageBoxButtons.OK, MessageBoxIcon.Information);
                        }
                        RefreshStatus();
                    }));
                });
            }
        }

        // ── Status refresh ──

        /// <summary>
        /// 异步刷新状态：后台采集数据，UI 线程更新控件。
        /// </summary>
        public void RefreshStatus()
        {
            // 纯 UI 状态可直接处理
            if (_detectError != null)
            {
                ApplyDetectError();
                return;
            }

            if (DateTime.Now < _cooldownUntil)
            {
                lblStatus.Text = "服务启动中...";
                lblDetail.Text = "等待服务就绪";
                SetButtonsEnabled(false);
                _tray?.UpdateMenuState(false, false, false);
                return;
            }

            // 后台采集状态数据（RPC + 注册表），不阻塞 UI
            Task.Run(() =>
            {
                bool running = _manager.ServiceRunning();
                bool registered = _manager.IsRegistered();
                string conflictReason;
                bool conflict = _manager.InstalledConflict(out conflictReason);
                string conflictPath = conflict ? _manager.InstalledConflictPath() : null;
                bool hasDeploySource = PortableConfig.FindDeploySourceDir() != null;

                try
                {
                    this.BeginInvoke((Action)(() =>
                        ApplyStatus(running, registered, conflict, conflictPath, hasDeploySource)));
                }
                catch (ObjectDisposedException) { }
                catch (InvalidOperationException) { }
            });
        }

        void ApplyDetectError()
        {
            lblStatus.Text = "便携模式不可用";
            lblDetail.Text = _detectError;
            lblRootHint.Text = "";
            SetButtonsEnabled(false);
            btnUpdate.Enabled = false;
            btnDeployCopy.Enabled = PortableConfig.FindDeploySourceDir() != null;
            btnDeployZip.Enabled = true;
            _tray?.UpdateMenuState(false, false, true);
        }

        void ApplyStatus(bool running, bool registered, bool conflict, string conflictPath, bool hasDeploySource)
        {
            bool stoppable = running || registered;

            if (conflict)
            {
                lblStatus.Text = "检测到已安装正式版";
                lblDetail.Text = "为避免覆盖现有输入法注册信息，便携模式已禁用";
                lblRootHint.Text = "已安装位置: " + CompactPathMax(conflictPath ?? "", 42);
                btnStart.Enabled = false;
                btnStop.Enabled = false;
                btnSetting.Enabled = false;
                btnData.Enabled = false;
                btnUpdate.Enabled = false;
                btnDeployCopy.Enabled = hasDeploySource;
                btnDeployZip.Enabled = true;
                _tray?.UpdateMenuState(false, false, true);
                return;
            }

            if (running)
            {
                lblStatus.Text = "服务状态: 运行中";
                lblDetail.Text = "输入法服务正在运行";
            }
            else
            {
                lblStatus.Text = "服务状态: 已停止";
                lblDetail.Text = "点击启动服务后会自动注册并启动";
            }
            lblRootHint.Text = "目录: " + CompactPathMax(_manager.Config.RootDir, 52);
            btnStart.Enabled = !running;
            btnStop.Enabled = stoppable;
            btnSetting.Enabled = running;
            btnData.Enabled = true;
            btnUpdate.Enabled = true;
            btnDeployCopy.Enabled = true;
            btnDeployZip.Enabled = true;
            _tray?.UpdateMenuState(running, stoppable, false);
        }

        void SetButtonsEnabled(bool enabled)
        {
            btnStart.Enabled = enabled;
            btnStop.Enabled = enabled;
            btnSetting.Enabled = enabled;
            btnData.Enabled = enabled;
            btnUpdate.Enabled = enabled;
            btnDeployCopy.Enabled = enabled;
            btnDeployZip.Enabled = enabled;
        }

        void ShowError(Exception ex)
        {
            MessageBox.Show(this, ex.Message, "清风输入法便携启动器",
                MessageBoxButtons.OK, MessageBoxIcon.Error);
        }

        // ── Update flow ──

        void PerformUpdate(string zipPath)
        {
            UpdateDetail("正在设置守卫标志...");
            _manager.SetStoppedFlag();

            UpdateDetail("正在停止服务...");
            try { _manager.StopService(); } catch { }

            UpdateDetail("正在替换文件...");
            bool needsRestart;
            try
            {
                needsRestart = DeployManager.DeployFromZip(zipPath, _manager.Config.RootDir);
            }
            catch (Exception ex)
            {
                _manager.ClearStoppedFlag();
                throw new Exception("文件替换失败: " + ex.Message);
            }

            _manager.ClearStoppedFlag();

            UpdateDetail("正在重新注册输入法...");
            try
            {
                _manager.StartService();
            }
            catch (Exception ex)
            {
                throw new Exception("重启服务失败: " + ex.Message);
            }

            this.Invoke((Action)(() =>
            {
                if (needsRestart)
                {
                    var ret = MessageBox.Show(this,
                        "启动器已更新到新版本，需要重新启动。\n是否立即重启？",
                        "更新完成", MessageBoxButtons.YesNo, MessageBoxIcon.Information);
                    if (ret == DialogResult.Yes)
                        RestartSelf();
                }
                else
                {
                    MessageBox.Show(this, "便携版更新完成！", "更新完成",
                        MessageBoxButtons.OK, MessageBoxIcon.Information);
                }
            }));
        }

        void UpdateDetail(string text)
        {
            if (InvokeRequired)
                this.Invoke((Action)(() => lblDetail.Text = text));
            else
                lblDetail.Text = text;
        }

        void RestartSelf()
        {
            string exePath = System.Reflection.Assembly.GetExecutingAssembly().Location;
            Process.Start(new ProcessStartInfo(exePath)
            {
                WorkingDirectory = Path.GetDirectoryName(exePath)
            });
            _cts.Cancel();
            Application.Exit();
        }

        // ── Poll + event listener ──

        void PollStatus(CancellationToken token)
        {
            DateTime fastUntil = DateTime.MinValue;
            int interval = 5000;

            while (!token.IsCancellationRequested)
            {
                // 等待信号或超时——RequestFastPoll() 会立即唤醒
                _pollWakeup.WaitOne(interval);
                if (token.IsCancellationRequested) return;

                // 检查是否被唤醒进入快速轮询
                if (_pollWakeup.WaitOne(0))
                {
                    _pollWakeup.Reset();
                    fastUntil = DateTime.Now.AddSeconds(15);
                    interval = 800;
                }

                // 后台采集状态
                bool running = _manager.ServiceRunning();
                bool registered = _manager.IsRegistered();
                string conflictReason;
                bool conflict = _manager.InstalledConflict(out conflictReason);
                string conflictPath = conflict ? _manager.InstalledConflictPath() : null;
                bool hasDeploySource = PortableConfig.FindDeploySourceDir() != null;

                try
                {
                    this.BeginInvoke((Action)(() =>
                        ApplyStatus(running, registered, conflict, conflictPath, hasDeploySource)));
                }
                catch (ObjectDisposedException) { return; }
                catch (InvalidOperationException) { return; }

                if (fastUntil > DateTime.MinValue && DateTime.Now > fastUntil)
                {
                    fastUntil = DateTime.MinValue;
                    interval = 5000;
                }
            }
        }

        void ListenShowEvent(CancellationToken token)
        {
            try
            {
                using (var evt = new EventWaitHandle(false, EventResetMode.AutoReset, BuildVariant.ShowEventName))
                {
                    while (!token.IsCancellationRequested)
                    {
                        if (evt.WaitOne(1000))
                        {
                            try { this.Invoke((Action)ShowFromTray); }
                            catch (ObjectDisposedException) { return; }
                            catch (InvalidOperationException) { return; }
                        }
                    }
                }
            }
            catch { }
        }

        void RequestFastPoll() { _pollWakeup.Set(); }

        // ── Tray ──

        public void HideToTray()
        {
            this.Hide();
        }

        public void ShowFromTray()
        {
            this.Visible = true;
            this.Show();
            this.WindowState = FormWindowState.Normal;
            this.ShowInTaskbar = true;
            // 强制前置窗口
            this.TopMost = true;
            this.TopMost = false;
            this.Activate();
        }

        public void ExitFromTray()
        {
            bool needStop = _manager != null && (_manager.ServiceRunning() || _manager.IsRegistered());

            if (needStop)
            {
                var ret = MessageBox.Show(this,
                    "输入法服务正在运行，退出启动器将同时停止服务并注销输入法。\n\n确定要退出吗？",
                    "清风输入法便携启动器", MessageBoxButtons.OKCancel, MessageBoxIcon.Question);
                if (ret != DialogResult.OK) return;
            }

            // 先隐藏窗口和托盘，让用户感觉已退出
            this.Hide();
            _tray?.Dispose();
            _tray = null;
            _cts.Cancel();

            if (needStop)
            {
                // 异步停止服务，完成后退出进程
                Task.Run(() =>
                {
                    try { _manager.StopService(); } catch { }
                    Environment.Exit(0);
                });
            }
            else
            {
                Environment.Exit(0);
            }
        }

        // ── Helpers ──

        static string CompactPath(string path) { return CompactPathMax(path, 58); }

        static string CompactPathMax(string path, int maxLen)
        {
            if (string.IsNullOrEmpty(path) || maxLen <= 0 || path.Length <= maxLen)
                return path;
            var parts = path.Split('\\');
            if (parts.Length < 4)
                return maxLen <= 3 ? path.Substring(0, maxLen) : path.Substring(0, maxLen - 3) + "...";
            string tail = parts[parts.Length - 2] + @"\" + parts[parts.Length - 1];
            string head = parts[0] + @"\...\";
            if (head.Length + tail.Length <= maxLen)
                return head + tail;
            if (tail.Length > maxLen - 6)
                tail = "..." + tail.Substring(tail.Length - (maxLen - 6));
            return head + tail;
        }
    }
}
