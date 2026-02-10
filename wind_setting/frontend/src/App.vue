<script setup lang="ts">
import { ref, onMounted, computed, watch, onUnmounted } from "vue";
import * as api from "./api/settings";
import * as wailsApi from "./api/wails";
import type { Config, Status, EngineInfo } from "./api/settings";
import type {
  PhraseItem,
  UserWordItem,
  ShadowRuleItem,
  DictStats,
  FileChangeStatus,
  ThemeInfo,
  ThemePreview,
} from "./api/wails";
import { getDefaultConfig } from "./api/settings";

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
const advancedSubTab = ref<"advanced" | "test">("advanced");
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

// 测试页面状态
const testInput = ref("");
const testCandidates = ref<any[]>([]);
const testEngine = ref("current");
const testFilterMode = ref("current");
const testLoading = ref(false);

// 日志页面状态
const logPath = "%APPDATA%\\WindInput\\";

// 词库管理状态
const dictSubTab = ref<"phrases" | "userdict" | "shadow">("phrases");
const phrases = ref<PhraseItem[]>([]);
const userDict = ref<UserWordItem[]>([]);
const shadowRules = ref<ShadowRuleItem[]>([]);
const dictStats = ref<DictStats>({
  word_count: 0,
  phrase_count: 0,
  shadow_count: 0,
});
const dictLoading = ref(false);
const dictMessage = ref("");
const dictMessageType = ref<"success" | "error">("success");

// 添加短语表单
const showAddPhraseForm = ref(false);
const newPhrase = ref({ code: "", text: "", weight: 0 });

// 用户词库引擎类型
const userDictEngine = ref<"wubi" | "pinyin">("wubi");

// 添加用户词条表单
const showAddWordForm = ref(false);
const newWord = ref({ code: "", text: "", weight: 0 });

// 添加 Shadow 规则表单
const showAddShadowForm = ref(false);
const newShadow = ref({ code: "", word: "", action: "pin", weight: 100 });

// 文件变化状态
const fileChangeStatus = ref<FileChangeStatus | null>(null);
const showFileChangeAlert = ref(false);
let fileCheckTimer: number | null = null;

// 主题相关状态
const availableThemes = ref<ThemeInfo[]>([]);
const themePreview = ref<ThemePreview | null>(null);
const themeLoading = ref(false);
const themeSelectOpen = ref(false);
const themeDropdownRef = ref<HTMLElement | null>(null);
const inputModeSelectOpen = ref(false);
const inputModeDropdownRef = ref<HTMLElement | null>(null);
const repoUrl = "https://github.com/huanfeng/WindInput";

const appIconUrl = new URL(
  "./assets/images/logo-universal.png",
  import.meta.url,
).href;

// 重新组织的标签页 - 按用户视角划分
const tabs = [
  { id: "general", label: "常用", icon: "🏠" },
  { id: "input", label: "输入", icon: "⌨" },
  { id: "hotkey", label: "按键", icon: "🎮" },
  { id: "appearance", label: "外观", icon: "🎨" },
  { id: "dictionary", label: "词库", icon: "📚" },
  { id: "advanced", label: "高级", icon: "🛠" },
  { id: "about", label: "关于", icon: "ℹ" },
];

const inputModeOptions = computed(() => {
  const base =
    engines.value.length > 0
      ? engines.value.map((engine) => ({
          value: engine.type,
          label: engine.displayName,
          description: engine.description,
          disabled: false,
        }))
      : [
          {
            value: "wubi",
            label: "五笔输入",
            description: "86版五笔",
            disabled: false,
          },
          {
            value: "pinyin",
            label: "拼音输入",
            description: "全拼输入法",
            disabled: false,
          },
        ];

  const preferredOrder = ["wubi", "pinyin"];
  const ordered = [...base].sort((a, b) => {
    const ai = preferredOrder.indexOf(a.value);
    const bi = preferredOrder.indexOf(b.value);
    if (ai === -1 && bi === -1)
      return a.label.localeCompare(b.label, "zh-Hans-CN");
    if (ai === -1) return 1;
    if (bi === -1) return -1;
    return ai - bi;
  });

  return [
    ...ordered,
    {
      value: "shuangpin",
      label: "双拼输入",
      description: "开发中",
      disabled: true,
    },
    {
      value: "mixed",
      label: "混合输入",
      description: "开发中",
      disabled: true,
    },
  ];
});

const currentInputMode = computed(() => {
  return inputModeOptions.value.find(
    (option) => option.value === formData.value.engine.type,
  );
});

const themeOptions = computed(() => {
  return availableThemes.value.map((theme) => ({
    name: theme.name,
    label: theme.display_name || theme.name,
    description: theme.author ? `作者 ${theme.author}` : "暂无描述",
    version: theme.version || "",
    isActive: theme.is_active,
    isBuiltin: theme.is_builtin,
  }));
});

const currentThemeOption = computed(() => {
  return themeOptions.value.find(
    (option) => option.name === formData.value.ui.theme,
  );
});

// 检查快捷键冲突
function checkConflicts() {
  const conflicts: string[] = [];
  const usedKeys = new Map<string, string>();

  // 中英切换键
  for (const key of formData.value.hotkeys.toggle_mode_keys) {
    if (usedKeys.has(key)) {
      conflicts.push(
        `按键 "${getKeyLabel(key)}" 同时用于: ${usedKeys.get(key)} 和 中英切换`,
      );
    } else {
      usedKeys.set(key, "中英切换");
    }
  }

  // 候选选择键组
  for (const group of formData.value.input.select_key_groups) {
    const keys = getGroupKeys(group);
    for (const key of keys) {
      if (usedKeys.has(key)) {
        conflicts.push(
          `按键 "${getKeyLabel(key)}" 同时用于: ${usedKeys.get(key)} 和 候选选择`,
        );
      } else {
        usedKeys.set(key, "候选选择");
      }
    }
  }

  hotkeyConflicts.value = conflicts;
}

function getGroupKeys(group: string): string[] {
  switch (group) {
    case "semicolon_quote":
      return ["semicolon", "quote"];
    case "comma_period":
      return ["comma", "period"];
    case "lrshift":
      return ["lshift", "rshift"];
    case "lrctrl":
      return ["lctrl", "rctrl"];
    default:
      return [];
  }
}

function getKeyLabel(key: string): string {
  const labels: Record<string, string> = {
    lshift: "左Shift",
    rshift: "右Shift",
    lctrl: "左Ctrl",
    rctrl: "右Ctrl",
    capslock: "CapsLock",
    semicolon: ";",
    quote: "'",
    comma: ",",
    period: ".",
  };
  return labels[key] || key;
}

