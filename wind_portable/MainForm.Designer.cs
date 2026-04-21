namespace WindPortable
{
    partial class MainForm
    {
        private System.ComponentModel.IContainer components = null;

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                _cts?.Cancel();
                _tray?.Dispose();
                components?.Dispose();
            }
            base.Dispose(disposing);
        }

        private void InitializeComponent()
        {
            this.components = new System.ComponentModel.Container();
            this.tabControl = new System.Windows.Forms.TabControl();
            this.tabRun = new System.Windows.Forms.TabPage();
            this.tabDeploy = new System.Windows.Forms.TabPage();

            this.lblStatus = new System.Windows.Forms.Label();
            this.lblDetail = new System.Windows.Forms.Label();
            this.lblRootHint = new System.Windows.Forms.Label();
            this.btnStart = new System.Windows.Forms.Button();
            this.btnStop = new System.Windows.Forms.Button();
            this.btnSetting = new System.Windows.Forms.Button();
            this.btnData = new System.Windows.Forms.Button();

            this.lblDeployHint = new System.Windows.Forms.Label();
            this.btnUpdate = new System.Windows.Forms.Button();
            this.btnDeployCopy = new System.Windows.Forms.Button();
            this.btnDeployZip = new System.Windows.Forms.Button();

            this.tabControl.SuspendLayout();
            this.tabRun.SuspendLayout();
            this.tabDeploy.SuspendLayout();
            this.SuspendLayout();

            // tabControl
            this.tabControl.Controls.Add(this.tabRun);
            this.tabControl.Controls.Add(this.tabDeploy);
            this.tabControl.Location = new System.Drawing.Point(4, 4);
            this.tabControl.Size = new System.Drawing.Size(446, 258);
            this.tabControl.HotTrack = true;
            this.tabControl.Name = "tabControl";

            // tabRun
            this.tabRun.Text = "运行";
            this.tabRun.Padding = new System.Windows.Forms.Padding(3);
            this.tabRun.Name = "tabRun";
            this.tabRun.Controls.Add(this.lblStatus);
            this.tabRun.Controls.Add(this.lblDetail);
            this.tabRun.Controls.Add(this.lblRootHint);
            this.tabRun.Controls.Add(this.btnStart);
            this.tabRun.Controls.Add(this.btnStop);
            this.tabRun.Controls.Add(this.btnSetting);
            this.tabRun.Controls.Add(this.btnData);

            // lblStatus
            this.lblStatus.Location = new System.Drawing.Point(10, 10);
            this.lblStatus.Size = new System.Drawing.Size(400, 20);
            this.lblStatus.Text = "正在检查服务状态...";
            this.lblStatus.Name = "lblStatus";

            // lblDetail
            this.lblDetail.Location = new System.Drawing.Point(10, 34);
            this.lblDetail.Size = new System.Drawing.Size(410, 50);
            this.lblDetail.Text = "准备就绪";
            this.lblDetail.Name = "lblDetail";

            // lblRootHint
            this.lblRootHint.Location = new System.Drawing.Point(10, 90);
            this.lblRootHint.Size = new System.Drawing.Size(410, 18);
            this.lblRootHint.Name = "lblRootHint";

            // btnStart
            this.btnStart.Location = new System.Drawing.Point(10, 118);
            this.btnStart.Size = new System.Drawing.Size(100, 32);
            this.btnStart.Text = "启动服务";
            this.btnStart.Name = "btnStart";
            this.btnStart.UseVisualStyleBackColor = true;
            this.btnStart.Click += new System.EventHandler(this.BtnStart_Click);

            // btnStop
            this.btnStop.Location = new System.Drawing.Point(120, 118);
            this.btnStop.Size = new System.Drawing.Size(100, 32);
            this.btnStop.Text = "停止服务";
            this.btnStop.Name = "btnStop";
            this.btnStop.UseVisualStyleBackColor = true;
            this.btnStop.Click += new System.EventHandler(this.BtnStop_Click);

            // btnSetting
            this.btnSetting.Location = new System.Drawing.Point(10, 160);
            this.btnSetting.Size = new System.Drawing.Size(100, 32);
            this.btnSetting.Text = "打开设置";
            this.btnSetting.Name = "btnSetting";
            this.btnSetting.UseVisualStyleBackColor = true;
            this.btnSetting.Click += new System.EventHandler(this.BtnSetting_Click);

            // btnData
            this.btnData.Location = new System.Drawing.Point(120, 160);
            this.btnData.Size = new System.Drawing.Size(120, 32);
            this.btnData.Text = "打开数据目录";
            this.btnData.Name = "btnData";
            this.btnData.UseVisualStyleBackColor = true;
            this.btnData.Click += new System.EventHandler(this.BtnData_Click);

            // tabDeploy
            this.tabDeploy.Text = "部署";
            this.tabDeploy.Padding = new System.Windows.Forms.Padding(3);
            this.tabDeploy.Name = "tabDeploy";
            this.tabDeploy.Controls.Add(this.lblDeployHint);
            this.tabDeploy.Controls.Add(this.btnUpdate);
            this.tabDeploy.Controls.Add(this.btnDeployCopy);
            this.tabDeploy.Controls.Add(this.btnDeployZip);

            // lblDeployHint
            this.lblDeployHint.Location = new System.Drawing.Point(10, 10);
            this.lblDeployHint.Size = new System.Drawing.Size(410, 36);
            this.lblDeployHint.Text = "更新当前安装、复制当前文件到新目录、或从 ZIP 包部署到新目录。";
            this.lblDeployHint.Name = "lblDeployHint";

            // btnUpdate
            this.btnUpdate.Location = new System.Drawing.Point(10, 56);
            this.btnUpdate.Size = new System.Drawing.Size(128, 32);
            this.btnUpdate.Text = "更新当前版本";
            this.btnUpdate.Name = "btnUpdate";
            this.btnUpdate.UseVisualStyleBackColor = true;
            this.btnUpdate.Click += new System.EventHandler(this.BtnUpdate_Click);

            // btnDeployCopy
            this.btnDeployCopy.Location = new System.Drawing.Point(148, 56);
            this.btnDeployCopy.Size = new System.Drawing.Size(128, 32);
            this.btnDeployCopy.Text = "复制到目录";
            this.btnDeployCopy.Name = "btnDeployCopy";
            this.btnDeployCopy.UseVisualStyleBackColor = true;
            this.btnDeployCopy.Click += new System.EventHandler(this.BtnDeployCopy_Click);

            // btnDeployZip
            this.btnDeployZip.Location = new System.Drawing.Point(286, 56);
            this.btnDeployZip.Size = new System.Drawing.Size(128, 32);
            this.btnDeployZip.Text = "从 ZIP 部署";
            this.btnDeployZip.Name = "btnDeployZip";
            this.btnDeployZip.UseVisualStyleBackColor = true;
            this.btnDeployZip.Click += new System.EventHandler(this.BtnDeployZip_Click);

            // MainForm
            this.AutoScaleDimensions = new System.Drawing.SizeF(96F, 96F);
            this.AutoScaleMode = System.Windows.Forms.AutoScaleMode.Dpi;
            this.ClientSize = new System.Drawing.Size(454, 268);
            this.Controls.Add(this.tabControl);
            this.FormBorderStyle = System.Windows.Forms.FormBorderStyle.FixedSingle;
            this.MaximizeBox = false;
            this.StartPosition = System.Windows.Forms.FormStartPosition.CenterScreen;
            this.Name = "MainForm";
            this.Text = "清风输入法便携启动器";

            this.tabControl.ResumeLayout(false);
            this.tabRun.ResumeLayout(false);
            this.tabDeploy.ResumeLayout(false);
            this.ResumeLayout(false);
        }

        private System.Windows.Forms.TabControl tabControl;
        private System.Windows.Forms.TabPage tabRun;
        private System.Windows.Forms.TabPage tabDeploy;
        private System.Windows.Forms.Label lblStatus;
        private System.Windows.Forms.Label lblDetail;
        private System.Windows.Forms.Label lblRootHint;
        private System.Windows.Forms.Button btnStart;
        private System.Windows.Forms.Button btnStop;
        private System.Windows.Forms.Button btnSetting;
        private System.Windows.Forms.Button btnData;
        private System.Windows.Forms.Label lblDeployHint;
        private System.Windows.Forms.Button btnUpdate;
        private System.Windows.Forms.Button btnDeployCopy;
        private System.Windows.Forms.Button btnDeployZip;
    }
}
