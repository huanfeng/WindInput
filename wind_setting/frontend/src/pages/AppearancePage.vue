<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { ChevronDown, Trash2 } from "lucide-vue-next";
import type { Config } from "../api/settings";
import type {
  ThemeInfo,
  ThemePreview,
  SystemFontInfo,
  ThemeServerStatus,
} from "../api/wails";
import {
  startThemeServer,
  stopThemeServer,
  getThemeServerStatus,
  deleteTheme,
  openThemesFolder,
  openExternalURL,
} from "../api/wails";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import SchemaRenderer from "@/components/SchemaRenderer.vue";
import ThemeImportModal from "@/components/ThemeImportModal.vue";
import {
  themeExtraSchema,
  candidateWindowSchema,
  statusIndicatorSchema,
  candidateTooltipSchema,
  toolbarSchema,
  indicatorSchema,
} from "@/schemas/appearance.schema";
import type { PageSchema } from "@/schemas/types";
import { useToast } from "../composables/useToast";

const props = defineProps<{
  formData: Config;
  isWailsEnv: boolean;
  // macOS 无悬浮可拖动工具栏（Toolbar 命令在 darwin 被重定向为菜单栏指示器），隐藏工具栏卡片
  isMac?: boolean;
  availableThemes: ThemeInfo[];
  themePreview: ThemePreview | null;
  systemFonts: SystemFontInfo[];
}>();

const emit = defineEmits<{
  themeSelect: [themeName: string];
  themeStyleChange: [themeStyle: string];
  themeImported: [themeName: string];
  themeDeleted: [themeName: string];
}>();

const { toast } = useToast();

const themeImportOpen = ref(false);

const themeServerRunning = ref(false);
const themeServerURL = ref("");
const themeServerError = ref("");
const themeServerLoading = ref(false);

function onThemeImported(themeName: string) {
  themeImportOpen.value = false;
  emit("themeImported", themeName);
  toast(`主题「${themeName}」导入成功`);
}

const themeSelectOpen = ref(false);
const themeDropdownRef = ref<HTMLElement | null>(null);

const themeOptions = computed(() => {
  return props.availableThemes.map((theme) => ({
    name: theme.name,
    label: theme.display_name || theme.name,
    description: theme.author ? `作者 ${theme.author}` : "暂无描述",
    version: theme.version || "",
    isActive: theme.is_active,
    isBuiltin: theme.is_builtin,
  }));
});

// 删除主题
const deleteConfirmOpen = ref(false);
const deletingThemeName = ref("");
const deleteLoading = ref(false);
const deleteError = ref("");

const deletingThemeLabel = computed(
  () =>
    themeOptions.value.find((t) => t.name === deletingThemeName.value)?.label ||
    deletingThemeName.value,
);

function askDeleteTheme(name: string, event: MouseEvent) {
  event.stopPropagation();
  deletingThemeName.value = name;
  deleteError.value = "";
  deleteConfirmOpen.value = true;
}

async function confirmDeleteTheme() {
  deleteLoading.value = true;
  deleteError.value = "";
  try {
    await deleteTheme(deletingThemeName.value);
    deleteConfirmOpen.value = false;
    emit("themeDeleted", deletingThemeName.value);
  } catch (e) {
    deleteError.value = String(e);
  } finally {
    deleteLoading.value = false;
  }
}

const currentThemeOption = computed(() => {
  return themeOptions.value.find(
    (option) => option.name === props.formData.ui.theme,
  );
});

const systemFontOptions = computed(() => {
  return props.systemFonts.map((font) => ({
    value: font.family,
    label: font.display_name || font.family,
  }));
});

// 状态提示 schema：macOS 上气泡始终锚定光标，position_mode 无效 → 移除「位置模式」项；
// 水平/垂直偏移仍生效，放开对 position_mode 的依赖，使其在启用时始终可调。
const statusSchema = computed<PageSchema>(() => {
  if (!props.isMac) return statusIndicatorSchema;
  return statusIndicatorSchema
    .filter(
      (item) =>
        !("key" in item && item.key === "ui.status_indicator.position_mode"),
    )
    .map((item) => {
      if (
        "key" in item &&
        (item.key === "ui.status_indicator.offset_x" ||
          item.key === "ui.status_indicator.offset_y")
      ) {
        return {
          ...item,
          dependsOn: (cfg: Config) => cfg.ui.status_indicator.enabled,
        };
      }
      return item;
    });
});