// 监听配置变化，检查冲突
watch(
  () => [
    formData.value.hotkeys.toggle_mode_keys,
    formData.value.input.select_key_groups,
  ],
  checkConflicts,
  { deep: true },
);

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
  error.value = "";

  try {
    // 在 Wails 环境中使用 Wails API
    if (isWailsEnv.value) {
      await loadDataFromWails();
    } else {
      // 非 Wails 环境，尝试使用 HTTP API（兼容开发模式）
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

// 从 Wails API 加载数据
async function loadDataFromWails() {
  connected.value = true; // Wails 环境始终视为已连接

  try {
    const cfg = await wailsApi.getConfig();
    if (cfg) {
      const mergedCfg = mergeWithDefaults(cfg);
      config.value = mergedCfg;
      formData.value = JSON.parse(JSON.stringify(mergedCfg));
      checkConflicts();
    }

    // 获取引擎列表（从配置推断）
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

    // 加载词库数据
    await loadDictData();

    // 加载主题列表
    await loadThemes();
  } catch (e) {
    console.error("Wails API 调用失败", e);
    throw e;
  }
}

// 从 HTTP API 加载数据（开发模式兼容）
async function loadDataFromHTTP() {
  const healthRes = await api.checkHealth();
  if (!healthRes.success) {
    connected.value = false;
    // 在开发模式下，如果 HTTP 不可用，不显示错误，而是提示用户
    error.value = "请使用 wails dev 命令启动开发服务器，或运行编译后的应用";
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
      // 使用 Wails API 保存配置
      await wailsApi.saveConfig(formData.value as any);
      saveMessageType.value = "success";
      saveMessage.value = "保存成功";
      config.value = JSON.parse(JSON.stringify(formData.value));
    } else {
      // 使用 HTTP API
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

// 切换引擎
async function handleSwitchEngine(type: string) {
  try {
    formData.value.engine.type = type;
    engines.value = engines.value.map((e) => ({
      ...e,
      isActive: e.type === type,
    }));
  } catch (e) {
    console.error("切换引擎失败", e);
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
        // 转换为前端期望的格式
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

// 重置为当前配置
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

  checkConflicts();
  saveMessageType.value = changed ? "success" : "error";
  saveMessage.value = changed ? "已恢复本页默认设置" : "本页没有可恢复的设置";
  setTimeout(() => {
    saveMessage.value = "";
  }, 2000);
}

// 加载可用主题列表
async function loadThemes() {
  if (!isWailsEnv.value) return;

  try {
    themeLoading.value = true;
    const themes = await wailsApi.getAvailableThemes();
    availableThemes.value = themes;

    // 加载当前选中主题的预览
    if (formData.value.ui.theme) {
      await loadThemePreview(formData.value.ui.theme);
    }
  } catch (e) {
    console.error("加载主题列表失败", e);
  } finally {
    themeLoading.value = false;
  }
}

// 加载主题预览
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

// 选择主题时更新 formData 并加载预览
async function onThemeSelect(themeName: string) {
  formData.value.ui.theme = themeName;
  await loadThemePreview(themeName);
  themeSelectOpen.value = false;
}

async function onInputModeSelect(mode: string) {
  formData.value.engine.type = mode;
  await handleSwitchEngine(mode);
  inputModeSelectOpen.value = false;
}

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

// 测试输入
async function handleTestInput() {
  if (!testInput.value.trim()) {
    testCandidates.value = [];
    return;
  }

  testLoading.value = true;
  try {
    const res = await api.testConvert(
      testInput.value,
      testEngine.value,
      testFilterMode.value,
    );
    if (res.success && res.data) {
      testCandidates.value = res.data.candidates || [];
    }
  } catch (e) {
    console.error("测试失败", e);
  } finally {
    testLoading.value = false;
  }
}

watch(testInput, handleTestInput);
watch([testEngine, testFilterMode], () => {
  if (testInput.value) handleTestInput();
});

watch([activeTab, advancedSubTab], ([tab]) => {
  if (tab === "dictionary") {
    loadDictData();
  }
});

// ========== 词库管理方法 ==========

// 加载词库数据
async function loadDictData() {
  if (!isWailsEnv.value) return;

  dictLoading.value = true;
  try {
    const [phrasesData, userDictData, shadowData, stats, engineType] =
      await Promise.all([
        wailsApi.getPhrases(),
        wailsApi.getUserDict(),
        wailsApi.getShadowRules(),
        wailsApi.getUserDictStats(),
        wailsApi.getUserDictEngineType(),
      ]);
    phrases.value = phrasesData || [];
    userDict.value = userDictData || [];
    shadowRules.value = shadowData || [];
    dictStats.value = stats || {
      word_count: 0,
      phrase_count: 0,
      shadow_count: 0,
    };
    if (engineType === "pinyin" || engineType === "wubi") {
      userDictEngine.value = engineType;
    }
  } catch (e) {
    console.error("加载词库数据失败", e);
    showDictMessage("加载词库数据失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

async function handleSwitchUserDictEngine(engineType: "wubi" | "pinyin") {
  if (engineType === userDictEngine.value) return;
  dictLoading.value = true;
  try {
    await wailsApi.switchUserDictEngine(engineType);
    userDictEngine.value = engineType;
    const [userDictData, stats] = await Promise.all([
      wailsApi.getUserDict(),
      wailsApi.getUserDictStats(),
    ]);
    userDict.value = userDictData || [];
    dictStats.value = {
      ...dictStats.value,
      word_count: stats?.word_count || 0,
    };
  } catch (e) {
    console.error("切换词库失败", e);
    showDictMessage("切换词库失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

// 显示词库消息
function showDictMessage(msg: string, type: "success" | "error") {
  dictMessage.value = msg;
  dictMessageType.value = type;
  setTimeout(() => {
    dictMessage.value = "";
  }, 3000);
}

// 添加短语
async function handleAddPhrase() {
  if (!newPhrase.value.code || !newPhrase.value.text) {
    showDictMessage("请填写编码和文本", "error");
    return;
  }

  try {
    await wailsApi.addPhrase(
      newPhrase.value.code,
      newPhrase.value.text,
      newPhrase.value.weight,
    );
    showDictMessage("添加成功", "success");
    showAddPhraseForm.value = false;
    newPhrase.value = { code: "", text: "", weight: 0 };
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "添加失败", "error");
  }
}

// 删除短语
async function handleRemovePhrase(item: PhraseItem) {
  if (!confirm(`确定删除短语 "${item.text}" 吗？`)) return;

  try {
    await wailsApi.removePhrase(item.code, item.text);
    showDictMessage("删除成功", "success");
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "删除失败", "error");
  }
}

// 添加用户词条
async function handleAddUserWord() {
  if (!newWord.value.code || !newWord.value.text) {
    showDictMessage("请填写编码和文本", "error");
    return;
  }

  try {
    await wailsApi.addUserWord(
      newWord.value.code,
      newWord.value.text,
      newWord.value.weight,
    );
    showDictMessage("添加成功", "success");
    showAddWordForm.value = false;
    newWord.value = { code: "", text: "", weight: 0 };
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "添加失败", "error");
  }
}

// 删除用户词条
async function handleRemoveUserWord(item: UserWordItem) {
  if (!confirm(`确定删除词条 "${item.text}" 吗？`)) return;

  try {
    await wailsApi.removeUserWord(item.code, item.text);
    showDictMessage("删除成功", "success");
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "删除失败", "error");
  }
}

// 添加 Shadow 规则
async function handleAddShadowRule() {
  if (!newShadow.value.code || !newShadow.value.word) {
    showDictMessage("请填写编码和词条", "error");
    return;
  }

  try {
    await wailsApi.addShadowRule(
      newShadow.value.code,
      newShadow.value.word,
      newShadow.value.action,
      newShadow.value.weight,
    );
    showDictMessage("添加成功", "success");
    showAddShadowForm.value = false;
    newShadow.value = { code: "", word: "", action: "pin", weight: 100 };
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "添加失败", "error");
  }
}

// 删除 Shadow 规则
async function handleRemoveShadowRule(item: ShadowRuleItem) {
  if (!confirm(`确定删除规则 "${item.word}" 吗？`)) return;

  try {
    await wailsApi.removeShadowRule(item.code, item.word);
    showDictMessage("删除成功", "success");
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "删除失败", "error");
  }
}

// 检查文件变化
async function checkFileChanges() {
  if (!isWailsEnv.value) return;

  try {
    const status = await wailsApi.checkAllFilesModified();
    fileChangeStatus.value = status;

    if (
      status.config_changed ||
      status.phrases_changed ||
      status.shadow_changed ||
      status.userdict_changed
    ) {
      showFileChangeAlert.value = true;
    }
  } catch (e) {
    console.error("检查文件变化失败", e);
  }
}

// 重新加载所有文件
async function handleReloadAllFiles() {
  try {
    await wailsApi.reloadAllFiles();
    showFileChangeAlert.value = false;
    fileChangeStatus.value = null;
    await loadDictData();
    showDictMessage("已重新加载所有文件", "success");
  } catch (e: any) {
    showDictMessage(e.message || "重新加载失败", "error");
  }
}

// 获取 Shadow action 的显示文本
function getShadowActionLabel(action: string): string {
  const labels: Record<string, string> = {
    pin: "置顶",
    delete: "删除",
    adjust: "调整权重",
  };
  return labels[action] || action;
}

onMounted(async () => {
  await loadData();
  if (isWailsEnv.value) {
    await refreshStatus();

    // 检查命令行指定的启动页面
    try {
      const page = await wailsApi.getStartPage();
      if (page) {
        activeTab.value = page;
      }
    } catch (e) {
      // 忽略错误，使用默认页面
    }
  }

  // 定期检查文件变化
  fileCheckTimer = window.setInterval(checkFileChanges, 5000);

  document.addEventListener("click", handleDocumentClick);
});

onUnmounted(() => {
  if (fileCheckTimer) clearInterval(fileCheckTimer);
  document.removeEventListener("click", handleDocumentClick);
});

function handleDocumentClick(event: MouseEvent) {
  const target = event.target as Node;
  if (themeDropdownRef.value && !themeDropdownRef.value.contains(target)) {
    themeSelectOpen.value = false;
  }
  if (
    inputModeDropdownRef.value &&
    !inputModeDropdownRef.value.contains(target)
  ) {
    inputModeSelectOpen.value = false;
  }
}
</script>

<template>
  <div class="app">
    <aside class="sidebar">
            <div class="logo">
              <span class="logo-icon">🌬</span>
              <div class="logo-title">
                <span class="logo-text">清风输入法</span>
                <span class="logo-version" v-if="status"
                  >v{{ status.service.version }}</span>
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
        <!-- ==================== 常用设置 ==================== -->
        <section v-if="activeTab === 'general'" class="section">
          <div class="section-header">
            <h2>常用设置</h2>
            <p class="section-desc">最基本的输入法设置</p>
          </div>

          <!-- 输入模式 -->
          <div class="settings-card">
            <div class="card-title">输入模式</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>输入方案</label>
                <p class="setting-hint">
                  {{ currentInputMode?.description || "选择适合的输入方案" }}
                </p>
              </div>
              <div class="setting-control">
                <div
                  class="theme-dropdown input-mode-dropdown"
                  ref="inputModeDropdownRef"
                >
                  <button
                    class="theme-select select-strong"
                    type="button"
                    @click="inputModeSelectOpen = !inputModeSelectOpen"
                  >
                    <div class="theme-select-main">
                      <div class="theme-select-title">
                        {{ currentInputMode?.label || "选择输入方案" }}
                      </div>
                      <div class="theme-select-sub">
                        <span>{{
                          currentInputMode?.description || "暂无描述"
                        }}</span>
                      </div>
                    </div>
                    <span class="theme-select-arrow">▾</span>
                  </button>

                  <div v-if="inputModeSelectOpen" class="theme-options">
                    <button
                      v-for="option in inputModeOptions"
                      :key="option.value"
                      type="button"
                      class="theme-option"
                      :class="{
                        selected: formData.engine.type === option.value,
                      }"
                      :disabled="option.disabled"
                      @click="
                        !option.disabled && onInputModeSelect(option.value)
                      "
                    >
                      <div class="theme-option-title">
                        <span class="theme-option-name">{{
                          option.label
                        }}</span>
                        <span v-if="option.disabled" class="theme-badge builtin"
                          >开发中</span
                        >
                      </div>
                      <div class="theme-option-sub">
                        <span>{{ option.description }}</span>
                      </div>
                    </button>
                  </div>
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
                <p class="setting-hint">
                  启用后将使用上次退出时的状态，忽略以下默认设置
                </p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.startup.remember_last_state"
                  />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div
              class="setting-item"
              :class="{ 'item-disabled': formData.startup.remember_last_state }"
            >
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
                  >
                    中文
                  </button>
                  <button
                    :class="{ active: !formData.startup.default_chinese_mode }"
                    @click="formData.startup.default_chinese_mode = false"
                    :disabled="formData.startup.remember_last_state"
                  >
                    英文
                  </button>
                </div>
              </div>
            </div>
            <div
              class="setting-item"
              :class="{ 'item-disabled': formData.startup.remember_last_state }"
            >
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
                  >
                    半角
                  </button>
                  <button
                    :class="{ active: formData.startup.default_full_width }"
                    @click="formData.startup.default_full_width = true"
                    :disabled="formData.startup.remember_last_state"
                  >
                    全角
                  </button>
                </div>
              </div>
            </div>
            <div
              class="setting-item"
              :class="{ 'item-disabled': formData.startup.remember_last_state }"
            >
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
                  >
                    中文标点
                  </button>
                  <button
                    :class="{ active: !formData.startup.default_chinese_punct }"
                    @click="formData.startup.default_chinese_punct = false"
                    :disabled="formData.startup.remember_last_state"
                  >
                    英文标点
                  </button>
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
                  <input
                    type="checkbox"
                    v-model="formData.input.punct_follow_mode"
                  />
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
                <label>四码唯一时自动上屏</label>
                <p class="setting-hint">
                  输入满四码且只有一个候选时，自动提交首选
                </p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.engine.wubi.auto_commit_at_4"
                  />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>四码为空时清空</label>
                <p class="setting-hint">输入满四码但无候选时，自动清空编码</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.engine.wubi.clear_on_empty_at_4"
                  />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>五码顶字</label>
                <p class="setting-hint">输入第五码时自动上屏首选</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.engine.wubi.top_code_commit"
                  />
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
                  <input
                    type="checkbox"
                    v-model="formData.engine.wubi.punct_commit"
                  />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>逐字键入</label>
                <p class="setting-hint">仅显示精确匹配候选，关闭逐码前缀匹配</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.engine.wubi.single_code_input"
                  />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>编码提示</label>
                <p class="setting-hint">
                  在逐码候选后显示剩余编码，帮助学习全码
                </p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.engine.wubi.show_code_hint"
                  />
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
                  <input
                    type="checkbox"
                    v-model="formData.engine.pinyin.show_wubi_hint"
                  />
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
            <p class="section-desc">定制候选窗口的视觉呈现</p>
          </div>

          <!-- 主题选择 -->
          <div class="settings-card" v-if="isWailsEnv">
            <div class="card-title">主题</div>
            <div class="setting-item align-start">
              <div class="setting-info">
                <label>主题选择</label>
                <p class="setting-hint">显示主题作者与版本信息</p>
              </div>
              <div class="setting-control">
                <div class="theme-dropdown" ref="themeDropdownRef">
                  <button
                    class="theme-select select-strong"
                    type="button"
                    @click="themeSelectOpen = !themeSelectOpen"
                  >
                    <div class="theme-select-main">
                      <div class="theme-select-title">
                        {{ currentThemeOption?.label || "选择主题" }}
                      </div>
                      <div class="theme-select-sub">
                        <span>{{
                          currentThemeOption?.description || "暂无描述"
                        }}</span>
                        <span
                          v-if="currentThemeOption?.version"
                          class="theme-select-version"
                        >
                          v{{ currentThemeOption?.version }}
                        </span>
                      </div>
                    </div>
                    <span class="theme-select-arrow">▾</span>
                  </button>

                  <div v-if="themeSelectOpen" class="theme-options">
                    <button
                      v-for="theme in themeOptions"
                      :key="theme.name"
                      type="button"
                      class="theme-option"
                      :class="{ selected: formData.ui.theme === theme.name }"
                      @click="onThemeSelect(theme.name)"
                    >
                      <div class="theme-option-title">
                        <span class="theme-option-name">{{ theme.label }}</span>
                        <span v-if="theme.isBuiltin" class="theme-badge builtin"
                          >内置</span
                        >
                        <span v-if="theme.isActive" class="theme-badge active"
                          >当前</span
                        >
                      </div>
                      <div class="theme-option-sub">
                        <span>{{ theme.description }}</span>
                        <span v-if="theme.version" class="theme-option-version"
                          >v{{ theme.version }}</span
                        >
                      </div>
                    </button>
                    <div
                      v-if="themeOptions.length === 0"
                      class="theme-option-empty"
                    >
                      暂无主题
                    </div>
                  </div>
                </div>
              </div>
            </div>

            <div class="setting-item align-start" v-if="themePreview">
              <div class="setting-info">
                <label>主题预览</label>
                <p class="setting-hint">候选窗口与工具栏预览</p>
              </div>
              <div class="setting-control">
                <div class="theme-preview preview-rows">
                  <div class="preview-row">
                    <div class="preview-row-label">候选窗口</div>
                    <div
                      class="preview-candidate-window"
                      :style="{
                        backgroundColor:
                          themePreview.candidate_window?.background_color,
                        borderColor:
                          themePreview.candidate_window?.border_color,
                      }"
                    >
                      <div class="preview-candidate-item">
                        <span
                          class="preview-index"
                          :style="{
                            backgroundColor:
                              themePreview.candidate_window?.index_bg_color,
                            color: themePreview.candidate_window?.index_color,
                          }"
                          >1</span
                        >
                        <span
                          class="preview-text"
                          :style="{
                            color: themePreview.candidate_window?.text_color,
                          }"
                          >中文</span
                        >
                      </div>
                      <div
                        class="preview-candidate-item preview-hover"
                        :style="{
                          backgroundColor:
                            themePreview.candidate_window?.hover_bg_color,
                        }"
                      >
                        <span
                          class="preview-index"
                          :style="{
                            backgroundColor:
                              themePreview.candidate_window?.index_bg_color,
                            color: themePreview.candidate_window?.index_color,
                          }"
                          >2</span
                        >
                        <span
                          class="preview-text"
                          :style="{
                            color: themePreview.candidate_window?.text_color,
                          }"
                          >输入</span
                        >
                      </div>
                    </div>
                  </div>

                  <div class="preview-row">
                    <div class="preview-row-label">工具栏</div>
                    <div
                      class="preview-toolbar"
                      :style="{
                        backgroundColor: themePreview.toolbar?.background_color,
                        borderColor: themePreview.toolbar?.border_color,
                      }"
                    >
                      <span
                        class="preview-toolbar-item"
                        :style="{
                          backgroundColor:
                            themePreview.toolbar?.mode_chinese_bg_color,
                        }"
                        >中</span
                      >
                      <span
                        class="preview-toolbar-item"
                        :style="{
                          backgroundColor:
                            themePreview.toolbar?.full_width_on_bg_color,
                        }"
                        >全</span
                      >
                      <span
                        class="preview-toolbar-item"
                        :style="{
                          backgroundColor:
                            themePreview.toolbar?.punct_chinese_bg_color,
                        }"
                        >。</span
                      >
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">候选窗口</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>字体大小</label>
                <p class="setting-hint">候选词的显示大小</p>
              </div>
              <div class="setting-control range-control">
                <input
                  type="range"
                  min="12"
                  max="36"
                  step="1"
                  v-model.number="formData.ui.font_size"
                />
                <span class="range-value">{{ formData.ui.font_size }}px</span>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>每页候选数</label>
                <p class="setting-hint">每页显示的候选词数量</p>
              </div>
              <div class="setting-control range-control">
                <input
                  type="range"
                  min="3"
                  max="9"
                  step="1"
                  v-model.number="formData.ui.candidates_per_page"
                />
                <span class="range-value"
                  >{{ formData.ui.candidates_per_page }} 个</span
                >
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>自定义字体</label>
                <p class="setting-hint">留空使用系统默认字体</p>
              </div>
              <div class="setting-control">
                <input
                  type="text"
                  v-model="formData.ui.font_path"
                  class="input"
                  placeholder="字体文件路径"
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>隐藏候选窗口</label>
                <p class="setting-hint">隐藏候选窗口渲染</p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.ui.hide_candidate_window"
                  />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">编码显示</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>嵌入式编码行</label>
                <p class="setting-hint">
                  输入码直接显示在光标处，而非候选窗上方
                </p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input type="checkbox" v-model="formData.ui.inline_preedit" />
                  <span class="slider"></span>
                </label>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>候选布局</label>
                <p class="setting-hint">候选词的排列方式</p>
              </div>
              <div class="setting-control">
                <select v-model="formData.ui.candidate_layout" class="select">
                  <option value="horizontal">横向</option>
                  <option value="vertical">纵向</option>
                </select>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">状态提示</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>显示时长</label>
                <p class="setting-hint">中英切换等状态提示的显示时间</p>
              </div>
              <div class="setting-control range-control">
                <input
                  type="range"
                  min="200"
                  max="2000"
                  step="100"
                  v-model.number="formData.ui.status_indicator_duration"
                />
                <span class="range-value"
                  >{{ formData.ui.status_indicator_duration }}ms</span
                >
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>水平偏移</label>
                <p class="setting-hint">状态提示相对光标的水平偏移</p>
              </div>
              <div class="setting-control range-control">
                <input
                  type="range"
                  min="-50"
                  max="50"
                  step="5"
                  v-model.number="formData.ui.status_indicator_offset_x"
                />
                <span class="range-value"
                  >{{ formData.ui.status_indicator_offset_x }}px</span
                >
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>垂直偏移</label>
                <p class="setting-hint">
                  状态提示相对光标的垂直偏移（负值=向上）
                </p>
              </div>
              <div class="setting-control range-control">
                <input
                  type="range"
                  min="-100"
                  max="100"
                  step="5"
                  v-model.number="formData.ui.status_indicator_offset_y"
                />
                <span class="range-value"
                  >{{ formData.ui.status_indicator_offset_y }}px</span
                >
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-title">工具栏</div>
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
        <section v-if="activeTab === 'dictionary'" class="section section-wide">
          <div class="section-header">
            <h2>词库管理</h2>
            <p class="section-desc">管理您的词库数据</p>
          </div>

          <!-- 文件变化提示 -->
          <div v-if="showFileChangeAlert" class="settings-card warning-card">
            <div class="warning-content">
              <span class="warning-icon">!</span>
              <div class="warning-text">
                <p class="warning-title">检测到文件被外部修改</p>
                <p class="warning-desc">
                  <span v-if="fileChangeStatus?.config_changed">配置文件 </span>
                  <span v-if="fileChangeStatus?.phrases_changed"
                    >短语文件
                  </span>
                  <span v-if="fileChangeStatus?.shadow_changed"
                    >Shadow文件
                  </span>
                  <span v-if="fileChangeStatus?.userdict_changed"
                    >用户词库
                  </span>
                  已被修改
                </p>
              </div>
              <button
                class="btn btn-sm btn-primary"
                @click="handleReloadAllFiles"
              >
                重新加载
              </button>
              <button class="btn btn-sm" @click="showFileChangeAlert = false">
                忽略
              </button>
            </div>
          </div>

          <!-- 消息提示 -->
          <div v-if="dictMessage" :class="['dict-message', dictMessageType]">
            {{ dictMessage }}
          </div>

          <!-- 子标签页 -->
          <div class="dict-tabs">
            <button
              :class="['dict-tab', { active: dictSubTab === 'phrases' }]"
              @click="dictSubTab = 'phrases'"
            >
              用户短语 ({{ dictStats.phrase_count }})
            </button>
            <button
              :class="['dict-tab', { active: dictSubTab === 'userdict' }]"
              @click="dictSubTab = 'userdict'"
            >
              用户词库 ({{ dictStats.word_count }})
            </button>
            <button
              :class="['dict-tab', { active: dictSubTab === 'shadow' }]"
              @click="dictSubTab = 'shadow'"
            >
              候选调整 ({{ dictStats.shadow_count }})
            </button>
          </div>

          <!-- 非 Wails 环境提示 -->
          <div v-if="!isWailsEnv" class="settings-card">
            <div class="dict-note-center">
              <p>词库管理功能需要在桌面应用中使用</p>
              <p class="dict-note">
                请使用 <code>wails dev</code> 或编译后的应用
              </p>
            </div>
          </div>

          <!-- 用户短语 -->
          <div v-else-if="dictSubTab === 'phrases'" class="dict-content">
            <div class="dict-toolbar">
              <button
                class="btn btn-primary btn-sm"
                @click="showAddPhraseForm = true"
              >
                + 添加短语
              </button>
              <button
                class="btn btn-sm"
                @click="loadDictData"
                :disabled="dictLoading"
              >
                {{ dictLoading ? "加载中..." : "刷新" }}
              </button>
            </div>

            <!-- 添加短语表单 -->
            <div v-if="showAddPhraseForm" class="dict-form-card">
              <div class="form-row">
                <label>编码</label>
                <input
                  type="text"
                  v-model="newPhrase.code"
                  class="input"
                  placeholder="如: rq"
                />
              </div>
              <div class="form-row">
                <label>文本</label>
                <input
                  type="text"
                  v-model="newPhrase.text"
                  class="input"
                  placeholder="如: {{date}}"
                />
              </div>
              <div class="form-row">
                <label>权重</label>
                <input
                  type="number"
                  v-model.number="newPhrase.weight"
                  class="input input-sm"
                />
              </div>
              <div class="form-actions">
                <button class="btn btn-sm" @click="showAddPhraseForm = false">
                  取消
                </button>
                <button class="btn btn-primary btn-sm" @click="handleAddPhrase">
                  添加
                </button>
              </div>
            </div>

            <!-- 短语列表 -->
            <div class="dict-list" v-if="phrases.length > 0">
              <div
                class="dict-list-item"
                v-for="(item, idx) in phrases"
                :key="idx"
              >
                <div class="dict-item-main">
                  <span class="dict-item-code">{{ item.code }}</span>
                  <span class="dict-item-text">{{ item.text }}</span>
                  <span v-if="item.type" class="dict-item-tag">{{
                    item.type
                  }}</span>
                </div>
                <div class="dict-item-actions">
                  <button
                    class="btn-icon btn-delete"
                    @click="handleRemovePhrase(item)"
                    title="删除"
                  >
                    &times;
                  </button>
                </div>
              </div>
            </div>
            <div v-else class="dict-empty">暂无用户短语</div>
          </div>

          <!-- 用户词库 -->
          <div v-else-if="dictSubTab === 'userdict'" class="dict-content">
            <div class="dict-engine-switcher">
              <span class="dict-engine-label">词库类型：</span>
              <button
                :class="[
                  'dict-engine-btn',
                  { active: userDictEngine === 'wubi' },
                ]"
                @click="handleSwitchUserDictEngine('wubi')"
                :disabled="dictLoading"
              >
                五笔
              </button>
              <button
                :class="[
                  'dict-engine-btn',
                  { active: userDictEngine === 'pinyin' },
                ]"
                @click="handleSwitchUserDictEngine('pinyin')"
                :disabled="dictLoading"
              >
                拼音
              </button>
            </div>
            <div class="dict-toolbar">
              <button
                class="btn btn-primary btn-sm"
                @click="showAddWordForm = true"
              >
                + 添加词条
              </button>
              <button
                class="btn btn-sm"
                @click="loadDictData"
                :disabled="dictLoading"
              >
                {{ dictLoading ? "加载中..." : "刷新" }}
              </button>
            </div>

            <!-- 添加词条表单 -->
            <div v-if="showAddWordForm" class="dict-form-card">
              <div class="form-row">
                <label>编码</label>
                <input
                  type="text"
                  v-model="newWord.code"
                  class="input"
                  placeholder="如: nihao"
                />
              </div>
              <div class="form-row">
                <label>词条</label>
                <input
                  type="text"
                  v-model="newWord.text"
                  class="input"
                  placeholder="如: 你好"
                />
              </div>
              <div class="form-row">
                <label>权重</label>
                <input
                  type="number"
                  v-model.number="newWord.weight"
                  class="input input-sm"
                />
              </div>
              <div class="form-actions">
                <button class="btn btn-sm" @click="showAddWordForm = false">
                  取消
                </button>
                <button
                  class="btn btn-primary btn-sm"
                  @click="handleAddUserWord"
                >
                  添加
                </button>
              </div>
            </div>

            <!-- 词条列表 -->
            <div class="dict-list" v-if="userDict.length > 0">
              <div
                class="dict-list-item"
                v-for="(item, idx) in userDict"
                :key="idx"
              >
                <div class="dict-item-main">
                  <span class="dict-item-code">{{ item.code }}</span>
                  <span class="dict-item-text">{{ item.text }}</span>
                  <span class="dict-item-weight" v-if="item.weight">{{
                    item.weight
                  }}</span>
                </div>
                <div class="dict-item-actions">
                  <button
                    class="btn-icon btn-delete"
                    @click="handleRemoveUserWord(item)"
                    title="删除"
                  >
                    &times;
                  </button>
                </div>
              </div>
            </div>
            <div v-else class="dict-empty">暂无用户词条</div>
          </div>

          <!-- 候选调整 (Shadow) -->
          <div v-else-if="dictSubTab === 'shadow'" class="dict-content">
            <div class="dict-toolbar">
              <button
                class="btn btn-primary btn-sm"
                @click="showAddShadowForm = true"
              >
                + 添加规则
              </button>
              <button
                class="btn btn-sm"
                @click="loadDictData"
                :disabled="dictLoading"
              >
                {{ dictLoading ? "加载中..." : "刷新" }}
              </button>
            </div>

            <!-- 添加规则表单 -->
            <div v-if="showAddShadowForm" class="dict-form-card">
              <div class="form-row">
                <label>编码</label>
                <input
                  type="text"
                  v-model="newShadow.code"
                  class="input"
                  placeholder="如: zg"
                />
              </div>
              <div class="form-row">
                <label>词条</label>
                <input
                  type="text"
                  v-model="newShadow.word"
                  class="input"
                  placeholder="如: 中国"
                />
              </div>
              <div class="form-row">
                <label>操作</label>
                <select v-model="newShadow.action" class="select">
                  <option value="pin">置顶</option>
                  <option value="delete">删除</option>
                  <option value="adjust">调整权重</option>
                </select>
              </div>
              <div
                class="form-row"
                v-if="
                  newShadow.action === 'adjust' || newShadow.action === 'pin'
                "
              >
                <label>权重</label>
                <input
                  type="number"
                  v-model.number="newShadow.weight"
                  class="input input-sm"
                />
              </div>
              <div class="form-actions">
                <button class="btn btn-sm" @click="showAddShadowForm = false">
                  取消
                </button>
                <button
                  class="btn btn-primary btn-sm"
                  @click="handleAddShadowRule"
                >
                  添加
                </button>
              </div>
            </div>

            <!-- 规则列表 -->
            <div class="dict-list" v-if="shadowRules.length > 0">
              <div
                class="dict-list-item"
                v-for="(item, idx) in shadowRules"
                :key="idx"
              >
                <div class="dict-item-main">
                  <span class="dict-item-code">{{ item.code }}</span>
                  <span class="dict-item-text">{{ item.word }}</span>
                  <span class="dict-item-tag" :class="'tag-' + item.action">
                    {{ getShadowActionLabel(item.action) }}
                  </span>
                  <span class="dict-item-weight" v-if="item.weight">{{
                    item.weight
                  }}</span>
                </div>
                <div class="dict-item-actions">
                  <button
                    class="btn-icon btn-delete"
                    @click="handleRemoveShadowRule(item)"
                    title="删除"
                  >
                    &times;
                  </button>
                </div>
              </div>
            </div>
            <div v-else class="dict-empty">暂无调整规则</div>
          </div>
        </section>

        <!-- ==================== 按键设置 ==================== -->
        <section v-if="activeTab === 'hotkey'" class="section">
          <div class="section-header">
            <h2>按键设置</h2>
            <p class="section-desc">自定义快捷键和候选操作</p>
          </div>

          <!-- 冲突警告 -->
          <div
            v-if="hotkeyConflicts.length > 0"
            class="settings-card warning-card"
          >
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
                  <label
                    class="checkbox-item"
                    v-for="key in [
                      'lshift',
                      'rshift',
                      'lctrl',
                      'rctrl',
                      'capslock',
                    ]"
                    :key="key"
                  >
                    <input
                      type="checkbox"
                      :checked="formData.hotkeys.toggle_mode_keys.includes(key)"
                      @change="
                        toggleArrayValue(formData.hotkeys.toggle_mode_keys, key)
                      "
                    />
                    <span>{{ getKeyLabel(key) }}</span>
                  </label>
                </div>
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>切换时编码上屏</label>
                <p class="setting-hint">
                  中文切换为英文时，将已输入的编码直接上屏
                </p>
              </div>
              <div class="setting-control">
                <label class="switch">
                  <input
                    type="checkbox"
                    v-model="formData.hotkeys.commit_on_switch"
                  />
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
                <select
                  v-model="formData.hotkeys.toggle_full_width"
                  class="select"
                >
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
                  <label
                    class="checkbox-item"
                    v-for="group in [
                      { value: 'semicolon_quote', label: '; \' 键' },
                      { value: 'comma_period', label: ', . 键' },
                      { value: 'lrshift', label: 'L/R Shift' },
                      { value: 'lrctrl', label: 'L/R Ctrl' },
                    ]"
                    :key="group.value"
                  >
                    <input
                      type="checkbox"
                      :checked="
                        formData.input.select_key_groups.includes(group.value)
                      "
                      @change="
                        toggleArrayValue(
                          formData.input.select_key_groups,
                          group.value,
                        )
                      "
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
                  <label
                    class="checkbox-item"
                    v-for="pk in [
                      { value: 'pageupdown', label: 'PgUp/PgDn' },
                      { value: 'minus_equal', label: '- / =' },
                      { value: 'brackets', label: '[ / ]' },
                      { value: 'shift_tab', label: 'Shift+Tab / Tab' },
                    ]"
                    :key="pk.value"
                  >
                    <input
                      type="checkbox"
                      :checked="formData.input.page_keys.includes(pk.value)"
                      @change="
                        toggleArrayValue(formData.input.page_keys, pk.value)
                      "
                    />
                    <span>{{ pk.label }}</span>
                  </label>
                </div>
              </div>
            </div>
          </div>
        </section>

        <!-- ==================== 高级设置 ==================== -->
        <section v-if="activeTab === 'advanced'" class="section section-full">
          <div class="section-header">
            <h2>高级设置</h2>
            <p class="section-desc">故障排查、调试与测试工具</p>
          </div>

          <div class="advanced-tabs">
            <button
              class="advanced-tab"
              :class="{ active: advancedSubTab === 'advanced' }"
              @click="advancedSubTab = 'advanced'"
            >
              高级
            </button>
            <button
              class="advanced-tab"
              :class="{ active: advancedSubTab === 'test' }"
              @click="advancedSubTab = 'test'"
            >
              测试
            </button>
          </div>

          <template v-if="advancedSubTab === 'advanced'">
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
              <div class="setting-item">
                <div class="setting-info">
                  <label>日志文件位置</label>
                  <p class="setting-hint">{{ logPath }}</p>
                </div>
                <div class="setting-control">
                  <button class="btn btn-sm" @click="handleOpenLogFolder">
                    打开文件夹
                  </button>
                </div>
              </div>
            </div>
          </template>

          <template v-else>
            <div class="settings-card">
              <div class="card-title">码表测试</div>
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

              <div
                class="test-candidates-wrap"
                v-if="testCandidates.length > 0"
              >
                <div class="test-candidates">
                  <div
                    class="test-candidate"
                    v-for="(cand, idx) in testCandidates"
                    :key="idx"
                  >
                    <span class="cand-index">{{ idx + 1 }}.</span>
                    <span class="cand-text">{{ cand.text }}</span>
                    <span class="cand-code" v-if="cand.code">{{
                      cand.code
                    }}</span>
                    <span class="cand-common" v-if="cand.isCommon">通用</span>
                    <span class="cand-rare" v-else>生僻</span>
                  </div>
                </div>
              </div>
              <div class="test-empty" v-else-if="testInput && !testLoading">
                无匹配候选词
              </div>
            </div>
          </template>
        </section>

        <!-- ==================== 关于 ==================== -->
        <section v-if="activeTab === 'about'" class="section">
          <div class="section-header">
            <h2>关于</h2>
            <p class="section-desc">清风输入法 信息</p>
          </div>

          <div class="settings-card about-card" v-if="status">
            <div class="about-simple">
              <div class="about-icon-wrap">
                <img :src="appIconUrl" alt="清风输入法" />
              </div>
              <div class="about-title">
                <h3>{{ status.service.name }}</h3>
                <p>{{ status.service.version }}</p>
              </div>
              <div class="about-links-inline">
                <button
                  class="link-button modern-link"
                  @click="handleOpenExternalLink(repoUrl)"
                >
                  <span class="link-icon" aria-hidden="true">
                    <svg
                      viewBox="0 0 24 24"
                      width="18"
                      height="18"
                      fill="currentColor"
                    >
                      <path
                        d="M12 2C6.48 2 2 6.58 2 12.26c0 4.58 2.87 8.46 6.84 9.83.5.1.68-.22.68-.49 0-.24-.01-.87-.01-1.71-2.78.62-3.37-1.39-3.37-1.39-.45-1.2-1.1-1.52-1.1-1.52-.9-.64.07-.63.07-.63 1 .07 1.52 1.06 1.52 1.06.89 1.56 2.34 1.11 2.9.85.09-.67.35-1.11.63-1.37-2.22-.26-4.56-1.14-4.56-5.08 0-1.12.39-2.03 1.02-2.75-.1-.26-.44-1.3.1-2.71 0 0 .84-.27 2.75 1.03.8-.23 1.66-.35 2.51-.35.85 0 1.71.12 2.51.35 1.9-1.3 2.74-1.03 2.74-1.03.54 1.41.2 2.45.1 2.71.63.72 1.02 1.63 1.02 2.75 0 3.95-2.35 4.82-4.58 5.07.36.32.68.94.68 1.9 0 1.37-.01 2.47-.01 2.8 0 .27.18.6.69.49 3.97-1.37 6.83-5.25 6.83-9.83C22 6.58 17.52 2 12 2z"
                      />
                    </svg>
                  </span>
                  <div class="link-text">
                    <span class="link-title">GitHub 项目主页</span>
                    <span class="link-desc">{{ repoUrl }}</span>
                  </div>
                </button>
              </div>
            </div>
          </div>
        </section>
      </div>
    </main>
  </div>
