<template>
  <Dialog :open="open" @update:open="$emit('update:open', $event)">
    <DialogContent class="ie-dialog-outer p-0 gap-0 bg-card max-w-[min(480px,90vw)]">
      <!-- 固定高度内部容器 -->
      <div class="ie-dialog">
        <!-- 标题 -->
        <div class="ie-header">
          <DialogTitle>词库数据管理</DialogTitle>
          <div class="ie-tabs">
            <button
              :class="['ie-tab', { active: activeTab === 'import' }]"
              @click="activeTab = 'import'"
            >导入</button>
            <button
              :class="['ie-tab', { active: activeTab === 'export' }]"
              @click="activeTab = 'export'"
            >导出</button>
          </div>
        </div>

        <!-- 内容区（滚动） -->
        <div class="ie-body">
        <!-- ===== 导入 ===== -->
        <template v-if="activeTab === 'import'">
          <!-- Step 1: 选择格式 -->
          <div v-if="importStep === 'format'">
            <label class="ie-field-label">导入格式</label>
            <div class="ie-rich-dropdown" ref="importDropdownRef">
              <button class="ie-rich-trigger" type="button" @click="importDropdownOpen = !importDropdownOpen">
                <div class="ie-rich-trigger-main">
                  <div class="ie-rich-trigger-title">{{ currentImportFormat.label }}</div>
                  <div class="ie-rich-trigger-sub">{{ currentImportFormat.desc }}</div>
                </div>
                <ChevronDown class="h-4 w-4 text-muted-foreground flex-shrink-0" />
              </button>
              <div v-if="importDropdownOpen" class="ie-rich-options">
                <button
                  v-for="fmt in importFormats" :key="fmt.value" type="button"
                  class="ie-rich-option" :class="{ selected: importFormat === fmt.value }"
                  @click="importFormat = fmt.value; importDropdownOpen = false"
                >
                  <div class="ie-rich-option-title">{{ fmt.label }}</div>
                  <div class="ie-rich-option-sub">{{ fmt.desc }}</div>
                </button>
              </div>
            </div>

            <div class="ie-format-example" v-if="currentImportFormat.example">
              <div class="ie-example-label">格式示例</div>
              <pre class="ie-example-code">{{ currentImportFormat.example }}</pre>
            </div>
          </div>

          <!-- Step 2: 预览确认 -->
          <div v-if="importStep === 'preview'">
            <div class="ie-preview-info">
              <p v-if="preview?.schema_name">
                <strong>方案:</strong> {{ preview.schema_name }}
                <span v-if="preview.schema_id"> ({{ preview.schema_id }})</span>
              </p>
              <p v-if="preview?.generator"><strong>来源:</strong> {{ preview.generator }}</p>
              <p v-if="preview?.exported_at"><strong>导出时间:</strong> {{ preview.exported_at }}</p>
            </div>

            <!-- 目标方案选择（纯词库格式） -->
            <div v-if="isWordOnlyFormat" class="ie-field">
              <label class="ie-field-label">导入到方案</label>
              <Select v-model="importTargetSchema">
                <SelectTrigger class="ie-select-trigger">
                  <SelectValue placeholder="选择目标方案" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem v-for="sid in nonMixedSchemaIds" :key="sid" :value="sid">
                    {{ allSchemaNames[sid] || sid }}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            <!-- Section 选择 -->
            <div v-if="preview?.sections" class="ie-section-grid">
              <label
                v-for="(count, section) in preview.sections" :key="section"
                class="ie-section-item"
              >
                <Checkbox
                  :checked="selectedSections.includes(section as string)"
                  @update:checked="toggleSection(section as string, $event)"
                />
                <span class="ie-section-name">{{ sectionLabel(section as string) }}</span>
                <span class="ie-section-count">{{ count }}</span>
              </label>
            </div>
          </div>

          <!-- Step 3: 导入完成 -->
          <div v-if="importStep === 'done'" class="ie-result-wrap">
            <p class="ie-result">{{ importMessage }}</p>
          </div>
        </template>

        <!-- ===== 导出 ===== -->
        <template v-if="activeTab === 'export'">
          <!-- 导出类型 -->
          <div class="ie-radio-group">
            <label class="ie-radio-item" :class="{ active: exportType === 'schema' }">
              <input type="radio" value="schema" v-model="exportType" /> 方案数据
            </label>
            <label class="ie-radio-item" :class="{ active: exportType === 'phrases' }">
              <input type="radio" value="phrases" v-model="exportType" /> 快捷短语
            </label>
            <label class="ie-radio-item" :class="{ active: exportType === 'backup' }">
              <input type="radio" value="backup" v-model="exportType" /> 全部备份 (ZIP)
            </label>
          </div>

          <!-- 方案选择（仅方案数据模式） -->
          <div v-if="exportType === 'schema'" class="ie-field">
            <label class="ie-field-label">选择方案</label>
            <Select v-model="exportSchemaId">
              <SelectTrigger class="ie-select-trigger">
                <SelectValue placeholder="选择方案" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem
                  v-for="sid in (exportableSchemaIds ?? allSchemaIds)"
                  :key="sid"
                  :value="sid"
                >
                  {{ allSchemaNames[sid] || sid }}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>

          <!-- 方案内容选择（两列） -->
          <div v-if="exportType === 'schema'">
            <label class="ie-field-label">导出内容</label>
            <div class="ie-section-grid">
              <label
                v-for="sec in exportableSections" :key="sec.key"
                class="ie-section-item ie-section-bordered"
                :class="{ disabled: sec.disabled || (exportCounts[sec.key] ?? -1) === 0 }"
              >
                <Checkbox
                  :checked="exportSections.includes(sec.key)"
                  :disabled="sec.disabled || (exportCounts[sec.key] ?? -1) === 0"
                  @update:checked="toggleExportSection(sec.key, $event)"
                />
                <span class="ie-section-name">{{ sec.label }}</span>
                <span v-if="sec.disabled" class="ie-section-count">-</span>
                <span v-else class="ie-section-count">{{ exportCounts[sec.key] ?? '-' }}</span>
              </label>
            </div>
          </div>

          <!-- 短语信息 -->
          <div v-if="exportType === 'phrases'" class="ie-preview-info">
            <p>快捷短语共 <strong>{{ exportCounts.phrases ?? 0 }}</strong> 条</p>
          </div>

          <!-- 导出结果 -->
          <p v-if="exportMessage" class="ie-result">{{ exportMessage }}</p>
        </template>
      </div>

      <!-- 按钮栏（固定底部） -->
      <div class="ie-footer">
        <template v-if="activeTab === 'import'">
          <template v-if="importStep === 'format'">
            <Button variant="outline" size="sm" @click="$emit('update:open', false)">取消</Button>
            <Button size="sm" @click="selectFile">选择文件...</Button>
          </template>
          <template v-if="importStep === 'preview'">
            <Button variant="outline" size="sm" @click="importStep = 'format'">返回</Button>
            <Button size="sm" :disabled="selectedSections.length === 0 || importing" @click="doImport">
              {{ importing ? '导入中...' : '确认导入' }}
            </Button>
          </template>
          <template v-if="importStep === 'done'">
            <Button size="sm" @click="resetImport">继续导入</Button>
            <Button variant="outline" size="sm" @click="closeAndRefresh">关闭</Button>
          </template>
        </template>
        <template v-if="activeTab === 'export'">
          <Button variant="outline" size="sm" @click="$emit('update:open', false)">取消</Button>
          <Button size="sm" :disabled="exporting || !canExport" @click="doExport">
            {{ exporting ? '导出中...' : '导出...' }}
          </Button>
        </template>
      </div>
      </div><!-- .ie-dialog -->
    </DialogContent>
  </Dialog>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { ChevronDown } from "lucide-vue-next";
