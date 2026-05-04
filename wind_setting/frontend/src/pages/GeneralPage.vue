<script setup lang="ts">
import { ref, computed, onMounted, watch } from "vue";
import type { Config, EngineInfo } from "../api/settings";
import * as wailsApi from "../api/wails";
import type { SchemaConfig, SchemaInfo, SchemaReference } from "../api/wails";
import SchemaDetailPanel from "../components/SchemaDetailPanel.vue";
import SchemaManagerDialog from "../components/SchemaManagerDialog.vue";
import SchemaSettingsDialog from "../components/SchemaSettingsDialog.vue";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

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

// 方案设置对话框
const settingsSchemaID = ref<string | null>(null);
const showSchemaSettings = ref(false);

function openSchemaSettings(schemaID: string) {
  settingsSchemaID.value = schemaID;
  showSchemaSettings.value = true;
}

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

    // 确保主方案有显式值，避免保存时后端依赖列表顺序自动选取
    if (!props.formData.schema.primaryCodetable) {
      const firstCodetable = enabledSchemaIDs.value.find((id) => {
        const info = allSchemas.value.find((s) => s.id === id);
        return info?.engine_type === "codetable";
      });
      if (firstCodetable) {
        props.formData.schema.primaryCodetable = firstCodetable;
      }
    }
    if (!props.formData.schema.primaryPinyin) {
      props.formData.schema.primaryPinyin = "pinyin";
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

async function onSchemaConfigSave(schemaID: string, cfg: SchemaConfig) {
  // 更新本地缓存
  schemaConfigs.value[schemaID] = cfg;
  // 保存到后端
  try {
    await wailsApi.saveSchemaConfig(schemaID, cfg);
  } catch (e) {
    console.error(`保存方案配置失败: ${schemaID}`, e);
  }
}

async function onSchemaConfigReset(schemaID: string) {
  await loadSchemaConfig(schemaID);
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
  const cfg = schemaConfigs.value[schemaID];
  if (!cfg) return "xiaohe";
  return cfg.engine?.pinyin?.shuangpin?.layout || "xiaohe";
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

// 打开方案详情对话框
function openSchemaDetail(schemaID: string) {
  detailSchemaID.value = schemaID;
}

// 外部重置方案列表时（如恢复本页默认），同步 enabledSchemaIDs
watch(
  () => props.formData.schema?.available,
  (newAvailable) => {
    if (!newAvailable || allSchemas.value.length === 0) return;
    const validIDs = newAvailable.filter((id: string) =>
      allSchemas.value.some((s) => s.id === id),
    );
    enabledSchemaIDs.value = validIDs;
    refreshSchemaReferences();
  },
);

onMounted(() => {
  loadAllSchemas();
});

// 已开启的码表方案（用于主码表方案选择）
const enabledCodetableSchemas = computed(() =>
  enabledSchemaIDs.value.filter((id) => {
    const info = allSchemas.value.find((s) => s.id === id);
    return info?.engine_type === "codetable";
  }),
);

// 主拼音方案选项：全拼必然排第一，然后是其他已开启的拼音方案
const pinyinSchemaOptions = computed(() => {
  const options: { id: string; label: string }[] = [
    { id: "pinyin", label: "全拼" },
  ];
  for (const id of enabledSchemaIDs.value) {
    if (id === "pinyin") continue;
    const info = allSchemas.value.find((s) => s.id === id);
    if (info?.engine_type === "pinyin") {
      options.push({
        id,
        label: getSchemaDisplayName(id) || info.name || id,
      });
    }
  }
  return options;
});

// 主码表方案：无"自动"选项，默认第一个已启用的码表方案
const primaryCodetable = computed({
  get: () =>
    props.formData.schema?.primaryCodetable ||
    enabledCodetableSchemas.value[0] ||
    "",
  set: (val: string) => {
    props.formData.schema.primaryCodetable = val;
  },
});

// 主拼音方案：无"自动"选项，默认全拼（"pinyin"）
const primaryPinyin = computed({
  get: () => props.formData.schema?.primaryPinyin || "pinyin",
  set: (val: string) => {
    props.formData.schema.primaryPinyin = val;
  },
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
              <Button
                variant="outline"
                size="sm"
                @click.stop="openSchemaSettings(schemaID)"
                :disabled="!!getSchemaInfo(schemaID)?.error"
              >
                方案设置
              </Button>
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

    <!-- 主方案设置卡片 -->
    <div class="settings-card primary-schema-card">
      <div class="card-title">主方案设置</div>

      <div class="setting-item">
        <div class="setting-info">
          <label>主码表方案</label>
          <p class="setting-hint">拼音方案"反查/编码提示"从此方案的码表派生</p>
        </div>
        <div class="setting-control">
          <Select v-model="primaryCodetable">
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="id in enabledCodetableSchemas"
                :key="id"
                :value="id"
              >
                {{ getSchemaDisplayName(id) || getSchemaInfo(id)?.name || id }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <div class="setting-item">
        <div class="setting-info">
          <label>主拼音方案</label>
          <p class="setting-hint">码表方案"临时拼音"使用此方案</p>
        </div>
        <div class="setting-control">
          <Select v-model="primaryPinyin">
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="opt in pinyinSchemaOptions"
                :key="opt.id"
                :value="opt.id"
              >
                {{ opt.label }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
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

    <!-- 方案设置对话框 -->
    <SchemaSettingsDialog
      :visible="showSchemaSettings"
      :schemaID="settingsSchemaID || ''"
      :schemaConfig="settingsSchemaID ? schemaConfigs[settingsSchemaID] : null"
      :schemaInfo="settingsSchemaID ? getSchemaInfo(settingsSchemaID) : undefined"
      :schemaReferences="schemaReferences"
      :allSchemaConfigs="schemaConfigs"
      @update:visible="showSchemaSettings = $event"
      @configSave="onSchemaConfigSave"
      @configReset="onSchemaConfigReset"
    />

  </section>
</template>

<style scoped>
/* Primary schema card */
.primary-schema-card {
  margin-top: 12px;
}

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

</style>
