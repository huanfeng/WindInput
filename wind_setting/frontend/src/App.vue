<script setup lang="ts">
import { ref, onMounted, computed } from "vue";
import * as api from "./api/settings";
import * as wailsApi from "./api/wails";
import type { Config, Status, EngineInfo } from "./api/settings";
import type { ThemeInfo, ThemePreview } from "./api/wails";
import { getDefaultConfig } from "./api/settings";

import GeneralPage from "./pages/GeneralPage.vue";
import InputPage from "./pages/InputPage.vue";
import HotkeyPage from "./pages/HotkeyPage.vue";
import AppearancePage from "./pages/AppearancePage.vue";
import DictionaryPage from "./pages/DictionaryPage.vue";
import AdvancedPage from "./pages/AdvancedPage.vue";
import AboutPage from "./pages/AboutPage.vue";

// 检测是否在 Wails 环境中
const isWailsEnv = computed(() => {
  return (
    typeof window !== "undefined" && (window as any).go?.main?.App !== undefined
  );
});

// 状态
const loading = ref(true);
const error = ref("");
const connected = ref(false);
const activeTab = ref("general");
const saving = ref(false);
const saveMessage = ref("");
const saveMessageType = ref<"success" | "error">("success");
const hotkeyConflicts = ref<string[]>([]);

// 数据
const config = ref<Config | null>(null);
const status = ref<Status | null>(null);
const engines = ref<EngineInfo[]>([]);

// 表单数据（用于编辑）
const formData = ref<Config>(getDefaultConfig());

// 主题相关状态
const availableThemes = ref<ThemeInfo[]>([]);
const themePreview = ref<ThemePreview | null>(null);

const repoUrl = "https://github.com/huanfeng/WindInput";
const appIconUrl = new URL(
  "./assets/images/logo-universal.png",
  import.meta.url,
).href;

// 标签页定义
const tabs = [
  { id: "general", label: "常用", icon: "🏠" },
  { id: "input", label: "输入", icon: "⌨" },
  { id: "hotkey", label: "按键", icon: "🎮" },
  { id: "appearance", label: "外观", icon: "🎨" },
  { id: "dictionary", label: "词库", icon: "📚" },
  { id: "advanced", label: "高级", icon: "🛠" },
  { id: "about", label: "关于", icon: "ℹ" },
];

// 加载数据
async function loadData() {
  loading.value = true;
  error.value = "";

  try {
    if (isWailsEnv.value) {
      await loadDataFromWails();
    } else {
      await loadDataFromHTTP();
    }
  } catch (e) {
    console.error("加载数据失败", e);
    error.value =
      "加载数据失败: " + (e instanceof Error ? e.message : String(e));
  } finally {
    loading.value = false;
  }
}

async function loadDataFromWails() {
  connected.value = true;

  try {
    const cfg = await wailsApi.getConfig();
    if (cfg) {
      const mergedCfg = mergeWithDefaults(cfg);
      config.value = mergedCfg;
      formData.value = JSON.parse(JSON.stringify(mergedCfg));
    }

    engines.value = [
      {
        type: "wubi",
        displayName: "五笔输入",
        description: "86版五笔",
        isActive: config.value?.engine?.type === "wubi",
      },
      {
        type: "pinyin",
        displayName: "拼音输入",
        description: "全拼输入法",
        isActive: config.value?.engine?.type === "pinyin",
      },
    ];

    await loadThemes();
  } catch (e) {
    console.error("Wails API 调用失败", e);
    throw e;
  }
}

async function loadDataFromHTTP() {
  const healthRes = await api.checkHealth();
  if (!healthRes.success) {
    connected.value = false;
    error.value = "请使用 wails dev 命令启动开发服务器，或运行编译后的应用";
    return;
  }
  connected.value = true;

  const configRes = await api.getConfig();
  if (configRes.success && configRes.data) {
    const cfg = mergeWithDefaults(configRes.data);
    config.value = cfg;
    formData.value = JSON.parse(JSON.stringify(cfg));
  }

  const statusRes = await api.getStatus();
  if (statusRes.success && statusRes.data) {
    status.value = statusRes.data;
  }

  const enginesRes = await api.getEngineList();
  if (enginesRes.success && enginesRes.data) {
    engines.value = enginesRes.data.engines;
  }
}

