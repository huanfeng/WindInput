<script setup lang="ts">
import { ref, watch, computed } from "vue";
import * as wailsApi from "../api/wails";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
  AlertDialogAction,
} from "@/components/ui/alert-dialog";

const props = defineProps<{
  visible: boolean;
}>();

const emit = defineEmits<{
  "update:visible": [value: boolean];
  changed: [];
}>();

// 状态
const loading = ref(false);
const executing = ref(false);
const error = ref("");
const warnings = ref<string[]>([]);
const done = ref(false);

// 数据
const currentDir = ref("");
const sizeText = ref("");
const fileCount = ref(0);
const targetDir = ref("");
const migrate = ref(true);
const overwrite = ref(false);
const deleteOld = ref(false);

// 验证
const validation = ref<wailsApi.DataDirValidation | null>(null);
const validating = ref(false);

// 对话框打开时加载信息
watch(
  () => props.visible,
  async (val) => {
    if (!val) return;
    // 重置所有状态
    error.value = "";
    targetDir.value = "";
    migrate.value = true;
    overwrite.value = false;
    deleteOld.value = false;
    validation.value = null;
    executing.value = false;
    warnings.value = [];
    done.value = false;

    loading.value = true;
    try {
      const info = await wailsApi.getDataDirInfo();
      currentDir.value = info.current_dir;
      sizeText.value = info.size_text;
      fileCount.value = info.file_count;
    } catch (e: any) {
      error.value = "获取数据目录信息失败: " + (e.message || e);
    } finally {
      loading.value = false;
    }
  },
);

async function selectTarget() {
  try {
    const path = await wailsApi.selectDataDir();
    if (!path) return;
    targetDir.value = path;
    await validateTarget();
  } catch (e: any) {
    error.value = "选择目录失败: " + (e.message || e);
  }
}

async function validateTarget() {
  if (!targetDir.value) {
    validation.value = null;
    return;
  }
  validating.value = true;
  error.value = "";
  try {
    validation.value = await wailsApi.validateDataDirPath(targetDir.value);
  } catch (e: any) {
    error.value = "验证失败: " + (e.message || e);
  } finally {
    validating.value = false;
  }
}

const canExecute = computed(() => {
  if (!targetDir.value) return false;
  if (!validation.value) return false;
  if (!validation.value.valid) return false;
  if (executing.value) return false;
  return true;
});

const targetWarning = computed(() => {
  if (!validation.value) return "";
  if (!validation.value.valid) return validation.value.warning;
  if (!validation.value.is_empty && migrate.value && !overwrite.value) {
    return "目标目录不为空，同名文件将被跳过";
  }
  if (!validation.value.is_empty && migrate.value && overwrite.value) {
    return "目标目录不为空，同名文件将被覆盖";
  }
  if (!validation.value.is_empty && !migrate.value) {
    return "目标目录不为空";
  }
  return "";
});

// 删除确认对话框
const deleteConfirmVisible = ref(false);

function onDeleteOldChange(checked: boolean) {
  if (checked) {
    deleteConfirmVisible.value = true;
  } else {
    deleteOld.value = false;
  }
}

function onDeleteConfirmed() {
  deleteConfirmVisible.value = false;
  deleteOld.value = true;
}

function onDeleteCancelled() {
  deleteConfirmVisible.value = false;
}

async function execute() {
  if (!canExecute.value) return;

  executing.value = true;
  error.value = "";
  warnings.value = [];
  done.value = false;

  try {
    const result = await wailsApi.changeUserDataDir({
      new_path: targetDir.value,
      migrate: migrate.value,
      overwrite: overwrite.value,
      delete_old_data: deleteOld.value,
    });
    if (result.warnings && result.warnings.length > 0) {
      warnings.value = result.warnings;
    }
    done.value = true;
    emit("changed");
  } catch (e: any) {
    error.value = e.message || String(e);
  } finally {
    executing.value = false;
  }
}
</script>

