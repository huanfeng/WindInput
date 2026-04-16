<script setup lang="ts">
import { ref, onMounted, computed } from "vue";
import {
  EventsOn,
  Quit,
  Show,
  WindowSetAlwaysOnTop,
} from "../wailsjs/runtime/runtime";
import * as api from "./api/settings";
import * as wailsApi from "./api/wails";
import type { Config, Status, EngineInfo, TSFLogConfig } from "./api/settings";
import type { ThemeInfo, ThemePreview, SystemFontInfo } from "./api/wails";
import { getDefaultConfig, getDefaultTSFLogConfig } from "./api/settings";
import { Sonner } from "@/components/ui/sonner";
import { provideToast } from "./composables/useToast";
import { useConfirm } from "./composables/useConfirm";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
  AlertDialogAction,
} from "./components/ui/alert-dialog";
import { Button } from "./components/ui/button";

import GeneralPage from "./pages/GeneralPage.vue";
import InputPage from "./pages/InputPage.vue";
import HotkeyPage from "./pages/HotkeyPage.vue";
import AppearancePage from "./pages/AppearancePage.vue";
import DictionaryPage from "./pages/DictionaryPage.vue";
import AdvancedPage from "./pages/AdvancedPage.vue";
import AboutPage from "./pages/AboutPage.vue";
import AddWordPage from "./pages/AddWordPage.vue";
import type { AddWordParams } from "./api/wails";

// 检测是否在 Wails 环境中
const isWailsEnv = computed(() => {
  return (
    typeof window !== "undefined" && (window as any).go?.main?.App !== undefined
  );
});

// 全局 Toast
const { toast } = provideToast();
const {
  confirmVisible,
  confirmMessage,
  confirm: customConfirm,
  handleConfirm,
  handleCancel,
} = useConfirm();

// 状态
const loading = ref(true);
const error = ref("");
const connected = ref(false);
const activeTab = ref("general");
const saving = ref(false);
const addWordParams = ref<AddWordParams | null>(null);
const showAddWordDialog = ref(false);
const isStandaloneAddWord = ref(false); // 独立加词窗口模式（无设置主界面）
const hotkeyConflicts = ref<string[]>([]);

// 数据
const config = ref<Config | null>(null);
const savedTSFLogConfig = ref<TSFLogConfig>(getDefaultTSFLogConfig());
const status = ref<Status | null>(null);
const engines = ref<EngineInfo[]>([]);

// 表单数据（用于编辑）
const formData = ref<Config>(getDefaultConfig());
const tsfLogConfig = ref<TSFLogConfig>(getDefaultTSFLogConfig());

// 系统默认配置缓存（代码默认值 + data/config.yaml 合并结果）
const systemDefaults = ref<Config>(getDefaultConfig());

// 主题相关状态
const availableThemes = ref<ThemeInfo[]>([]);
const themePreview = ref<ThemePreview | null>(null);
const systemFonts = ref<SystemFontInfo[]>([]);

const repoUrl = "https://github.com/huanfeng/WindInput";
const appIconUrl = new URL(
  "./assets/images/logo-universal.png",
  import.meta.url,
).href;