</template>

<style>
* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

body {
  font-family:
    -apple-system, BlinkMacSystemFont, "Segoe UI", "Microsoft YaHei UI",
    sans-serif;
  background: #f0f2f5;
  color: #1f2937;
  line-height: 1.5;
  font-size: 14px;
}

.app {
  display: flex;
  min-height: 100vh;
  height: 100vh;
}

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
.logo-icon {
  font-size: 24px;
}
.logo-title {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.logo-text {
  font-size: 16px;
  font-weight: 600;
  color: #1f2937;
}
.logo-version {
  font-size: 11px;
  color: #9ca3af;
}
.status-dot-inline {
  width: 8px;
  height: 8px;
  border-radius: 999px;
  margin-left: auto;
  position: relative;
  top: 1px;
  background: #9ca3af;
}
.status-dot-inline.connected {
  background: #16a34a;
}
.status-dot-inline.disconnected {
  background: #dc2626;
}

.nav {
  flex: 1;
  padding: 12px;
  overflow-y: auto;
}
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
.nav-item:hover {
  background: #f3f4f6;
  color: #374151;
}
.nav-item.active {
  background: #eff6ff;
  color: #2563eb;
  font-weight: 500;
}
.nav-icon {
  font-size: 16px;
  width: 20px;
  text-align: center;
}

.sidebar-footer {
  padding: 16px;
  border-top: 1px solid #e5e7eb;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.sidebar-actions {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.sidebar-actions .btn {
  width: 100%;
  justify-content: center;
  padding: 6px 12px;
  font-size: 13px;
}
.sidebar-message .message {
  display: inline-block;
  width: 100%;
}

/* Main */
.main {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  min-width: 0;
}
.content {
  flex: 1;
  padding: 24px 32px;
  overflow-y: auto;
}

/* Section */
.section {
  max-width: none;
  width: 100%;
}
.section-full {
  max-width: none;
}
.section-header {
  margin-bottom: 20px;
}
.section-header h2 {
  font-size: 20px;
  font-weight: 600;
  color: #111827;
  margin-bottom: 4px;
  text-align: left;
}
.section-desc {
  font-size: 13px;
  color: #6b7280;
  text-align: left;
}

/* Settings Card */
.settings-card {
  background: #fff;
  border-radius: 12px;
  padding: 20px;
  margin-bottom: 16px;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.05);
}
.card-title {
  font-size: 14px;
  font-weight: 700;
  color: #111827;
  margin-bottom: 16px;
  text-align: left;
  padding: 4px 8px;
  border-left: 3px solid #2563eb;
  border-radius: 0;
  background: transparent;
}

