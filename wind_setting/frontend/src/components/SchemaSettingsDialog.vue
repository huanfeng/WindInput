<script setup lang="ts">
import { ref, watch, computed } from "vue";
import type { SchemaConfig, SchemaInfo, SchemaReference } from "../api/wails";
import * as wailsApi from "../api/wails";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import SchemaEngineRenderer from "@/components/SchemaEngineRenderer.vue";
import { engineSchema } from "@/schemas/engine.schema";
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
  visible: boolean;
  schemaID: string;
  schemaConfig: SchemaConfig | null;
  schemaInfo?: SchemaInfo;
  schemaReferences?: Record<string, SchemaReference>;
  allSchemaConfigs?: Record<string, SchemaConfig>;
}>();

const emit = defineEmits<{
  "update:visible": [value: boolean];
  configSave: [schemaID: string, config: SchemaConfig];
  configReset: [schemaID: string];
  dictChanged: [];
}>();

// 本地编辑副本（不直接修改 props）
const localConfig = ref<SchemaConfig | null>(null);

// 码表设置 Tab（基础 / 高级）
const codetableTab = ref<'basic' | 'advanced'>('basic');

// 附加词库开关相关
const togglingDict = ref<string | null>(null);
const dictWasToggled = ref(false);

const extraDicts = computed(
  () => localConfig.value?.dictionaries?.filter((d) => !d.default) ?? [],
);

function isDictEnabled(d: NonNullable<SchemaConfig['dictionaries']>[number]): boolean {
  if (d.enabled !== undefined && d.enabled !== null) return d.enabled;
  if (d.default_enabled !== undefined && d.default_enabled !== null) return d.default_enabled;
  return true;
}

function dictDisplayLabel(d: NonNullable<SchemaConfig['dictionaries']>[number]): string {
  return d.label || d.id;
}

async function toggleDict(dictID: string, currentEnabled: boolean): Promise<void> {
  if (togglingDict.value) return;
  togglingDict.value = dictID;
  try {
    await wailsApi.setDictEnabled(props.schemaID, dictID, !currentEnabled);
    const dict = localConfig.value?.dictionaries?.find((d) => d.id === dictID);
    if (dict) dict.enabled = !currentEnabled;
    dictWasToggled.value = true;
  } finally {
    togglingDict.value = null;
  }
}

// 对话框打开时深拷贝配置
watch(() => props.visible, (val) => {
  if (val && props.schemaConfig) {
    localConfig.value = JSON.parse(JSON.stringify(props.schemaConfig));
    dictWasToggled.value = false;
  }
});

// 当父组件重新加载配置后（恢复默认触发），同步更新本地副本
watch(() => props.schemaConfig, (newVal) => {
  if (props.visible && newVal) {
    localConfig.value = JSON.parse(JSON.stringify(newVal));
  }
}, { deep: true });

const resetting = ref(false);

async function resetSchemaDefaults() {
  if (!props.schemaID || resetting.value) return;

  resetting.value = true;
  try {
    await wailsApi.resetSchemaConfig(props.schemaID);
    emit("configReset", props.schemaID);
  } catch (e) {
    console.error("重置方案配置失败", e);
  } finally {
    resetting.value = false;
  }
}

function saveConfig() {
  if (localConfig.value) {
    emit("configSave", props.schemaID, localConfig.value);
    emit("update:visible", false);
  }
}

function cancelEdit() {
  emit("update:visible", false);
  if (dictWasToggled.value) {
    emit("dictChanged");
  }
}

// 模糊音对话框
const showFuzzyDialog = ref(false);
const fuzzyEditSchemaID = ref("");

// 双拼方案名称映射
const shuangpinLayoutNames: Record<string, string> = {
  xiaohe: "小鹤双拼",
  ziranma: "自然码",
  mspy: "微软双拼",
  sogou: "搜狗双拼",
  abc: "智能ABC",
  ziguang: "紫光双拼",
};

// 模糊音配对列表
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

// 获取方案的引擎类型
function getEngineType(schemaID: string): string {
  const cfg = schemaID === props.schemaID ? localConfig.value : props.allSchemaConfigs?.[schemaID];
  return cfg?.engine?.type || "";
}

// 获取方案显示名称
function getSchemaDisplayName(schemaID: string): string {
  const cfg = schemaID === props.schemaID ? localConfig.value : props.allSchemaConfigs?.[schemaID];
  if (!cfg) return schemaID;
  const baseName = cfg.schema?.name || schemaID;
  if (cfg.engine?.pinyin?.scheme === "shuangpin") {
    return `${baseName} · ${getShuangpinLayoutName(schemaID)}`;
  }
  return baseName;
}

