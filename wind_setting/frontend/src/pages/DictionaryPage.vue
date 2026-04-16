<template>
  <section class="section dict-page">
    <!-- ===== 紧凑标题 ===== -->
    <div class="dict-header">
      <h2>词库管理</h2>
      <span class="dict-header-desc">管理您的词库数据（修改即时生效）</span>
      <span class="dict-header-spacer"></span>
      <Button
        v-if="isWailsEnv"
        variant="outline"
        size="sm"
        class="dict-refresh-btn"
        @click="handleRefresh"
        title="刷新数据"
      >
        ↻
      </Button>
    </div>

    <!-- 非 Wails 环境提示 -->
    <div v-if="!isWailsEnv" class="dict-note-center-wrap">
      <p>词库管理功能需要在桌面应用中使用</p>
      <p class="dict-note">请使用 <code>wails dev</code> 或编译后的应用</p>
    </div>

    <template v-else>
      <!-- ===== 内容卡片（包含类型选择器 + 面板） ===== -->
      <div class="dict-content-card">
        <!-- ===== 类型选择器行 ===== -->
        <DictTypeSelector :schemas="allSchemaStatuses" v-model="selection">
          <template #actions>
            <!-- 导入/导出（短语模式 或 方案非混输用户词库 或 混输候选调整） -->
            <Button
              v-if="showImportExport"
              variant="outline"
              size="sm"
              @click="handleImport"
            >
              导入
            </Button>
            <Button
              v-if="showImportExport"
              variant="outline"
              size="sm"
              @click="handleExport"
            >
              导出
            </Button>
            <!-- 方案操作菜单 -->
            <DropdownMenu v-if="selection.mode === 'schema'">
              <DropdownMenuTrigger as-child>
                <Button variant="destructive" size="sm">操作 ▾</Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem
                  class="text-destructive"
                  @click="handleResetCurrentSchema"
                >
                  重置当前方案
                </DropdownMenuItem>
                <DropdownMenuItem
                  class="text-destructive"
                  @click="handleResetAllSchemas"
                >
                  重置所有方案
                </DropdownMenuItem>
                <DropdownMenuItem
                  v-if="selectedSchemaOrphaned"
                  class="text-destructive"
                  @click="handleDeleteOrphanedSchema"
                >
                  删除当前方案
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </template>
        </DictTypeSelector>

        <!-- ===== 残留方案警告 ===== -->
        <div
          v-if="selection.mode === 'schema' && selectedSchemaOrphaned"
          class="orphan-banner"
        >
          ⚠ 此方案数据为历史残留（仅可查看和删除，不可添加）
        </div>

        <!-- ===== 快捷短语面板 ===== -->
        <PhrasePanel
          v-if="selection.mode === 'phrases'"
          ref="phrasePanelRef"
          @loading="onLoading"
        />

        <!-- ===== 方案模式 ===== -->
        <template v-if="selection.mode === 'schema' && selection.schemaId">
          <!-- 方案子标签页 -->
          <div class="schema-sub-tabs">
            <button
              v-for="tab in schemaTabs"
              :key="tab.key"
              :class="['sub-tab', { active: schemaSubTab === tab.key }]"
              @click="switchSchemaSubTab(tab.key)"
            >
              {{ tab.label }}
            </button>
          </div>

          <!-- 混输方案提示（用户词库/临时词库继承自主方案） -->
          <div
            v-if="
              selectedSchemaIsMixed &&
              (schemaSubTab === 'userdict' || schemaSubTab === 'temp')
            "
            class="mixed-hint"
          >
            <p>此方案为混输方案，{{ schemaSubTabLabel }}继承自主方案。</p>
            <p class="dict-note">请切换到对应的主方案进行设置。</p>
          </div>

          <!-- 各子面板 — 用 :key 强制切换方案时重建 -->
          <template
            v-if="
              !selectedSchemaIsMixed ||
              schemaSubTab === 'shadow' ||
              schemaSubTab === 'freq'
            "
          >
            <UserDictPanel
              v-if="schemaSubTab === 'userdict'"
              ref="userDictPanelRef"
              :key="'ud-' + selection.schemaId"
              :schema-id="selection.schemaId"
              :readonly="selectedSchemaOrphaned"
              @loading="onLoading"
              @schema-changed="handleSchemaChanged"
            />
            <FreqPanel
              v-if="schemaSubTab === 'freq'"
              ref="freqPanelRef"
              :key="'fq-' + selection.schemaId"
              :schema-id="selection.schemaId"
              :schema-name="selectedSchemaName"
              @loading="onLoading"
            />
            <TempDictPanel
              v-if="schemaSubTab === 'temp'"
              ref="tempDictPanelRef"
              :key="'tp-' + selection.schemaId"
              :schema-id="selection.schemaId"
              @loading="onLoading"
              @schema-changed="handleSchemaChanged"
            />
            <ShadowPanel
              v-if="schemaSubTab === 'shadow'"
              ref="shadowPanelRef"
              :key="'sw-' + selection.schemaId"
              :schema-id="selection.schemaId"
              :readonly="selectedSchemaOrphaned"
              @loading="onLoading"
              @schema-changed="handleSchemaChanged"
            />
          </template>
        </template>
      </div>
    </template>
  </section>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from "vue";
