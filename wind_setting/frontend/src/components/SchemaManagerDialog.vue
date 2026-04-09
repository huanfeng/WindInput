<script setup lang="ts">
import { ref, computed, watch } from "vue";
import type {
  SchemaConfig,
  SchemaInfo,
  SchemaReference,
  ImportPreview,
} from "../api/wails";
import * as wailsApi from "../api/wails";
import SchemaDetailPanel from "./SchemaDetailPanel.vue";

const props = defineProps<{
  visible: boolean;
  enabledSchemaIDs: string[];
  allSchemas: SchemaInfo[];
  schemaConfigs: Record<string, SchemaConfig>;
  schemaReferences: Record<string, SchemaReference>;
}>();

const emit = defineEmits<{
  close: [];
  enableSchema: [id: string];
  disableSchema: [id: string];
  schemasChanged: [];
}>();

const activeTab = ref<"local" | "online">("local");
const searchQuery = ref("");
const detailSchemaID = ref<string | null>(null);

// Single selection for export (click row)
const selectedID = ref<string | null>(null);

// Export
const exporting = ref(false);
const showExportConfirm = ref(false);
const exportRelatedIDs = ref<string[]>([]);
const exportIncludeRelated = ref(true);
const exportSuccess = ref(false);

// Import
const importPreview = ref<ImportPreview | null>(null);
const importLoading = ref(false);

// Delete
const deleteConfirmID = ref<string | null>(null);
const deleting = ref(false);

// Local configs cache
const localConfigs = ref<Record<string, SchemaConfig>>({});

const engineTypeLabels: Record<string, string> = {
  codetable: "码表",
  pinyin: "拼音",
  mixed: "混输",
};

const sourceLabels: Record<string, string> = {
  builtin: "内置",
  user: "用户",
};

watch(
  () => props.visible,
  (val) => {
    if (val) {
      searchQuery.value = "";
      detailSchemaID.value = null;
      activeTab.value = "local";
      selectedID.value = null;
      importPreview.value = null;
      deleteConfirmID.value = null;
      showExportConfirm.value = false;
      exportSuccess.value = false;
    }
  },
);

// Sorted schemas: builtin first, mixed after their primary
const sortedSchemas = computed(() => {
  const schemas = [...props.allSchemas];
  const mixedPrimaryMap: Record<string, string> = {};
  for (const s of schemas) {
    if (s.engine_type === "mixed") {
      const ref = props.schemaReferences[s.id];
      if (ref?.primary_schema) {
        mixedPrimaryMap[s.id] = ref.primary_schema;
      }
    }
  }
  schemas.sort((a, b) => {
    const srcA = (a as any).source || "builtin";
    const srcB = (b as any).source || "builtin";
    if (srcA !== srcB) return srcA === "builtin" ? -1 : 1;
    if (mixedPrimaryMap[a.id] === b.id) return 1;
    if (mixedPrimaryMap[b.id] === a.id) return -1;
    return a.name.localeCompare(b.name);
  });
  return schemas;
});

const filteredSchemas = computed(() => {
  const q = searchQuery.value.toLowerCase().trim();
  if (!q) return sortedSchemas.value;
  return sortedSchemas.value.filter(
    (s) =>
      s.name.toLowerCase().includes(q) ||
      s.id.toLowerCase().includes(q) ||
      (s.description || "").toLowerCase().includes(q),
  );
});

function isEnabled(schemaID: string): boolean {
  return props.enabledSchemaIDs.includes(schemaID);
}

function canExport(schemaID: string): boolean {
  const schema = props.allSchemas.find((s) => s.id === schemaID);
  return !!schema && schema.engine_type !== "pinyin";
}

function selectSchema(schemaID: string) {
  selectedID.value = selectedID.value === schemaID ? null : schemaID;
  exportSuccess.value = false;
}

function getConfig(schemaID: string): SchemaConfig | undefined {
  return props.schemaConfigs[schemaID] || localConfigs.value[schemaID];
}

function getReference(schemaID: string): SchemaReference | undefined {
  return props.schemaReferences[schemaID];
}

function openDetail(schemaID: string) {
  detailSchemaID.value = schemaID;
  if (!getConfig(schemaID)) {
    wailsApi.getSchemaConfig(schemaID).then((cfg) => {
      localConfigs.value[schemaID] = cfg;
    });
  }
}

function handleToggleEnabled(schemaID: string) {
  if (isEnabled(schemaID)) {
    emit("disableSchema", schemaID);
  } else {
    emit("enableSchema", schemaID);
  }
}

