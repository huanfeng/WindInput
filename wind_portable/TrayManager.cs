using System;
using System.Drawing;
using System.Windows.Forms;

namespace WindPortable
{
    class TrayManager : IDisposable
    {
        readonly MainForm _form;
        readonly ServiceManager _manager;
        readonly NotifyIcon _notifyIcon;
        readonly ContextMenuStrip _menu;
        readonly ToolStripMenuItem _menuStart;
        readonly ToolStripMenuItem _menuStop;
        readonly ToolStripMenuItem _menuSetting;
        readonly ToolStripMenuItem _menuData;

        public TrayManager(MainForm form, ServiceManager manager)
        {
            _form = form;
            _manager = manager;

            _menu = new ContextMenuStrip();
            var menuShow = new ToolStripMenuItem("显示窗口");
            menuShow.Click += (s, e) => _form.ShowFromTray();
            _menu.Items.Add(menuShow);
            _menu.Items.Add(new ToolStripSeparator());

            _menuStart = new ToolStripMenuItem("启动服务");
            _menuStart.Click += (s, e) =>
            {
                try { _manager.StartService(); }
                catch (Exception ex) { MessageBox.Show(ex.Message, "错误", MessageBoxButtons.OK, MessageBoxIcon.Error); }
                _form.RefreshStatus();
            };
            _menu.Items.Add(_menuStart);

            _menuStop = new ToolStripMenuItem("停止服务");
            _menuStop.Click += (s, e) =>
            {
                try { _manager.StopService(); }
                catch (Exception ex) { MessageBox.Show(ex.Message, "错误", MessageBoxButtons.OK, MessageBoxIcon.Error); }
                _form.RefreshStatus();
            };
            _menu.Items.Add(_menuStop);

            _menuSetting = new ToolStripMenuItem("打开设置");
            _menuSetting.Click += (s, e) =>
            {
                try { _manager.OpenSettings(); }
                catch (Exception ex) { MessageBox.Show(ex.Message, "错误", MessageBoxButtons.OK, MessageBoxIcon.Error); }
            };
            _menu.Items.Add(_menuSetting);

            _menuData = new ToolStripMenuItem("打开数据目录");
            _menuData.Click += (s, e) =>
            {
                try { _manager.OpenUserdataDir(); }
                catch (Exception ex) { MessageBox.Show(ex.Message, "错误", MessageBoxButtons.OK, MessageBoxIcon.Error); }
            };
            _menu.Items.Add(_menuData);

            _menu.Items.Add(new ToolStripSeparator());
            var menuExit = new ToolStripMenuItem("退出");
            menuExit.Click += (s, e) => _form.ExitFromTray();
            _menu.Items.Add(menuExit);

            _notifyIcon = new NotifyIcon
            {
                Text = "清风输入法便携启动器",
                ContextMenuStrip = _menu,
                Visible = true,
            };

            try { _notifyIcon.Icon = _form.Icon; }
            catch { _notifyIcon.Icon = SystemIcons.Application; }

            _notifyIcon.MouseClick += (s, e) =>
            {
                if (e.Button == MouseButtons.Left)
                    _form.ShowFromTray();
            };

            // 初始菜单状态全部禁用，等第一次 RefreshStatus 后由外部调用 UpdateMenuState 更新
            UpdateMenuState(false, false, false);
        }

        public void UpdateMenuState(bool running, bool stoppable, bool conflict)
        {
            bool enable = !conflict;
            _menuStart.Enabled = enable && !running;
            _menuStop.Enabled = enable && stoppable;
            _menuSetting.Enabled = enable && running; // 设置需要服务运行
            _menuData.Enabled = enable;
        }

        public void Dispose()
        {
            _notifyIcon.Visible = false;
            _notifyIcon.Dispose();
            _menu.Dispose();
        }
    }
}