import {
  Dialog,
  DialogContent,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import * as wailsApi from "@/api/wails";
import type { DictImportPreview } from "@/api/wails";

const props = defineProps<{
  open: boolean;
  currentSchemaId: string;
  currentSchemaName: string;
  currentSchemaMixed?: boolean;
  allSchemaIds: string[];
  allSchemaNames: Record<string, string>;
  nonMixedSchemaIds: string[];
  /** 可作为"导出"目标的方案 ID 列表（已排除双拼方案）。
   *  与 allSchemaIds 区别：导出下拉不显示双拼方案，避免与全拼共享数据导致重复导出。 */
  exportableSchemaIds?: string[];
  /** 进入对话框时的模式：phrases 或 schema */
  initialMode?: string;
  /** 初始 Tab：import 或 export */
  initialTab?: "import" | "export";
}>();

const emit = defineEmits<{
  "update:open": [value: boolean];
  imported: [];
}>();

const activeTab = ref<"import" | "export">("import");

// ===== 下拉关闭逻辑 =====
const importDropdownRef = ref<HTMLElement | null>(null);
const importDropdownOpen = ref(false);

function handleDocClick(e: MouseEvent) {
  const t = e.target as Node;
  if (importDropdownRef.value && !importDropdownRef.value.contains(t)) {
    importDropdownOpen.value = false;
  }
}

onMounted(() => document.addEventListener("click", handleDocClick));
onUnmounted(() => document.removeEventListener("click", handleDocClick));

// ===== 导入状态 =====
const importStep = ref<"format" | "preview" | "done">("format");
const importFormat = ref("winddict");
const importFilePath = ref("");
const importTargetSchema = ref(props.currentSchemaId);
const preview = ref<DictImportPreview | null>(null);
const selectedSections = ref<string[]>([]);
const importMessage = ref("");
const importing = ref(false);

const isWordOnlyFormat = computed(() =>
  ["tsv", "rime", "textlist"].includes(importFormat.value),
);

const importFormats = [
  {
    value: "winddict",
    label: "WindDict (.wdict.yaml)",
    desc: "WindInput 自有格式，支持全部数据类型",
    example: `wind_dict:
  version: 1
  schema_id: wubi86
  sections:
    user_words:
      columns: [code, text, weight]
--- !user_words
a\t工\t100
aa\t式\t200`,
  },
  {
    value: "tsv",
    label: "纯文本 TSV (.txt)",
    desc: "Tab 分隔，每行: 编码<Tab>词条<Tab>权重",
    example: `# code\ttext\tweight
a\t工\t100
aa\t式\t200
abcf\t输入法\t150`,
  },
  {
    value: "rime",
    label: "Rime 词库 (.dict.yaml)",
    desc: "Rime 输入法用户词库格式",
    example: `---
name: my_dict
version: "1.0"
columns:
  - text
  - code
  - weight
...
工\ta\t100
中国\tgg\t500`,
  },
  {
    value: "textlist",
    label: "纯词语列表",
    desc: "一行一个词语，自动根据码表生成编码",
    example: `# 一行一个词语
输入法
候选词
中国
工作`,
  },
  {
    value: "phrase_yaml",
    label: "短语 YAML (.yaml)",
    desc: "旧版短语 YAML 格式",
    example: `phrases:
  - code: sj
    text: "$time_now"
    position: 1
  - code: yx
    texts: "a@b.com\\nb@c.com"
    name: 常用邮箱`,
  },
  {
    value: "zip",
    label: "ZIP 备份包 (.zip)",
    desc: "由完整备份导出的 ZIP 包",
    example: "",
  },
];

const currentImportFormat = computed(() =>
  importFormats.find((f) => f.value === importFormat.value) || importFormats[0],
);

function sectionLabel(section: string): string {
  const labels: Record<string, string> = {
    user_words: "用户词库", temp_words: "临时词库",
    freq: "词频数据", shadow: "候选调整", phrases: "快捷短语",
  };
  return labels[section] || section;
}

function toggleSection(section: string, checked: boolean | "indeterminate") {
  if (checked === true) {
    if (!selectedSections.value.includes(section)) selectedSections.value.push(section);
  } else {
    selectedSections.value = selectedSections.value.filter((s) => s !== section);
  }
}

async function selectFile() {
  try {
    const path = await wailsApi.selectImportFile(importFormat.value);
    if (!path) return;
    importFilePath.value = path;

    if (importFormat.value === "zip") {
      const zipPreview = await wailsApi.previewZipImport(path);
      const sections: Record<string, number> = {};
      for (const s of zipPreview.schemas) {
        for (const [k, v] of Object.entries(s.sections || {})) {
          sections[k] = (sections[k] || 0) + v;
        }
      }
      if (zipPreview.has_phrases) sections["phrases"] = zipPreview.phrase_count;
      preview.value = {
        schema_id: "", schema_name: `${zipPreview.schemas.length} 个方案`,
        generator: "", exported_at: "",
        sections, source_file: path,
      };
    } else {
      preview.value = await wailsApi.previewImportFile(importFormat.value, path);
    }

    selectedSections.value = Object.keys(preview.value?.sections || {});
    importStep.value = "preview";
  } catch (e: unknown) {
    importMessage.value = (e as Error).message || "预览失败";
    importStep.value = "done";
  }
}

async function doImport() {
  importing.value = true;
  try {
    let result: wailsApi.ImportExportResult;
    if (importFormat.value === "zip") {
      const zipPreview = await wailsApi.previewZipImport(importFilePath.value);
      result = await wailsApi.executeZipImport(
        importFilePath.value, zipPreview.schemas.map((s) => s.schema_id), true, {},
      );
    } else {
      const targetSchema = isWordOnlyFormat.value ? importTargetSchema.value : props.currentSchemaId;
      result = await wailsApi.executeImport(
        importFilePath.value, importFormat.value, targetSchema, selectedSections.value, {},
      );
    }
    importMessage.value = `导入成功，共 ${result.count} 条`;
    importStep.value = "done";
  } catch (e: unknown) {
    importMessage.value = (e as Error).message || "导入失败";
    importStep.value = "done";
  } finally {
    importing.value = false;
  }
}

function resetImport() {
  importStep.value = "format";
  preview.value = null;
  selectedSections.value = [];
  importFilePath.value = "";
  importMessage.value = "";
}

function closeAndRefresh() {
  emit("imported");
  emit("update:open", false);
}

// ===== 导出状态 =====
const exportType = ref<string>("schema");
const exportSchemaId = ref(props.currentSchemaId);
const exportSections = ref(["user_words", "temp_words", "freq", "shadow"]);
const exportMessage = ref("");
const exporting = ref(false);
const exportCounts = ref<Record<string, number>>({});

// 当前导出方案是否为混输
const exportSchemaIsMixed = computed(() => {
  return !props.nonMixedSchemaIds.includes(exportSchemaId.value);
});

const exportableSections = computed(() => {
  const all = [
    { key: "user_words", label: "用户词库" },
    { key: "temp_words", label: "临时词库" },
    { key: "freq", label: "词频数据" },
    { key: "shadow", label: "候选调整" },
  ];
  if (exportSchemaIsMixed.value) {
    return all.map((s) => ({
      ...s,
      disabled: s.key === "user_words" || s.key === "temp_words",
    }));
  }
  return all.map((s) => ({ ...s, disabled: false }));
});

// 导出按钮可用条件：选中的内容里至少有一项是非空的
//   - 方案数据：选中的 section 中至少一项 count > 0
//   - 快捷短语：phrases 数量 > 0
//   - 全部备份：始终允许（备份本身可能包含至少一类数据）
const canExport = computed(() => {
  if (exportType.value === "schema") {
    if (!exportSchemaId.value || exportSections.value.length === 0) return false;
    return exportSections.value.some(
      (k) => (exportCounts.value[k] ?? 0) > 0,
    );
  }
  if (exportType.value === "phrases") {
    return (exportCounts.value.phrases ?? 0) > 0;
  }
  return true;
});

function toggleExportSection(key: string, checked: boolean | "indeterminate") {
  const sec = exportableSections.value.find((s) => s.key === key);
  if (sec?.disabled || (exportCounts.value[key] ?? -1) === 0) return;
  if (checked === true) {
    if (!exportSections.value.includes(key)) exportSections.value.push(key);
  } else {
    exportSections.value = exportSections.value.filter((s) => s !== key);
  }
}

async function loadExportCounts() {
  const counts: Record<string, number> = {};
  try {
    const allStatuses = await wailsApi.getAllSchemaStatuses();
    const s = allStatuses.find((s: any) => s.schema_id === exportSchemaId.value);
    if (s) {
      const isMixed = !props.nonMixedSchemaIds.includes(exportSchemaId.value);
      if (!isMixed) {
        counts.user_words = s.user_words;
        counts.temp_words = s.temp_words;
      }
      counts.shadow = s.shadow_rules;
      counts.freq = s.freq_records;
    }
  } catch { /* ignore */ }
  try {
    const phrases = await wailsApi.getPhraseList();
    counts.phrases = phrases?.length ?? 0;
  } catch { /* ignore */ }
  exportCounts.value = counts;
}

// 切换导出方案时重新加载统计
watch(exportSchemaId, () => {
  loadExportCounts();
  // 切换方案 → 上一次"导出成功"消息已无效，清除以避免误导
  exportMessage.value = "";
  // 混输方案自动去掉 user_words/temp_words
  if (exportSchemaIsMixed.value) {
    exportSections.value = exportSections.value.filter(
      (s) => s !== "user_words" && s !== "temp_words",
    );
  }
});

// 切换导出类型/调整选中内容 → 清除旧消息
watch(exportType, () => {
  exportMessage.value = "";
});
watch(
  exportSections,
  () => {
    exportMessage.value = "";
  },
  { deep: true },
);

watch(
  () => props.open,
  (val) => {
    if (val) {
      // 设置初始 Tab
      activeTab.value = props.initialTab || "import";
      // 根据进入模式设置默认选中
      if (props.initialMode === "phrases") {
        exportType.value = "phrases";
      } else {
        exportType.value = "schema";
      }
      // 默认导出目标：当前方案；若当前方案不可导出（如双拼），回退到首个可导出方案
      const exportable = props.exportableSchemaIds ?? props.allSchemaIds;
      if (exportable.includes(props.currentSchemaId)) {
        exportSchemaId.value = props.currentSchemaId;
      } else if (exportable.length > 0) {
        exportSchemaId.value = exportable[0];
      } else {
        exportSchemaId.value = props.currentSchemaId;
      }
      if (props.currentSchemaMixed && props.nonMixedSchemaIds.length > 0) {
        importTargetSchema.value = props.nonMixedSchemaIds[0];
      } else {
        importTargetSchema.value = props.currentSchemaId;
      }
      // 每次打开对话框都重置一次性状态：避免上次"导入成功/导出成功"残留
      resetImport();
      exportMessage.value = "";
      loadExportCounts();
    }
  },
);

async function doExport() {
  exporting.value = true;
  exportMessage.value = "";
  try {
    let result: wailsApi.ImportExportResult;
    switch (exportType.value) {
      case "schema": {
        const name = props.allSchemaNames[exportSchemaId.value] || exportSchemaId.value;
        result = await wailsApi.exportSchemaData(exportSchemaId.value, exportSections.value, name);
        break;
      }
      case "phrases":
        result = await wailsApi.exportPhrasesFile("winddict");
        break;
      case "backup":
        result = await wailsApi.exportFullBackup(props.allSchemaIds, props.allSchemaNames, true);
        break;
      default:
        return;
    }
    if (result.cancelled) { exportMessage.value = ""; return; }
    exportMessage.value = `导出成功，共 ${result.count} 条`;
  } catch (e: unknown) {
    exportMessage.value = (e as Error).message || "导出失败";
  } finally {
    exporting.value = false;
  }
}
</script>

<style scoped>
.ie-dialog-outer {
  /* padding/gap/bg/max-w 通过 Tailwind 类在 template 中覆盖（cn/twMerge） */
}

.ie-dialog {
  width: 100%;
  height: min(480px, 76vh);
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 14px 16px 12px;
}

.ie-header {
  flex-shrink: 0;
  padding-bottom: 0;
}

.ie-tabs {
  display: flex;
  gap: 0;
  border-bottom: 1px solid hsl(var(--border));
  margin-top: 8px;
}

.ie-tab {
  padding: 6px 16px;
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  cursor: pointer;
  font-size: 14px;
  color: hsl(var(--muted-foreground));
  transition: all 0.15s;
}

.ie-tab.active {
  color: hsl(var(--foreground));
  border-bottom-color: hsl(var(--primary));
}

.ie-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 12px 0 4px;
}

