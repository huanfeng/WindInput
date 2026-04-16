<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import type { Config, EngineInfo } from "../api/settings";
import * as wailsApi from "../api/wails";
import type { SchemaConfig, SchemaInfo, SchemaReference } from "../api/wails";
import SchemaDetailPanel from "../components/SchemaDetailPanel.vue";
import SchemaManagerDialog from "../components/SchemaManagerDialog.vue";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";

const props = defineProps<{
  formData: Config;
  engines: EngineInfo[];
}>();

const emit = defineEmits<{
  switchEngine: [type: string];
}>();

// 所有可用方案
const allSchemas = ref<SchemaInfo[]>([]);

// 已启用方案的 ID 列表（有序）
const enabledSchemaIDs = ref<string[]>([]);

// 各方案的配置（schemaID -> config）
const schemaConfigs = ref<Record<string, SchemaConfig>>({});
const schemaLoading = ref(false);

// 方案管理对话框
const showSchemaManager = ref(false);

// 方案详情浮层
const detailSchemaID = ref<string | null>(null);

// 模糊音对话框
const showFuzzyDialog = ref(false);
const fuzzyEditSchemaID = ref("");

// 方案引用关系
const schemaReferences = ref<Record<string, SchemaReference>>({});
// 仅通过引用显示的方案ID（不在 available 列表中）
const referencedOnlyIDs = ref<string[]>([]);

// 当前活跃方案 ID
const activeSchemaID = computed(() => props.formData.schema?.active || "");

// 获取引擎类型的显示文本
function getEngineTypeLabel(schemaID: string): string {
  const info = allSchemas.value.find((s) => s.id === schemaID);
  const type =
    info?.engine_type || schemaConfigs.value[schemaID]?.engine?.type || "";
  const labels: Record<string, string> = {
    codetable: "码表",
    pinyin: "拼音",
    mixed: "混输",
  };
  return labels[type] || type || "";
}

// 获取方案副标题（作者 + 描述）
function getSchemaSubtitle(schemaID: string): string {
  const info = allSchemas.value.find((s) => s.id === schemaID);
  const cfg = schemaConfigs.value[schemaID];
  const parts: string[] = [];
  const author = cfg?.schema?.author;
  if (author) parts.push(author);
  const desc = info?.description || cfg?.schema?.description;
  if (desc) parts.push(desc);
  return parts.join(" · ") || schemaID;
}

// 获取方案版本
function getSchemaVersion(schemaID: string): string {
  const info = allSchemas.value.find((s) => s.id === schemaID);
  return info?.version || schemaConfigs.value[schemaID]?.schema?.version || "";
}

// 加载所有方案信息和配置
async function loadAllSchemas() {
  schemaLoading.value = true;
  try {
    const schemas = await wailsApi.getAvailableSchemas();
    allSchemas.value = schemas || [];

    const available = props.formData.schema?.available || [];
    if (available.length > 0) {
      enabledSchemaIDs.value = available.filter((id: string) =>
        schemas.some((s) => s.id === id),
      );
      // 如果有无效的方案 ID 被过滤掉了，同步更新配置以清理脏数据
      if (enabledSchemaIDs.value.length !== available.length) {
        props.formData.schema.available = [...enabledSchemaIDs.value];
      }
    } else {
      enabledSchemaIDs.value = schemas.map((s) => s.id);
    }

    // 如果当前活跃方案已不存在，自动切换到第一个可用方案
    const activeID = props.formData.schema?.active;
    if (activeID && !enabledSchemaIDs.value.includes(activeID)) {
      const firstValid = enabledSchemaIDs.value.find(
        (id) => !schemas.find((s) => s.id === id)?.error,
      );
      if (firstValid) {
        props.formData.schema.active = firstValid;
      }
    }

    for (const id of enabledSchemaIDs.value) {
      await loadSchemaConfig(id);
    }

    // 加载方案引用关系
    try {
      schemaReferences.value = (await wailsApi.getSchemaReferences()) || {};
      // 加载被引用但未启用的方案配置（仅加载配置，不加入管理列表）
      const refIDs = (await wailsApi.getReferencedSchemaIDs()) || [];
      referencedOnlyIDs.value = [];
      for (const id of refIDs) {
        if (!schemaConfigs.value[id]) {
          await loadSchemaConfig(id);
        }
        referencedOnlyIDs.value.push(id);
      }
    } catch (e) {
      console.warn("加载方案引用关系失败", e);
    }
  } catch (e) {
    console.error("加载方案列表失败", e);
  } finally {
    schemaLoading.value = false;
  }
}

async function loadSchemaConfig(schemaID: string) {
  try {
    const cfg = await wailsApi.getSchemaConfig(schemaID);
    schemaConfigs.value[schemaID] = cfg;
  } catch (e) {
    console.error(`加载方案配置失败: ${schemaID}`, e);
  }
}