// --- Export ---
// 只处理主码表 <-> 混输的直接关联
function getRelatedMixedID(schemaID: string): string | null {
  const schema = props.allSchemas.find((s) => s.id === schemaID);
  if (!schema) return null;

  if (schema.engine_type === "codetable") {
    // 码表方案：找以它为 primary_schema 的混输方案
    const ref = props.schemaReferences[schemaID];
    if (ref?.referenced_by) {
      for (const refBy of ref.referenced_by) {
        const refByRef = props.schemaReferences[refBy];
        if (refByRef?.primary_schema === schemaID) {
          const refBySchema = props.allSchemas.find((s) => s.id === refBy);
          if (refBySchema?.engine_type === "mixed") {
            return refBy;
          }
        }
      }
    }
  } else if (schema.engine_type === "mixed") {
    // 混输方案：找它的 primary_schema
    const ref = props.schemaReferences[schemaID];
    if (ref?.primary_schema) {
      return ref.primary_schema;
    }
  }
  return null;
}

function handleExportClick() {
  if (!selectedID.value) return;
  const relatedID = getRelatedMixedID(selectedID.value);
  if (relatedID) {
    exportRelatedIDs.value = [relatedID];
    exportIncludeRelated.value = true;
    showExportConfirm.value = true;
  } else {
    doExport([selectedID.value]);
  }
}

function confirmExport() {
  if (!selectedID.value) return;
  const ids = [selectedID.value];
  if (exportIncludeRelated.value && exportRelatedIDs.value.length > 0) {
    ids.push(exportRelatedIDs.value[0]);
  }
  showExportConfirm.value = false;
  doExport(ids);
}

async function doExport(ids: string[]) {
  exporting.value = true;
  try {
    const path = await wailsApi.exportSchemas(ids);
    if (path) {
      exportSuccess.value = true;
      selectedID.value = null;
    }
  } catch (e) {
    console.error("导出方案失败", e);
  } finally {
    exporting.value = false;
  }
}

// --- Import ---
async function handleImportPreview() {
  importLoading.value = true;
  try {
    const preview = await wailsApi.previewImportSchema();
    if (preview) {
      importPreview.value = preview;
    }
  } catch (e) {
    console.error("预览导入方案失败", e);
  } finally {
    importLoading.value = false;
  }
}

async function confirmImport() {
  if (!importPreview.value) return;
  importLoading.value = true;
  try {
    const result = await wailsApi.confirmImportSchema(
      importPreview.value.zip_path,
    );
    if (result) {
      emit("schemasChanged");
    }
    importPreview.value = null;
  } catch (e) {
    console.error("导入方案失败", e);
  } finally {
    importLoading.value = false;
  }
}

function cancelImport() {
  importPreview.value = null;
}

// --- Delete ---
const deleteRelatedIDs = ref<string[]>([]);

function getDependentMixedIDs(schemaID: string): string[] {
  const result: string[] = [];
  const ref = props.schemaReferences[schemaID];
  if (!ref?.referenced_by) return result;
  for (const refBy of ref.referenced_by) {
    const refByRef = props.schemaReferences[refBy];
    // 只找以此方案为 primary_schema 的混输方案
    if (refByRef?.primary_schema === schemaID) {
      const refBySchema = props.allSchemas.find((s) => s.id === refBy);
      if (refBySchema?.engine_type === "mixed" && (refBySchema as any).source === "user") {
        result.push(refBy);
      }
    }
  }
  return result;
}

function requestDelete(schemaID: string) {
  deleteConfirmID.value = schemaID;
  deleteRelatedIDs.value = getDependentMixedIDs(schemaID);
}

async function confirmDelete() {
  if (!deleteConfirmID.value) return;
  deleting.value = true;
  try {
    // 先删除依赖的混输方案
    for (const rid of deleteRelatedIDs.value) {
      await wailsApi.deleteSchema(rid);
      if (selectedID.value === rid) selectedID.value = null;
    }
    // 再删除主方案
    await wailsApi.deleteSchema(deleteConfirmID.value);
    if (selectedID.value === deleteConfirmID.value) {
      selectedID.value = null;
    }
    emit("schemasChanged");
    deleteConfirmID.value = null;
    deleteRelatedIDs.value = [];
  } catch (e) {
    console.error("删除方案失败", e);
  } finally {
    deleting.value = false;
  }
}

function cancelDelete() {
  deleteConfirmID.value = null;
}