.ie-footer {
  flex-shrink: 0;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding-top: 10px;
  border-top: 1px solid hsl(var(--border));
}

/* ===== 字段 ===== */
.ie-field {
  margin-bottom: 12px;
}

.ie-field-label {
  display: block;
  font-weight: 500;
  font-size: 13px;
  margin-bottom: 6px;
}

.ie-select-trigger {
  width: 100%;
}

/* Select 触发器在 .ie-body (overflow-y: auto) 内部时，
   focus ring 默认是 outset（box-shadow 在 border 外侧），会被横向 overflow 裁切。
   改为 inset shadow，让高亮边框始终完整可见。 */
.ie-body :deep(.ie-select-trigger:focus),
.ie-body :deep(.ie-select-trigger:focus-visible) {
  outline: none;
  box-shadow: inset 0 0 0 1px hsl(var(--ring));
  --tw-ring-shadow: 0 0 #0000;
}

/* ===== Radio 组 ===== */
.ie-radio-group {
  display: flex;
  gap: 6px;
  margin-bottom: 12px;
}

.ie-radio-item {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 5px 10px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  cursor: pointer;
  font-size: 13px;
  transition: all 0.1s;
}

.ie-radio-item:hover {
  background: hsl(var(--accent));
}

.ie-radio-item.active {
  border-color: hsl(var(--primary) / 0.5);
  background: hsl(var(--primary) / 0.06);
}