// 判断方案是否为引用式混输
function isMixedWithRef(schemaID: string): boolean {
  const ref = props.schemaReferences?.[schemaID];
  return !!(ref && (ref.primary_schema || ref.secondary_schema));
}

// 获取方案的引用信息文案
function getReferenceNote(schemaID: string): string {
  const ref = props.schemaReferences?.[schemaID];
  if (!ref) return "";
  const parts: string[] = [];
  if (ref.primary_schema)
    parts.push(`码表: ${getSchemaDisplayName(ref.primary_schema)}`);
  if (ref.secondary_schema)
    parts.push(`拼音: ${getSchemaDisplayName(ref.secondary_schema)}`);
  return parts.join(", ");
}

// 码表配置
function getCodetableConfig(schemaID: string) {
  const cfg = schemaID === props.schemaID ? localConfig.value : props.allSchemaConfigs?.[schemaID];
  if (!cfg) return {} as any;
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
  const cfg = schemaID === props.schemaID ? localConfig.value : props.allSchemaConfigs?.[schemaID];
  if (!cfg) return {} as any;
  if (!cfg.engine.pinyin) cfg.engine.pinyin = {};
  return cfg.engine.pinyin;
}

// 混输配置
function getMixedConfig(schemaID: string) {
  const cfg = schemaID === props.schemaID ? localConfig.value : props.allSchemaConfigs?.[schemaID];
  if (!cfg) return {} as any;
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
function getLearningConfig(schemaID: string): {
  auto_learn: { enabled: boolean };
  freq: { enabled: boolean; protect_top_n?: number };
  protect_top_n?: number;
  temp_promote_count?: number;
} {
  const cfg = schemaID === props.schemaID ? localConfig.value : props.allSchemaConfigs?.[schemaID];
  if (!cfg) return { auto_learn: { enabled: false }, freq: { enabled: false } };
  if (!cfg.learning) (cfg as any).learning = {};
  const learning = cfg.learning as any;
  if (!learning.auto_learn) learning.auto_learn = { enabled: false };
  if (!learning.freq) learning.freq = { enabled: false };
  return learning;
}

function getFuzzyConfig(schemaID: string) {
  const py = getPinyinConfig(schemaID);
  if (!py.fuzzy) py.fuzzy = {};
  return py.fuzzy;
}

function getShuangpinLayout(schemaID: string): string {
  const py = getPinyinConfig(schemaID);
  return py.shuangpin?.layout || "xiaohe";
}

function getShuangpinLayoutName(schemaID: string): string {
  const layout = getShuangpinLayout(schemaID);
  return shuangpinLayoutNames[layout] || layout;
}

function onShuangpinLayoutChange(schemaID: string, value: string) {
  const py = getPinyinConfig(schemaID);
  if (!py.shuangpin) py.shuangpin = {};
  py.shuangpin.layout = value;
}

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
}

// 引擎类型标签
function getEngineTypeLabel(schemaID: string): string {
  const type = getEngineType(schemaID);
  const labels: Record<string, string> = {
    codetable: "码表",
    pinyin: "拼音",
    mixed: "混输",
  };
  return labels[type] || type || "";
}

// 是否被其他方案引用
function isReferencedBy(schemaID: string): boolean {
  const ref = props.schemaReferences?.[schemaID];
  return !!(ref?.referenced_by?.length);
}
</script>