// resolveIndexLabel 根据当前主题的 index_labels 模板返回第 slotIdx (0..9) 个候选项的序号字符串
// 优先级：IndexLabels (斜杠分隔 / 10 字符) > 默认 1-9,0
function resolveIndexLabel(slotIdx: number): string {
  const labels = props.themePreview?.style?.index_labels ?? "";
  if (labels) {
    if (labels.includes("/")) {
      const parts = labels.split("/");
      if (slotIdx >= 0 && slotIdx < parts.length) return parts[slotIdx];
    } else {
      const chars = [...labels];
      if (slotIdx >= 0 && slotIdx < chars.length) return chars[slotIdx];
    }
  }
  // 默认数字
  return slotIdx === 9 ? "0" : String(slotIdx + 1);
}

// 命令直通车标注模式: 把 cmdbar_candidate_prefix 单字段映射成 3 种模式。
// undefined / "⚡" → default, "" → none, 其他 → custom。
type CmdbarPrefixMode = "default" | "none" | "custom";
const cmdbarPrefixMode = computed<CmdbarPrefixMode>(() => {
  const v = props.formData.ui.cmdbar_candidate_prefix;
  if (v == null || v === "⚡") return "default";
  if (v === "") return "none";
  return "custom";
});
function setCmdbarPrefixMode(mode: CmdbarPrefixMode) {
  if (mode === "default") {
    props.formData.ui.cmdbar_candidate_prefix = "⚡";
  } else if (mode === "none") {
    props.formData.ui.cmdbar_candidate_prefix = "";
  } else {
    // custom: 已有自定义值直接保留 + 打开弹框给用户改;
    // 没有自定义值时先记住旧值, 打开弹框等用户输入, 取消或空输入回退到旧值。
    openCmdbarPrefixDialog();
  }
}

// 自定义符号编辑弹框: 用临时草稿避免在弹框输入时直接 mutate 主表单引发布局抖动。
const cmdbarPrefixDialogOpen = ref(false);
const cmdbarPrefixDraft = ref("");
// 打开弹框前的原值, 用于取消或空提交时回退
const cmdbarPrefixFallback = ref<string | null | undefined>(undefined);
function openCmdbarPrefixDialog() {
  const cur = props.formData.ui.cmdbar_candidate_prefix;
  cmdbarPrefixFallback.value = cur;
  // 已经是自定义符号 (非空且非默认 ⚡) 时, 用现值作为草稿; 否则草稿留空
  cmdbarPrefixDraft.value = cur && cur !== "⚡" ? cur : "";
  cmdbarPrefixDialogOpen.value = true;
}
function confirmCmdbarPrefixDialog() {
  const v = cmdbarPrefixDraft.value.trim();
  if (v === "") {
    cancelCmdbarPrefixDialog();
    return;
  }
  props.formData.ui.cmdbar_candidate_prefix = v;
  cmdbarPrefixDialogOpen.value = false;
}
function cancelCmdbarPrefixDialog() {
  // 把字段恢复到打开弹框前的值, 让 cmdbarPrefixMode 计算属性切回正确的下拉项
  props.formData.ui.cmdbar_candidate_prefix = cmdbarPrefixFallback.value;
  cmdbarPrefixDialogOpen.value = false;
}

function onThemeSelect(themeName: string) {
  props.formData.ui.theme = themeName;
  emit("themeSelect", themeName);
  themeSelectOpen.value = false;
}

watch(
  () => props.formData.ui.theme_style,
  (val) => emit("themeStyleChange", val),
);

function handleDocumentClick(event: MouseEvent) {
  const target = event.target as Node;
  if (themeDropdownRef.value && !themeDropdownRef.value.contains(target)) {
    themeSelectOpen.value = false;
  }
}

onMounted(async () => {
  document.addEventListener("click", handleDocumentClick);
  if (props.isWailsEnv) {
    try {
      const status = await getThemeServerStatus();
      themeServerRunning.value = status.running;
      themeServerURL.value = status.url;
      // 配置了自动开启且服务尚未运行时，自动启动
      if (props.formData.ui.theme_editor_auto_start && !status.running) {
        const started = await startThemeServer();
        themeServerRunning.value = true;
        themeServerURL.value = started.url;
      }
    } catch {
      // 后端未就绪时静默忽略，保持默认关闭状态
    }
  }
});