// 标签页定义
const tabs = [
  { id: "general", label: "方案", icon: "🏠" },
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
    // 加载系统默认配置（代码默认值 + data/config.yaml）
    try {
      const sysDefaults = await wailsApi.fetchSystemDefaultConfig();
      if (sysDefaults) {
        systemDefaults.value = mergeWithDefaults(sysDefaults);
      }
    } catch (e) {
      console.warn("加载系统默认配置失败，使用硬编码默认值", e);
    }

    const cfg = await wailsApi.getConfig();
    if (cfg) {
      const mergedCfg = mergeWithDefaults(cfg);
      config.value = mergedCfg;
      formData.value = JSON.parse(JSON.stringify(mergedCfg));
    }

    const currentTSFLogConfig = await wailsApi.getTSFLogConfig();
    tsfLogConfig.value = JSON.parse(JSON.stringify(currentTSFLogConfig));
    savedTSFLogConfig.value = JSON.parse(JSON.stringify(currentTSFLogConfig));

    // 从 schema.available 动态构建方案列表
    const schemaDisplayMap: Record<string, { name: string; desc: string }> = {
      wubi86: { name: "五笔输入", desc: "86版五笔" },
      pinyin: { name: "拼音输入", desc: "全拼输入法" },
    };
    const available = config.value?.schema?.available || ["wubi86", "pinyin"];
    const activeSchema = config.value?.schema?.active || "wubi86";
    engines.value = available.map((id: string) => ({
      type: id,
      displayName: schemaDisplayMap[id]?.name || id,
      description: schemaDisplayMap[id]?.desc || "",
      isActive: id === activeSchema,
    }));

    await loadThemes();
    try {
      systemFonts.value = await wailsApi.getSystemFonts();
    } catch (e) {
      console.warn("加载系统字体失败", e);
      systemFonts.value = [];
    }
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
  tsfLogConfig.value = getDefaultTSFLogConfig();
  savedTSFLogConfig.value = getDefaultTSFLogConfig();

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
    schema: { ...defaults.schema, ...cfg.schema },
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
      codetable: { ...defaults.engine.codetable, ...cfg.engine?.codetable },
    },
    hotkeys: { ...defaults.hotkeys, ...cfg.hotkeys },
    ui: {
      ...defaults.ui,
      ...cfg.ui,
      status_indicator: {
        ...defaults.ui.status_indicator,
        ...cfg.ui?.status_indicator,
      },
    },
    toolbar: { ...defaults.toolbar, ...cfg.toolbar },
    input: {
      ...defaults.input,
      ...cfg.input,
      temp_pinyin: {
        ...defaults.input.temp_pinyin,
        ...cfg.input?.temp_pinyin,
      },
      auto_pair: {
        ...defaults.input.auto_pair,
        ...cfg.input?.auto_pair,
      },
      overflow_behavior: {
        ...defaults.input.overflow_behavior,
        ...cfg.input?.overflow_behavior,
      },
    },
    advanced: { ...defaults.advanced, ...cfg.advanced },
  };
}

// 保存配置
async function saveConfig() {
  if (hotkeyConflicts.value.length > 0) {
    toast("存在快捷键冲突，请先解决", "error");
    return;
  }

  saving.value = true;

  try {
    if (isWailsEnv.value) {
      await wailsApi.saveConfig(formData.value as any);
      await wailsApi.saveTSFLogConfig(tsfLogConfig.value);
      toast("保存成功");
      config.value = JSON.parse(JSON.stringify(formData.value));
      savedTSFLogConfig.value = JSON.parse(JSON.stringify(tsfLogConfig.value));
    } else {
      const res = await api.updateConfig(formData.value);
      if (res.success && res.data) {
        let msg = "保存成功";
        if (res.data.needReload.length > 0) {
          msg += "（部分设置需要重载生效）";
        }
        toast(msg);
        config.value = JSON.parse(JSON.stringify(formData.value));
      } else {
        toast(res.error || "保存失败", "error");
      }
    }
  } catch (e: any) {
    toast(e.message || "保存失败", "error");
  } finally {
    saving.value = false;
  }
}

// 检测是否有未保存的修改
function hasUnsavedChanges(): boolean {
  if (!config.value) return false;
  const configChanged =
    JSON.stringify(formData.value) !== JSON.stringify(config.value);
  if (!isWailsEnv.value) return configChanged;

  return (
    configChanged ||
    JSON.stringify(tsfLogConfig.value) !==
      JSON.stringify(savedTSFLogConfig.value)
  );
}

// 关闭加词对话框
function handleAddWordClose() {
  showAddWordDialog.value = false;
  // 独立窗口模式下关闭 = 退出应用
  if (isStandaloneAddWord.value) {
    try {
      Quit();
    } catch {
      // 忽略
    }
  }
}