/* Setting Item */
.setting-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 14px 0;
  border-bottom: 1px solid #f3f4f6;
  gap: 16px;
}
.setting-item.align-start {
  align-items: flex-start;
}
.setting-item:last-child {
  border-bottom: none;
  padding-bottom: 0;
}
.setting-item:first-child {
  padding-top: 0;
}
.setting-item.item-disabled {
  opacity: 0.5;
}
.setting-info {
  flex: 1;
  min-width: 0;
}
.setting-info label {
  font-size: 14px;
  font-weight: 500;
  color: #1f2937;
  display: block;
  text-align: left;
}
.setting-hint {
  font-size: 12px;
  color: #9ca3af;
  margin-top: 2px;
  text-align: left;
}
.setting-control {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 10px;
}

/* Form Controls */
.select {
  padding: 8px 32px 8px 12px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  font-size: 14px;
  background: #fff
    url("data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e")
    no-repeat right 8px center;
  background-size: 16px;
  cursor: pointer;
  color: #1f2937;
  min-width: 160px;
  -webkit-appearance: none;
  appearance: none;
}
.select:focus {
  outline: none;
  border-color: #2563eb;
  box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.1);
}
.select-sm {
  padding: 6px 28px 6px 10px;
  font-size: 13px;
  min-width: 100px;
}