function getSchemaName(id: string): string {
  return props.allSchemas.find((s) => s.id === id)?.name || id;
}

function hasConflict(): boolean {
  return (
    importPreview.value?.schemas?.some((s) => s.conflict) ?? false
  );
}

function close() {
  emit("close");
}
</script>

<template>
  <div v-if="visible" class="dialog-overlay" @click.self="close">
    <div class="dialog-box dialog-sectioned schema-manager-dialog">
      <div class="dialog-header">
        <h3>方案管理</h3>
        <button class="dialog-close" @click="close">&times;</button>
      </div>

      <div class="schema-mgr-tabs">
        <button
          class="schema-mgr-tab"
          :class="{ active: activeTab === 'local' }"
          @click="activeTab = 'local'"
        >
          本地方案
        </button>
        <button
          class="schema-mgr-tab"
          :class="{ active: activeTab === 'online' }"
          @click="activeTab = 'online'"
        >
          在线下载
        </button>
      </div>

      <div class="dialog-body schema-mgr-body">
        <template v-if="activeTab === 'local'">
          <div class="schema-mgr-search">
            <input
              type="text"
              v-model="searchQuery"
              placeholder="搜索方案..."
              class="input"
            />
          </div>

          <div class="schema-mgr-list">
            <div
              v-for="schema in filteredSchemas"
              :key="schema.id"
              class="schema-mgr-item"
              :class="{ 'schema-mgr-item-selected': selectedID === schema.id }"
              @click="selectSchema(schema.id)"
            >
              <div class="schema-mgr-row">
                <div class="schema-mgr-info">
                  <div class="schema-mgr-main">
                    <span class="schema-mgr-name">{{ schema.name }}</span>
                    <span class="schema-mgr-type">{{
                      engineTypeLabels[schema.engine_type] ||
                      schema.engine_type
                    }}</span>
                    <span v-if="schema.version" class="schema-mgr-version"
                      >v{{ schema.version }}</span
                    >
                    <span
                      class="schema-mgr-source"
                      :class="'source-' + ((schema as any).source || 'builtin')"
                    >
                      {{ sourceLabels[(schema as any).source] || "内置" }}
                    </span>
                    <span v-if="schema.error" class="schema-mgr-error-badge"
                      >异常</span
                    >
                  </div>
                  <div v-if="schema.description" class="schema-mgr-desc">
                    {{ schema.description }}
                  </div>
                </div>
                <div class="schema-mgr-actions" @click.stop>
                  <button
                    class="btn-icon schema-mgr-info-btn"
                    @click="openDetail(schema.id)"
                    title="查看详情"
                  >
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                      <circle cx="8" cy="8" r="7" stroke="currentColor" stroke-width="1.5" />
                      <path d="M8 7v4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
                      <circle cx="8" cy="5" r="0.75" fill="currentColor" />
                    </svg>
                  </button>
                  <label class="switch switch-sm" title="启用/禁用">
                    <input
                      type="checkbox"
                      :checked="isEnabled(schema.id)"
                      @change="handleToggleEnabled(schema.id)"
                    />
                    <span class="slider"></span>
                  </label>
                  <button
                    v-if="(schema as any).source === 'user'"
                    class="btn-icon schema-mgr-delete-btn"
                    @click="requestDelete(schema.id)"
                    title="删除方案"
                  >
                    <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
                      <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
                    </svg>
                  </button>
                </div>
              </div>
            </div>
            <div v-if="filteredSchemas.length === 0" class="schema-mgr-empty">
              {{ searchQuery ? "没有匹配的方案" : "暂无可用方案" }}
            </div>
          </div>

        </template>

        <template v-if="activeTab === 'online'">
          <div class="schema-mgr-placeholder">
            <div class="schema-mgr-placeholder-icon">
              <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="#9ca3af" stroke-width="1.5">
                <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2z" />
                <path d="M2 12h20M12 2c2.5 3 4 6.5 4 10s-1.5 7-4 10c-2.5-3-4-6.5-4-10s1.5-7 4-10z" />
              </svg>
            </div>
            <p class="schema-mgr-placeholder-text">在线方案下载功能即将推出</p>
            <p class="schema-mgr-placeholder-hint">届时可从方案仓库浏览和下载第三方输入方案</p>
          </div>
        </template>
      </div>

      <div class="dialog-footer schema-mgr-footer">
        <div class="schema-mgr-footer-left">
          <button
            class="btn btn-sm"
            :disabled="importLoading"
            @click="handleImportPreview"
          >
            {{ importLoading ? "处理中..." : "导入方案" }}
          </button>
          <button
            class="btn btn-sm"
            :disabled="exporting || !selectedID || !canExport(selectedID!)"
            @click="handleExportClick"
            :title="selectedID && !canExport(selectedID) ? '拼音方案暂不支持导出' : ''"
          >
            {{ exporting ? "导出中..." : "导出方案" }}
          </button>
        </div>
        <div class="schema-mgr-footer-center">
          <span v-if="exportSuccess" class="schema-mgr-footer-toast">导出成功</span>
          <span v-else-if="selectedID && !canExport(selectedID)" class="schema-mgr-footer-hint">拼音方案暂不支持导出</span>
          <span v-else-if="selectedID" class="schema-mgr-footer-hint">已选中「{{ getSchemaName(selectedID) }}」</span>
        </div>
        <button class="btn btn-sm" @click="close">关闭</button>
      </div>
    </div>

    <!-- 方案详情对话框 -->
    <div
      v-if="detailSchemaID"
      class="dialog-overlay schema-nested-overlay"
      @click.self="detailSchemaID = null"
    >
      <div class="dialog-box dialog-sectioned schema-detail-dialog">
        <div class="dialog-header">
          <h3>方案详情</h3>
          <button class="dialog-close" @click="detailSchemaID = null">&times;</button>
        </div>
        <div class="dialog-body">
          <SchemaDetailPanel
            v-if="allSchemas.find((s) => s.id === detailSchemaID)"
            :schema="allSchemas.find((s) => s.id === detailSchemaID)!"
            :config="getConfig(detailSchemaID)"
            :references="getReference(detailSchemaID)"
          />
        </div>
        <div class="dialog-footer">
          <button class="btn btn-sm btn-primary" @click="detailSchemaID = null">关闭</button>
        </div>
      </div>
    </div>

    <!-- 导出确认对话框（有关联方案时） -->
    <div
      v-if="showExportConfirm"
      class="dialog-overlay schema-nested-overlay"
      @click.self="showExportConfirm = false"
    >
      <div class="dialog-box" style="max-width: 400px">
        <div class="dialog-title">导出方案</div>
        <div style="font-size: 14px; color: #374151; padding: 4px 0 12px">
          <p style="margin-bottom: 8px">
            方案「{{ getSchemaName(selectedID!) }}」存在关联方案：
          </p>
          <ul style="margin: 0; padding-left: 20px; color: #6b7280; font-size: 13px">
            <li v-for="rid in exportRelatedIDs" :key="rid">
              {{ getSchemaName(rid) }}
              ({{ engineTypeLabels[allSchemas.find((s) => s.id === rid)?.engine_type || ''] || '' }})
            </li>
          </ul>
        </div>
        <label
          style="display: flex; align-items: center; gap: 8px; font-size: 13px; color: #374151; padding: 4px 0 12px; cursor: pointer"
        >
          <input type="checkbox" v-model="exportIncludeRelated" style="accent-color: #2563eb" />
          一起导出关联方案
        </label>
        <div class="dialog-actions">
          <button class="btn" @click="showExportConfirm = false">取消</button>
          <button class="btn btn-primary" @click="confirmExport">确认导出</button>
        </div>
      </div>
    </div>

    <!-- 导入预览对话框 -->
    <div
      v-if="importPreview"
      class="dialog-overlay schema-nested-overlay"
      @click.self="cancelImport"
    >
      <div class="dialog-box dialog-sectioned schema-import-dialog">
        <div class="dialog-header">
          <h3>导入方案</h3>
          <button class="dialog-close" @click="cancelImport">&times;</button>
        </div>
        <div class="dialog-body">
          <div class="import-file-info">
            包含 {{ importPreview.schemas?.length || 0 }} 个方案，{{ importPreview.file_count }} 个文件
          </div>

          <div
            v-for="(schema, idx) in importPreview.schemas"
            :key="schema.id"
            class="import-schema-card"
          >
            <div class="import-schema-header">
              <span class="import-schema-name">{{ schema.name || schema.id }}</span>
              <span class="schema-mgr-type">{{
                engineTypeLabels[schema.engine_type] || schema.engine_type
              }}</span>
              <span v-if="schema.version" class="schema-mgr-version">v{{ schema.version }}</span>
            </div>
            <div class="import-preview-grid">
              <div class="import-preview-row">
                <span class="import-preview-label">方案 ID</span>
                <span class="import-preview-value">{{ schema.id }}</span>
              </div>
              <div v-if="schema.author" class="import-preview-row">
                <span class="import-preview-label">作者</span>
                <span class="import-preview-value">{{ schema.author }}</span>
              </div>
              <div class="import-preview-row">
                <span class="import-preview-label">词典</span>
                <span class="import-preview-value">{{ schema.dict_count }} 个</span>
              </div>
              <div v-if="schema.description" class="import-preview-row">
                <span class="import-preview-label">描述</span>
                <span class="import-preview-value">{{ schema.description }}</span>
              </div>
            </div>
            <div v-if="schema.conflict" class="import-conflict-warning">
              <span class="import-conflict-icon">&#9888;</span>
              <span>
                系统中已存在{{ schema.conflict_src === "builtin" ? "内置" : "用户"
                }}方案「{{ schema.id }}」，导入将覆盖现有配置
              </span>
            </div>
            <div
              v-if="idx < (importPreview.schemas?.length || 0) - 1"
              class="import-schema-divider"
            ></div>
          </div>
        </div>
        <div class="dialog-footer">
          <button class="btn btn-sm" @click="cancelImport">取消</button>
          <button
            class="btn btn-sm btn-primary"
            :disabled="importLoading"
            @click="confirmImport"
          >
            {{ hasConflict()
              ? importLoading ? "覆盖中..." : "覆盖导入"
              : importLoading ? "导入中..." : "确认导入"
            }}
          </button>
        </div>
      </div>
    </div>

    <!-- 删除确认对话框 -->
    <div
      v-if="deleteConfirmID"
      class="dialog-overlay schema-nested-overlay"
      @click.self="cancelDelete"
    >
      <div class="dialog-box" style="max-width: 400px">
        <div class="dialog-title">确认删除</div>
        <div style="padding: 4px 0 16px; font-size: 14px; color: #374151">
          <p>
            确定要删除方案「{{
              getSchemaName(deleteConfirmID!)
            }}」吗？此操作将删除方案文件及其词典，不可恢复。
          </p>
          <div v-if="deleteRelatedIDs.length > 0" class="delete-related-warning">
            <span class="import-conflict-icon">&#9888;</span>
            <div>
              <p style="margin: 0 0 4px">以下混输方案依赖此方案，将一并删除：</p>
              <ul style="margin: 0; padding-left: 18px">
                <li v-for="rid in deleteRelatedIDs" :key="rid">
                  {{ getSchemaName(rid) }}
                </li>
              </ul>
            </div>
          </div>
        </div>
        <div class="dialog-actions">
          <button class="btn" @click="cancelDelete">取消</button>
          <button
            class="btn"
            :disabled="deleting"
            style="background: #dc2626; color: #fff"
            @click="confirmDelete"
          >
            {{ deleting ? "删除中..." : "删除" }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.schema-manager-dialog {
  width: 560px;
  max-width: 90vw;
  max-height: 80vh;
  display: flex;
  flex-direction: column;
}

/* Tabs */
.schema-mgr-tabs {
  display: flex;
  border-bottom: 1px solid var(--border-color, #e5e7eb);
  padding: 0 20px;
}
.schema-mgr-tab {
  padding: 10px 16px;
  font-size: 13px;
  font-weight: 500;
  color: #6b7280;
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  cursor: pointer;
  transition: all 0.15s;
}
.schema-mgr-tab:hover {
  color: #374151;
}
.schema-mgr-tab.active {
  color: #2563eb;
  border-bottom-color: #2563eb;
}

/* Body */
.schema-mgr-body {
  flex: 1;
  overflow-y: auto;
  min-height: 0;
}

/* Search */
.schema-mgr-search {
  margin-bottom: 12px;
}
.schema-mgr-search .input {
  width: 100%;
  padding: 8px 12px;
  font-size: 13px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  outline: none;
  transition: border-color 0.15s;
}
.schema-mgr-search .input:focus {
  border-color: #2563eb;
}

/* List */
.schema-mgr-list {
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}
.schema-mgr-item {
  border-bottom: 1px solid #f3f4f6;
  cursor: pointer;
  transition: background-color 0.15s;
}
.schema-mgr-item:last-child {
  border-bottom: none;
}
.schema-mgr-item:hover {
  background: #fafafa;
}
.schema-mgr-item-selected {
  background: #eff6ff;
}
.schema-mgr-item-selected:hover {
  background: #dbeafe;
}

/* Row */
.schema-mgr-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
}

/* Info */
.schema-mgr-info {
  flex: 1;
  min-width: 0;
}
.schema-mgr-main {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}
.schema-mgr-name {
  font-size: 13px;
  font-weight: 500;
  color: #1f2937;
}
.schema-mgr-type {
  font-size: 11px;
  padding: 1px 5px;
  border-radius: 3px;
  background: #f3f4f6;
  color: #6b7280;
}
.schema-mgr-version {
  font-size: 11px;
  color: #9ca3af;
}
.schema-mgr-source {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 3px;
  font-weight: 500;
}
.schema-mgr-source.source-builtin {
  background: #eff6ff;
  color: #2563eb;
}
.schema-mgr-source.source-user {
  background: #f0fdf4;
  color: #16a34a;
}
.schema-mgr-error-badge {
  font-size: 11px;
  padding: 1px 5px;
  border-radius: 3px;
  background: #fef2f2;
  color: #dc2626;
  font-weight: 500;
}
.schema-mgr-desc {
  font-size: 12px;
  color: #9ca3af;
  margin-top: 2px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Actions */
.schema-mgr-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}
.schema-mgr-info-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  background: none;
  color: #9ca3af;
  cursor: pointer;
  border-radius: 6px;
  transition: all 0.15s;
}
.schema-mgr-info-btn:hover {
  background: #f3f4f6;
  color: #2563eb;
}
.schema-mgr-delete-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  background: none;
  color: #d1d5db;
  cursor: pointer;
  border-radius: 6px;
  transition: all 0.15s;
}
.schema-mgr-delete-btn:hover {
  background: #fef2f2;
  color: #dc2626;
}
.switch-sm {
  transform: scale(0.8);
  transform-origin: center;
}