// 重新加载配置（丢弃本地修改，从实际文件重新读取）
async function handleReloadConfig() {
  if (hasUnsavedChanges()) {
    const ok = await customConfirm(
      "当前有未保存的修改，重新加载将丢弃这些修改。确定继续吗？",
    );
    if (!ok) return;
  }
  await handleReload();
}

// 重载配置
async function handleReload() {
  try {
    if (isWailsEnv.value) {
      await wailsApi.reloadConfig();
      toast("重载成功");
      await loadData();
    } else {
      const res = await api.reloadConfig();
      if (res.success) {
        toast("重载成功");
        await loadData();
      } else {
        toast(res.error || "重载失败", "error");
      }
    }
  } catch (e: any) {
    toast("重载失败", "error");
  }
}

// 刷新状态
async function refreshStatus() {
  try {
    if (isWailsEnv.value) {
      const serviceStatus = await wailsApi.getServiceStatus();
      const appVersion = await wailsApi.getVersion().catch(() => "dev");
      if (serviceStatus) {
        status.value = {
          service: {
            name: "清风输入法",
            version: appVersion,
            uptime: "",
            uptimeSec: 0,
          },
          engine: {
            type: serviceStatus.engine_type || "",
            displayName:
              { pinyin: "拼音", codetable: "码表", mixed: "混输" }[
                serviceStatus.engine_type
              ] ?? "码表",
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

// 重置为当前页面默认（使用系统默认配置：代码默认值 + data/config.yaml）
async function resetCurrentPageDefaults() {
  const defaults = systemDefaults.value;
  let changed = true;

  switch (activeTab.value) {
    case "general":
      formData.value.startup = { ...defaults.startup };
      formData.value.schema.active = defaults.schema.active;
      break;
    case "input":
      formData.value.engine = {
        ...formData.value.engine,
        pinyin: { ...defaults.engine.pinyin },
        codetable: { ...defaults.engine.codetable },
      };
      formData.value.input = {
        ...formData.value.input,
        punct_follow_mode: defaults.input.punct_follow_mode,
        filter_mode: defaults.input.filter_mode,
        smart_punct_after_digit: defaults.input.smart_punct_after_digit,
        auto_pair: { ...defaults.input.auto_pair },
        quick_input: { ...defaults.input.quick_input },
        overflow_behavior: { ...defaults.input.overflow_behavior },
      };
      break;
    case "hotkey":
      formData.value.hotkeys = { ...defaults.hotkeys };
      formData.value.input = {
        ...formData.value.input,
        select_key_groups: [...defaults.input.select_key_groups],
        page_keys: [...defaults.input.page_keys],
        highlight_keys: [...defaults.input.highlight_keys],
        select_char_keys: [...defaults.input.select_char_keys],
      };
      break;
    case "appearance":
      formData.value.ui = {
        ...defaults.ui,
        status_indicator: { ...defaults.ui.status_indicator },
      };
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

  toast(
    changed ? "已恢复本页默认设置" : "本页没有可恢复的设置",
    changed ? "success" : "error",
    2000,
  );
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
    const themeStyle = formData.value.ui.theme_style || "system";
    const preview = await wailsApi.getThemePreview(themeName, themeStyle);
    themePreview.value = preview;
  } catch (e) {
    console.error("加载主题预览失败", e);
    themePreview.value = null;
  }
}

async function onThemeSelect(themeName: string) {
  await loadThemePreview(themeName);
}

async function onThemeStyleChange(_themeStyle: string) {
  // Reload preview to show the correct light/dark variant
  if (formData.value.ui.theme) {
    await loadThemePreview(formData.value.ui.theme);
  }
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

async function handleOpenConfigFolder() {
  try {
    if (isWailsEnv.value) {
      await wailsApi.openConfigFolder();
    }
  } catch (e) {
    console.error("打开配置目录失败", e);
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
      if (page && page !== "add-word") {
        activeTab.value = page;
      }
      // 加词模式：作为独立窗口打开
      if (page === "add-word") {
        isStandaloneAddWord.value = true;
        try {
          addWordParams.value = await wailsApi.getAddWordParams();
        } catch {
          addWordParams.value = { text: "", code: "", schema_id: "" };
        }
        showAddWordDialog.value = true;
        // 强制窗口前置（从后台进程启动时 Windows 不会自动给予前台权限）
        try {
          WindowSetAlwaysOnTop(true);
          setTimeout(() => WindowSetAlwaysOnTop(false), 300);
        } catch {}
      }
    } catch (e) {
      // 忽略错误，使用默认页面
    }

    // 监听其他实例发来的页面切换请求
    EventsOn("navigate", (page: string) => {
      if (page) {
        activeTab.value = page;
      }
    });

    // 监听加词导航事件（从已有实例的 IPC 传来）
    EventsOn("navigate-addword", (params: any) => {
      addWordParams.value = {
        text: params.text || "",
        code: params.code || "",
        schema_id: params.schema_id || "",
      };
      showAddWordDialog.value = true;
      // 将窗口拉到最前
      try {
        Show();
      } catch {}
    });
  }
});
</script>

<template>
  <Sonner />
  <div class="app">
    <!-- 加词对话框（模态浮层，可在任何页面上弹出） -->
    <AddWordPage
      v-if="showAddWordDialog"
      :initialText="addWordParams?.text"
      :initialCode="addWordParams?.code"
      :initialSchema="addWordParams?.schema_id"
      :standalone="isStandaloneAddWord"
      @close="handleAddWordClose"
    />

    <aside v-show="!isStandaloneAddWord" class="sidebar">
      <div class="logo">
        <img class="logo-icon" :src="appIconUrl" alt="清风输入法" />
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
        <div class="sidebar-actions">
          <Button variant="outline" @click="resetCurrentPageDefaults">
            恢复本页默认
          </Button>
          <Button variant="outline" @click="handleReloadConfig"
            >重新加载</Button
          >
          <Button
            @click="saveConfig"
            :disabled="saving || hotkeyConflicts.length > 0"
          >
            {{ saving ? "保存中..." : "保存设置" }}
          </Button>
        </div>
      </div>
    </aside>

    <main v-show="!isStandaloneAddWord" class="main">
      <div v-if="loading" class="loading">
        <div class="spinner"></div>
        <p>加载中...</p>
      </div>

      <div v-else-if="error" class="error-panel">
        <div class="error-icon">⚠</div>
        <p>{{ error }}</p>
        <Button @click="loadData">重试</Button>
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
          :systemDefaults="systemDefaults"
          @update:hotkeyConflicts="hotkeyConflicts = $event"
        />

        <AppearancePage
          v-show="activeTab === 'appearance'"
          :formData="formData"
          :isWailsEnv="isWailsEnv"
          :availableThemes="availableThemes"
          :themePreview="themePreview"
          :systemFonts="systemFonts"
          @themeSelect="onThemeSelect"
          @themeStyleChange="onThemeStyleChange"
        />

        <DictionaryPage
          v-show="activeTab === 'dictionary'"
          :isWailsEnv="isWailsEnv"
        />

        <AdvancedPage
          v-show="activeTab === 'advanced'"
          :formData="formData"
          :tsfLogConfig="tsfLogConfig"
          :isWailsEnv="isWailsEnv"
          @openLogFolder="handleOpenLogFolder"
          @openConfigFolder="handleOpenConfigFolder"
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
    <!-- 确认对话框 -->
    <AlertDialog :open="confirmVisible">
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>确认</AlertDialogTitle>
          <AlertDialogDescription class="whitespace-pre-line">{{
            confirmMessage
          }}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel @click="handleCancel">取消</AlertDialogCancel>
          <AlertDialogAction @click="handleConfirm">确定</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>
</template>