<template>
  <Dialog :open="visible" @update:open="(v) => emit('update:visible', v)">
    <DialogContent class="sm:max-w-[500px]">
      <DialogHeader>
        <DialogTitle>更改数据存储目录</DialogTitle>
      </DialogHeader>

      <div v-if="loading" class="dialog-loading">加载中...</div>

      <!-- 完成状态 -->
      <div v-else-if="done" class="dialog-body">
        <div class="done-section">
          <div class="done-title">数据目录已切换</div>
          <div class="field-hint">新目录：{{ targetDir }}</div>
        </div>

        <div v-if="warnings.length > 0" class="warnings-section">
          <div v-for="(w, i) in warnings" :key="i" class="warning-msg">
            {{ w }}
          </div>
        </div>
      </div>

      <!-- 配置表单 -->
      <div v-else class="dialog-body">
        <!-- 当前目录信息 -->
        <div class="info-section">
          <label class="field-label">当前目录</label>
          <div class="field-value path-value">{{ currentDir }}</div>
          <div class="field-hint">
            {{ fileCount }} 个文件，共 {{ sizeText }}
          </div>
        </div>

        <!-- 目标目录选择 -->
        <div class="info-section">
          <label class="field-label">目标目录</label>
          <div class="target-row">
            <input
              v-model="targetDir"
              type="text"
              class="target-input"
              placeholder="请选择或输入目标目录"
              @blur="validateTarget"
            />
            <Button variant="outline" size="sm" @click="selectTarget">
              浏览...
            </Button>
          </div>
          <div v-if="targetWarning" class="field-warning">
            {{ targetWarning }}
          </div>
        </div>

        <!-- 迁移选项 -->
        <div class="info-section">
          <label class="field-label">迁移选项</label>
          <div class="option-row">
            <Checkbox
              id="migrate"
              :checked="migrate"
              @update:checked="migrate = $event"
            />
            <label for="migrate" class="option-label">
              迁移现有数据到新目录
            </label>
          </div>
          <div class="option-hint">
            {{
              migrate
                ? "将暂停输入法服务以确保数据一致性，迁移完成后自动恢复"
                : "仅切换目录指向，不迁移数据。切换后将暂停并恢复输入法服务。"
            }}
          </div>
          <div class="option-row sub-option">
            <Checkbox
              id="overwrite"
              :disabled="!migrate"
              :checked="overwrite"
              @update:checked="overwrite = $event"
            />
            <label
              for="overwrite"
              class="option-label"
              :class="{ disabled: !migrate }"
            >
              覆盖目标目录中的已有文件
            </label>
          </div>
          <div class="option-row sub-option">
            <Checkbox
              id="deleteOld"
              :disabled="!migrate"
              :checked="deleteOld"
              @update:checked="onDeleteOldChange"
            />
            <label
              for="deleteOld"
              class="option-label"
              :class="{ disabled: !migrate }"
            >
              迁移后<span class="text-destructive">删除</span>旧目录中的数据文件
            </label>
          </div>
        </div>

        <!-- 错误提示 -->
        <div v-if="error" class="error-msg">{{ error }}</div>
      </div>

      <DialogFooter>
        <template v-if="done">
          <Button size="sm" @click="emit('update:visible', false)">
            关闭
          </Button>
        </template>
        <template v-else>
          <Button
            variant="outline"
            size="sm"
            :disabled="executing"
            @click="emit('update:visible', false)"
          >
            取消
          </Button>
          <Button size="sm" :disabled="!canExecute" @click="execute">
            {{ executing ? "执行中..." : "确定" }}
          </Button>
        </template>
      </DialogFooter>
    </DialogContent>

    <!-- 删除确认对话框（局部，避免被父 Dialog 遮挡） -->
    <AlertDialog :open="deleteConfirmVisible">
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>确认删除源目录数据</AlertDialogTitle>
          <AlertDialogDescription>
            勾选后，迁移完成时将清空源目录中的所有文件，此操作不可撤销。请确认源目录中没有其他重要数据。
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel @click="onDeleteCancelled">取消</AlertDialogCancel>
          <AlertDialogAction @click="onDeleteConfirmed">确认</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </Dialog>
</template>

<style scoped>
.dialog-loading {
  padding: 24px;
  text-align: center;
  color: hsl(var(--muted-foreground));
  min-height: 280px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 4px 0;
  min-height: 280px;
}

.info-section {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.field-label {
  font-size: 13px;
  font-weight: 500;
  color: hsl(var(--foreground));
}

.field-value {
  font-size: 13px;
  color: hsl(var(--muted-foreground));
}

.path-value {
  word-break: break-all;
  font-family: monospace;
  font-size: 12px;
  padding: 4px 8px;
  background: hsl(var(--muted));
  border-radius: 4px;
}

.field-hint {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
}

.field-warning {
  font-size: 12px;
  color: hsl(var(--warning, 38 92% 50%));
}

.target-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.target-input {
  flex: 1;
  padding: 4px 8px;
  font-size: 13px;
  border: 1px solid hsl(var(--border));
  border-radius: 4px;
  background: hsl(var(--background));
  color: hsl(var(--foreground));
  outline: none;
}

.target-input:focus {
  border-color: hsl(var(--ring));
}

.option-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.sub-option {
  margin-left: 24px;
}

.option-label {
  font-size: 13px;
  cursor: pointer;
  user-select: none;
}

.option-label.disabled {
  opacity: 0.5;
  cursor: default;
}

.option-hint {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
  margin-left: 24px;
}

.error-msg {
  font-size: 13px;
  color: hsl(var(--destructive));
  padding: 8px;
  background: hsl(var(--destructive) / 0.1);
  border-radius: 4px;
}

.done-section {
  text-align: center;
  padding: 8px 0;
}

.done-title {
  font-size: 15px;
  font-weight: 600;
  color: hsl(var(--foreground));
  margin-bottom: 4px;
}

.warnings-section {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.warning-msg {
  font-size: 13px;
  color: hsl(var(--warning, 38 92% 50%));
  padding: 8px;
  background: hsl(var(--warning, 38 92% 50%) / 0.1);
  border-radius: 4px;
}
</style>