<template>
  <Dialog
    :open="visible"
    @update:open="(v: boolean) => emit('update:visible', v)"
  >
    <DialogContent class="schema-settings-dialog">
      <DialogHeader>
        <DialogTitle class="dialog-title-row">
          <span>{{ schemaConfig?.schema?.name || schemaID }}</span>
          <span v-if="getEngineTypeLabel(schemaID)" class="engine-type-badge">
            {{ getEngineTypeLabel(schemaID) }}
          </span>
          <span
            v-if="isReferencedBy(schemaID)"
            class="ref-badge"
          >
            被引用
          </span>
        </DialogTitle>
      </DialogHeader>

      <div class="dialog-body">
        <!-- 码表类型 -->
        <template v-if="getEngineType(schemaID) === 'codetable'">
          <div class="settings-tab-nav">
            <button class="settings-tab-btn" :class="{ active: codetableTab === 'basic' }" @click="codetableTab = 'basic'">基础</button>
            <button class="settings-tab-btn" :class="{ active: codetableTab === 'advanced' }" @click="codetableTab = 'advanced'">高级</button>
          </div>
          <div v-show="codetableTab === 'basic'">
            <SchemaEngineRenderer
              v-if="localConfig"
              :schema="engineSchema"
              :model-value="localConfig"
              engine-type="codetable"
              active-tab="basic"
            />
            <!-- 附加词库开关（仅有附加词库时显示） -->
            <template v-if="extraDicts.length > 0">
              <div class="setting-section-title">附加词库</div>
              <div
                v-for="d in extraDicts"
                :key="d.id"
                class="setting-item"
              >
                <div class="setting-info">
                  <label>{{ dictDisplayLabel(d) }}</label>
                </div>
                <div class="setting-control">
                  <Switch
                    :checked="isDictEnabled(d)"
                    :disabled="!!togglingDict"
                    @update:checked="toggleDict(d.id, isDictEnabled(d))"
                  />
                </div>
              </div>
            </template>
          </div>
          <div v-show="codetableTab === 'advanced'">
            <div class="advanced-warning">
              ⚠ 此页选项通常由词库作者预设。修改后可能导致候选顺序异常或词库行为不符合预期，请谨慎调整。
            </div>
            <SchemaEngineRenderer
              v-if="localConfig"
              :schema="engineSchema"
              :model-value="localConfig"
              engine-type="codetable"
              active-tab="advanced"
            />
          </div>
        </template>

        <!-- 拼音类型 -->
        <template v-if="getEngineType(schemaID) === 'pinyin'">
          <SchemaEngineRenderer
            v-if="localConfig"
            :schema="engineSchema"
            :model-value="localConfig"
            engine-type="pinyin"
          />
          <!-- 模糊音（checkbox+button 组合，不在 schema 中） -->
          <div class="setting-item">
            <div class="setting-info">
              <label>模糊音</label>
              <p class="setting-hint">允许近似发音输入（已启用 {{ getFuzzyEnabledCount(schemaID) }} 组）</p>
            </div>
            <div class="setting-control inline-control">
              <label class="checkbox-label">
                <input type="checkbox" v-model="getFuzzyConfig(schemaID).enabled" />
                启用
              </label>
              <Button variant="outline" size="sm" :disabled="!getFuzzyConfig(schemaID).enabled" @click="openFuzzyDialog(schemaID)">
                配置
              </Button>
            </div>
          </div>
        </template>

        <!-- 混输类型 -->
        <template v-if="getEngineType(schemaID) === 'mixed'">
          <!-- 引用式混输：显示提示 + 手写混输设置 -->
          <template v-if="isMixedWithRef(schemaID)">
            <div
              class="setting-item"
              style="background: var(--bg-secondary, #f5f5f5); border-radius: 6px; padding: 10px 14px; margin-bottom: 12px;"
            >
              <div class="setting-info" style="flex: 1">
                <label style="font-weight: 500">引用方案</label>
                <p class="setting-hint">
                  {{ getReferenceNote(schemaID) }}。如需修改码表或拼音配置，请在对应方案中设置。
                </p>
              </div>
            </div>
            <div class="setting-section-title">混输设置</div>
            <div class="setting-item">
              <div class="setting-info">
                <label>拼音最小触发长度</label>
                <p class="setting-hint">输入几码后开始查询拼音候选（1=始终查询，2=两码起查询）</p>
              </div>
              <div class="setting-control">
                <Select
                  :model-value="String(getMixedConfig(schemaID).min_pinyin_length)"
                  @update:model-value="(v: string) => { getMixedConfig(schemaID).min_pinyin_length = Number(v); }"
                >
                  <SelectTrigger class="w-[140px]"><SelectValue /></SelectTrigger>
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
                  @update:checked="(v: boolean) => { getMixedConfig(schemaID).show_source_hint = v; }"
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>简拼匹配</label>
                <p class="setting-hint">允许输入声母缩写查找拼音候选（如 bg 匹配"不过"）</p>
              </div>
              <div class="setting-control">
                <Switch
                  :checked="getMixedConfig(schemaID).enable_abbrev_match"
                  @update:checked="(v: boolean) => { getMixedConfig(schemaID).enable_abbrev_match = v; }"
                />
              </div>
            </div>
            <div class="setting-item">
              <div class="setting-info">
                <label>Z键重复上屏</label>
                <p class="setting-hint">输入z时首选为上一次上屏的内容，快速重复输入</p>
              </div>
              <div class="setting-control">
                <Switch
                  :checked="getMixedConfig(schemaID).z_key_repeat"
                  @update:checked="(v: boolean) => { getMixedConfig(schemaID).z_key_repeat = v; }"
                />
              </div>
            </div>
          </template>

          <!-- 非引用式混输：schema 渲染全部字段 + 模糊音手写 -->
          <template v-else>
            <SchemaEngineRenderer
              v-if="localConfig"
              :schema="engineSchema"
              :model-value="localConfig"
              engine-type="mixed"
            />
            <!-- 混输拼音模糊音 -->
            <div class="setting-item">
              <div class="setting-info">
                <label>模糊音</label>
                <p class="setting-hint">允许近似发音输入（已启用 {{ getFuzzyEnabledCount(schemaID) }} 组）</p>
              </div>
              <div class="setting-control inline-control">
                <label class="checkbox-label">
                  <input type="checkbox" v-model="getFuzzyConfig(schemaID).enabled" />
                  启用
                </label>
                <button class="btn btn-sm" :disabled="!getFuzzyConfig(schemaID).enabled" @click="openFuzzyDialog(schemaID)">
                  配置
                </button>
              </div>
            </div>
          </template>
        </template>
      </div>

      <DialogFooter class="dialog-footer-elevated">
        <Button
          variant="outline"
          size="sm"
          :disabled="resetting"
          @click="resetSchemaDefaults"
        >
          {{ resetting ? '重置中...' : '恢复默认' }}
        </Button>
        <div style="flex: 1" />
        <Button variant="outline" size="sm" @click="cancelEdit">取消</Button>
        <Button size="sm" @click="saveConfig">确定</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <!-- 模糊音配置子对话框 -->
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
</template>