.input {
  padding: 8px 12px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  font-size: 14px;
  width: 240px;
  color: #1f2937;
}
.input:focus {
  outline: none;
  border-color: #2563eb;
  box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.1);
}
.input::placeholder {
  color: #9ca3af;
}
.input-sm {
  padding: 6px 10px;
  font-size: 13px;
  width: 160px;
}

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
.segmented-control button:hover:not(:disabled) {
  color: #374151;
}
.segmented-control button:disabled {
  cursor: not-allowed;
}
.segmented-control button.active {
  background: #fff;
  color: #2563eb;
  font-weight: 500;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.05);
}

/* Switch */
.switch {
  position: relative;
  display: inline-block;
  width: 44px;
  height: 24px;
}
.switch input {
  opacity: 0;
  width: 0;
  height: 0;
}
.slider {
  position: absolute;
  cursor: pointer;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background-color: #d1d5db;
  transition: 0.2s;
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
  transition: 0.2s;
  border-radius: 50%;
}
input:checked + .slider {
  background-color: #2563eb;
}
input:checked + .slider:before {
  transform: translateX(20px);
}

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
.range-control {
  flex-direction: column;
  align-items: flex-end;
  gap: 4px;
}
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
.range-value {
  font-size: 13px;
  font-weight: 500;
  color: #374151;
}