import * as wailsApi from "../api/wails";
import type { SchemaStatusItem, DictEvent } from "../api/wails";
import { useToast } from "../composables/useToast";
import { useConfirm } from "../composables/useConfirm";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import DictTypeSelector from "../components/dict/DictTypeSelector.vue";
import PhrasePanel from "../components/dict/PhrasePanel.vue";
import UserDictPanel from "../components/dict/UserDictPanel.vue";
import FreqPanel from "../components/dict/FreqPanel.vue";
import TempDictPanel from "../components/dict/TempDictPanel.vue";
import ShadowPanel from "../components/dict/ShadowPanel.vue";

const props = defineProps<{
  isWailsEnv: boolean;
}>();

const { toast } = useToast();
const { confirm } = useConfirm();

// ===== 选择状态 =====
const selection = ref<{ mode: "phrases" | "schema"; schemaId: string }>({
  mode: "phrases",
  schemaId: "",
});

const schemaSubTab = ref<"userdict" | "freq" | "temp" | "shadow">("userdict");

const schemaTabs = [
  { key: "userdict" as const, label: "用户词库" },
  { key: "temp" as const, label: "临时词库" },
  { key: "freq" as const, label: "词频" },
  { key: "shadow" as const, label: "候选调整" },
];

// ===== 方案列表 =====
const allSchemaStatuses = ref<SchemaStatusItem[]>([]);

const selectedSchema = computed(() =>
  allSchemaStatuses.value.find((s) => s.schema_id === selection.value.schemaId),
);

const selectedSchemaName = computed(
  () => selectedSchema.value?.schema_name || selection.value.schemaId,
);

const selectedSchemaOrphaned = computed(
  () => selectedSchema.value?.status === "orphaned",
);

const selectedSchemaIsMixed = computed(
  () => selectedSchema.value?.is_mixed === true,
);

const schemaSubTabLabel = computed(() => {
  const tab = schemaTabs.find((t) => t.key === schemaSubTab.value);
  return tab?.label || "";
});

// 导入导出可见性
const showImportExport = computed(() => {
  if (selection.value.mode === "phrases") return true;
  if (selection.value.mode !== "schema") return false;
  // 非混输的用户词库 tab
  if (schemaSubTab.value === "userdict" && !selectedSchemaIsMixed.value)
    return true;
  return false;
});

// ===== 面板引用 =====
const phrasePanelRef = ref<InstanceType<typeof PhrasePanel> | null>(null);
const userDictPanelRef = ref<InstanceType<typeof UserDictPanel> | null>(null);
const freqPanelRef = ref<InstanceType<typeof FreqPanel> | null>(null);
const tempDictPanelRef = ref<InstanceType<typeof TempDictPanel> | null>(null);
const shadowPanelRef = ref<InstanceType<typeof ShadowPanel> | null>(null);

function onLoading(_loading: boolean) {}

// ===== 数据加载 =====
async function loadSchemaStatuses() {
  try {
    const list = await wailsApi.getAllSchemaStatuses();
    allSchemaStatuses.value = list || [];

    if (
      selection.value.schemaId &&
      !allSchemaStatuses.value.find(
        (s) => s.schema_id === selection.value.schemaId,
      )
    ) {
      const first = allSchemaStatuses.value.find((s) => s.status === "enabled");
      selection.value.schemaId = first?.schema_id || "";
    }
    if (!selection.value.schemaId && allSchemaStatuses.value.length > 0) {
      const first = allSchemaStatuses.value.find((s) => s.status === "enabled");
      selection.value.schemaId =
        first?.schema_id || allSchemaStatuses.value[0].schema_id;
    }
  } catch (e) {
    console.error("加载方案状态失败", e);
  }
}