// 保存方案配置（防抖）
const saveTimers: Record<string, ReturnType<typeof setTimeout>> = {};
function onSchemaConfigChange(schemaID: string) {
  if (saveTimers[schemaID]) clearTimeout(saveTimers[schemaID]);
  saveTimers[schemaID] = setTimeout(() => {
    const cfg = schemaConfigs.value[schemaID];
    if (cfg) {
      wailsApi.saveSchemaConfig(schemaID, cfg).catch((e) => {
        console.error(`保存方案配置失败: ${schemaID}`, e);
      });
    }
  }, 800);
}

// 启用方案
function enableSchema(schemaID: string) {
  if (enabledSchemaIDs.value.includes(schemaID)) return;
  enabledSchemaIDs.value.push(schemaID);
  loadSchemaConfig(schemaID);
  props.formData.schema.available = [...enabledSchemaIDs.value];
  refreshSchemaReferences();
}

// 禁用方案
function disableSchema(schemaID: string) {
  if (enabledSchemaIDs.value.length <= 1) return;
  if (schemaID === activeSchemaID.value) return;
  const idx = enabledSchemaIDs.value.indexOf(schemaID);
  if (idx >= 0) {
    enabledSchemaIDs.value.splice(idx, 1);
    delete schemaConfigs.value[schemaID];
  }
  props.formData.schema.available = [...enabledSchemaIDs.value];
  refreshSchemaReferences();
}

// 刷新方案引用关系（启用/禁用方案后需要重新计算）
async function refreshSchemaReferences() {
  // 根据当前启用列表和已加载的引用关系，本地计算被引用方案
  const enabled = new Set(enabledSchemaIDs.value);
  const newRefOnly: string[] = [];

  for (const id of enabled) {
    const ref = schemaReferences.value[id];
    if (!ref) continue;
    if (ref.primary_schema && !enabled.has(ref.primary_schema)) {
      if (!newRefOnly.includes(ref.primary_schema)) {
        newRefOnly.push(ref.primary_schema);
      }
    }
    if (ref.secondary_schema && !enabled.has(ref.secondary_schema)) {
      if (!newRefOnly.includes(ref.secondary_schema)) {
        newRefOnly.push(ref.secondary_schema);
      }
    }
    if (ref.temp_pinyin_schema && !enabled.has(ref.temp_pinyin_schema)) {
      if (!newRefOnly.includes(ref.temp_pinyin_schema)) {
        newRefOnly.push(ref.temp_pinyin_schema);
      }
    }
  }

  // 清理不再被引用的方案配置
  for (const id of referencedOnlyIDs.value) {
    if (!newRefOnly.includes(id) && !enabled.has(id)) {
      delete schemaConfigs.value[id];
    }
  }

  // 加载新增的引用方案配置
  for (const id of newRefOnly) {
    if (!schemaConfigs.value[id]) {
      await loadSchemaConfig(id);
    }
  }

  referencedOnlyIDs.value = newRefOnly;
}

// 设为当前方案
function setActiveSchema(schemaID: string) {
  if (schemaID === activeSchemaID.value) return;
  props.formData.schema.active = schemaID;
  props.engines.forEach((engine) => {
    engine.isActive = engine.type === schemaID;
  });
  emit("switchEngine", schemaID);
}

// 箭头排序
function moveSchema(index: number, direction: -1 | 1) {
  const targetIndex = index + direction;
  if (targetIndex < 0 || targetIndex >= enabledSchemaIDs.value.length) return;
  const arr = [...enabledSchemaIDs.value];
  [arr[index], arr[targetIndex]] = [arr[targetIndex], arr[index]];
  enabledSchemaIDs.value = arr;
  props.formData.schema.available = [...arr];
}

function getSchemaInfo(schemaID: string): SchemaInfo | undefined {
  return allSchemas.value.find((s) => s.id === schemaID);
}

// 获取方案的引擎类型
function getEngineType(schemaID: string): string {
  return schemaConfigs.value[schemaID]?.engine?.type || "";
}

// 判断方案是否为引用式混输
function isMixedWithRef(schemaID: string): boolean {
  const ref = schemaReferences.value[schemaID];
  return !!(ref && (ref.primary_schema || ref.secondary_schema));
}

// 获取方案的引用信息文案
function getReferenceNote(schemaID: string): string {
  const ref = schemaReferences.value[schemaID];
  if (!ref) return "";
  const parts: string[] = [];
  if (ref.primary_schema)
    parts.push(`码表: ${getSchemaDisplayName(ref.primary_schema)}`);
  if (ref.secondary_schema)
    parts.push(`拼音: ${getSchemaDisplayName(ref.secondary_schema)}`);
  return parts.join(", ");
}