/* Engine Cards */
.engine-cards {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
}
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
.engine-card:hover:not(.disabled) {
  border-color: #93c5fd;
}
.engine-card.active {
  border-color: #2563eb;
  background: #eff6ff;
}
.engine-card.disabled {
  cursor: not-allowed;
  opacity: 0.6;
}
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
.engine-icon-disabled {
  background: #9ca3af;
}
.engine-info {
  flex: 1;
  min-width: 0;
  text-align: left;
}
.engine-name {
  font-weight: 500;
  color: #1f2937;
}
.engine-desc {
  font-size: 12px;
  color: #6b7280;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.engine-check {
  color: #2563eb;
  font-size: 18px;
  flex-shrink: 0;
}

/* Warning Card */
.warning-card {
  background: #fef3c7;
  border: 1px solid #f59e0b;
}
.warning-content {
  display: flex;
  align-items: flex-start;
  gap: 12px;
}
.warning-icon {
  font-size: 20px;
  flex-shrink: 0;
}
.warning-title {
  font-weight: 500;
  color: #92400e;
  margin-bottom: 4px;
}
.warning-list {
  font-size: 13px;
  color: #b45309;
  margin: 0;
  padding-left: 16px;
}

/* Dictionary */
.section-wide {
  max-width: none;
}
.dict-info {
  margin-bottom: 16px;
}
.dict-item {
  display: flex;
  justify-content: space-between;
  padding: 10px 0;
  border-bottom: 1px solid #f3f4f6;
}
.dict-item:last-child {
  border-bottom: none;
}
.dict-label {
  color: #6b7280;
}
.dict-value {
  font-family: monospace;
  color: #374151;
  font-size: 13px;
}
.dict-actions {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}
.dict-note {
  font-size: 12px;
  color: #9ca3af;
  font-style: italic;
}
.dict-note-center {
  text-align: center;
  padding: 32px;
  color: #6b7280;
}
.dict-note-center code {
  background: #f3f4f6;
  padding: 2px 6px;
  border-radius: 4px;
}
.btn-danger {
  color: #dc2626;
  border-color: #fecaca;
}
.btn-danger:hover {
  background: #fee2e2;
}