// ===== 模式切换 =====
watch(
  () => selection.value.mode,
  () => {
    schemaSubTab.value = "userdict";
  },
);

function switchSchemaSubTab(tab: "userdict" | "freq" | "temp" | "shadow") {
  schemaSubTab.value = tab;
}

// ===== 刷新 =====
async function handleRefresh() {
  await loadSchemaStatuses();
  await nextTick();
  if (selection.value.mode === "phrases") {
    phrasePanelRef.value?.loadData();
  } else {
    reloadCurrentPanel();
  }
  toast("已刷新", "success");
}

async function reloadCurrentPanel() {
  await nextTick();
  switch (schemaSubTab.value) {
    case "userdict":
      userDictPanelRef.value?.loadData();
      break;
    case "freq":
      freqPanelRef.value?.loadData();
      break;
    case "temp":
      tempDictPanelRef.value?.loadData();
      break;
    case "shadow":
      shadowPanelRef.value?.loadData();
      break;
  }
}

// ===== 导入/导出 =====
async function handleImport() {
  try {
    if (selection.value.mode === "phrases") {
      const result = await wailsApi.importPhrases();
      if (result.cancelled) return;
      toast(`导入成功，共 ${result.count} 条`, "success");
      await nextTick();
      phrasePanelRef.value?.loadData();
    } else if (schemaSubTab.value === "userdict") {
      const result = await wailsApi.importUserDictForSchema(
        selection.value.schemaId,
      );
      if (result.cancelled) return;
      toast(`导入成功，新增 ${result.count} 条`, "success");
      await nextTick();
      userDictPanelRef.value?.loadData();
      await loadSchemaStatuses();
    }
  } catch (e: unknown) {
    toast((e as Error).message || "导入失败", "error");
  }
}

async function handleExport() {
  try {
    if (selection.value.mode === "phrases") {
      const result = await wailsApi.exportPhrases();
      if (result.cancelled) return;
      toast(`导出成功，共 ${result.count} 条`, "success");
    } else if (schemaSubTab.value === "userdict") {
      const result = await wailsApi.exportUserDictForSchema(
        selection.value.schemaId,
      );
      if (result.cancelled) return;
      toast(`导出成功，共 ${result.count} 条`, "success");
    }
  } catch (e: unknown) {
    toast((e as Error).message || "导出失败", "error");
  }
}

// ===== 重置/删除 =====
async function handleResetCurrentSchema() {
  const name = selectedSchemaName.value;
  if (
    !(await confirm(
      `确定重置「${name}」的所有用户数据吗？\n\n将清除：用户词库、临时词库、候选调整、词频数据\n\n此操作不可恢复！`,
    ))
  )
    return;
  try {
    await wailsApi.resetUserData(selection.value.schemaId);
    toast(`已重置「${name}」的所有用户数据`, "success");
    await reloadCurrentPanel();
    await loadSchemaStatuses();
  } catch (e: unknown) {
    toast((e as Error).message || "重置失败", "error");
  }
}

async function handleResetAllSchemas() {
  if (
    !(await confirm(
      "确定重置所有方案的用户数据吗？\n\n将清除所有方案的：用户词库、临时词库、候选调整、词频数据\n\n此操作不可恢复！",
    ))
  )
    return;
  try {
    await wailsApi.resetUserData("");
    toast("已重置所有方案的用户数据", "success");
    await reloadCurrentPanel();
    await loadSchemaStatuses();
  } catch (e: unknown) {
    toast((e as Error).message || "重置失败", "error");
  }
}