onUnmounted(() => {
  document.removeEventListener("click", handleDocumentClick);
});

// Web 编辑器连接模式：off=关闭, once=本次开启, auto=打开设置时自动开启
type ThemeEditorMode = "off" | "once" | "auto";

const themeEditorMode = computed<ThemeEditorMode>(() => {
  if (props.formData.ui.theme_editor_auto_start) return "auto";
  if (themeServerRunning.value) return "once";
  return "off";
});

async function setThemeEditorMode(mode: ThemeEditorMode) {
  themeServerError.value = "";
  themeServerLoading.value = true;
  try {
    if (mode === "off") {
      if (themeServerRunning.value) {
        await stopThemeServer();
        themeServerRunning.value = false;
        themeServerURL.value = "";
      }
      props.formData.ui.theme_editor_auto_start = false;
    } else if (mode === "once") {
      props.formData.ui.theme_editor_auto_start = false;
      if (!themeServerRunning.value) {
        const status = await startThemeServer();
        themeServerRunning.value = true;
        themeServerURL.value = status.url;
      }
    } else if (mode === "auto") {
      props.formData.ui.theme_editor_auto_start = true;
      if (!themeServerRunning.value) {
        const status = await startThemeServer();
        themeServerRunning.value = true;
        themeServerURL.value = status.url;
      }
    }
  } catch (e) {
    themeServerError.value = String(e);
  } finally {
    themeServerLoading.value = false;
  }
}

async function openThemeEditor() {
  await openExternalURL("https://theme.windinput.com");
}

async function openThemeMarket() {
  await openExternalURL("https://market.windinput.com");
}

async function copyServerURL() {
  if (!themeServerURL.value) return;
  try {
    await navigator.clipboard.writeText(themeServerURL.value);
  } catch {
    // Wails webview 中 clipboard API 不可用时静默忽略
  }
}
</script>