/* Dictionary Stats Bar */
.dict-stats-bar {
  display: flex;
  gap: 12px;
  margin-bottom: 16px;
  flex-wrap: wrap;
}
.stat-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: #f3f4f6;
  border-radius: 20px;
  font-size: 13px;
}
.stat-chip-label {
  color: #6b7280;
}
.stat-chip-value {
  font-weight: 500;
  color: #1f2937;
}
.stat-running {
  background: #dcfce7;
}
.stat-running .stat-chip-value {
  color: #166534;
}
.stat-stopped {
  background: #fee2e2;
}
.stat-stopped .stat-chip-value {
  color: #991b1b;
}

/* Dictionary Message */
.dict-message {
  padding: 10px 16px;
  border-radius: 8px;
  margin-bottom: 16px;
  font-size: 14px;
}
.dict-message.success {
  background: #dcfce7;
  color: #166534;
}
.dict-message.error {
  background: #fee2e2;
  color: #991b1b;
}

/* Dictionary Tabs */
.dict-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 16px;
  border-bottom: 1px solid #e5e7eb;
  padding-bottom: 0;
}
.dict-tab {
  padding: 10px 20px;
  border: none;
  background: none;
  cursor: pointer;
  font-size: 14px;
  color: #6b7280;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  transition: all 0.15s;
}
.dict-tab:hover {
  color: #374151;
}
.dict-tab.active {
  color: #2563eb;
  border-bottom-color: #2563eb;
  font-weight: 500;
}

/* Dictionary Content */
.dict-content {
  min-height: 300px;
}
.dict-engine-switcher {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 12px;
}
.dict-engine-label {
  font-size: 13px;
  color: #6b7280;
}
.dict-engine-btn {
  padding: 4px 14px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  background: #fff;
  cursor: pointer;
  font-size: 13px;
  color: #374151;
  transition: all 0.15s;
}
.dict-engine-btn:hover:not(:disabled) {
  border-color: #93c5fd;
  color: #2563eb;
}
.dict-engine-btn.active {
  background: #2563eb;
  color: #fff;
  border-color: #2563eb;
}
.dict-engine-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.dict-toolbar {
  display: flex;
  gap: 12px;
  margin-bottom: 16px;
}
.dict-empty {
  text-align: center;
  padding: 48px;
  color: #9ca3af;
  background: #f9fafb;
  border-radius: 8px;
}

/* Dictionary Form */
.dict-form-card {
  background: #f9fafb;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  padding: 16px;
  margin-bottom: 16px;
}
.form-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}
.form-row:last-child {
  margin-bottom: 0;
}
.form-row label {
  width: 60px;
  font-size: 13px;
  color: #6b7280;
  flex-shrink: 0;
}
.form-row .input {
  flex: 1;
  max-width: 300px;
}
.form-row .input-sm {
  max-width: 120px;
}
.form-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 16px;
  padding-top: 12px;
  border-top: 1px solid #e5e7eb;
}

/* Dictionary List */
.dict-list {
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}
.dict-list-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  border-bottom: 1px solid #f3f4f6;
  transition: background 0.15s;
}
.dict-list-item:last-child {
  border-bottom: none;
}
.dict-list-item:hover {
  background: #f9fafb;
}
.dict-item-main {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
}
.dict-item-code {
  font-family: "Consolas", "Monaco", monospace;
  font-size: 13px;
  color: #6b7280;
  background: #f3f4f6;
  padding: 2px 8px;
  border-radius: 4px;
  min-width: 60px;
}
.dict-item-text {
  font-size: 14px;
  color: #1f2937;
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dict-item-tag {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 4px;
  background: #e0e7ff;
  color: #3730a3;
}
.dict-item-tag.tag-pin {
  background: #dcfce7;
  color: #166534;
}
.dict-item-tag.tag-delete {
  background: #fee2e2;
  color: #991b1b;
}
.dict-item-tag.tag-adjust {
  background: #fef3c7;
  color: #92400e;
}
.dict-item-weight {
  font-size: 12px;
  color: #9ca3af;
}
.dict-item-actions {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
}
.btn-icon {
  width: 28px;
  height: 28px;
  border: none;
  background: transparent;
  cursor: pointer;
  border-radius: 4px;
  font-size: 18px;
  color: #9ca3af;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.15s;
}
.btn-icon:hover {
  background: #f3f4f6;
  color: #6b7280;
}
.btn-icon.btn-delete:hover {
  background: #fee2e2;
  color: #dc2626;
}

/* Warning Card Enhancement */
.warning-card .warning-content {
  display: flex;
  align-items: center;
  gap: 12px;
}
.warning-card .warning-text {
  flex: 1;
}
.warning-card .warning-desc {
  font-size: 12px;
  color: #b45309;
  margin-top: 2px;
}

.message {
  font-size: 13px;
  padding: 6px 12px;
  border-radius: 6px;
}
.message.success {
  background: #dcfce7;
  color: #166534;
}
.message.error {
  background: #fee2e2;
  color: #991b1b;
}

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
.btn:hover:not(:disabled) {
  background: #f9fafb;
  border-color: #9ca3af;
}
.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.btn-primary {
  background: #2563eb;
  color: #fff;
  border-color: #2563eb;
}
.btn-primary:hover:not(:disabled) {
  background: #1d4ed8;
}
.btn-sm {
  padding: 5px 10px;
  font-size: 12px;
}

/* Test Page */
.test-options {
  display: flex;
  gap: 16px;
  margin-bottom: 16px;
  flex-wrap: wrap;
}
.test-option {
  display: flex;
  align-items: center;
  gap: 8px;
}
.test-option label {
  font-size: 13px;
  color: #6b7280;
}

.test-input-wrap {
  position: relative;
  margin-bottom: 16px;
}
.test-input {
  width: 100%;
  padding: 12px 16px;
  border: 2px solid #e5e7eb;
  border-radius: 8px;
  font-size: 16px;
  color: #1f2937;
}
.test-input:focus {
  outline: none;
  border-color: #2563eb;
}
.test-loading {
  position: absolute;
  right: 16px;
  top: 50%;
  transform: translateY(-50%);
  font-size: 12px;
  color: #9ca3af;
}

.test-candidates-wrap {
  max-height: 400px;
  overflow-y: auto;
}
.test-candidates {
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}
.test-candidate {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  border-bottom: 1px solid #f3f4f6;
}
.test-candidate:last-child {
  border-bottom: none;
}
.test-candidate:hover {
  background: #f9fafb;
}
.cand-index {
  color: #9ca3af;
  font-size: 13px;
  min-width: 24px;
  flex-shrink: 0;
}
.cand-text {
  font-size: 16px;
  color: #1f2937;
  word-break: break-all;
}
.cand-code {
  font-size: 12px;
  color: #6b7280;
  background: #f3f4f6;
  padding: 2px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}
.cand-common {
  font-size: 11px;
  color: #059669;
  background: #d1fae5;
  padding: 2px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}
.cand-rare {
  font-size: 11px;
  color: #dc2626;
  background: #fee2e2;
  padding: 2px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}
.test-empty {
  text-align: center;
  padding: 32px;
  color: #9ca3af;
}

/* Log Page */
.log-card {
  display: flex;
  flex-direction: column;
  min-height: 300px;
}
.log-count {
  font-weight: normal;
  font-size: 12px;
  color: #9ca3af;
  margin-left: 8px;
  text-transform: none;
}
.log-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid #e5e7eb;
  flex-wrap: wrap;
}
.checkbox-inline {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: #6b7280;
  cursor: pointer;
  white-space: nowrap;
}
.checkbox-inline input {
  cursor: pointer;
}
.log-content {
  flex: 1;
  overflow-y: auto;
  font-family: "Consolas", "Monaco", "Courier New", monospace;
  font-size: 12px;
  padding: 12px 0;
  max-height: 400px;
}
.log-line {
  display: flex;
  gap: 12px;
  padding: 4px 0;
  text-align: left;
  align-items: flex-start;
}
.log-time {
  color: #9ca3af;
  flex-shrink: 0;
}
.log-level {
  font-weight: 600;
  min-width: 50px;
  flex-shrink: 0;
}
.log-msg {
  color: #374151;
  word-break: break-all;
  flex: 1;
}
.log-debug .log-level {
  color: #6b7280;
}
.log-info .log-level {
  color: #2563eb;
}
.log-warn .log-level {
  color: #d97706;
}
.log-error .log-level {
  color: #dc2626;
}
.log-empty {
  color: #9ca3af;
  text-align: center;
  padding: 32px;
}