// 获取方案被引用信息（区分引用类型）
function getReferencedByNote(schemaID: string): string {
  const ref = schemaReferences.value[schemaID];
  if (!ref?.referenced_by?.length) return "";
  const parts: string[] = [];
  for (const refByID of ref.referenced_by) {
    const refBy = schemaReferences.value[refByID];
    if (
      refBy?.primary_schema === schemaID ||
      refBy?.secondary_schema === schemaID
    ) {
      parts.push(`${getSchemaDisplayName(refByID)}(混输)`);
    } else if (refBy?.temp_pinyin_schema === schemaID) {
      parts.push(`${getSchemaDisplayName(refByID)}(临时拼音)`);
    } else {
      parts.push(getSchemaDisplayName(refByID));
    }
  }
  return parts.join(", ");
}

// 判断方案是否仅通过引用显示（未在 available 中）
function isReferencedOnly(schemaID: string): boolean {
  return referencedOnlyIDs.value.includes(schemaID);
}

// 所有需要显示配置卡片的方案（启用 + 被引用）
const allConfigSchemaIDs = computed(() => {
  return [...enabledSchemaIDs.value, ...referencedOnlyIDs.value];
});

// 码表配置
function getCodetableConfig(schemaID: string) {
  const cfg = schemaConfigs.value[schemaID];
  if (!cfg) return {};
  if (!cfg.engine.codetable) cfg.engine.codetable = {};
  return cfg.engine.codetable;
}

// 获取方案的最大码长（默认4）
function getMaxCodeLength(schemaID: string): number {
  const ct = getCodetableConfig(schemaID);
  return (ct as any).max_code_length || 4;
}

// 拼音配置
function getPinyinConfig(schemaID: string) {
  const cfg = schemaConfigs.value[schemaID];
  if (!cfg) return {};
  if (!cfg.engine.pinyin) cfg.engine.pinyin = {};
  return cfg.engine.pinyin;
}

// 混输配置
function getMixedConfig(schemaID: string) {
  const cfg = schemaConfigs.value[schemaID];
  if (!cfg) return {};
  if (!cfg.engine.mixed) cfg.engine.mixed = {};
  return cfg.engine.mixed;
}

// 临时拼音配置
function getTempPinyinConfig(schemaID: string) {
  const ct = getCodetableConfig(schemaID);
  if (!ct.temp_pinyin) ct.temp_pinyin = { enabled: true };
  return ct.temp_pinyin;
}

// 学习配置
function getLearningConfig(schemaID: string) {
  const cfg = schemaConfigs.value[schemaID];
  if (!cfg) return { mode: "frequency" };
  if (!cfg.learning) cfg.learning = { mode: "frequency" };
  return cfg.learning;
}

function getFuzzyConfig(schemaID: string) {
  const py = getPinyinConfig(schemaID);
  if (!py.fuzzy) py.fuzzy = {};
  return py.fuzzy;
}

// 双拼方案
const shuangpinLayoutNames: Record<string, string> = {
  xiaohe: "小鹤双拼",
  ziranma: "自然码",
  mspy: "微软双拼",
  sogou: "搜狗双拼",
  abc: "智能ABC",
  ziguang: "紫光双拼",
};

function getShuangpinLayout(schemaID: string): string {
  const py = getPinyinConfig(schemaID);
  return py.shuangpin?.layout || "xiaohe";
}

function getShuangpinLayoutName(schemaID: string): string {
  const layout = getShuangpinLayout(schemaID);
  return shuangpinLayoutNames[layout] || layout;
}

function getSchemaDisplayName(schemaID: string): string {
  const cfg = schemaConfigs.value[schemaID];
  if (!cfg) return ""; // 未加载时返回空，让模板 fallback
  const baseName = cfg.schema?.name || schemaID;
  // 双拼方案：显示 "双拼 · 小鹤双拼" 格式
  if (cfg.engine?.pinyin?.scheme === "shuangpin") {
    return `${baseName} · ${getShuangpinLayoutName(schemaID)}`;
  }
  return baseName;
}

function onShuangpinLayoutChange(schemaID: string, value: string) {
  const py = getPinyinConfig(schemaID);
  if (!py.shuangpin) py.shuangpin = {};
  py.shuangpin.layout = value;
  onSchemaConfigChange(schemaID);
}

