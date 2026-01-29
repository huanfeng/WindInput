<script setup lang="ts">
import { ref, onMounted, computed, watch, onUnmounted, nextTick } from 'vue';
import * as api from './api/settings';
import type { Config, Status, EngineInfo, LogEntry } from './api/settings';
import { getDefaultConfig } from './api/settings';

// 状态
const loading = ref(true);
const error = ref('');
const connected = ref(false);
const activeTab = ref('general');
const saving = ref(false);
const saveMessage = ref('');
const saveMessageType = ref<'success' | 'error'>('success');
const hotkeyConflicts = ref<string[]>([]);

// 数据
const config = ref<Config | null>(null);
const status = ref<Status | null>(null);
const engines = ref<EngineInfo[]>([]);

// 表单数据（用于编辑）
const formData = ref<Config>(getDefaultConfig());

// 测试页面状态
const testInput = ref('');
const testCandidates = ref<any[]>([]);
const testEngine = ref('current');
const testFilterMode = ref('current');
const testLoading = ref(false);

// 日志页面状态
const logs = ref<LogEntry[]>([]);
const logLevel = ref('all');
const logFilter = ref('');
const logAutoScroll = ref(true);
const logTotal = ref(0);
const logContentRef = ref<HTMLElement | null>(null);
let logTimer: number | null = null;

// 重新组织的标签页 - 按用户视角划分
const tabs = [
  { id: 'general', label: '常用', icon: '🏠' },
  { id: 'input', label: '输入', icon: '⌨' },
  { id: 'appearance', label: '外观', icon: '🎨' },
  { id: 'dictionary', label: '词库', icon: '📚' },
  { id: 'hotkey', label: '按键', icon: '🎮' },
  { id: 'test', label: '测试', icon: '🧪' },
  { id: 'advanced', label: '高级', icon: '🛠' },
  { id: 'about', label: '关于', icon: 'ℹ' },
];

// 是否显示底部操作栏
const showActionBar = computed(() => {
  return !['about', 'dictionary', 'test'].includes(activeTab.value);
});

// 检查快捷键冲突
function checkConflicts() {
  const conflicts: string[] = [];
  const usedKeys = new Map<string, string>();

  // 中英切换键
  for (const key of formData.value.hotkeys.toggle_mode_keys) {
    if (usedKeys.has(key)) {
      conflicts.push(`按键 "${getKeyLabel(key)}" 同时用于: ${usedKeys.get(key)} 和 中英切换`);
    } else {
      usedKeys.set(key, '中英切换');
    }
  }

  // 候选选择键组
  for (const group of formData.value.input.select_key_groups) {
    const keys = getGroupKeys(group);
    for (const key of keys) {
      if (usedKeys.has(key)) {
        conflicts.push(`按键 "${getKeyLabel(key)}" 同时用于: ${usedKeys.get(key)} 和 候选选择`);
      } else {
        usedKeys.set(key, '候选选择');
      }
    }
  }

  hotkeyConflicts.value = conflicts;
}

function getGroupKeys(group: string): string[] {
  switch (group) {
    case 'semicolon_quote': return ['semicolon', 'quote'];
    case 'comma_period': return ['comma', 'period'];
    case 'lrshift': return ['lshift', 'rshift'];
    case 'lrctrl': return ['lctrl', 'rctrl'];
    default: return [];
  }
}

function getKeyLabel(key: string): string {
  const labels: Record<string, string> = {
    'lshift': '左Shift', 'rshift': '右Shift',
    'lctrl': '左Ctrl', 'rctrl': '右Ctrl',
    'capslock': 'CapsLock',
    'semicolon': ';', 'quote': "'",
    'comma': ',', 'period': '.',
  };
  return labels[key] || key;
}

// 监听配置变化，检查冲突
watch(() => [formData.value.hotkeys.toggle_mode_keys, formData.value.input.select_key_groups], checkConflicts, { deep: true });

// 切换多选数组中的值
function toggleArrayValue(arr: string[], value: string) {
  const idx = arr.indexOf(value);
  if (idx >= 0) {
    arr.splice(idx, 1);
  } else {
    arr.push(value);
  }
  checkConflicts();
}

