<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import type { Config, EngineInfo } from "../api/settings";
import * as wailsApi from "../api/wails";
import type {
  SchemaConfig,
  SchemaInfo,
  SchemaReference,
} from "../api/wails";

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

// 添加方案下拉
const showAddDropdown = ref(false);
const addDropdownRef = ref<HTMLElement | null>(null);

// 模糊音对话框
const showFuzzyDialog = ref(false);
const fuzzyEditSchemaID = ref("");

// 方案引用关系
const schemaReferences = ref<Record<string, SchemaReference>>({});
// 仅通过引用显示的方案ID（不在 available 列表中）
const referencedOnlyIDs = ref<string[]>([]);

// 当前活跃方案 ID
const activeSchemaID = computed(() => props.formData.schema?.active || "");

// 未启用的方案列表
const disabledSchemas = computed(() =>
  allSchemas.value.filter((s) => !enabledSchemaIDs.value.includes(s.id)),
);

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
    } else {
      enabledSchemaIDs.value = schemas.map((s) => s.id);
    }

    for (const id of enabledSchemaIDs.value) {
      await loadSchemaConfig(id);
    }

    // 加载方案引用关系
    try {
      schemaReferences.value = await wailsApi.getSchemaReferences();
      // 加载被引用但未启用的方案配置（仅加载配置，不加入管理列表）
      const refIDs = await wailsApi.getReferencedSchemaIDs();
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
  showAddDropdown.value = false;
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
    if (refBy?.primary_schema === schemaID || refBy?.secondary_schema === schemaID) {
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

function onShuangpinLayoutChange(schemaID: string, event: Event) {
  const target = event.target as HTMLSelectElement;
  const py = getPinyinConfig(schemaID);
  if (!py.shuangpin) py.shuangpin = {};
  py.shuangpin.layout = target.value;
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

function handleDocumentClick(event: MouseEvent) {
  if (
    addDropdownRef.value &&
    !addDropdownRef.value.contains(event.target as Node)
  ) {
    showAddDropdown.value = false;
  }
}

onMounted(() => {
  document.addEventListener("click", handleDocumentClick);
  loadAllSchemas();
});

onUnmounted(() => {
  document.removeEventListener("click", handleDocumentClick);
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
        <div class="schema-add-dropdown" ref="addDropdownRef">
          <button
            class="btn btn-sm btn-primary"
            :disabled="disabledSchemas.length === 0"
            @click="showAddDropdown = !showAddDropdown"
          >
            + 添加方案
          </button>
          <div v-if="showAddDropdown" class="schema-add-menu">
            <div
              v-for="schema in disabledSchemas"
              :key="schema.id"
              class="schema-add-option"
              @click="enableSchema(schema.id)"
            >
              <div class="schema-add-option-main">
                <span class="schema-add-option-name">{{ schema.name }}</span>
                <span class="schema-add-option-type">{{
                  { codetable: "码表", pinyin: "拼音", mixed: "混输" }[
                    schema.engine_type
                  ] || schema.engine_type
                }}</span>
              </div>
              <div v-if="schema.description" class="schema-add-option-desc">
                {{ schema.description }}
              </div>
            </div>
          </div>
        </div>
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
              </div>
              <div class="schema-row-sub">
                {{ getSchemaSubtitle(schemaID) }}
              </div>
            </div>
            <div class="schema-row-actions">
              <button
                v-if="schemaID !== activeSchemaID"
                class="btn btn-sm"
                @click.stop="setActiveSchema(schemaID)"
              >
                设为当前
              </button>
              <span v-else class="schema-active-badge">当前方案</span>
              <button
                v-if="
                  schemaID !== activeSchemaID && enabledSchemaIDs.length > 1
                "
                class="btn-icon btn-delete"
                @click.stop="disableSchema(schemaID)"
                title="移除方案"
              >
                &times;
              </button>
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
            style="margin-left: 8px; background: var(--warning-bg, #fff3e0); color: var(--warning, #e65100)"
            >仅被引用</span
          >
          <span
            v-if="getReferencedByNote(schemaID)"
            class="theme-badge"
            style="margin-left: 8px; background: var(--accent-bg, #e8f0fe); color: var(--accent-text, #1a73e8)"
            >被 {{ getReferencedByNote(schemaID) }} 引用</span
          >
        </div>

        <!-- 码表类型 -->
        <template v-if="getEngineType(schemaID) === 'codetable'">
          <div class="setting-item">
            <div class="setting-info">
              <label>四码唯一自动上屏</label>
              <p class="setting-hint">输入四码且只有唯一候选时自动上屏</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).auto_commit_unique"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>四码空码清空</label>
              <p class="setting-hint">输入四码无匹配时自动清空</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).clear_on_empty_max"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>顶码上屏</label>
              <p class="setting-hint">输入第五码时自动上屏首选</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).top_code_commit"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>标点顶码上屏</label>
              <p class="setting-hint">输入标点时自动上屏首选</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).punct_commit"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>逐码模式</label>
              <p class="setting-hint">关闭前缀匹配，仅显示精确匹配</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).single_code_input"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>显示编码提示</label>
              <p class="setting-hint">在前缀匹配的候选词旁显示剩余编码</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).show_code_hint"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>候选排序</label>
              <p class="setting-hint">候选词的排列方式</p>
            </div>
            <div class="setting-control">
              <select
                v-model="getCodetableConfig(schemaID).candidate_sort_mode"
                @change="onSchemaConfigChange(schemaID)"
              >
                <option value="frequency">词频优先</option>
                <option value="natural">原始顺序</option>
              </select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>候选去重</label>
              <p class="setting-hint">合并相同文字的多个候选词</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).dedup_candidates"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).skip_single_char_freq"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).z_key_repeat"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getTempPinyinConfig(schemaID).enabled"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
              <select
                :value="getShuangpinLayout(schemaID)"
                @change="onShuangpinLayoutChange(schemaID, $event)"
              >
                <option value="xiaohe">小鹤双拼</option>
                <option value="ziranma">自然码</option>
                <option value="mspy">微软双拼</option>
                <option value="sogou">搜狗双拼</option>
                <option value="abc">智能ABC</option>
                <option value="ziguang">紫光双拼</option>
              </select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>编码反查提示</label>
              <p class="setting-hint">在候选词旁显示对应的码表编码</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getPinyinConfig(schemaID).show_code_hint"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>智能组句</label>
              <p class="setting-hint">使用语言模型优化多字词组匹配</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getPinyinConfig(schemaID).use_smart_compose"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getFuzzyConfig(schemaID).enabled"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div v-if="getFuzzyConfig(schemaID).enabled" class="setting-item">
            <div class="setting-info">
              <label>模糊音配置</label>
              <p class="setting-hint">选择需要启用的模糊音组</p>
            </div>
            <div class="setting-control">
              <button class="btn btn-sm" @click="openFuzzyDialog(schemaID)">
                配置
              </button>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>词频学习</label>
              <p class="setting-hint">选择输入法如何学习你的用词习惯</p>
            </div>
            <div class="setting-control">
              <select
                v-model="getLearningConfig(schemaID).mode"
                @change="onSchemaConfigChange(schemaID)"
              >
                <option value="manual">不学习</option>
                <option value="frequency">仅调频</option>
                <option value="auto">自动造词</option>
              </select>
            </div>
          </div>
        </template>

        <!-- 混输类型 -->
        <template v-if="getEngineType(schemaID) === 'mixed'">
          <!-- 引用式混输：显示引用提示，不显示码表/拼音配置 -->
          <div v-if="isMixedWithRef(schemaID)" class="setting-item" style="background: var(--bg-secondary, #f5f5f5); border-radius: 6px; padding: 10px 14px; margin-bottom: 12px;">
            <div class="setting-info" style="flex: 1;">
              <label style="font-weight: 500;">引用方案</label>
              <p class="setting-hint">{{ getReferenceNote(schemaID) }}。如需修改码表或拼音配置，请在对应方案中设置。</p>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).show_code_hint"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>标点顶码上屏</label>
              <p class="setting-hint">输入标点时自动上屏首选</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getCodetableConfig(schemaID).punct_commit"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>候选排序</label>
              <p class="setting-hint">码表候选词的排列方式</p>
            </div>
            <div class="setting-control">
              <select
                v-model="getCodetableConfig(schemaID).candidate_sort_mode"
                @change="onSchemaConfigChange(schemaID)"
              >
                <option value="frequency">词频优先</option>
                <option value="natural">原始顺序</option>
              </select>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getPinyinConfig(schemaID).show_code_hint"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>智能组句</label>
              <p class="setting-hint">使用语言模型优化多字词组匹配</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getPinyinConfig(schemaID).use_smart_compose"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getFuzzyConfig(schemaID).enabled"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
          <div v-if="getFuzzyConfig(schemaID).enabled" class="setting-item">
            <div class="setting-info">
              <label>模糊音配置</label>
              <p class="setting-hint">选择需要启用的模糊音组</p>
            </div>
            <div class="setting-control">
              <button class="btn btn-sm" @click="openFuzzyDialog(schemaID)">
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
              <select
                v-model="getLearningConfig(schemaID).mode"
                @change="onSchemaConfigChange(schemaID)"
              >
                <option value="manual">不学习</option>
                <option value="frequency">仅调频</option>
                <option value="auto">自动造词</option>
              </select>
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
              <select
                v-model.number="getMixedConfig(schemaID).min_pinyin_length"
                @change="onSchemaConfigChange(schemaID)"
              >
                <option :value="1">1码</option>
                <option :value="2">2码</option>
                <option :value="3">3码</option>
              </select>
            </div>
          </div>
          <div class="setting-item">
            <div class="setting-info">
              <label>显示来源标记</label>
              <p class="setting-hint">在拼音候选旁显示"拼"标记以区分来源</p>
            </div>
            <div class="setting-control">
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getMixedConfig(schemaID).show_source_hint"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getMixedConfig(schemaID).enable_abbrev_match"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
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
              <label class="switch">
                <input
                  type="checkbox"
                  v-model="getMixedConfig(schemaID).z_key_repeat"
                  @change="onSchemaConfigChange(schemaID)"
                />
                <span class="slider"></span>
              </label>
            </div>
          </div>
        </template>
      </div>
    </template>

    <!-- 模糊音配置对话框 -->
    <div
      class="dialog-overlay"
      v-if="showFuzzyDialog"
      @click.self="showFuzzyDialog = false"
    >
      <div class="dialog-box dialog-sectioned">
        <div class="dialog-header">
          <h3>模糊音配置</h3>
          <button class="dialog-close" @click="showFuzzyDialog = false">
            ×
          </button>
        </div>
        <div class="dialog-body">
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
        </div>
        <div class="dialog-footer">
          <button class="btn btn-sm" @click="setAllFuzzyPairs(true)">
            全选
          </button>
          <button class="btn btn-sm" @click="setAllFuzzyPairs(false)">
            全不选
          </button>
          <button
            class="btn btn-sm btn-primary"
            @click="showFuzzyDialog = false"
          >
            确定
          </button>
        </div>
      </div>
    </div>
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
  color: #9ca3af;
  margin-bottom: 12px;
  text-align: left;
}
.schema-list-loading,
.schema-list-empty {
  text-align: center;
  padding: 24px;
  color: #9ca3af;
}