function mergeWithDefaults(cfg: any): Config {
  const defaults = getDefaultConfig();
  return {
    startup: { ...defaults.startup, ...cfg.startup },
    dictionary: { ...defaults.dictionary, ...cfg.dictionary },
    engine: {
      ...defaults.engine,
      ...cfg.engine,
      pinyin: {
        ...defaults.engine.pinyin,
        ...cfg.engine?.pinyin,
        fuzzy: {
          ...defaults.engine.pinyin.fuzzy,
          ...cfg.engine?.pinyin?.fuzzy,
        },
      },
      wubi: { ...defaults.engine.wubi, ...cfg.engine?.wubi },
    },
    hotkeys: { ...defaults.hotkeys, ...cfg.hotkeys },
    ui: { ...defaults.ui, ...cfg.ui },
    toolbar: { ...defaults.toolbar, ...cfg.toolbar },
    input: {
      ...defaults.input,
      ...cfg.input,
      temp_pinyin: {
        ...defaults.input.temp_pinyin,
        ...cfg.input?.temp_pinyin,
      },
    },
    advanced: { ...defaults.advanced, ...cfg.advanced },
  };
}

// 保存配置
async function saveConfig() {
  if (hotkeyConflicts.value.length > 0) {
    saveMessageType.value = "error";
    saveMessage.value = "存在快捷键冲突，请先解决";
    setTimeout(() => {
      saveMessage.value = "";
    }, 3000);
    return;
  }

  saving.value = true;
  saveMessage.value = "";

  try {
    if (isWailsEnv.value) {
      await wailsApi.saveConfig(formData.value as any);
      saveMessageType.value = "success";
      saveMessage.value = "保存成功";
      config.value = JSON.parse(JSON.stringify(formData.value));
    } else {
      const res = await api.updateConfig(formData.value);
      if (res.success && res.data) {
        saveMessageType.value = "success";
        saveMessage.value = "保存成功";
        if (res.data.needReload.length > 0) {
          saveMessage.value += "（部分设置需要重载生效）";
        }
        config.value = JSON.parse(JSON.stringify(formData.value));
      } else {
        saveMessageType.value = "error";
        saveMessage.value = res.error || "保存失败";
      }
    }
  } catch (e: any) {
    saveMessageType.value = "error";
    saveMessage.value = e.message || "保存失败";
  } finally {
    saving.value = false;
    setTimeout(() => {
      saveMessage.value = "";
    }, 3000);
  }
}

// 重载配置
async function handleReload() {
  try {
    if (isWailsEnv.value) {
      await wailsApi.reloadConfig();
      saveMessageType.value = "success";
      saveMessage.value = "重载成功";
      await loadData();
    } else {
      const res = await api.reloadConfig();
      if (res.success) {
        saveMessageType.value = "success";
        saveMessage.value = "重载成功";
        await loadData();
      } else {
        saveMessageType.value = "error";
        saveMessage.value = res.error || "重载失败";
      }
    }
  } catch (e: any) {
    saveMessageType.value = "error";
    saveMessage.value = "重载失败";
  }
  setTimeout(() => {
    saveMessage.value = "";
  }, 3000);
}

// 刷新状态
async function refreshStatus() {
  try {
    if (isWailsEnv.value) {
      const serviceStatus = await wailsApi.getServiceStatus();
      if (serviceStatus) {
        status.value = {
          service: {
            name: "清风输入法",
            version: "1.0.0",
            uptime: "",
            uptimeSec: 0,
          },
          engine: {
            type: serviceStatus.engine_type || "",
            displayName:
              serviceStatus.engine_type === "pinyin" ? "拼音" : "五笔",
            info: serviceStatus.engine_type || "",
          },
          memory: {
            alloc: 0,
            sys: 0,
            allocMB: "",
            sysMB: "",
          },
        };
      }
    } else {
      const statusRes = await api.getStatus();
      if (statusRes.success && statusRes.data) {
        status.value = statusRes.data;
      }
    }
  } catch (e) {
    console.error("刷新状态失败", e);
  }
}