// 模糊音
const fuzzyPairs = [
  { field: "zh_z", label: "zh ↔ z", example: "yi'zi → 一直" },
  { field: "ch_c", label: "ch ↔ c", example: "ci'chang → 持常" },
  { field: "sh_s", label: "sh ↔ s", example: "si'jian → 时间" },
  { field: "n_l", label: "n ↔ l", example: "ni → 里" },
  { field: "f_h", label: "f ↔ h", example: "fui → 灰" },
  { field: "r_l", label: "r ↔ l", example: "ren → 冷" },
  { field: "an_ang", label: "an ↔ ang", example: "shan → 上" },
  { field: "en_eng", label: "en ↔ eng", example: "fen → 风" },
  { field: "in_ing", label: "in ↔ ing", example: "xin → 星" },
];

function getFuzzyEnabledCount(schemaID: string) {
  const fuzzy = getFuzzyConfig(schemaID);
  return fuzzyPairs.filter((p) => (fuzzy as any)[p.field]).length;
}

function openFuzzyDialog(schemaID: string) {
  fuzzyEditSchemaID.value = schemaID;
  showFuzzyDialog.value = true;
}

function setAllFuzzyPairs(enabled: boolean) {
  const fuzzy = getFuzzyConfig(fuzzyEditSchemaID.value);
  fuzzyPairs.forEach((p) => {
    (fuzzy as any)[p.field] = enabled;
  });
  onSchemaConfigChange(fuzzyEditSchemaID.value);
}

// 打开方案详情对话框
function openSchemaDetail(schemaID: string) {
  detailSchemaID.value = schemaID;
}

onMounted(() => {
  loadAllSchemas();
});

onUnmounted(() => {
  Object.values(saveTimers).forEach(clearTimeout);
});
</script>