/* Schema list */
.schema-list {
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}

/* Schema item */
.schema-item {
  border-bottom: 1px solid #e5e7eb;
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
  color: #c0c4cc;
  font-size: 9px;
  cursor: pointer;
  border-radius: 3px;
  padding: 0;
  line-height: 1;
  transition: all 0.15s;
}
.schema-sort-btn:hover:not(:disabled) {
  background: #e5e7eb;
  color: #374151;
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
  color: #1f2937;
}
.schema-row-type {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: #f3f4f6;
  color: #6b7280;
  flex-shrink: 0;
}
.schema-row-version {
  font-size: 11px;
  color: #9ca3af;
  flex-shrink: 0;
}
.schema-row-sub {
  font-size: 12px;
  color: #9ca3af;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Schema row actions */
.schema-row-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.schema-active-badge {
  font-size: 12px;
  font-weight: 500;
  color: #2563eb;
  padding: 4px 10px;
  background: #eff6ff;
  border-radius: 6px;
}

/* Add schema dropdown */
.schema-add-dropdown {
  position: relative;
}
.schema-add-menu {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 10;
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 10px;
  box-shadow: 0 10px 30px rgba(15, 23, 42, 0.08);
  min-width: 260px;
  max-height: 320px;
  overflow-y: auto;
  padding: 6px;
}
.schema-add-option {
  padding: 10px 12px;
  border-radius: 8px;
  cursor: pointer;
  transition: background-color 0.15s;
}
.schema-add-option:hover {
  background-color: #f3f4f6;
}
.schema-add-option-main {
  display: flex;
  align-items: center;
  gap: 8px;
}
.schema-add-option-name {
  font-size: 13px;
  color: #1f2937;
}
.schema-add-option-type {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: #f3f4f6;
  color: #6b7280;
}
.schema-add-option-desc {
  font-size: 12px;
  color: #9ca3af;
  margin-top: 3px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 混输设置分区标题 */
.setting-section-title {
  font-size: 13px;
  font-weight: 600;
  color: #6b7280;
  padding: 10px 0 4px 0;
  border-top: 1px solid #f0f0f0;
  margin-top: 4px;
}
.setting-section-title:first-child {
  border-top: none;
  margin-top: 0;
}
</style>