// 加载数据
async function loadData() {
  loading.value = true;
  error.value = '';

  try {
    const healthRes = await api.checkHealth();
    if (!healthRes.success) {
      connected.value = false;
      error.value = '无法连接到输入法服务，请确保 WindInput 正在运行';
      loading.value = false;
      return;
    }
    connected.value = true;

    const configRes = await api.getConfig();
    if (configRes.success && configRes.data) {
      const cfg = mergeWithDefaults(configRes.data);
      config.value = cfg;
      formData.value = JSON.parse(JSON.stringify(cfg));
      checkConflicts();
    }

    const statusRes = await api.getStatus();
    if (statusRes.success && statusRes.data) {
      status.value = statusRes.data;
    }

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

// 合并默认配置
function mergeWithDefaults(cfg: any): Config {
  const defaults = getDefaultConfig();
  return {
    startup: { ...defaults.startup, ...cfg.startup },
    dictionary: { ...defaults.dictionary, ...cfg.dictionary },
    engine: {
      ...defaults.engine,
      ...cfg.engine,
      pinyin: { ...defaults.engine.pinyin, ...cfg.engine?.pinyin },
      wubi: { ...defaults.engine.wubi, ...cfg.engine?.wubi },
    },
    hotkeys: { ...defaults.hotkeys, ...cfg.hotkeys },
    ui: { ...defaults.ui, ...cfg.ui },
    toolbar: { ...defaults.toolbar, ...cfg.toolbar },
    input: { ...defaults.input, ...cfg.input },
    advanced: { ...defaults.advanced, ...cfg.advanced },
  };
}

// 保存配置
async function saveConfig() {
  if (hotkeyConflicts.value.length > 0) {
    saveMessageType.value = 'error';
    saveMessage.value = '存在快捷键冲突，请先解决';
    setTimeout(() => { saveMessage.value = ''; }, 3000);
    return;
  }

  saving.value = true;
  saveMessage.value = '';

  try {
    const res = await api.updateConfig(formData.value);
    if (res.success && res.data) {
      saveMessageType.value = 'success';
      saveMessage.value = '保存成功';
      if (res.data.needReload.length > 0) {
        saveMessage.value += '（部分设置需要重载生效）';
      }
      config.value = JSON.parse(JSON.stringify(formData.value));
    } else {
      saveMessageType.value = 'error';
      saveMessage.value = res.error || '保存失败';
    }
  } catch (e) {
    saveMessageType.value = 'error';
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
      formData.value.engine.type = type;
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
      saveMessageType.value = 'success';
      saveMessage.value = '重载成功';
      await loadData();
    } else {
      saveMessageType.value = 'error';
      saveMessage.value = res.error || '重载失败';
    }
  } catch (e) {
    saveMessageType.value = 'error';
    saveMessage.value = '重载失败';
  }
  setTimeout(() => { saveMessage.value = ''; }, 3000);
}

// 刷新状态
async function refreshStatus() {
  try {
    const statusRes = await api.getStatus();
    if (statusRes.success && statusRes.data) {
      status.value = statusRes.data;
    }
  } catch (e) {
    console.error('刷新状态失败', e);
  }
}

// 重置为当前配置
function resetToDefault() {
  if (config.value) {
    formData.value = JSON.parse(JSON.stringify(config.value));
    checkConflicts();
    saveMessageType.value = 'success';
    saveMessage.value = '已重置为当前配置';
    setTimeout(() => { saveMessage.value = ''; }, 2000);
  }
}

// 测试输入
async function handleTestInput() {
  if (!testInput.value.trim()) {
    testCandidates.value = [];
    return;
  }

  testLoading.value = true;
  try {
    const res = await api.testConvert(testInput.value, testEngine.value, testFilterMode.value);
    if (res.success && res.data) {
      testCandidates.value = res.data.candidates || [];
    }
  } catch (e) {
    console.error('测试失败', e);
  } finally {
    testLoading.value = false;
  }
}

watch(testInput, handleTestInput);
watch([testEngine, testFilterMode], () => {
  if (testInput.value) handleTestInput();
});

// 日志相关
async function loadLogs() {
  try {
    const res = await api.getLogs(logLevel.value, logFilter.value);
    if (res.success && res.data) {
      logs.value = res.data.logs || [];
      logTotal.value = res.data.total || 0;
      if (logAutoScroll.value) {
        nextTick(() => {
          if (logContentRef.value) {
            logContentRef.value.scrollTop = logContentRef.value.scrollHeight;
          }
        });
      }
    }
  } catch (e) {
    console.error('加载日志失败', e);
  }
}

async function handleClearLogs() {
  try {
    await api.clearLogs();
    logs.value = [];
    logTotal.value = 0;
  } catch (e) {
    console.error('清空日志失败', e);
  }
}

watch([logLevel, logFilter], loadLogs);

watch(activeTab, (tab) => {
  if (tab === 'advanced') {
    loadLogs();
    if (logTimer) clearInterval(logTimer);
    logTimer = window.setInterval(loadLogs, 2000);
  } else {
    if (logTimer) {
      clearInterval(logTimer);
      logTimer = null;
    }
  }
});

onMounted(() => {
  loadData();
});

onUnmounted(() => {
  if (logTimer) clearInterval(logTimer);
});
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="logo">
        <span class="logo-icon">🌬</span>
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
        <div :class="['status-badge', connected ? 'connected' : 'disconnected']">
          <span class="status-dot"></span>
          {{ connected ? '已连接' : '未连接' }}
        </div>
      </div>
    </aside>

    <main class="main">
      <div v-if="loading" class="loading">
        <div class="spinner"></div>
        <p>加载中...</p>
      </div>

      <div v-else-if="error" class="error-panel">
        <div class="error-icon">⚠</div>
        <p>{{ error }}</p>
        <button class="btn btn-primary" @click="loadData">重试</button>
      </div>

      <div v-else class="content">
        <!-- ==================== 常用设置 ==================== -->
        <section v-if="activeTab === 'general'" class="section">
          <div class="section-header">
            <h2>常用设置</h2>
            <p class="section-desc">最基本的输入法设置</p>
          </div>

          <!-- 输入模式 -->
          <div class="settings-card">
            <div class="card-title">输入模式</div>
            <div class="engine-cards">
              <div
                v-for="engine in engines"
                :key="engine.type"
                :class="['engine-card', { active: engine.isActive }]"
                @click="handleSwitchEngine(engine.type)"
              >
                <div class="engine-icon">{{ engine.type === 'pinyin' ? '拼' : '五' }}</div>
                <div class="engine-info">
                  <div class="engine-name">{{ engine.type === 'pinyin' ? '拼音输入' : '五笔输入' }}</div>
                  <div class="engine-desc">{{ engine.description }}</div>
                </div>
                <div v-if="engine.isActive" class="engine-check">✓</div>
              </div>
              <!-- 预留：双拼 -->
              <div class="engine-card disabled">
                <div class="engine-icon engine-icon-disabled">双</div>
                <div class="engine-info">
                  <div class="engine-name">双拼输入</div>
                  <div class="engine-desc">开发中...</div>
                </div>
              </div>
              <!-- 预留：混输 -->
              <div class="engine-card disabled">
                <div class="engine-icon engine-icon-disabled">混</div>
                <div class="engine-info">
                  <div class="engine-name">混合输入</div>
                  <div class="engine-desc">开发中...</div>
                </div>
              </div>
            </div>
          </div>

          <!-- 默认/启动状态 -->
          <div class="settings-card">
            <div class="card-title">默认状态</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>记忆前次状态</label>
                <p class="setting-hint">启用后将使用上次退出时的状态，忽略以下默认设置</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.startup.remember_last_state" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item" :class="{ 'item-disabled': formData.startup.remember_last_state }">
              <div class="setting-info">
                <label>初始语言模式</label>
                <p class="setting-hint">每次激活输入法时的默认语言</p>
              </div>
              <div class="setting-control">
                <div class="segmented-control">
                  <button
                    :class="{ active: formData.startup.default_chinese_mode }"
                    @click="formData.startup.default_chinese_mode = true"
                    :disabled="formData.startup.remember_last_state"
                  >中文</button>
                  <button
                    :class="{ active: !formData.startup.default_chinese_mode }"
                    @click="formData.startup.default_chinese_mode = false"
                    :disabled="formData.startup.remember_last_state"
                  >英文</button>
                </div>
              </div>
            </div>
            <div class="setting-item" :class="{ 'item-disabled': formData.startup.remember_last_state }">
              <div class="setting-info">
                <label>初始字符宽度</label>
                <p class="setting-hint">每次激活输入法时的默认字符宽度</p>
              </div>
              <div class="setting-control">
                <div class="segmented-control">
                  <button
                    :class="{ active: !formData.startup.default_full_width }"
                    @click="formData.startup.default_full_width = false"
                    :disabled="formData.startup.remember_last_state"
                  >半角</button>
                  <button
                    :class="{ active: formData.startup.default_full_width }"
                    @click="formData.startup.default_full_width = true"
                    :disabled="formData.startup.remember_last_state"
                  >全角</button>
                </div>
              </div>
            </div>
            <div class="setting-item" :class="{ 'item-disabled': formData.startup.remember_last_state }">
              <div class="setting-info">
                <label>初始标点模式</label>
                <p class="setting-hint">每次激活输入法时的默认标点类型</p>
              </div>
              <div class="setting-control">
                <div class="segmented-control">
                  <button
                    :class="{ active: formData.startup.default_chinese_punct }"
                    @click="formData.startup.default_chinese_punct = true"
                    :disabled="formData.startup.remember_last_state"
                  >中文标点</button>
                  <button
                    :class="{ active: !formData.startup.default_chinese_punct }"
                    @click="formData.startup.default_chinese_punct = false"
                    :disabled="formData.startup.remember_last_state"
                  >英文标点</button>
                </div>
              </div>
            </div>
          </div>
        </section>

        <!-- ==================== 输入习惯 ==================== -->
        <section v-if="activeTab === 'input'" class="section">
          <div class="section-header">
            <h2>输入习惯</h2>
            <p class="section-desc">定制您的打字体验</p>
          </div>

          <!-- 字符与标点 -->
          <div class="settings-card">
            <div class="card-title">字符与标点</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>候选字符范围</label>
                <p class="setting-hint">控制候选词中显示的字符范围</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.engine.filter_mode" class="select">
                  <option value="smart">智能模式（推荐）</option>
                  <option value="general">仅常用字</option>
                  <option value="gb18030">大字符集</option>
                </select>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>标点随中英文切换</label>
                <p class="setting-hint">切换到英文模式时自动切换英文标点</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.input.punct_follow_mode" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>

          <!-- 五笔设置（始终显示） -->
          <div class="settings-card">
            <div class="card-title">五笔设置</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>自动上屏</label>
                <p class="setting-hint">满足条件时自动提交首选</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.engine.wubi.auto_commit" class="select">
                  <option value="none">不自动上屏</option>
                  <option value="unique">候选唯一时</option>
                  <option value="unique_at_4">四码唯一时</option>
                  <option value="unique_full_match">完整匹配唯一时</option>
                </select>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>空码处理</label>
                <p class="setting-hint">输入无匹配时的处理方式</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.engine.wubi.empty_code" class="select">
                  <option value="none">继续输入</option>
                  <option value="clear">清空编码</option>
                  <option value="clear_at_4">四码时清空</option>
                  <option value="to_english">转为英文</option>
                </select>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>五码顶字</label>
                <p class="setting-hint">输入第五码时自动上屏首选</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.engine.wubi.top_code_commit" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>标点顶字</label>
                <p class="setting-hint">输入标点时自动上屏首选</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.engine.wubi.punct_commit" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>

          <!-- 拼音设置（始终显示） -->
          <div class="settings-card">
            <div class="card-title">拼音设置</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>五笔反查提示</label>
                <p class="setting-hint">在候选词旁显示对应的五笔编码</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.engine.pinyin.show_wubi_hint" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>
        </section>

        <!-- ==================== 外观设置 ==================== -->
        <section v-if="activeTab === 'appearance'" class="section">
          <div class="section-header">
            <h2>外观设置</h2>
            <p class="section-desc">定制候选框的视觉呈现</p>
          </div>

          <div class="settings-card">
            <div class="card-title">候选窗口</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>字体大小</label>
                <p class="setting-hint">候选词的显示大小</p>
              </div>
              <div class="setting-control range-control">
                <input type="range" min="12" max="36" step="1" v-model.number="formData.ui.font_size" />
                <span class="range-value">{{ formData.ui.font_size }}px</span>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>每页候选数</label>
                <p class="setting-hint">每页显示的候选词数量</p>
              </div>
              <div class="setting-control range-control">
                <input type="range" min="3" max="9" step="1" v-model.number="formData.ui.candidates_per_page" />
                <span class="range-value">{{ formData.ui.candidates_per_page }} 个</span>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>自定义字体</label>
                <p class="setting-hint">留空使用系统默认字体</p>
              </div>
              <div class="setting-control">
                <input type="text" v-model="formData.ui.font_path" class="input" placeholder="字体文件路径" />
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">编码显示</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>嵌入式编码行</label>
                <p class="setting-hint">输入码直接显示在光标处，而非候选窗上方</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.ui.inline_preedit" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">状态栏</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>显示工具栏</label>
                <p class="setting-hint">在屏幕上显示可拖动的输入法状态栏</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.toolbar.visible" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>
        </section>

        <!-- ==================== 词库管理 ==================== -->
        <section v-if="activeTab === 'dictionary'" class="section">
          <div class="section-header">
            <h2>词库管理</h2>
            <p class="section-desc">管理您的词库数据</p>
          </div>

          <div class="settings-card">
            <div class="card-title">系统词库</div>
            <div class="dict-info">
              <div class="dict-item">
                <span class="dict-label">拼音词库</span>
                <span class="dict-value">{{ config?.dictionary?.pinyin_dict || '未配置' }}</span>
              </div>
              <div class="dict-item">
                <span class="dict-label">五笔词库</span>
                <span class="dict-value">dict/wubi/wubi86.txt</span>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">用户词库</div>
            <div class="dict-info">
              <div class="dict-item">
                <span class="dict-label">词库路径</span>
                <span class="dict-value">{{ config?.dictionary?.user_dict || 'user_dict.txt' }}</span>
              </div>
            </div>
            <div class="dict-actions">
              <button class="btn btn-sm" disabled>导出词库</button>
              <button class="btn btn-sm" disabled>导入词库</button>
              <button class="btn btn-sm btn-danger" disabled>清空词库</button>
            </div>
            <p class="dict-note">词库管理功能开发中...</p>
          </div>
        </section>

        <!-- ==================== 按键设置 ==================== -->
        <section v-if="activeTab === 'hotkey'" class="section">
          <div class="section-header">
            <h2>按键设置</h2>
            <p class="section-desc">自定义快捷键和候选操作</p>
          </div>

          <!-- 冲突警告 -->
          <div v-if="hotkeyConflicts.length > 0" class="settings-card warning-card">
            <div class="warning-content">
              <span class="warning-icon">⚠</span>
              <div>
                <p class="warning-title">快捷键冲突</p>
                <ul class="warning-list">
                  <li v-for="(c, i) in hotkeyConflicts" :key="i">{{ c }}</li>
                </ul>
              </div>
            </div>
          </div>

          <!-- 中英文切换 -->
          <div class="settings-card">
            <div class="card-title">中英文切换</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>切换按键</label>
                <p class="setting-hint">可多选，按下任意一个即切换</p>
              </div>
              <div class="setting-control">
                <div class="checkbox-group two-columns">
                  <label class="checkbox-item" v-for="key in ['lshift', 'rshift', 'lctrl', 'rctrl', 'capslock']" :key="key">
                    <input
                      type="checkbox"
                      :checked="formData.hotkeys.toggle_mode_keys.includes(key)"
                      @change="toggleArrayValue(formData.hotkeys.toggle_mode_keys, key)"
                    />
                    <span>{{ getKeyLabel(key) }}</span>
                  </label>
                </div>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>切换时编码上屏</label>
                <p class="setting-hint">中文切换为英文时，将已输入的编码直接上屏</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.hotkeys.commit_on_switch" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>

          <!-- 功能快捷键 -->
          <div class="settings-card">
            <div class="card-title">功能快捷键</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>切换拼音/五笔</label>
                <p class="setting-hint">在拼音和五笔引擎间切换</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.hotkeys.switch_engine" class="select">
                  <option value="ctrl+`">Ctrl + `</option>
                  <option value="ctrl+shift+e">Ctrl + Shift + E</option>
                  <option value="none">不使用</option>
                </select>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>切换全角/半角</label>
                <p class="setting-hint">切换字符宽度模式</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.hotkeys.toggle_full_width" class="select">
                  <option value="shift+space">Shift + Space</option>
                  <option value="ctrl+shift+space">Ctrl + Shift + Space</option>
                  <option value="none">不使用</option>
                </select>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>切换中/英文标点</label>
                <p class="setting-hint">切换标点符号类型</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.hotkeys.toggle_punct" class="select">
                  <option value="ctrl+.">Ctrl + .</option>
                  <option value="ctrl+,">Ctrl + ,</option>
                  <option value="none">不使用</option>
                </select>
              </div>
            </div>
          </div>

          <!-- 候选操作 -->
          <div class="settings-card">
            <div class="card-title">候选选择键</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>2/3候选快捷键组</label>
                <p class="setting-hint">可多选，同时启用多组快捷键</p>
              </div>
              <div class="setting-control">
                <div class="checkbox-group two-columns">
                  <label class="checkbox-item" v-for="group in [
                    { value: 'semicolon_quote', label: '; \' 键' },
                    { value: 'comma_period', label: ', . 键' },
                    { value: 'lrshift', label: 'L/R Shift' },
                    { value: 'lrctrl', label: 'L/R Ctrl' },
                  ]" :key="group.value">
                    <input
                      type="checkbox"
                      :checked="formData.input.select_key_groups.includes(group.value)"
                      @change="toggleArrayValue(formData.input.select_key_groups, group.value)"
                    />
                    <span>{{ group.label }}</span>
                  </label>
                </div>
              </div>
            </div>
          </div>

          <!-- 翻页键 -->
          <div class="settings-card">
            <div class="card-title">翻页键</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>翻页快捷键</label>
                <p class="setting-hint">可多选，同时启用多组翻页键</p>
              </div>
              <div class="setting-control">
                <div class="checkbox-group two-columns">
                  <label class="checkbox-item" v-for="pk in [
                    { value: 'pageupdown', label: 'PgUp/PgDn' },
                    { value: 'minus_equal', label: '- / =' },
                    { value: 'brackets', label: '[ / ]' },
                    { value: 'shift_tab', label: 'Shift+Tab / Tab' },
                  ]" :key="pk.value">
                    <input
                      type="checkbox"
                      :checked="formData.input.page_keys.includes(pk.value)"
                      @change="toggleArrayValue(formData.input.page_keys, pk.value)"
                    />
                    <span>{{ pk.label }}</span>
                  </label>
                </div>
              </div>
            </div>
          </div>
        </section>

        <!-- ==================== 测试页面 ==================== -->
        <section v-if="activeTab === 'test'" class="section">
          <div class="section-header">
            <h2>码表测试</h2>
            <p class="section-desc">在此测试输入法候选词，无需切换系统输入法</p>
          </div>

          <div class="settings-card">
            <div class="test-options">
              <div class="test-option">
                <label>引擎</label>
                <select v-model="testEngine" class="select select-sm">
                  <option value="current">当前引擎</option>
                  <option value="pinyin">拼音</option>
                  <option value="wubi">五笔</option>
                </select>
              </div>
              <div class="test-option">
                <label>过滤</label>
                <select v-model="testFilterMode" class="select select-sm">
                  <option value="current">当前设置</option>
                  <option value="smart">智能模式</option>
                  <option value="general">仅通用字</option>
                  <option value="gb18030">全部字符</option>
                </select>
              </div>
            </div>

            <div class="test-input-wrap">
              <input
                type="text"
                v-model="testInput"
                class="test-input"
                placeholder="输入编码进行测试..."
                @keydown.enter.prevent
              />
              <span v-if="testLoading" class="test-loading">加载中...</span>
            </div>

            <div class="test-candidates-wrap" v-if="testCandidates.length > 0">
              <div class="test-candidates">
                <div class="test-candidate" v-for="(cand, idx) in testCandidates" :key="idx">
                  <span class="cand-index">{{ idx + 1 }}.</span>
                  <span class="cand-text">{{ cand.text }}</span>
                  <span class="cand-code" v-if="cand.code">{{ cand.code }}</span>
                  <span class="cand-common" v-if="cand.isCommon">通用</span>
                  <span class="cand-rare" v-else>生僻</span>
                </div>
              </div>
            </div>
            <div class="test-empty" v-else-if="testInput && !testLoading">
              无匹配候选词
            </div>
          </div>
        </section>

        <!-- ==================== 高级设置 ==================== -->
        <section v-if="activeTab === 'advanced'" class="section section-full">
          <div class="section-header">
            <h2>高级设置</h2>
            <p class="section-desc">故障排查与调试选项</p>
          </div>

          <div class="settings-card">
            <div class="card-title">日志设置</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>日志级别</label>
                <p class="setting-hint">更改日志级别需要重启服务才能生效</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.advanced.log_level" class="select">
                  <option value="debug">Debug（调试）</option>
                  <option value="info">Info（信息）</option>
                  <option value="warn">Warn（警告）</option>
                  <option value="error">Error（错误）</option>
                </select>
              </div>
            </div>
          </div>

          <div class="settings-card log-card">
            <div class="card-title">调试日志 <span class="log-count">共 {{ logTotal }} 条</span></div>
            <div class="log-toolbar">
              <select v-model="logLevel" class="select select-sm">
                <option value="all">全部级别</option>
                <option value="DEBUG">DEBUG</option>
                <option value="INFO">INFO</option>
                <option value="WARN">WARN</option>
                <option value="ERROR">ERROR</option>
              </select>
              <input type="text" v-model="logFilter" class="input input-sm" placeholder="搜索日志..." />
              <label class="checkbox-inline">
                <input type="checkbox" v-model="logAutoScroll" />
                自动滚动
              </label>
              <button class="btn btn-sm" @click="loadLogs">刷新</button>
              <button class="btn btn-sm" @click="handleClearLogs">清空</button>
            </div>
            <div class="log-content" ref="logContentRef">
              <div v-for="(log, idx) in logs" :key="idx" :class="['log-line', 'log-' + log.level.toLowerCase()]">
                <span class="log-time">{{ log.time }}</span>
                <span class="log-level">{{ log.level }}</span>
                <span class="log-msg">{{ log.message }}</span>
              </div>
              <div v-if="logs.length === 0" class="log-empty">暂无日志</div>
            </div>
          </div>
        </section>

        <!-- ==================== 关于 ==================== -->
        <section v-if="activeTab === 'about'" class="section">
          <div class="section-header">
            <h2>关于</h2>
            <p class="section-desc">WindInput 输入法信息</p>
          </div>

          <div class="settings-card about-card" v-if="status">
            <div class="about-header">
              <div class="about-icon-wrap">🌬</div>
              <div class="about-title">
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
            <button class="btn" @click="handleReload">重载配置</button>
            <button class="btn" @click="refreshStatus">刷新状态</button>
          </div>

          <div class="settings-card about-links">
            <div class="about-link-item">
              <span class="about-link-icon">📋</span>
              <div class="about-link-info">
                <span class="about-link-title">项目主页</span>
                <span class="about-link-desc">查看源代码和文档</span>
              </div>
            </div>
            <div class="about-link-item">
              <span class="about-link-icon">🐛</span>
              <div class="about-link-info">
                <span class="about-link-title">反馈问题</span>
                <span class="about-link-desc">报告 Bug 或提出建议</span>
              </div>
            </div>
          </div>
        </section>

        <!-- 底部操作栏 -->
        <div class="action-bar" v-if="showActionBar">
          <div class="action-message">
            <span v-if="saveMessage" :class="['message', saveMessageType]">
              {{ saveMessage }}
            </span>
          </div>
          <div class="action-buttons">
            <button class="btn" @click="resetToDefault">重置</button>
            <button class="btn btn-primary" @click="saveConfig" :disabled="saving || hotkeyConflicts.length > 0">
              {{ saving ? '保存中...' : '保存设置' }}
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
  background: #f0f2f5;
  color: #1f2937;
  line-height: 1.5;
  font-size: 14px;
}

.app { display: flex; min-height: 100vh; height: 100vh; }

/* Sidebar */
.sidebar {
  width: 200px;
  min-width: 200px;
  background: #fff;
  border-right: 1px solid #e5e7eb;
  display: flex;
  flex-direction: column;
}

.logo {
  padding: 20px;
  display: flex;
  align-items: center;
  gap: 10px;
  border-bottom: 1px solid #e5e7eb;
}
.logo-icon { font-size: 24px; }
.logo-text { font-size: 16px; font-weight: 600; color: #1f2937; }

.nav { flex: 1; padding: 12px; overflow-y: auto; }
.nav-item {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border: none;
  background: none;
  cursor: pointer;
  border-radius: 8px;
  font-size: 14px;
  color: #6b7280;
  transition: all 0.15s;
  text-align: left;
}
.nav-item:hover { background: #f3f4f6; color: #374151; }
.nav-item.active { background: #eff6ff; color: #2563eb; font-weight: 500; }
.nav-icon { font-size: 16px; width: 20px; text-align: center; }

.sidebar-footer { padding: 16px; border-top: 1px solid #e5e7eb; }
.status-badge {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  padding: 6px 12px;
  border-radius: 20px;
}
.status-badge.connected { background: #dcfce7; color: #166534; }
.status-badge.disconnected { background: #fee2e2; color: #991b1b; }
.status-dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }

/* Main */
.main { flex: 1; display: flex; flex-direction: column; overflow: hidden; min-width: 0; }
.content { flex: 1; padding: 24px 32px; overflow-y: auto; padding-bottom: 80px; }

/* Section */
.section { max-width: 700px; width: 100%; }
.section-full { max-width: none; }
.section-header { margin-bottom: 20px; }
.section-header h2 { font-size: 20px; font-weight: 600; color: #111827; margin-bottom: 4px; text-align: left; }
.section-desc { font-size: 13px; color: #6b7280; text-align: left; }

/* Settings Card */
.settings-card {
  background: #fff;
  border-radius: 12px;
  padding: 20px;
  margin-bottom: 16px;
  box-shadow: 0 1px 3px rgba(0,0,0,0.05);
}
.card-title { font-size: 13px; font-weight: 600; color: #374151; margin-bottom: 16px; text-transform: uppercase; letter-spacing: 0.5px; text-align: left; }

/* Setting Item */
.setting-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 14px 0;
  border-bottom: 1px solid #f3f4f6;
  gap: 16px;
}
.setting-item:last-child { border-bottom: none; padding-bottom: 0; }
.setting-item:first-child { padding-top: 0; }
.setting-item.item-disabled { opacity: 0.5; }
.setting-info { flex: 1; min-width: 0; }
.setting-info label { font-size: 14px; font-weight: 500; color: #1f2937; display: block; text-align: left; }
.setting-hint { font-size: 12px; color: #9ca3af; margin-top: 2px; text-align: left; }
.setting-control { flex-shrink: 0; display: flex; align-items: center; gap: 10px; }

/* Form Controls */
.select {
  padding: 8px 32px 8px 12px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  font-size: 14px;
  background: #fff url("data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e") no-repeat right 8px center;
  background-size: 16px;
  cursor: pointer;
  color: #1f2937;
  min-width: 160px;
  -webkit-appearance: none;
  appearance: none;
}
.select:focus { outline: none; border-color: #2563eb; box-shadow: 0 0 0 3px rgba(37,99,235,0.1); }
.select-sm { padding: 6px 28px 6px 10px; font-size: 13px; min-width: 100px; }

.input {
  padding: 8px 12px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  font-size: 14px;
  width: 240px;
  color: #1f2937;
}
.input:focus { outline: none; border-color: #2563eb; box-shadow: 0 0 0 3px rgba(37,99,235,0.1); }
.input::placeholder { color: #9ca3af; }
.input-sm { padding: 6px 10px; font-size: 13px; width: 160px; }

/* Segmented Control */
.segmented-control {
  display: inline-flex;
  background: #f3f4f6;
  border-radius: 6px;
  padding: 2px;
}
.segmented-control button {
  padding: 6px 14px;
  border: none;
  background: transparent;
  font-size: 13px;
  color: #6b7280;
  cursor: pointer;
  border-radius: 4px;
  transition: all 0.15s;
}
.segmented-control button:hover:not(:disabled) { color: #374151; }
.segmented-control button:disabled { cursor: not-allowed; }
.segmented-control button.active {
  background: #fff;
  color: #2563eb;
  font-weight: 500;
  box-shadow: 0 1px 2px rgba(0,0,0,0.05);
}

/* Switch */
.switch {
  position: relative;
  display: inline-block;
  width: 44px;
  height: 24px;
}
.switch input { opacity: 0; width: 0; height: 0; }
.slider {
  position: absolute;
  cursor: pointer;
  top: 0; left: 0; right: 0; bottom: 0;
  background-color: #d1d5db;
  transition: .2s;
  border-radius: 24px;
}
.slider:before {
  position: absolute;
  content: "";
  height: 18px;
  width: 18px;
  left: 3px;
  bottom: 3px;
  background-color: white;
  transition: .2s;
  border-radius: 50%;
}
input:checked + .slider { background-color: #2563eb; }
input:checked + .slider:before { transform: translateX(20px); }

/* Checkbox Group */
.checkbox-group {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(100px, 1fr));
  gap: 10px 16px;
  width: 100%;
  max-width: 400px;
}
.checkbox-group.two-columns {
  grid-template-columns: repeat(2, 1fr);
}
.checkbox-group.vertical {
  grid-template-columns: 1fr;
  gap: 10px;
}
.checkbox-item {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  font-size: 13px;
  color: #374151;
  padding: 4px 8px;
  border-radius: 6px;
  transition: background-color 0.15s;
  white-space: nowrap;
}
.checkbox-item:hover {
  background-color: #f3f4f6;
}
.checkbox-item input {
  width: 16px;
  height: 16px;
  cursor: pointer;
  accent-color: #2563eb;
  flex-shrink: 0;
}
.checkbox-item span {
  overflow: hidden;
  text-overflow: ellipsis;
}

/* Range */
.range-control { flex-direction: column; align-items: flex-end; gap: 4px; }
.range-control input[type="range"] {
  width: 160px;
  height: 4px;
  -webkit-appearance: none;
  background: #e5e7eb;
  border-radius: 2px;
}
.range-control input[type="range"]::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 16px;
  height: 16px;
  background: #2563eb;
  border-radius: 50%;
  cursor: pointer;
}
.range-value { font-size: 13px; font-weight: 500; color: #374151; }

/* Engine Cards */
.engine-cards { display: flex; gap: 12px; flex-wrap: wrap; }
.engine-card {
  flex: 1;
  min-width: 140px;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px;
  border: 2px solid #e5e7eb;
  border-radius: 10px;
  cursor: pointer;
  transition: all 0.15s;
}
.engine-card:hover:not(.disabled) { border-color: #93c5fd; }
.engine-card.active { border-color: #2563eb; background: #eff6ff; }
.engine-card.disabled { cursor: not-allowed; opacity: 0.6; }
.engine-icon {
  width: 40px;
  height: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #2563eb;
  color: #fff;
  border-radius: 8px;
  font-size: 18px;
  font-weight: 600;
  flex-shrink: 0;
}
.engine-icon-disabled { background: #9ca3af; }
.engine-info { flex: 1; min-width: 0; text-align: left; }
.engine-name { font-weight: 500; color: #1f2937; }
.engine-desc { font-size: 12px; color: #6b7280; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.engine-check { color: #2563eb; font-size: 18px; flex-shrink: 0; }

/* Warning Card */
.warning-card { background: #fef3c7; border: 1px solid #f59e0b; }
.warning-content { display: flex; align-items: flex-start; gap: 12px; }
.warning-icon { font-size: 20px; flex-shrink: 0; }
.warning-title { font-weight: 500; color: #92400e; margin-bottom: 4px; }
.warning-list { font-size: 13px; color: #b45309; margin: 0; padding-left: 16px; }

/* Dictionary */
.dict-info { margin-bottom: 16px; }
.dict-item {
  display: flex;
  justify-content: space-between;
  padding: 10px 0;
  border-bottom: 1px solid #f3f4f6;
}
.dict-item:last-child { border-bottom: none; }
.dict-label { color: #6b7280; }
.dict-value { font-family: monospace; color: #374151; font-size: 13px; }
.dict-actions { display: flex; gap: 8px; margin-bottom: 12px; }
.dict-note { font-size: 12px; color: #9ca3af; font-style: italic; }
.btn-danger { color: #dc2626; border-color: #fecaca; }
.btn-danger:hover { background: #fee2e2; }

/* Action Bar */
.action-bar {
  position: fixed;
  bottom: 0;
  left: 200px;
  right: 0;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px 32px;
  background: #fff;
  border-top: 1px solid #e5e7eb;
  z-index: 100;
}
.action-message { flex: 1; }
.message { font-size: 13px; padding: 6px 12px; border-radius: 6px; }
.message.success { background: #dcfce7; color: #166534; }
.message.error { background: #fee2e2; color: #991b1b; }
.action-buttons { display: flex; gap: 10px; }

.btn {
  padding: 8px 16px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.15s;
  background: #fff;
  color: #374151;
}
.btn:hover:not(:disabled) { background: #f9fafb; border-color: #9ca3af; }
.btn:disabled { opacity: 0.5; cursor: not-allowed; }
.btn-primary { background: #2563eb; color: #fff; border-color: #2563eb; }
.btn-primary:hover:not(:disabled) { background: #1d4ed8; }
.btn-sm { padding: 5px 10px; font-size: 12px; }

/* Test Page */
.test-options { display: flex; gap: 16px; margin-bottom: 16px; flex-wrap: wrap; }
.test-option { display: flex; align-items: center; gap: 8px; }
.test-option label { font-size: 13px; color: #6b7280; }

.test-input-wrap { position: relative; margin-bottom: 16px; }
.test-input {
  width: 100%;
  padding: 12px 16px;
  border: 2px solid #e5e7eb;
  border-radius: 8px;
  font-size: 16px;
  color: #1f2937;
}
.test-input:focus { outline: none; border-color: #2563eb; }
.test-loading { position: absolute; right: 16px; top: 50%; transform: translateY(-50%); font-size: 12px; color: #9ca3af; }

.test-candidates-wrap { max-height: 400px; overflow-y: auto; }
.test-candidates { border: 1px solid #e5e7eb; border-radius: 8px; overflow: hidden; }
.test-candidate {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  border-bottom: 1px solid #f3f4f6;
}
.test-candidate:last-child { border-bottom: none; }
.test-candidate:hover { background: #f9fafb; }
.cand-index { color: #9ca3af; font-size: 13px; min-width: 24px; flex-shrink: 0; }
.cand-text { font-size: 16px; color: #1f2937; word-break: break-all; }
.cand-code { font-size: 12px; color: #6b7280; background: #f3f4f6; padding: 2px 6px; border-radius: 4px; flex-shrink: 0; }
.cand-common { font-size: 11px; color: #059669; background: #d1fae5; padding: 2px 6px; border-radius: 4px; flex-shrink: 0; }
.cand-rare { font-size: 11px; color: #dc2626; background: #fee2e2; padding: 2px 6px; border-radius: 4px; flex-shrink: 0; }
.test-empty { text-align: center; padding: 32px; color: #9ca3af; }

/* Log Page */
.log-card { display: flex; flex-direction: column; min-height: 300px; }
.log-count { font-weight: normal; font-size: 12px; color: #9ca3af; margin-left: 8px; text-transform: none; }
.log-toolbar { display: flex; align-items: center; gap: 12px; padding-bottom: 12px; border-bottom: 1px solid #e5e7eb; flex-wrap: wrap; }
.checkbox-inline { display: flex; align-items: center; gap: 6px; font-size: 13px; color: #6b7280; cursor: pointer; white-space: nowrap; }
.checkbox-inline input { cursor: pointer; }
.log-content { flex: 1; overflow-y: auto; font-family: 'Consolas', 'Monaco', 'Courier New', monospace; font-size: 12px; padding: 12px 0; max-height: 400px; }
.log-line { display: flex; gap: 12px; padding: 4px 0; text-align: left; align-items: flex-start; }
.log-time { color: #9ca3af; flex-shrink: 0; }
.log-level { font-weight: 600; min-width: 50px; flex-shrink: 0; }
.log-msg { color: #374151; word-break: break-all; flex: 1; }
.log-debug .log-level { color: #6b7280; }
.log-info .log-level { color: #2563eb; }
.log-warn .log-level { color: #d97706; }
.log-error .log-level { color: #dc2626; }
.log-empty { color: #9ca3af; text-align: center; padding: 32px; }

/* About Page */
.about-card { padding: 24px; }
.about-header { display: flex; align-items: center; gap: 16px; margin-bottom: 24px; }
.about-icon-wrap { font-size: 48px; width: 64px; height: 64px; display: flex; align-items: center; justify-content: center; background: #eff6ff; border-radius: 12px; flex-shrink: 0; }
.about-title { text-align: left; }
.about-title h3 { font-size: 18px; font-weight: 600; margin: 0; color: #1f2937; }
.about-title p { color: #6b7280; margin: 4px 0 0; }
.about-stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 12px; }
.stat-item { padding: 12px 16px; background: #f9fafb; border-radius: 8px; text-align: left; }
.stat-label { display: block; font-size: 12px; color: #9ca3af; margin-bottom: 4px; }
.stat-value { font-size: 14px; font-weight: 500; color: #1f2937; }
.about-actions { display: flex; gap: 12px; margin-bottom: 16px; }
.about-links { padding: 0; }
.about-link-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 20px;
  border-bottom: 1px solid #f3f4f6;
  cursor: pointer;
  transition: background 0.15s;
}
.about-link-item:last-child { border-bottom: none; }
.about-link-item:hover { background: #f9fafb; }
.about-link-icon { font-size: 20px; }
.about-link-info { flex: 1; text-align: left; }
.about-link-title { display: block; font-weight: 500; color: #1f2937; }
.about-link-desc { font-size: 12px; color: #9ca3af; }

/* Loading & Error */
.loading {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  color: #6b7280;
}
.spinner {
  width: 32px;
  height: 32px;
  border: 3px solid #e5e7eb;
  border-top-color: #2563eb;
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin { to { transform: rotate(360deg); } }

.error-panel {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  text-align: center;
  padding: 32px;
}
.error-icon { font-size: 48px; }
.error-panel p { color: #6b7280; max-width: 300px; }

/* Responsive */
@media (max-width: 768px) {
  .sidebar { width: 60px; min-width: 60px; }
  .logo-text { display: none; }
  .nav-label { display: none; }
  .nav-item { justify-content: center; padding: 12px; }
  .action-bar { left: 60px; }
  .content { padding: 16px; }
}
</style>