<template>
  <section class="section">
    <div class="section-header">
      <h2>外观设置</h2>
      <p class="section-desc">主题、候选窗与状态显示</p>
    </div>

    <!-- 主题选择 -->
    <div class="settings-card" v-if="isWailsEnv">
      <div class="card-title card-title-row">
        <span>主题</span>
        <div class="flex gap-2">
          <Button variant="outline" size="sm" @click="openThemesFolder">
            打开目录
          </Button>
          <Button variant="outline" size="sm" @click="themeImportOpen = true">
            导入主题
          </Button>
        </div>
      </div>
      <div class="setting-item align-start" data-search-anchor="ui.theme">
        <div class="setting-info">
          <label>主题选择</label>
          <p class="setting-hint">候选窗与工具栏的主题样式</p>
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
                    >v{{ currentThemeOption?.version }}</span
                  >
                </div>
              </div>
              <ChevronDown
                class="h-4 w-4 text-muted-foreground flex-shrink-0"
              />
            </button>
            <div v-if="themeSelectOpen" class="theme-options">
              <div
                v-for="theme in themeOptions"
                :key="theme.name"
                class="theme-option-row"
              >
                <button
                  type="button"
                  class="theme-option"
                  :class="{ selected: formData.ui.theme === theme.name }"
                  @click="onThemeSelect(theme.name)"
                >
                  <div class="theme-option-title">
                    <span class="theme-option-name">{{ theme.label }}</span>
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
                <button
                  v-if="!theme.isBuiltin"
                  type="button"
                  class="theme-option-delete-btn"
                  title="删除主题"
                  @click="askDeleteTheme(theme.name, $event)"
                >
                  <Trash2 class="h-3.5 w-3.5" />
                </button>
              </div>
              <div v-if="themeOptions.length === 0" class="theme-option-empty">
                暂无主题
              </div>
            </div>
          </div>
        </div>
      </div>

      <SchemaRenderer
        :schema="themeExtraSchema"
        :form-data="formData"
        mode="bare"
      />

      <div class="setting-item align-start" v-if="themePreview">
        <div class="setting-info">
          <label class="inline-flex items-center gap-1">
            主题预览
            <span class="hint-tip" data-tip="预览效果可能和实际有所差异"
              >?</span
            >
          </label>
          <p class="setting-hint">候选窗与工具栏的实时预览</p>
        </div>
        <div class="setting-control">
          <div
            class="theme-preview"
            :style="{
              background: themePreview.is_dark?.active ? '#1a1a1a' : '#f0f0f0',
            }"
          >
            <div class="preview-layout">
              <!-- 候选窗口 -->
              <div class="preview-block">
                <div class="preview-section-label">候选窗口</div>
                <div
                  class="preview-candidate-window"
                  :style="{
                    backgroundColor:
                      themePreview.candidate_window?.background_color,
                    borderColor: themePreview.candidate_window?.border_color,
                    boxShadow: themePreview.candidate_window?.shadow_color
                      ? '0 3px 8px ' +
                        themePreview.candidate_window.shadow_color
                      : '0 3px 8px rgba(0,0,0,0.06)',
                  }"
                >
                  <!-- 输入行（嵌入编码模式下隐藏） -->
                  <div
                    v-if="!formData.ui.inline_preedit"
                    class="preview-input-bar"
                    :style="{
                      backgroundColor:
                        themePreview.candidate_window?.input_bg_color,
                    }"
                  >
                    <span
                      :style="{
                        color: themePreview.candidate_window?.input_text_color,
                      }"
                      >zhong'wen</span
                    >
                  </div>
                  <!-- 候选项 -->
                  <div class="preview-candidates">
                    <div
                      v-for="(item, idx) in [
                        { n: resolveIndexLabel(0), text: '中文', hover: true },
                        {
                          n: resolveIndexLabel(1),
                          text: '清风',
                          comment: 'igmq',
                        },
                        { n: resolveIndexLabel(2), text: '输入' },
                      ]"
                      :key="idx"
                      class="preview-candidate-item"
                      :style="{
                        backgroundColor: item.hover
                          ? themePreview.candidate_window?.hover_bg_color
                          : undefined,
                      }"
                    >
                      <!-- accent bar（微软风格：仅高亮项显示） -->
                      <div
                        v-if="
                          themePreview.style?.accent_bar_color && item.hover
                        "
                        class="preview-item-accent"
                        :style="{
                          backgroundColor: themePreview.style.accent_bar_color,
                        }"
                      ></div>
                      <span
                        class="preview-index"
                        :class="{
                          'preview-index-circle':
                            themePreview.style?.index_style !== 'text',
                          'preview-index-text':
                            themePreview.style?.index_style === 'text',
                        }"
                        :style="
                          themePreview.style?.index_style === 'text'
                            ? {
                                color:
                                  themePreview.candidate_window?.index_color,
                              }
                            : {
                                backgroundColor:
                                  themePreview.candidate_window?.index_bg_color,
                                color:
                                  themePreview.candidate_window?.index_color,
                              }
                        "
                        >{{ item.n }}</span
                      >
                      <span
                        class="preview-text"
                        :style="{
                          color: themePreview.candidate_window?.text_color,
                        }"
                        >{{ item.text }}</span
                      >
                      <span
                        v-if="item.comment"
                        class="preview-comment"
                        :style="{
                          color: themePreview.candidate_window?.comment_color,
                        }"
                        >{{ item.comment }}</span
                      >
                    </div>
                  </div>
                </div>
              </div>

              <!-- 工具栏 -->
              <div class="preview-block">
                <div class="preview-section-label">工具栏</div>
                <div
                  class="preview-toolbar"
                  :style="{
                    backgroundColor: themePreview.toolbar?.background_color,
                    borderColor: themePreview.toolbar?.border_color,
                  }"
                >
                  <span
                    class="preview-toolbar-grip"
                    :style="{
                      color: themePreview.toolbar?.grip_color || '#c0c0c0',
                    }"
                    >⠿</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor:
                        themePreview.toolbar?.mode_chinese_bg_color,
                      color: themePreview.toolbar?.mode_text_color || '#fff',
                    }"
                    >中</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor:
                        themePreview.toolbar?.full_width_off_bg_color,
                      color:
                        themePreview.toolbar?.full_width_off_color || '#666',
                    }"
                    >半</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor:
                        themePreview.toolbar?.punct_english_bg_color,
                      color:
                        themePreview.toolbar?.punct_english_color || '#666',
                    }"
                    >。</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor: themePreview.toolbar?.settings_bg_color,
                      color:
                        themePreview.toolbar?.settings_icon_color || '#666',
                    }"
                    >⚙</span
                  >
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 主题工具入口 -->
      <div
        v-if="isWailsEnv"
        class="setting-item"
        style="
          margin-top: 12px;
          padding-top: 12px;
          border-top: 1px solid var(--border);
        "
      >
        <div class="setting-info">
          <label>主题工具</label>
          <p class="setting-hint">在线编辑或浏览社区主题</p>
        </div>
        <div class="setting-control inline-control">
          <Button variant="outline" size="sm" @click="openThemeEditor"
            >主题编辑器</Button
          >
          <Button variant="outline" size="sm" @click="openThemeMarket"
            >主题市场</Button
          >
        </div>
      </div>
      <!-- Web 编辑器连接 -->
      <div
        v-if="isWailsEnv"
        class="setting-item"
        style="padding-top: 12px; border-top: 1px solid var(--border)"
      >
        <div class="setting-info">
          <label>Web 编辑器连接</label>
          <p class="setting-hint">允许 Web 编辑器推送主题到本地输入法</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="themeEditorMode"
            :disabled="themeServerLoading"
            @update:model-value="setThemeEditorMode($event as ThemeEditorMode)"
          >
            <SelectTrigger class="w-[180px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="off">关闭</SelectItem>
              <SelectItem value="once">本次开启</SelectItem>
              <SelectItem value="auto">打开设置时自动开启</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div
        v-if="isWailsEnv && themeServerRunning"
        class="flex items-center gap-2 text-xs"
        style="margin-top: 6px"
      >
        <span class="text-green-500">●</span>
        <span class="text-muted-foreground flex-1 truncate">{{
          themeServerURL
        }}</span>
        <Button size="sm" variant="outline" @click="copyServerURL"
          >复制地址</Button
        >
      </div>
      <p
        v-if="isWailsEnv && themeServerError"
        class="text-xs text-destructive"
        style="margin-top: 4px"
      >
        {{ themeServerError }}
      </p>
    </div>

    <div class="settings-card">
      <div class="card-title">候选窗口</div>
      <div class="setting-item" data-search-anchor="ui.font_size_follow_theme">
        <div class="setting-info">
          <label>字号跟随主题</label>
          <p class="setting-hint">
            开启后候选字号由主题决定；关闭可在下方自定义
          </p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.ui.font_size_follow_theme"
            @update:checked="formData.ui.font_size_follow_theme = $event"
          />
        </div>
      </div>
      <div
        class="setting-item"
        data-search-anchor="ui.font_size"
        :class="{ 'setting-item-disabled': formData.ui.font_size_follow_theme }"
      >
        <div class="setting-info">
          <label>字体大小</label>
          <p class="setting-hint">候选词字体大小（跟随主题时由主题决定）</p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="12"
            max="36"
            step="1"
            :disabled="formData.ui.font_size_follow_theme"
            v-model.number="formData.ui.font_size"
          />
          <span class="range-value">{{ formData.ui.font_size }}px</span>
        </div>
      </div>
      <div
        class="setting-item"
        data-search-anchor="ui.font_family"
        v-if="isWailsEnv"
      >
        <div class="setting-info">
          <label>候选字体</label>
          <p class="setting-hint">自定义字体，留空跟随系统默认</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.ui.font_family || '__default__'"
            @update:model-value="
              formData.ui.font_family = $event === '__default__' ? '' : $event
            "
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue placeholder="跟随系统默认" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__default__">跟随系统默认</SelectItem>
              <SelectItem
                v-for="font in systemFontOptions"
                :key="font.value"
                :value="font.value"
              >
                {{ font.label }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <SchemaRenderer
        :schema="candidateWindowSchema"
        :form-data="formData"
        mode="bare"
      />
      <div class="setting-item" data-search-anchor="ui.cmdbar_candidate_prefix">
        <div class="setting-info">
          <label>命令直通车标注</label>
          <p class="setting-hint">命令候选前的提示符号</p>
        </div>
        <div class="setting-control inline-control">
          <Select
            :model-value="cmdbarPrefixMode"
            @update:model-value="setCmdbarPrefixMode($event as any)"
          >
            <SelectTrigger class="w-[140px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">默认 ⚡</SelectItem>
              <SelectItem value="none">不显示</SelectItem>
              <SelectItem value="custom">自定义</SelectItem>
            </SelectContent>
          </Select>
          <template v-if="cmdbarPrefixMode === 'custom'">
            <span class="cmdbar-prefix-chip">{{
              formData.ui.cmdbar_candidate_prefix
            }}</span>
            <Button variant="outline" size="sm" @click="openCmdbarPrefixDialog">
              编辑
            </Button>
          </template>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">状态提示</div>
      <SchemaRenderer
        :schema="statusSchema"
        :form-data="formData"
        mode="bare"
      />
      <!-- 显示内容（复选框组，无对应 schema 类型） -->
      <div
        class="setting-item"
        data-search-anchor="ui.status_indicator.show_items"
        v-if="formData.ui.status_indicator.enabled"
      >
        <div class="setting-info">
          <label>显示内容</label>
          <p class="setting-hint">状态提示中要显示的图标</p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.ui.status_indicator.show_mode"
            />
            模式
          </label>
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.ui.status_indicator.show_punct"
            />
            标点
          </label>
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.ui.status_indicator.show_full_width"
            />
            全半角
          </label>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">候选项提示信息</div>
      <SchemaRenderer
        :schema="candidateTooltipSchema"
        :form-data="formData"
        mode="bare"
      />
    </div>

    <div class="settings-card" v-if="!isMac">
      <div class="card-title">工具栏</div>
      <SchemaRenderer
        :schema="toolbarSchema"
        :form-data="formData"
        mode="bare"
      />
    </div>

    <!-- macOS：无悬浮工具栏，改为菜单栏状态指示器（NSStatusItem）开关，复用 toolbar.visible -->
    <div class="settings-card" v-if="isMac">
      <div class="card-title">菜单栏指示器</div>
      <SchemaRenderer
        :schema="indicatorSchema"
        :form-data="formData"
        mode="bare"
      />
    </div>

    <Dialog
      :open="cmdbarPrefixDialogOpen"
      @update:open="(v: boolean) => !v && cancelCmdbarPrefixDialog()"
    >
      <DialogContent class="sm:max-w-[360px]">
        <DialogHeader>
          <DialogTitle>命令直通车标注符号</DialogTitle>
          <DialogDescription>
            输入 1-4 个字符作为命令候选前的提示符号
          </DialogDescription>
        </DialogHeader>
        <input
          type="text"
          class="cmdbar-prefix-input"
          maxlength="4"
          v-model="cmdbarPrefixDraft"
          @keydown.enter="confirmCmdbarPrefixDialog"
        />
        <DialogFooter>
          <Button variant="outline" size="sm" @click="cancelCmdbarPrefixDialog">
            取消
          </Button>
          <Button size="sm" @click="confirmCmdbarPrefixDialog">确定</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <!-- 删除主题确认弹框 -->
    <Dialog
      :open="deleteConfirmOpen"
      @update:open="
        (v: boolean) => {
          if (!v) deleteConfirmOpen = false;
        }
      "
    >
      <DialogContent class="sm:max-w-[360px]">
        <DialogHeader>
          <DialogTitle>删除主题</DialogTitle>
          <DialogDescription>
            确定要删除主题「{{ deletingThemeLabel }}」吗？此操作不可恢复。
          </DialogDescription>
        </DialogHeader>
        <p v-if="deleteError" class="text-sm text-destructive">
          {{ deleteError }}
        </p>
        <DialogFooter>
          <Button
            variant="outline"
            size="sm"
            :disabled="deleteLoading"
            @click="deleteConfirmOpen = false"
          >
            取消
          </Button>
          <Button
            variant="destructive"
            size="sm"
            :disabled="deleteLoading"
            @click="confirmDeleteTheme"
          >
            {{ deleteLoading ? "删除中…" : "确认删除" }}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <ThemeImportModal
      :open="themeImportOpen"
      @update:open="themeImportOpen = $event"
      @imported="onThemeImported"
    />
  </section>
</template>

<style scoped>
/* 命令直通车标注 — 当前符号 chip + 弹框输入框 */
.cmdbar-prefix-chip {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 28px;
  padding: 0 8px;
  height: 28px;
  border-radius: 6px;
  background: var(--muted, #f1f5f9);
  border: 1px solid var(--border, #e2e8f0);
  font-size: 14px;
  line-height: 1;
}
.cmdbar-prefix-input {
  width: 100%;
  height: 36px;
  padding: 0 12px;
  border-radius: 6px;
  border: 1px solid var(--border, #e2e8f0);
  background: var(--background, #fff);
  color: inherit;
  font-size: 18px;
  text-align: center;
  outline: none;
}
.cmdbar-prefix-input:focus {
  border-color: var(--primary, #3b82f6);
}

/* 问号提示图标 */
.preview-hint-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  font-size: 10px;
  font-weight: 600;
  border-radius: 50%;
  background: hsl(var(--muted-foreground));
  color: hsl(var(--card));
  margin-left: 4px;
  cursor: help;
  vertical-align: middle;
}
/* 预览容器 */
.theme-preview {
  border-radius: 10px;
  padding: 16px;
  transition: background 0.2s;
}
.preview-layout {
  display: flex;
  gap: 24px;
  align-items: flex-start;
}
.preview-block {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.preview-section-label {
  font-size: 11px;
  color: hsl(var(--muted-foreground));
  letter-spacing: 0.5px;
}
/* 候选窗口 */
.preview-candidate-window {
  display: flex;
  flex-direction: column;
  border: 1px solid hsl(var(--border));
  border-radius: 8px;
  overflow: hidden;
}
.preview-input-bar {
  padding: 4px 10px;
  font-size: 11px;
  font-family: monospace;
}
.preview-candidates {
  display: flex;
  align-items: center;
  gap: 1px;
  padding: 5px 6px;
}
.preview-candidate-item {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 2px 5px;
  border-radius: 4px;
  position: relative;
}
/* accent bar（微软风格：绑定在每个候选项左侧） */
.preview-item-accent {
  position: absolute;
  left: 0;
  top: 3px;
  bottom: 3px;
  width: 2px;
  border-radius: 0 1px 1px 0;
}
/* 圆形序号（默认主题） */
.preview-index {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 9px;
  font-weight: 500;
  flex-shrink: 0;
}
.preview-index.preview-index-circle {
  width: 15px;
  height: 15px;
  border-radius: 50%;
}
/* 文字序号（微软风格） */
.preview-index.preview-index-text {
  background: transparent !important;
  font-size: 11px;
  font-weight: 600;
  width: auto;
  padding: 0 1px;
}
.preview-text {
  font-size: 12px;
  white-space: nowrap;
}
.preview-comment {
  font-size: 10px;
  margin-left: 2px;
  white-space: nowrap;
}
/* 工具栏 */
.preview-toolbar {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  padding: 3px 6px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
}
.preview-toolbar-grip {
  font-size: 9px;
  margin-right: 1px;
  opacity: 0.7;
  user-select: none;
}
.preview-toolbar-item {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  font-size: 10px;
  border-radius: 4px;
}
@media (max-width: 768px) {
  .preview-layout {
    flex-direction: column;
    gap: 12px;
  }
}

.font-family-select {
  max-width: 200px;
  max-height: 300px;
  overflow-y: auto;
  text-overflow: ellipsis;
}

.checkbox-label {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 13px;
  cursor: pointer;
  white-space: nowrap;
}

.checkbox-label input[type="checkbox"] {
  cursor: pointer;
}

/* 字号跟随主题时，字体大小行整体变暗并禁止交互，给出明确的禁用视觉反馈 */
.setting-item-disabled .range-control {
  opacity: 0.45;
  pointer-events: none;
}
.setting-item-disabled .range-control input[type="range"] {
  cursor: not-allowed;
}

.card-title-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

/* 主题选项行：选项本体 + 右侧删除按钮 */
.theme-option-row {
  display: flex;
  align-items: stretch;
  gap: 2px;
}

.theme-option-row .theme-option {
  flex: 1;
  min-width: 0;
}

.theme-option-delete-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  width: 28px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: hsl(var(--muted-foreground));
  cursor: pointer;
  opacity: 0;
  transition:
    opacity 0.15s,
    background 0.15s,
    color 0.15s;
}

.theme-option-row:hover .theme-option-delete-btn {
  opacity: 1;
}

.theme-option-delete-btn:hover {
  background: hsl(var(--destructive) / 0.1);
  color: hsl(var(--destructive));
}
</style>