/* Nested overlays */
.schema-nested-overlay {
  z-index: 1100;
}
.schema-detail-dialog,
.schema-import-dialog {
  width: 440px;
  max-width: 90vw;
}

/* Import preview */
.import-file-info {
  font-size: 12px;
  color: #9ca3af;
  margin-bottom: 12px;
}
.import-schema-card {
  padding: 4px 0;
}
.import-schema-header {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 8px;
}
.import-schema-name {
  font-size: 14px;
  font-weight: 500;
  color: #1f2937;
}
.import-schema-divider {
  height: 1px;
  background: #e5e7eb;
  margin: 12px 0;
}
.import-preview-grid {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.import-preview-row {
  display: flex;
  align-items: baseline;
  gap: 12px;
  font-size: 13px;
  line-height: 1.5;
}
.import-preview-label {
  flex-shrink: 0;
  width: 55px;
  color: #9ca3af;
  text-align: right;
}
.import-preview-value {
  color: #374151;
}
.import-conflict-warning {
  margin-top: 8px;
  padding: 8px 10px;
  background: #fef3c7;
  border: 1px solid #f59e0b;
  border-radius: 6px;
  font-size: 12px;
  color: #92400e;
  display: flex;
  align-items: flex-start;
  gap: 6px;
}
.import-conflict-icon {
  font-size: 14px;
  flex-shrink: 0;
}

/* Delete related warning */
.delete-related-warning {
  margin-top: 10px;
  padding: 8px 10px;
  background: #fef3c7;
  border: 1px solid #f59e0b;
  border-radius: 6px;
  font-size: 13px;
  color: #92400e;
  display: flex;
  align-items: flex-start;
  gap: 6px;
}

/* Empty / Placeholder */
.schema-mgr-empty {
  text-align: center;
  padding: 24px;
  color: #9ca3af;
  font-size: 13px;
}
.schema-mgr-placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 48px 20px;
  text-align: center;
}
.schema-mgr-placeholder-icon {
  margin-bottom: 16px;
  opacity: 0.5;
}
.schema-mgr-placeholder-text {
  font-size: 14px;
  color: #6b7280;
  font-weight: 500;
  margin-bottom: 4px;
}
.schema-mgr-placeholder-hint {
  font-size: 12px;
  color: #9ca3af;
}

/* Footer */
.schema-mgr-footer {
  justify-content: space-between;
  align-items: center;
}
.schema-mgr-footer-left {
  display: flex;
  gap: 8px;
}
.schema-mgr-footer-center {
  flex: 1;
  text-align: center;
  min-width: 0;
}
.schema-mgr-footer-hint {
  font-size: 12px;
  color: #9ca3af;
}
.schema-mgr-footer-toast {
  font-size: 12px;
  color: #16a34a;
  font-weight: 500;
}
</style>