async function handleDeleteOrphanedSchema() {
  const name = selectedSchemaName.value;
  if (!(await confirm(`确定删除「${name}」的残留数据吗？\n\n此操作不可恢复！`)))
    return;
  try {
    await wailsApi.deleteSchemaData(selection.value.schemaId);
    toast(`已删除「${name}」的残留数据`, "success");
    // 先刷新列表
    await loadSchemaStatuses();
    // 如果方案仍在列表（数据未完全清除），选第一个其他方案
    const remaining = allSchemaStatuses.value.filter(
      (s) => s.schema_id !== selection.value.schemaId,
    );
    if (remaining.length > 0) {
      const first =
        remaining.find((s) => s.status === "enabled") || remaining[0];
      selection.value = { mode: "schema", schemaId: first.schema_id };
    } else {
      selection.value = { mode: "phrases", schemaId: "" };
    }
  } catch (e: unknown) {
    toast((e as Error).message || "删除失败", "error");
  }
}

// ===== Schema 变更回调 =====
async function handleSchemaChanged() {
  await loadSchemaStatuses();
}

// ===== 事件监听 =====
function handleDictEvent(event: DictEvent) {
  if (!event) return;

  if (event.type === "phrase") {
    if (selection.value.mode === "phrases") {
      phrasePanelRef.value?.loadData();
    }
  } else if (selection.value.mode === "schema") {
    const matchesSchema =
      !event.schema_id || event.schema_id === selection.value.schemaId;
    if (!matchesSchema) {
      loadSchemaStatuses();
      return;
    }
    switch (event.type) {
      case "userdict":
        if (schemaSubTab.value === "userdict")
          userDictPanelRef.value?.loadData();
        break;
      case "freq":
        if (schemaSubTab.value === "freq") freqPanelRef.value?.loadData();
        break;
      case "temp":
        if (schemaSubTab.value === "temp") tempDictPanelRef.value?.loadData();
        break;
      case "shadow":
        if (schemaSubTab.value === "shadow") shadowPanelRef.value?.loadData();
        break;
    }
    loadSchemaStatuses();
  }
}

onMounted(async () => {
  if (!props.isWailsEnv) return;
  await loadSchemaStatuses();
  wailsApi.onDictEvent(handleDictEvent);
});

onUnmounted(() => {
  wailsApi.offDictEvent();
});
</script>

<style scoped>
.dict-page {
  display: flex;
  flex-direction: column;
  height: 100%;
  overflow: hidden;
}

.dict-header {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding: 12px 0 8px;
}
.dict-header h2 {
  font-size: 18px;
  font-weight: 600;
  color: hsl(var(--foreground));
  margin: 0;
}
.dict-header-desc {
  font-size: 13px;
  color: hsl(var(--muted-foreground));
}
.dict-header-spacer {
  flex: 1;
}
.dict-refresh-btn {
  font-size: 15px;
  padding: 2px 8px;
  line-height: 1;
}

.dict-note-center-wrap {
  text-align: center;
  padding: 32px;
  color: hsl(var(--muted-foreground));
}
.dict-note-center-wrap code {
  background: hsl(var(--secondary));
  padding: 2px 6px;
  border-radius: 4px;
}

.orphan-banner {
  background: hsl(var(--warning) / 0.1);
  border: 1px solid hsl(var(--warning));
  border-radius: 6px;
  padding: 6px 14px;
  font-size: 13px;
  color: hsl(var(--warning));
  margin-bottom: 8px;
  flex-shrink: 0;
}

.mixed-hint {
  text-align: center;
  padding: 36px 24px;
  color: hsl(var(--muted-foreground));
  background: hsl(var(--muted));
  border-radius: 8px;
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
}
.mixed-hint p {
  margin: 0;
}
.mixed-hint .dict-note {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
  font-style: italic;
  margin-top: 6px;
}

.dict-content-card {
  background: hsl(var(--card));
  border-radius: 12px;
  padding: 16px 20px;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.05);
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  min-height: 0;
}

.schema-sub-tabs {
  display: flex;
  gap: 0;
  margin-bottom: 8px;
  flex-shrink: 0;
  border-bottom: 1px solid hsl(var(--border));
}
.sub-tab {
  padding: 6px 16px;
  font-size: 13px;
  border: none;
  background: none;
  color: hsl(var(--muted-foreground));
  cursor: pointer;
  border-bottom: 2px solid transparent;
  transition: all 0.15s;
  margin-bottom: -1px;
}
.sub-tab:hover {
  color: hsl(var(--foreground));
}
.sub-tab.active {
  color: hsl(var(--primary));
  border-bottom-color: hsl(var(--primary));
  font-weight: 500;
}
</style>
