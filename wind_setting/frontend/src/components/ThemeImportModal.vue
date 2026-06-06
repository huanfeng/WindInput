<script setup lang="ts">
import { ref } from "vue";
import * as wailsApi from "../api/wails";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";

const props = defineProps<{
  open: boolean;
}>();

const emit = defineEmits<{
  "update:open": [value: boolean];
  imported: [themeName: string];
}>();

type Tab = "file" | "text" | "url";
const activeTab = ref<Tab>("file");

const loading = ref(false);
const errorMsg = ref("");
const conflictName = ref("");
const yamlText = ref("");
const urlInput = ref("");

function resetState() {
  errorMsg.value = "";
  conflictName.value = "";
}

function close() {
  if (loading.value) return;
  resetState();
  yamlText.value = "";
  urlInput.value = "";
  activeTab.value = "file";
  emit("update:open", false);
}

async function handleFileImport(force = false) {
  loading.value = true;
  errorMsg.value = "";
  try {
    const result = await wailsApi.importThemeFromFile(force);
    handleResult(result);
  } finally {
    loading.value = false;
  }
}

async function handleTextImport(force = false) {
  loading.value = true;
  errorMsg.value = "";
  try {
    const result = await wailsApi.importThemeFromText(yamlText.value, force);
    handleResult(result);
  } finally {
    loading.value = false;
  }
}

async function handleURLImport(force = false) {
  loading.value = true;
  errorMsg.value = "";
  try {
    const result = await wailsApi.importThemeFromURL(urlInput.value, force);
    handleResult(result);
  } finally {
    loading.value = false;
  }
}

function handleResult(result: wailsApi.ImportThemeResult) {
  if (result.cancelled) return;
  if (result.success) {
    emit("imported", result.theme_name);
    close();
    return;
  }
  if (result.conflict) {
    conflictName.value = result.theme_name;
    errorMsg.value = "";
    return;
  }
  errorMsg.value = result.error_msg || "导入失败";
}

async function confirmOverwrite() {
  if (activeTab.value === "file") {
    await handleFileImport(true);
  } else if (activeTab.value === "text") {
    await handleTextImport(true);
  } else {
    await handleURLImport(true);
  }
  conflictName.value = "";
}

function cancelOverwrite() {
  conflictName.value = "";
}

async function pasteFromClipboard() {
  try {
    const text = await navigator.clipboard.readText();
    yamlText.value = text;
  } catch {
    errorMsg.value = "无法读取剪贴板，请手动粘贴";
  }
}

async function pasteURL() {
  try {
    const text = await navigator.clipboard.readText();
    urlInput.value = text.trim();
  } catch {
    errorMsg.value = "无法读取剪贴板，请手动粘贴";
  }
}
</script>