.ie-radio-item input[type="radio"] {
  margin: 0;
}

/* ===== 详细信息下拉 ===== */
.ie-rich-dropdown {
  position: relative;
  margin-bottom: 12px;
}

.ie-rich-trigger {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 12px;
  border: 1px solid hsl(var(--border));
  border-radius: 8px;
  background: hsl(var(--background));
  cursor: pointer;
  text-align: left;
  transition: border-color 0.15s, box-shadow 0.15s;
}

.ie-rich-trigger:hover {
  border-color: hsl(var(--ring) / 0.4);
}

.ie-rich-trigger-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.ie-rich-trigger-title {
  font-weight: 600;
  font-size: 13px;
  color: hsl(var(--foreground));
}

.ie-rich-trigger-sub {
  font-size: 11px;
  color: hsl(var(--muted-foreground));
}




.ie-rich-options {
  position: absolute;
  top: calc(100% + 4px);
  left: 0;
  right: 0;
  z-index: 50;
  background: hsl(var(--popover));
  border: 1px solid hsl(var(--border));
  border-radius: 8px;
  box-shadow: 0 4px 16px hsl(0 0% 0% / 0.1);
  padding: 4px;
  max-height: 200px;
  overflow-y: auto;
}

.ie-rich-option {
  width: 100%;
  text-align: left;
  border: 1px solid transparent;
  background: transparent;
  padding: 7px 10px;
  border-radius: 6px;
  cursor: pointer;
  transition: background 0.1s;
}

