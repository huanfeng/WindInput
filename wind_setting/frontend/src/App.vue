<script setup lang="ts">
import { ref, onMounted } from 'vue';
import * as api from './api/settings';
import type { Config, Status, EngineInfo } from './api/settings';

// 状态
const loading = ref(true);
const error = ref('');
const connected = ref(false);
const activeTab = ref('general');
const saving = ref(false);
const saveMessage = ref('');

// 数据
const config = ref<Config | null>(null);
const status = ref<Status | null>(null);
const engines = ref<EngineInfo[]>([]);

// 表单数据（用于编辑）
const formData = ref<Partial<Config>>({});

// 标签页
const tabs = [
  { id: 'general', label: '常规', icon: '⚙️' },
  { id: 'engine', label: '引擎', icon: '🔧' },
  { id: 'ui', label: '界面', icon: '🎨' },
  { id: 'hotkey', label: '快捷键', icon: '⌨️' },
  { id: 'about', label: '关于', icon: 'ℹ️' },
];

// 加载数据
async function loadData() {
  loading.value = true;
  error.value = '';

  try {
    // 检查连接
    const healthRes = await api.checkHealth();
    if (!healthRes.success) {
      connected.value = false;
      error.value = '无法连接到输入法服务，请确保 WindInput 正在运行';
      loading.value = false;
      return;
    }
    connected.value = true;

    // 加载配置
    const configRes = await api.getConfig();
    if (configRes.success && configRes.data) {
      config.value = configRes.data;
      formData.value = JSON.parse(JSON.stringify(configRes.data));
    }

    // 加载状态
    const statusRes = await api.getStatus();
    if (statusRes.success && statusRes.data) {
      status.value = statusRes.data;
    }

    // 加载引擎列表
    const enginesRes = await api.getEngineList();
    if (enginesRes.success && enginesRes.data) {
      engines.value = enginesRes.data.engines;
    }
  } catch (e) {
    error.value = '加载数据失败';
  } finally {
    loading.value = false;
  }
}

// 保存配置
async function saveConfig() {
  if (!formData.value) return;

  saving.value = true;
  saveMessage.value = '';

  try {
    const res = await api.updateConfig(formData.value);
    if (res.success && res.data) {
      saveMessage.value = '保存成功';
      if (res.data.needReload.length > 0) {
        saveMessage.value += '（部分设置需要重载生效）';
      }
      await loadData();
    } else {
      saveMessage.value = res.error || '保存失败';
    }
  } catch (e) {
    saveMessage.value = '保存失败';
  } finally {
    saving.value = false;
    setTimeout(() => { saveMessage.value = ''; }, 3000);
  }
}

// 切换引擎
async function handleSwitchEngine(type: string) {
  try {
    const res = await api.switchEngine(type);
    if (res.success) {
      await loadData();
    }
  } catch (e) {
    console.error('切换引擎失败', e);
  }
}

// 重载配置
async function handleReload() {
  try {
    const res = await api.reloadConfig();
    if (res.success) {
      saveMessage.value = '重载成功';
      await loadData();
    } else {
      saveMessage.value = res.error || '重载失败';
    }
  } catch (e) {
    saveMessage.value = '重载失败';
  }
}

// 重置为默认值
function resetToDefault() {
  if (config.value) {
    formData.value = JSON.parse(JSON.stringify(config.value));
  }
}