// 重置为当前页面默认
async function resetCurrentPageDefaults() {
  const defaults = getDefaultConfig();
  let changed = true;

  switch (activeTab.value) {
    case "general":
      formData.value.startup = { ...defaults.startup };
      formData.value.engine.type = defaults.engine.type;
      break;
    case "input":
      formData.value.engine = {
        ...formData.value.engine,
        filter_mode: defaults.engine.filter_mode,
        pinyin: { ...defaults.engine.pinyin },
        wubi: { ...defaults.engine.wubi },
      };
      formData.value.input = {
        ...formData.value.input,
        punct_follow_mode: defaults.input.punct_follow_mode,
      };
      break;
    case "hotkey":
      formData.value.hotkeys = { ...defaults.hotkeys };
      formData.value.input = {
        ...formData.value.input,
        select_key_groups: [...defaults.input.select_key_groups],
        page_keys: [...defaults.input.page_keys],
      };
      break;
    case "appearance":
      formData.value.ui = { ...defaults.ui };
      formData.value.toolbar = { ...defaults.toolbar };
      if (isWailsEnv.value) {
        await loadThemePreview(formData.value.ui.theme);
      }
      break;
    case "advanced":
      formData.value.advanced = { ...defaults.advanced };
      break;
    default:
      changed = false;
      break;
  }

  saveMessageType.value = changed ? "success" : "error";
  saveMessage.value = changed ? "已恢复本页默认设置" : "本页没有可恢复的设置";
  setTimeout(() => {
    saveMessage.value = "";
  }, 2000);
}

// 主题管理
async function loadThemes() {
  if (!isWailsEnv.value) return;
  try {
    const themes = await wailsApi.getAvailableThemes();
    availableThemes.value = themes;
    if (formData.value.ui.theme) {
      await loadThemePreview(formData.value.ui.theme);
    }
  } catch (e) {
    console.error("加载主题列表失败", e);
  }
}

async function loadThemePreview(themeName: string) {
  if (!isWailsEnv.value) return;
  try {
    const preview = await wailsApi.getThemePreview(themeName);
    themePreview.value = preview;
  } catch (e) {
    console.error("加载主题预览失败", e);
    themePreview.value = null;
  }
}

async function onThemeSelect(themeName: string) {
  await loadThemePreview(themeName);
}

// 外部链接和工具
async function handleOpenLogFolder() {
  try {
    if (isWailsEnv.value) {
      await wailsApi.openLogFolder();
    }
  } catch (e) {
    console.error("打开日志目录失败", e);
  }
}

async function handleOpenExternalLink(url: string) {
  try {
    if (isWailsEnv.value) {
      await wailsApi.openExternalURL(url);
    }
  } catch (e) {
    console.error("打开链接失败", e);
  }
}

onMounted(async () => {
  await loadData();
  if (isWailsEnv.value) {
    await refreshStatus();

    try {
      const page = await wailsApi.getStartPage();
      if (page) {
        activeTab.value = page;
      }
    } catch (e) {
      // 忽略错误，使用默认页面
    }
  }
});
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="logo">
        <span class="logo-icon">🌬</span>
        <div class="logo-title">
          <span class="logo-text">清风输入法</span>
          <span class="logo-version" v-if="status"
            >v{{ status.service.version }}</span
          >
        </div>
        <span
          class="status-dot-inline"
          :class="connected ? 'connected' : 'disconnected'"
          :title="connected ? '已连接' : '未连接'"
        ></span>
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
        <div class="sidebar-message">
          <span v-if="saveMessage" :class="['message', saveMessageType]">
            {{ saveMessage }}
          </span>
        </div>
        <div class="sidebar-actions">
          <button class="btn" @click="resetCurrentPageDefaults">
            恢复本页默认
          </button>
          <button
            class="btn btn-primary"
            @click="saveConfig"
            :disabled="saving || hotkeyConflicts.length > 0"
          >
            {{ saving ? "保存中..." : "保存设置" }}
          </button>
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
        <GeneralPage
          v-show="activeTab === 'general'"
          :formData="formData"
          :engines="engines"
        />

        <InputPage v-show="activeTab === 'input'" :formData="formData" />

        <HotkeyPage
          v-show="activeTab === 'hotkey'"
          :formData="formData"
          :hotkeyConflicts="hotkeyConflicts"
          @update:hotkeyConflicts="hotkeyConflicts = $event"
        />

        <AppearancePage
          v-show="activeTab === 'appearance'"
          :formData="formData"
          :isWailsEnv="isWailsEnv"
          :availableThemes="availableThemes"
          :themePreview="themePreview"
          @themeSelect="onThemeSelect"
        />

        <DictionaryPage
          v-show="activeTab === 'dictionary'"
          :isWailsEnv="isWailsEnv"
        />

        <AdvancedPage
          v-show="activeTab === 'advanced'"
          :formData="formData"
          :isWailsEnv="isWailsEnv"
          @openLogFolder="handleOpenLogFolder"
        />

        <AboutPage
          v-show="activeTab === 'about'"
          :status="status"
          :appIconUrl="appIconUrl"
          :repoUrl="repoUrl"
          @openExternalLink="handleOpenExternalLink"
        />
      </div>
    </main>
  </div>
</template>