.ie-rich-option:hover {
  background: hsl(var(--muted));
}

.ie-rich-option.selected {
  border-color: hsl(var(--primary) / 0.4);
  background: hsl(var(--primary) / 0.08);
}

.ie-rich-option-title {
  font-weight: 500;
  font-size: 13px;
  color: hsl(var(--foreground));
}

.ie-rich-option-sub {
  font-size: 11px;
  color: hsl(var(--muted-foreground));
  margin-top: 1px;
}

/* ===== ���式示例 ===== */
.ie-format-example {
  margin-bottom: 8px;
}

.ie-example-label {
  font-size: 12px;
  font-weight: 500;
  color: hsl(var(--muted-foreground));
  margin-bottom: 4px;
}

.ie-example-code {
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 11px;
  line-height: 1.5;
  background: hsl(var(--muted));
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  padding: 8px 10px;
  overflow-x: auto;
  white-space: pre;
  color: hsl(var(--foreground));
  max-height: 100px;
  overflow-y: auto;
}

/* ===== 预览信息 ===== */
.ie-preview-info {
  background: hsl(var(--muted));
  border-radius: 6px;
  padding: 8px 12px;
  margin-bottom: 10px;
  font-size: 13px;
}

.ie-preview-info p {
  margin: 2px 0;
}

/* ===== Section 两列网格 ===== */
.ie-section-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 4px 12px;
  margin-bottom: 8px;
}

.ie-section-item {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  font-size: 13px;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.1s;
}

.ie-section-item:hover {
  background: hsl(var(--accent));
}

.ie-section-bordered {
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  padding: 6px 10px;
}

.ie-section-bordered.disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.ie-section-name {
  flex: 1;
  min-width: 0;
}

.ie-section-count {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
  font-variant-numeric: tabular-nums;
  background: hsl(var(--muted));
  padding: 1px 6px;
  border-radius: 4px;
}

.ie-result-wrap {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 80px;
}

.ie-result {
  padding: 12px;
  text-align: center;
  font-size: 14px;
  color: hsl(var(--foreground));
}
</style>