onMounted(() => {
  loadData();
});
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="logo">
        <span class="logo-icon">🌬️</span>
        <span class="logo-text">WindInput</span>
      </div>
      <nav class="nav">
        <button
          v-for="tab in tabs"
          :key="tab.id"
          :class="['nav-item', { active: activeTab === tab.id }]"
          @click="activeTab = tab.id"
        >
          <span class="nav-icon">{{ tab.icon }}</span>
          <span class="nav-label">{{ tab.label }}</span>
        </button>
      </nav>
      <div class="sidebar-footer">
        <div v-if="connected" class="status-badge connected">
          <span class="status-dot"></span>
          已连接
        </div>
        <div v-else class="status-badge disconnected">
          <span class="status-dot"></span>
          未连接
        </div>
      </div>
    </aside>

    <main class="main">
      <div v-if="loading" class="loading">
        <div class="spinner"></div>
        <p>加载中...</p>
      </div>

      <div v-else-if="error" class="error-panel">
        <div class="error-icon">⚠️</div>
        <p>{{ error }}</p>
        <button class="btn btn-primary" @click="loadData">重试</button>
      </div>

      <div v-else class="content">
        <!-- 常规设置 -->
        <section v-if="activeTab === 'general'" class="section">
          <h2>常规设置</h2>
          <div class="form-group">
            <label class="checkbox-label">
              <input type="checkbox" v-model="formData.general!.start_in_chinese_mode" />
              <span>启动时默认中文模式</span>
            </label>
            <p class="hint">启用后，每次激活输入法时默认为中文输入状态</p>
          </div>
          <div class="form-group">
            <label>日志级别</label>
            <select v-model="formData.general!.log_level" class="select">
              <option value="debug">Debug（调试）</option>
              <option value="info">Info（信息）</option>
              <option value="warn">Warn（警告）</option>
              <option value="error">Error（错误）</option>
            </select>
            <p class="hint">更改日志级别需要重启服务才能生效</p>
          </div>
        </section>

        <!-- 引擎设置 -->
        <section v-if="activeTab === 'engine'" class="section">
          <h2>引擎设置</h2>
          <div class="engine-selector">
            <h3>当前引擎</h3>
            <div class="engine-cards">
              <div
                v-for="engine in engines"
                :key="engine.type"
                :class="['engine-card', { active: engine.isActive }]"
                @click="handleSwitchEngine(engine.type)"
              >
                <div class="engine-name">
                  <span class="engine-badge">{{ engine.displayName }}</span>
                  {{ engine.type === 'pinyin' ? '拼音' : '五笔' }}
                </div>
                <p class="engine-desc">{{ engine.description }}</p>
                <div v-if="engine.isActive" class="engine-active-badge">当前使用</div>
              </div>
            </div>
          </div>

          <template v-if="formData.engine?.type === 'pinyin'">
            <h3>拼音设置</h3>
            <div class="form-group">
              <label class="checkbox-label">
                <input type="checkbox" v-model="formData.engine!.pinyin.show_wubi_hint" />
                <span>显示五笔编码提示</span>
              </label>
              <p class="hint">在候选词旁边显示对应的五笔编码，方便学习五笔</p>
            </div>
          </template>

          <template v-if="formData.engine?.type === 'wubi'">
            <h3>五笔设置</h3>
            <div class="form-group">
              <label>自动上屏模式</label>
              <select v-model="formData.engine!.wubi.auto_commit" class="select">
                <option value="none">不自动上屏</option>
                <option value="unique">候选唯一时上屏</option>
                <option value="unique_at_4">四码唯一时上屏</option>
                <option value="unique_full_match">完整匹配唯一时上屏</option>
              </select>
            </div>
            <div class="form-group">
              <label>空码处理</label>
              <select v-model="formData.engine!.wubi.empty_code" class="select">
                <option value="none">不处理（继续输入）</option>
                <option value="clear">清空编码</option>
                <option value="clear_at_4">四码时清空</option>
                <option value="to_english">转为英文上屏</option>
              </select>
            </div>
            <div class="form-group">
              <label class="checkbox-label">
                <input type="checkbox" v-model="formData.engine!.wubi.top_code_commit" />
                <span>五码顶字上屏</span>
              </label>
              <p class="hint">输入第五码时自动上屏首选并将第五码作为新输入</p>
            </div>
            <div class="form-group">
              <label class="checkbox-label">
                <input type="checkbox" v-model="formData.engine!.wubi.punct_commit" />
                <span>标点顶字上屏</span>
              </label>
            </div>
          </template>
        </section>

        <!-- 界面设置 -->
        <section v-if="activeTab === 'ui'" class="section">
          <h2>界面设置</h2>
          <div class="form-group">
            <label>字体大小</label>
            <div class="range-input">
              <input type="range" min="12" max="36" step="1" v-model.number="formData.ui!.font_size" />
              <span class="range-value">{{ formData.ui?.font_size }}px</span>
            </div>
          </div>
          <div class="form-group">
            <label>每页候选数</label>
            <div class="range-input">
              <input type="range" min="3" max="9" step="1" v-model.number="formData.ui!.candidates_per_page" />
              <span class="range-value">{{ formData.ui?.candidates_per_page }}</span>
            </div>
          </div>
          <div class="form-group">
            <label>自定义字体路径</label>
            <input type="text" v-model="formData.ui!.font_path" class="input" placeholder="留空使用系统默认字体" />
            <p class="hint">指定字体文件路径，如 C:\Windows\Fonts\msyh.ttc</p>
          </div>
        </section>

        <!-- 快捷键设置 -->
        <section v-if="activeTab === 'hotkey'" class="section">
          <h2>快捷键设置</h2>
          <div class="form-group">
            <label>切换中英文</label>
            <select v-model="formData.hotkeys!.toggle_mode" class="select">
              <option value="shift">Shift</option>
              <option value="ctrl+space">Ctrl + Space</option>
            </select>
          </div>
          <div class="form-group">
            <label>切换引擎</label>
            <select v-model="formData.hotkeys!.switch_engine" class="select">
              <option value="ctrl+`">Ctrl + `</option>
              <option value="ctrl+shift+e">Ctrl + Shift + E</option>
            </select>
          </div>
        </section>

        <!-- 关于 -->
        <section v-if="activeTab === 'about'" class="section">
          <h2>关于</h2>
          <div class="about-card" v-if="status">
            <div class="about-header">
              <span class="about-icon">🌬️</span>
              <div>
                <h3>{{ status.service.name }}</h3>
                <p>版本 {{ status.service.version }}</p>
              </div>
            </div>
            <div class="about-stats">
              <div class="stat-item">
                <span class="stat-label">运行时间</span>
                <span class="stat-value">{{ status.service.uptime }}</span>
              </div>
              <div class="stat-item">
                <span class="stat-label">当前引擎</span>
                <span class="stat-value">{{ status.engine.info }}</span>
              </div>
              <div class="stat-item">
                <span class="stat-label">内存使用</span>
                <span class="stat-value">{{ status.memory.allocMB }}</span>
              </div>
              <div class="stat-item">
                <span class="stat-label">系统内存</span>
                <span class="stat-value">{{ status.memory.sysMB }}</span>
              </div>
            </div>
          </div>
          <div class="about-actions">
            <button class="btn btn-secondary" @click="handleReload">重载配置</button>
            <button class="btn btn-secondary" @click="loadData">刷新状态</button>
          </div>
        </section>

        <!-- 底部操作栏 -->
        <div class="action-bar" v-if="activeTab !== 'about'">
          <div class="action-message">
            <span v-if="saveMessage" :class="['message', saveMessage.includes('失败') ? 'error' : 'success']">
              {{ saveMessage }}
            </span>
          </div>
          <div class="action-buttons">
            <button class="btn btn-secondary" @click="resetToDefault">重置</button>
            <button class="btn btn-primary" @click="saveConfig" :disabled="saving">
              {{ saving ? '保存中...' : '保存' }}
            </button>
          </div>
        </div>
      </div>
    </main>
  </div>
</template>

<style>
* { margin: 0; padding: 0; box-sizing: border-box; }

body {
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'Microsoft YaHei UI', sans-serif;
  background: #f5f5f5;
  color: #333;
  line-height: 1.6;
}

.app { display: flex; min-height: 100vh; }

.sidebar {
  width: 200px;
  background: #fff;
  border-right: 1px solid #e0e0e0;
  display: flex;
  flex-direction: column;
}

.logo {
  padding: 20px;
  display: flex;
  align-items: center;
  gap: 10px;
  border-bottom: 1px solid #e0e0e0;
}

.logo-icon { font-size: 24px; }
.logo-text { font-size: 18px; font-weight: 600; color: #333; }

.nav { flex: 1; padding: 10px; }

.nav-item {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 16px;
  border: none;
  background: none;
  cursor: pointer;
  border-radius: 8px;
  font-size: 14px;
  color: #666;
  transition: all 0.2s;
}

.nav-item:hover { background: #f0f0f0; }
.nav-item.active { background: #e8f0fe; color: #1a73e8; }
.nav-icon { font-size: 18px; }

.sidebar-footer { padding: 15px; border-top: 1px solid #e0e0e0; }

.status-badge {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  padding: 6px 10px;
  border-radius: 20px;
}

.status-badge.connected { background: #e6f4ea; color: #1e8e3e; }
.status-badge.disconnected { background: #fce8e6; color: #d93025; }
.status-dot { width: 8px; height: 8px; border-radius: 50%; background: currentColor; }

.main { flex: 1; display: flex; flex-direction: column; overflow: hidden; }
.content { flex: 1; padding: 30px; overflow-y: auto; }
.section { max-width: 600px; }
.section h2 { font-size: 24px; font-weight: 600; margin-bottom: 24px; color: #333; }
.section h3 { font-size: 16px; font-weight: 600; margin: 24px 0 16px; color: #555; }

.form-group { margin-bottom: 20px; }
.form-group > label { display: block; font-size: 14px; font-weight: 500; margin-bottom: 8px; color: #333; }
.checkbox-label { display: flex; align-items: center; gap: 10px; cursor: pointer; }
.checkbox-label input[type="checkbox"] { width: 18px; height: 18px; cursor: pointer; }
.hint { font-size: 12px; color: #888; margin-top: 6px; }

.select {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid #ddd;
  border-radius: 6px;
  font-size: 14px;
  background: #fff;
  cursor: pointer;
}
.select:focus { outline: none; border-color: #1a73e8; }

.input {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid #ddd;
  border-radius: 6px;
  font-size: 14px;
}
.input:focus { outline: none; border-color: #1a73e8; }

.range-input { display: flex; align-items: center; gap: 15px; }
.range-input input[type="range"] {
  flex: 1;
  height: 6px;
  -webkit-appearance: none;
  background: #ddd;
  border-radius: 3px;
}
.range-input input[type="range"]::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 18px;
  height: 18px;
  background: #1a73e8;
  border-radius: 50%;
  cursor: pointer;
}
.range-value { min-width: 50px; font-size: 14px; font-weight: 500; color: #333; }

.engine-selector { margin-bottom: 30px; }
.engine-cards { display: grid; grid-template-columns: repeat(2, 1fr); gap: 15px; }

.engine-card {
  padding: 20px;
  border: 2px solid #e0e0e0;
  border-radius: 12px;
  cursor: pointer;
  transition: all 0.2s;
  position: relative;
}
.engine-card:hover { border-color: #1a73e8; }
.engine-card.active { border-color: #1a73e8; background: #e8f0fe; }

.engine-name {
  font-size: 16px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 8px;
}

.engine-badge {
  width: 28px;
  height: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #1a73e8;
  color: #fff;
  border-radius: 6px;
  font-size: 14px;
}

.engine-desc { font-size: 12px; color: #666; }

.engine-active-badge {
  position: absolute;
  top: 10px;
  right: 10px;
  font-size: 11px;
  padding: 3px 8px;
  background: #1a73e8;
  color: #fff;
  border-radius: 10px;
}

.about-card {
  background: #fff;
  border-radius: 12px;
  padding: 24px;
  box-shadow: 0 1px 3px rgba(0,0,0,0.1);
}

.about-header { display: flex; align-items: center; gap: 15px; margin-bottom: 20px; }
.about-icon { font-size: 48px; }
.about-header h3 { font-size: 20px; margin: 0; }
.about-header p { color: #666; margin: 4px 0 0; }

.about-stats { display: grid; grid-template-columns: repeat(2, 1fr); gap: 15px; }
.stat-item { padding: 12px; background: #f5f5f5; border-radius: 8px; }
.stat-label { display: block; font-size: 12px; color: #888; margin-bottom: 4px; }
.stat-value { font-size: 14px; font-weight: 500; }

.about-actions { margin-top: 20px; display: flex; gap: 10px; }

.action-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 15px 30px;
  background: #fff;
  border-top: 1px solid #e0e0e0;
}

.action-message { flex: 1; }
.message { font-size: 14px; padding: 6px 12px; border-radius: 4px; }
.message.success { background: #e6f4ea; color: #1e8e3e; }
.message.error { background: #fce8e6; color: #d93025; }
.action-buttons { display: flex; gap: 10px; }

.btn {
  padding: 10px 20px;
  border: none;
  border-radius: 6px;
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s;
}
.btn:disabled { opacity: 0.6; cursor: not-allowed; }
.btn-primary { background: #1a73e8; color: #fff; }
.btn-primary:hover:not(:disabled) { background: #1557b0; }
.btn-secondary { background: #f0f0f0; color: #333; }
.btn-secondary:hover:not(:disabled) { background: #e0e0e0; }

.loading {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 15px;
  color: #666;
}

.spinner {
  width: 40px;
  height: 40px;
  border: 3px solid #e0e0e0;
  border-top-color: #1a73e8;
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

@keyframes spin { to { transform: rotate(360deg); } }

.error-panel {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 15px;
  text-align: center;
  padding: 30px;
}

.error-icon { font-size: 48px; }
.error-panel p { color: #666; max-width: 300px; }
</style>