<template>
  <Dialog :open="open" @update:open="(v) => !v && close()">
    <DialogContent class="theme-import-dialog">
      <DialogHeader>
        <DialogTitle>导入主题</DialogTitle>
      </DialogHeader>

      <!-- Tab 切换 -->
      <div class="tab-bar">
        <button
          class="tab-btn"
          :class="{ active: activeTab === 'file' }"
          type="button"
          @click="activeTab = 'file'"
        >
          从文件导入
        </button>
        <button
          class="tab-btn"
          :class="{ active: activeTab === 'url' }"
          type="button"
          @click="activeTab = 'url'"
        >
          从链接导入
        </button>
        <button
          class="tab-btn"
          :class="{ active: activeTab === 'text' }"
          type="button"
          @click="activeTab = 'text'"
        >
          粘贴 YAML
        </button>
      </div>

      <!-- 从文件导入 -->
      <div v-if="activeTab === 'file'" class="tab-content">
        <p class="tab-hint">选择一个 .yaml 格式的主题文件</p>
      </div>

      <!-- 从链接导入 -->
      <div v-if="activeTab === 'url'" class="tab-content url-content">
        <p class="tab-hint-sm">粘贴主题市场提供的下载链接（.yaml）</p>
        <input
          v-model="urlInput"
          class="url-input"
          type="url"
          placeholder="https://..."
          spellcheck="false"
          @keydown.enter="!loading && urlInput.trim() && handleURLImport(false)"
        />
      </div>

      <!-- 粘贴 YAML -->
      <div v-if="activeTab === 'text'" class="tab-content">
        <textarea
          v-model="yamlText"
          class="yaml-textarea"
          placeholder="将主题 YAML 内容粘贴到此处..."
          spellcheck="false"
        />
      </div>

      <!-- 冲突提示 -->
      <div v-if="conflictName" class="conflict-box">
        已存在主题「{{ conflictName }}」，是否覆盖？
        <div class="conflict-actions">
          <Button variant="outline" size="sm" @click="cancelOverwrite">
            取消
          </Button>
          <Button size="sm" @click="confirmOverwrite" :disabled="loading">
            覆盖导入
          </Button>
        </div>
      </div>

      <!-- 错误信息 -->
      <p v-if="errorMsg" class="error-msg">{{ errorMsg }}</p>

      <DialogFooter class="footer">
        <div class="footer-left">
          <Button
            v-if="activeTab === 'url'"
            variant="outline"
            size="sm"
            @click="pasteURL"
            :disabled="loading"
          >
            粘贴链接
          </Button>
          <Button
            v-if="activeTab === 'text'"
            variant="outline"
            size="sm"
            @click="pasteFromClipboard"
            :disabled="loading"
          >
            粘贴剪贴板
          </Button>
        </div>
        <div class="footer-right">
          <Button
            variant="outline"
            size="sm"
            @click="close"
            :disabled="loading"
          >
            取消
          </Button>
          <Button
            v-if="activeTab === 'file'"
            size="sm"
            @click="handleFileImport(false)"
            :disabled="loading"
          >
            {{ loading ? "导入中..." : "选择文件..." }}
          </Button>
          <Button
            v-if="activeTab === 'url'"
            size="sm"
            @click="handleURLImport(false)"
            :disabled="loading || !urlInput.trim()"
          >
            {{ loading ? "下载中..." : "下载并导入" }}
          </Button>
          <Button
            v-if="activeTab === 'text'"
            size="sm"
            @click="handleTextImport(false)"
            :disabled="loading || !yamlText.trim()"
          >
            {{ loading ? "导入中..." : "导入" }}
          </Button>
        </div>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

<style scoped>
.theme-import-dialog {
  width: 480px;
  max-width: 95vw;
}

.tab-bar {
  display: flex;
  gap: 4px;
  border-bottom: 1px solid hsl(var(--border));
  margin-bottom: 12px;
}

.tab-btn {
  padding: 6px 14px;
  font-size: 13px;
  border: none;
  background: none;
  cursor: pointer;
  color: hsl(var(--muted-foreground));
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  transition:
    color 0.15s,
    border-color 0.15s;
}

.tab-btn:hover {
  color: hsl(var(--foreground));
}

.tab-btn.active {
  color: hsl(var(--foreground));
  border-bottom-color: hsl(var(--primary));
  font-weight: 500;
}

.tab-content {
  height: 180px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tab-hint {
  font-size: 13px;
  color: hsl(var(--muted-foreground));
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
}

.url-content {
  justify-content: center;
}

.tab-hint-sm {
  font-size: 13px;
  color: hsl(var(--muted-foreground));
  margin: 0;
}

.url-input {
  width: 100%;
  padding: 8px 12px;
  font-size: 13px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  background: hsl(var(--background));
  color: hsl(var(--foreground));
  outline: none;
  line-height: 1.5;
}

.url-input:focus {
  border-color: hsl(var(--primary));
}

.url-input::placeholder {
  color: hsl(var(--muted-foreground));
}

.yaml-textarea {
  width: 100%;
  flex: 1;
  padding: 10px 12px;
  font-size: 12px;
  font-family: monospace;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  background: hsl(var(--background));
  color: hsl(var(--foreground));
  resize: vertical;
  outline: none;
  line-height: 1.5;
}

.yaml-textarea:focus {
  border-color: hsl(var(--primary));
}

.conflict-box {
  font-size: 13px;
  color: hsl(var(--foreground));
  background: hsl(var(--muted));
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.conflict-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}

.error-msg {
  font-size: 13px;
  color: hsl(var(--destructive));
  margin: 4px 0 0;
}

.footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 8px;
}

.footer-left {
  display: flex;
}

.footer-right {
  display: flex;
  gap: 8px;
}
</style>