/* Advanced Tabs */
.advanced-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 16px;
  border-bottom: 1px solid #e5e7eb;
}
.advanced-tab {
  padding: 10px 18px;
  border: none;
  background: none;
  color: #6b7280;
  cursor: pointer;
  border-bottom: 2px solid transparent;
  transition: all 0.15s;
  font-size: 14px;
}
.advanced-tab:hover {
  color: #374151;
}
.advanced-tab.active {
  color: #2563eb;
  border-bottom-color: #2563eb;
  font-weight: 600;
}

/* About Page */
.about-card {
  padding: 24px;
}
.about-simple {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  text-align: center;
}
.about-icon-wrap {
  width: 96px;
  height: 96px;
  background: #f3f4f6;
  border-radius: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.04);
}
.about-icon-wrap img {
  width: 72px;
  height: 72px;
  object-fit: contain;
}
.about-title h3 {
  font-size: 22px;
  font-weight: 700;
  margin: 0;
  color: #111827;
}
.about-title p {
  color: #9ca3af;
  margin: 6px 0 0;
  font-size: 13px;
}
.about-links-inline {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 8px;
  width: 100%;
  max-width: 420px;
}
.link-button {
  border: 1px solid #e5e7eb;
  background: #fff;
  padding: 12px 14px;
  border-radius: 14px;
  color: #111827;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 12px;
  transition:
    border-color 0.15s,
    box-shadow 0.15s,
    transform 0.15s;
}
.link-button:hover {
  border-color: #cbd5f5;
  box-shadow: 0 8px 20px rgba(37, 99, 235, 0.08);
  transform: translateY(-1px);
}
.modern-link .link-icon {
  width: 36px;
  height: 36px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 12px;
  background: #111827;
  color: #fff;
  flex-shrink: 0;
}
.modern-link .link-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.modern-link .link-title {
  font-weight: 600;
  font-size: 14px;
  color: #111827;
}
.modern-link .link-desc {
  color: #6b7280;
  font-size: 12px;
}

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
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

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
.error-icon {
  font-size: 48px;
}
.error-panel p {
  color: #6b7280;
  max-width: 300px;
}

/* Theme Dropdown */
.theme-dropdown {
  position: relative;
}
.theme-dropdown,
.input-mode-dropdown {
  min-width: 320px;
}
.input-mode-dropdown {
  min-width: 360px;
}
.theme-select {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 14px;
  border: 1px solid #e5e7eb;
  border-radius: 10px;
  background: #fff;
  cursor: pointer;
  text-align: left;
  transition:
    border-color 0.15s,
    box-shadow 0.15s;
}
.select-strong {
  border-width: 2px;
}
.theme-select:hover {
  border-color: #cbd5f5;
}
.theme-select:focus {
  outline: none;
  box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.12);
  border-color: #2563eb;
}
.theme-select-main {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.theme-select-title {
  font-weight: 600;
  color: #111827;
}
.theme-select-sub {
  font-size: 12px;
  color: #6b7280;
  display: flex;
  gap: 8px;
  align-items: center;
}
.theme-select-version {
  font-weight: 600;
  color: #2563eb;
}
.theme-select-arrow {
  color: #6b7280;
  font-size: 14px;
}

.theme-options {
  position: absolute;
  top: calc(100% + 8px);
  left: 0;
  right: 0;
  z-index: 10;
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 10px;
  box-shadow: 0 10px 30px rgba(15, 23, 42, 0.08);
  max-height: 320px;
  overflow-y: auto;
  padding: 8px;
  min-width: 320px;
}
.theme-option {
  width: 100%;
  text-align: left;
  border: 1px solid transparent;
  background: #fff;
  padding: 10px 12px;
  border-radius: 8px;
  cursor: pointer;
  transition:
    background 0.15s,
    border-color 0.15s;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.theme-option:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}
.theme-option:hover {
  background: #f9fafb;
}
.theme-option.selected {
  border-color: #93c5fd;
  background: #eff6ff;
}
.theme-option-title {
  display: flex;
  align-items: center;
  gap: 8px;
}
.theme-option-name {
  font-weight: 600;
  color: #111827;
}
.theme-option-sub {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: #6b7280;
}
.theme-option-version {
  font-weight: 600;
  color: #2563eb;
}
.theme-option-empty {
  padding: 12px;
  color: #9ca3af;
  font-size: 12px;
}
.theme-badge {
  font-size: 10px;
  padding: 2px 6px;
  border-radius: 4px;
  font-weight: 600;
}
.theme-badge.builtin {
  background: #e5e7eb;
  color: #6b7280;
}
.theme-badge.active {
  background: #dcfce7;
  color: #166534;
}

.theme-preview {
  background: #f9fafb;
  border-radius: 8px;
  padding: 16px;
}
.theme-preview.preview-rows {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.preview-row {
  display: flex;
  align-items: center;
  gap: 12px;
}
.preview-row-label {
  font-size: 12px;
  color: #6b7280;
  width: 80px;
  flex-shrink: 0;
  text-align: left;
}

.preview-candidate-window {
  display: inline-flex;
  gap: 12px;
  padding: 10px 14px;
  border: 1px solid #ccc;
  border-radius: 6px;
  background: #fff;
}

.preview-candidate-item {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  border-radius: 4px;
}

.preview-hover {
  background: #e6f0ff;
}

.preview-index {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  font-size: 11px;
  font-weight: 500;
  border-radius: 3px;
}

.preview-text {
  font-size: 14px;
}

.preview-toolbar {
  display: inline-flex;
  gap: 6px;
  padding: 6px 10px;
  border: 1px solid #ccc;
  border-radius: 6px;
  background: #fff;
}

.preview-toolbar-item {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  font-size: 12px;
  border-radius: 4px;
  color: #fff;
}

/* Responsive */
@media (max-width: 768px) {
  .content {
    padding: 16px;
  }
  .theme-preview {
    min-width: auto;
  }
}
</style>