<style scoped>
.schema-settings-dialog {
  width: 560px;
  max-width: 90vw;
}

.dialog-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.engine-type-badge {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: hsl(var(--secondary));
  color: hsl(var(--muted-foreground));
  font-weight: 400;
}

.ref-badge {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--accent-bg, #e8f0fe);
  color: var(--accent-text, #1a73e8);
  font-weight: 400;
}

.item-disabled {
  opacity: 0.5;
  pointer-events: none;
}

.dialog-body {
  max-height: 70vh;
  overflow-y: auto;
  padding-right: 4px;
}

.dialog-footer-elevated {
  border-top: 1px solid hsl(var(--border) / 0.5);
  box-shadow: 0 -2px 8px hsl(var(--foreground) / 0.06);
  padding-top: 16px;
  margin-top: 8px;
}

/* Setting items */
.setting-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 10px 0;
  border-bottom: 1px solid hsl(var(--border) / 0.4);
}

.setting-item:last-child {
  border-bottom: none;
}

.setting-info {
  flex: 1;
  min-width: 0;
}

.setting-info label {
  font-size: 14px;
  font-weight: 500;
  color: hsl(var(--foreground));
  display: block;
  margin-bottom: 2px;
}

.setting-hint {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
  margin: 0;
  line-height: 1.4;
}

.setting-control {
  flex-shrink: 0;
}

.inline-control {
  display: flex;
  align-items: center;
  gap: 8px;
}

/* Checkbox label */
.checkbox-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  cursor: pointer;
  user-select: none;
}


/* Codetable settings tab nav */
.settings-tab-nav {
  display: flex;
  gap: 0;
  border-bottom: 1px solid hsl(var(--border));
  margin-bottom: 2px;
}

.settings-tab-btn {
  padding: 6px 18px;
  font-size: 13px;
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  cursor: pointer;
  color: hsl(var(--muted-foreground));
  transition: color 0.15s, border-color 0.15s;
}

.settings-tab-btn.active {
  color: hsl(var(--foreground));
  border-bottom-color: hsl(var(--primary));
}

.settings-tab-btn:hover:not(.active) {
  color: hsl(var(--foreground));
}

.advanced-warning {
  margin: 12px 0 4px 0;
  padding: 8px 12px;
  font-size: 12px;
  line-height: 1.5;
  color: #92400e;
  background: #fef3c7;
  border: 1px solid #fcd34d;
  border-radius: 6px;
}

@media (prefers-color-scheme: dark) {
  .advanced-warning {
    color: #fde68a;
    background: rgba(120, 53, 15, 0.25);
    border-color: rgba(252, 211, 77, 0.4);
  }
}

/* Fuzzy pairs grid */
.fuzzy-pairs-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 10px;
  padding: 4px 0;
}

.fuzzy-pair-item {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  padding: 6px 8px;
  border-radius: 6px;
  border: 1px solid hsl(var(--border) / 0.5);
  transition: background 0.15s;
}

.fuzzy-pair-item:hover {
  background: hsl(var(--secondary));
}

.fuzzy-pair-label {
  font-size: 13px;
  font-weight: 500;
  min-width: 60px;
}

.fuzzy-pair-example {
  font-size: 11px;
  color: hsl(var(--muted-foreground));
}
</style>