<template>
  <section class="section">
    <div class="section-header">
      <h2>方案设置</h2>
      <p class="section-desc">管理输入方案和方案专属设置</p>
    </div>

    <!-- 方案列表 -->
    <div class="settings-card schema-list-card">
      <div class="card-title schema-list-header">
        <span>输入方案</span>
        <Button size="sm" @click="showSchemaManager = true"> 方案管理 </Button>
      </div>

      <p class="schema-list-hint">使用箭头调整顺序，快捷键切换时按此顺序循环</p>

      <div v-if="schemaLoading" class="schema-list-loading">加载中...</div>

      <div v-else class="schema-list">
        <div
          v-for="(schemaID, index) in enabledSchemaIDs"
          :key="schemaID"
          class="schema-item"
          :class="{ 'schema-item-active': schemaID === activeSchemaID }"
        >
          <div class="schema-row">
            <!-- 排序箭头 -->
            <div class="schema-sort-btns">
              <button
                class="schema-sort-btn"
                :disabled="index === 0"
                @click.stop="moveSchema(index, -1)"
                title="上移"
              >
                &#9650;
              </button>
              <button
                class="schema-sort-btn"
                :disabled="index === enabledSchemaIDs.length - 1"
                @click.stop="moveSchema(index, 1)"
                title="下移"
              >
                &#9660;
              </button>
            </div>
            <div class="schema-row-info">
              <div class="schema-row-main">
                <span class="schema-row-name">
                  {{
                    getSchemaDisplayName(schemaID) ||
                    getSchemaInfo(schemaID)?.name ||
                    schemaID
                  }}
                </span>
                <span class="schema-row-type">{{
                  getEngineTypeLabel(schemaID)
                }}</span>
                <span
                  v-if="getSchemaVersion(schemaID)"
                  class="schema-row-version"
                >
                  v{{ getSchemaVersion(schemaID) }}
                </span>
                <span
                  v-if="getSchemaInfo(schemaID)?.error"
                  class="schema-row-error"
                  :title="getSchemaInfo(schemaID)?.error"
                >
                  异常
                </span>
              </div>
              <div class="schema-row-sub">
                <template v-if="getSchemaInfo(schemaID)?.error">
                  <span class="schema-error-msg">{{
                    getSchemaInfo(schemaID)?.error
                  }}</span>
                </template>
                <template v-else>
                  {{ getSchemaSubtitle(schemaID) }}
                </template>
              </div>
            </div>
            <div class="schema-row-actions">
              <button
                class="btn-icon btn-detail"
                @click.stop="openSchemaDetail(schemaID)"
                title="查看详情"
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                  <circle
                    cx="8"
                    cy="8"
                    r="7"
                    stroke="currentColor"
                    stroke-width="1.5"
                  />
                  <path
                    d="M8 7v4"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  />
                  <circle cx="8" cy="5" r="0.75" fill="currentColor" />
                </svg>
              </button>
              <Button
                v-if="schemaID !== activeSchemaID"
                variant="outline"
                size="sm"
                @click.stop="setActiveSchema(schemaID)"
                :disabled="!!getSchemaInfo(schemaID)?.error"
                :title="
                  getSchemaInfo(schemaID)?.error ? '方案异常，无法设为当前' : ''
                "
              >
                设为当前
              </Button>
              <span v-else class="schema-active-badge">当前方案</span>
            </div>
          </div>
        </div>
      </div>

      <div
        v-if="!schemaLoading && enabledSchemaIDs.length === 0"
        class="schema-list-empty"
      >
        暂无已启用的方案
      </div>
    </div>

    <!-- 方案管理对话框 -->
    <SchemaManagerDialog
      :visible="showSchemaManager"
      :enabledSchemaIDs="enabledSchemaIDs"
      :allSchemas="allSchemas"
      :schemaConfigs="schemaConfigs"
      :schemaReferences="schemaReferences"
      @close="showSchemaManager = false"
      @enable-schema="enableSchema"
      @disable-schema="disableSchema"
      @schemas-changed="loadAllSchemas"
    />

    <!-- 方案详情对话框 -->
    <Dialog
      :open="!!detailSchemaID"
      @update:open="
        (v: boolean) => {
          if (!v) detailSchemaID = null;
        }
      "
    >
      <DialogContent class="schema-detail-dialog">
        <DialogHeader>
          <DialogTitle>方案详情</DialogTitle>
        </DialogHeader>
        <SchemaDetailPanel
          v-if="detailSchemaID && getSchemaInfo(detailSchemaID)"
          :schema="getSchemaInfo(detailSchemaID)!"
          :config="schemaConfigs[detailSchemaID]"
          :references="schemaReferences[detailSchemaID]"
        />
        <DialogFooter>
          <Button size="sm" @click="detailSchemaID = null">关闭</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <!-- 各方案配置 Card -->
    <template v-for="schemaID in allConfigSchemaIDs" :key="'cfg-' + schemaID">
      <div v-if="schemaConfigs[schemaID]" class="settings-card">
        <div class="card-title">
          <span>{{ getSchemaDisplayName(schemaID) }}</span>
          <span
            v-if="schemaID === activeSchemaID"
            class="theme-badge active"
            style="margin-left: 8px"
            >当前</span
          >
          <span
            v-if="isReferencedOnly(schemaID)"
            class="theme-badge"
            style="
              margin-left: 8px;
              background: var(--warning-bg, #fff3e0);
              color: var(--warning, #e65100);
            "
            >仅被引用</span
          >
          <span
            v-if="getReferencedByNote(schemaID)"
            class="theme-badge"
            style="
              margin-left: 8px;
              background: var(--accent-bg, #e8f0fe);
              color: var(--accent-text, #1a73e8);
            "
            >被 {{ getReferencedByNote(schemaID) }} 引用</span
          >
        </div>

        <!-- 码表类型 -->
        <template v-if="getEngineType(schemaID) === 'codetable'">
          <div class="setting-item">
            <div class="setting-info">
              <label>满码唯一自动上屏</label>
              <p class="setting-hint">
                输入达到最大码长（{{
                  getMaxCodeLength(schemaID)
                }}码）且只有唯一候选时自动上屏
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).auto_commit_unique"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).auto_commit_unique = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>满码空码清空</label>
              <p class="setting-hint">
                输入达到最大码长（{{
                  getMaxCodeLength(schemaID)
                }}码）无匹配时自动清空
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).clear_on_empty_max"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).clear_on_empty_max = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>顶码上屏</label>
              <p class="setting-hint">
                超过最大码长（{{ getMaxCodeLength(schemaID) }}码）时自动上屏首选
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).top_code_commit"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).top_code_commit = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>标点顶码上屏</label>
              <p class="setting-hint">输入标点时自动上屏首选</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).punct_commit"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).punct_commit = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>逐码模式</label>
              <p class="setting-hint">关闭前缀匹配，仅显示精确匹配</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).single_code_input"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).single_code_input = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>显示编码提示</label>
              <p class="setting-hint">在前缀匹配的候选词旁显示剩余编码</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).show_code_hint"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).show_code_hint = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>候选排序</label>
              <p class="setting-hint">候选词的排列方式</p>
            </div>
            <div class="setting-control">
              <Select
                :model-value="getCodetableConfig(schemaID).candidate_sort_mode"
                @update:model-value="
                  (v: string) => {
                    getCodetableConfig(schemaID).candidate_sort_mode = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              >
                <SelectTrigger class="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="frequency">词频优先</SelectItem>
                  <SelectItem value="natural">原始顺序</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>候选去重</label>
              <p class="setting-hint">合并相同文字的多个候选词</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).dedup_candidates"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).dedup_candidates = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>单字不调频</label>
              <p class="setting-hint">
                防止高频单字打乱码表顺序（仅对词频学习模式生效）
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).skip_single_char_freq"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).skip_single_char_freq = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>Z键重复上屏</label>
              <p class="setting-hint">
                输入z时首选为上一次上屏的内容，快速重复输入
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getCodetableConfig(schemaID).z_key_repeat"
                @update:checked="
                  (v: boolean) => {
                    getCodetableConfig(schemaID).z_key_repeat = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>临时拼音</label>
              <p class="setting-hint">
                通过触发键临时切换拼音输入，用于查找不会打的字
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getTempPinyinConfig(schemaID).enabled"
                @update:checked="
                  (v: boolean) => {
                    getTempPinyinConfig(schemaID).enabled = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
        </template>

        <!-- 拼音类型 -->
        <template v-if="getEngineType(schemaID) === 'pinyin'">
          <!-- 双拼方案选择 -->
          <div
            v-if="getPinyinConfig(schemaID).scheme === 'shuangpin'"
            class="setting-item"
          >
            <div class="setting-info">
              <label>双拼方案</label>
              <p class="setting-hint">选择双拼键位布局</p>
            </div>
            <div class="setting-control">
              <Select
                :model-value="getShuangpinLayout(schemaID)"
                @update:model-value="
                  (v: string) => onShuangpinLayoutChange(schemaID, v)
                "
              >
                <SelectTrigger class="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="xiaohe">小鹤双拼</SelectItem>
                  <SelectItem value="ziranma">自然码</SelectItem>
                  <SelectItem value="mspy">微软双拼</SelectItem>
                  <SelectItem value="sogou">搜狗双拼</SelectItem>
                  <SelectItem value="abc">智能ABC</SelectItem>
                  <SelectItem value="ziguang">紫光双拼</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>编码反查提示</label>
              <p class="setting-hint">在候选词旁显示对应的码表编码</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getPinyinConfig(schemaID).show_code_hint"
                @update:checked="
                  (v: boolean) => {
                    getPinyinConfig(schemaID).show_code_hint = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>智能组句</label>
              <p class="setting-hint">使用语言模型优化多字词组匹配</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getPinyinConfig(schemaID).use_smart_compose"
                @update:checked="
                  (v: boolean) => {
                    getPinyinConfig(schemaID).use_smart_compose = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>模糊音</label>
              <p class="setting-hint">
                允许近似发音输入（已启用
                {{ getFuzzyEnabledCount(schemaID) }} 组）
              </p>
            </div>
            <div class="setting-control inline-control">
              <label class="checkbox-label">
                <input
                  type="checkbox"
                  v-model="getFuzzyConfig(schemaID).enabled"
                  @change="onSchemaConfigChange(schemaID)"
                />
                启用
              </label>
              <Button
                variant="outline"
                size="sm"
                :disabled="!getFuzzyConfig(schemaID).enabled"
                @click="openFuzzyDialog(schemaID)"
              >
                配置
              </Button>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>词频学习</label>
              <p class="setting-hint">选择输入法如何学习你的用词习惯</p>
            </div>
            <div class="setting-control">
              <Select
                :model-value="getLearningConfig(schemaID).mode"
                @update:model-value="
                  (v: string) => {
                    getLearningConfig(schemaID).mode = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              >
                <SelectTrigger class="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="manual">不学习</SelectItem>
                  <SelectItem value="frequency">仅调频</SelectItem>
                  <SelectItem value="auto">自动造词</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </template>

        <!-- 混输类型 -->
        <template v-if="getEngineType(schemaID) === 'mixed'">
          <!-- 引用式混输：显示引用提示，不显示码表/拼音配置 -->
          <div
            v-if="isMixedWithRef(schemaID)"
            class="setting-item"
            style="
              background: var(--bg-secondary, #f5f5f5);
              border-radius: 6px;
              padding: 10px 14px;
              margin-bottom: 12px;
            "
          >
            <div class="setting-info" style="flex: 1">
              <label style="font-weight: 500">引用方案</label>
              <p class="setting-hint">
                {{
                  getReferenceNote(schemaID)
                }}。如需修改码表或拼音配置，请在对应方案中设置。
              </p>
            </div>
          </div>

          <!-- 非引用式混输：显示完整的码表和拼音配置 -->
          <template v-if="!isMixedWithRef(schemaID)">
            <!-- 码表配置区 -->
            <div class="setting-section-title">码表设置</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>显示编码提示</label>
                <p class="setting-hint">在前缀匹配的候选词旁显示剩余编码</p>
              </div>
              <div class="setting-control">
                <Switch
                  :checked="getCodetableConfig(schemaID).show_code_hint"
                  @update:checked="
                    (v: boolean) => {
                      getCodetableConfig(schemaID).show_code_hint = v;
                      onSchemaConfigChange(schemaID);
                    }
                  "
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>标点顶码上屏</label>
                <p class="setting-hint">输入标点时自动上屏首选</p>
              </div>
              <div class="setting-control">
                <Switch
                  :checked="getCodetableConfig(schemaID).punct_commit"
                  @update:checked="
                    (v: boolean) => {
                      getCodetableConfig(schemaID).punct_commit = v;
                      onSchemaConfigChange(schemaID);
                    }
                  "
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>候选排序</label>
                <p class="setting-hint">码表候选词的排列方式</p>
              </div>
              <div class="setting-control">
                <Select
                  :model-value="
                    getCodetableConfig(schemaID).candidate_sort_mode
                  "
                  @update:model-value="
                    (v: string) => {
                      getCodetableConfig(schemaID).candidate_sort_mode = v;
                      onSchemaConfigChange(schemaID);
                    }
                  "
                >
                  <SelectTrigger class="w-[140px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="frequency">词频优先</SelectItem>
                    <SelectItem value="natural">原始顺序</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>

            <!-- 拼音配置区 -->
            <div class="setting-section-title">拼音设置</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>编码反查提示</label>
                <p class="setting-hint">在拼音候选词旁显示对应的码表编码</p>
              </div>
              <div class="setting-control">
                <Switch
                  :checked="getPinyinConfig(schemaID).show_code_hint"
                  @update:checked="
                    (v: boolean) => {
                      getPinyinConfig(schemaID).show_code_hint = v;
                      onSchemaConfigChange(schemaID);
                    }
                  "
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>智能组句</label>
                <p class="setting-hint">使用语言模型优化多字词组匹配</p>
              </div>
              <div class="setting-control">
                <Switch
                  :checked="getPinyinConfig(schemaID).use_smart_compose"
                  @update:checked="
                    (v: boolean) => {
                      getPinyinConfig(schemaID).use_smart_compose = v;
                      onSchemaConfigChange(schemaID);
                    }
                  "
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>模糊音</label>
                <p class="setting-hint">
                  允许近似发音输入（已启用
                  {{ getFuzzyEnabledCount(schemaID) }} 组）
                </p>
              </div>
              <div class="setting-control inline-control">
                <label class="checkbox-label">
                  <input
                    type="checkbox"
                    v-model="getFuzzyConfig(schemaID).enabled"
                    @change="onSchemaConfigChange(schemaID)"
                  />
                  启用
                </label>
                <button
                  class="btn btn-sm"
                  :disabled="!getFuzzyConfig(schemaID).enabled"
                  @click="openFuzzyDialog(schemaID)"
                >
                  配置
                </button>
              </div>
            </div>
          </template>
          <!-- /非引用式混输配置 -->

          <!-- 混输专属配置区（引用式和非引用式都显示） -->
          <div class="setting-section-title">混输设置</div>
          <div class="setting-item">
            <div class="setting-info">
              <label>词频学习</label>
              <p class="setting-hint">选择输入法如何学习你的用词习惯</p>
            </div>
            <div class="setting-control">
              <Select
                :model-value="getLearningConfig(schemaID).mode"
                @update:model-value="
                  (v: string) => {
                    getLearningConfig(schemaID).mode = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              >
                <SelectTrigger class="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="manual">不学习</SelectItem>
                  <SelectItem value="frequency">仅调频</SelectItem>
                  <SelectItem value="auto">自动造词</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>拼音最小触发长度</label>
              <p class="setting-hint">
                输入几码后开始查询拼音候选（1=始终查询，2=两码起查询）
              </p>
            </div>
            <div class="setting-control">
              <Select
                :model-value="
                  String(getMixedConfig(schemaID).min_pinyin_length)
                "
                @update:model-value="
                  (v: string) => {
                    getMixedConfig(schemaID).min_pinyin_length = Number(v);
                    onSchemaConfigChange(schemaID);
                  }
                "
              >
                <SelectTrigger class="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="1">1码</SelectItem>
                  <SelectItem value="2">2码</SelectItem>
                  <SelectItem value="3">3码</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>显示来源标记</label>
              <p class="setting-hint">在拼音候选旁显示"拼"标记以区分来源</p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getMixedConfig(schemaID).show_source_hint"
                @update:checked="
                  (v: boolean) => {
                    getMixedConfig(schemaID).show_source_hint = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>简拼匹配</label>
              <p class="setting-hint">
                允许输入声母缩写查找拼音候选（如 bg 匹配"不过"）
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getMixedConfig(schemaID).enable_abbrev_match"
                @update:checked="
                  (v: boolean) => {
                    getMixedConfig(schemaID).enable_abbrev_match = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>Z键重复上屏</label>
              <p class="setting-hint">
                输入z时首选为上一次上屏的内容，快速重复输入
              </p>
            </div>
            <div class="setting-control">
              <Switch
                :checked="getMixedConfig(schemaID).z_key_repeat"
                @update:checked="
                  (v: boolean) => {
                    getMixedConfig(schemaID).z_key_repeat = v;
                    onSchemaConfigChange(schemaID);
                  }
                "
              />
            </div>
          </div>
        </template>
      </div>
    </template>

    <!-- 模糊音配置对话框 -->
    <Dialog :open="showFuzzyDialog" @update:open="showFuzzyDialog = $event">
      <DialogContent>
        <DialogHeader>
          <DialogTitle>模糊音配置</DialogTitle>
        </DialogHeader>
        <div class="fuzzy-pairs-grid">
          <label
            class="fuzzy-pair-item"
            v-for="pair in fuzzyPairs"
            :key="pair.field"
          >
            <input
              type="checkbox"
              v-model="(getFuzzyConfig(fuzzyEditSchemaID) as any)[pair.field]"
              @change="onSchemaConfigChange(fuzzyEditSchemaID)"
            />
            <span class="fuzzy-pair-label">{{ pair.label }}</span>
            <span class="fuzzy-pair-example">{{ pair.example }}</span>
          </label>
        </div>
        <DialogFooter>
          <Button variant="outline" size="sm" @click="setAllFuzzyPairs(true)"
            >全选</Button
          >
          <Button variant="outline" size="sm" @click="setAllFuzzyPairs(false)"
            >全不选</Button
          >
          <Button size="sm" @click="showFuzzyDialog = false">确定</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </section>
</template>

<style scoped>
/* Schema list card */
.schema-list-card {
  padding-bottom: 8px;
}
.schema-list-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.schema-list-hint {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
  margin-bottom: 12px;
  text-align: left;
}
.schema-list-loading,
.schema-list-empty {
  text-align: center;
  padding: 24px;
  color: hsl(var(--muted-foreground));
}

/* Schema list */
.schema-list {
  border: 1px solid hsl(var(--border) / 0.5);
  border-radius: 8px;
  overflow: hidden;
}

/* Schema item */
.schema-item {
  border-bottom: 1px solid hsl(var(--border) / 0.5);
}
.schema-item:last-child {
  border-bottom: none;
}

/* Schema row */
.schema-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 14px;
  transition: background-color 0.15s;
}

/* Sort buttons */
.schema-sort-btns {
  display: flex;
  flex-direction: column;
  gap: 1px;
  flex-shrink: 0;
}
.schema-sort-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 14px;
  border: none;
  background: none;
  color: hsl(var(--muted-foreground));
  font-size: 9px;
  cursor: pointer;
  border-radius: 3px;
  padding: 0;
  line-height: 1;
  transition: all 0.15s;
}
.schema-sort-btn:hover:not(:disabled) {
  background: hsl(var(--border));
  color: hsl(var(--foreground));
}
.schema-sort-btn:disabled {
  opacity: 0.25;
  cursor: default;
}

/* Schema row info (two lines) */
.schema-row-info {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}
.schema-row-main {
  display: flex;
  align-items: center;
  gap: 8px;
}
.schema-row-name {
  font-size: 14px;
  font-weight: 500;
  color: hsl(var(--foreground));
}
.schema-row-type {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: hsl(var(--secondary));
  color: hsl(var(--muted-foreground));
  flex-shrink: 0;
}
.schema-row-version {
  font-size: 11px;
  color: hsl(var(--muted-foreground));
  flex-shrink: 0;
}
.schema-row-error {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: hsl(var(--destructive) / 0.1);
  color: hsl(var(--destructive));
  flex-shrink: 0;
  font-weight: 500;
}
.schema-error-msg {
  color: hsl(var(--destructive));
}
.schema-row-sub {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Schema detail dialog */
.schema-detail-dialog {
  width: 420px;
  max-width: 90vw;
}

/* Schema row actions */
.schema-row-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.btn-detail {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  background: none;
  color: hsl(var(--muted-foreground));
  cursor: pointer;
  border-radius: 6px;
  transition: all 0.15s;
  padding: 0;
}
.btn-detail:hover {
  background: hsl(var(--secondary));
  color: hsl(var(--primary));
}
.schema-active-badge {
  font-size: 12px;
  font-weight: 500;
  color: hsl(var(--primary));
  padding: 4px 10px;
  background: hsl(var(--primary) / 0.1);
  border-radius: 6px;
}

/* 混输设置分区标题 */
.setting-section-title {
  font-size: 13px;
  font-weight: 600;
  color: hsl(var(--muted-foreground));
  padding: 10px 0 4px 0;
  border-top: 1px solid hsl(var(--secondary));
  margin-top: 4px;
}
.setting-section-title:first-child {
  border-top: none;
  margin-top: 0;
}
</style>
