<script setup lang="ts">
import { ref, onMounted, watch } from "vue";
import type { Status } from "../api/settings";
import { useUpdate } from "@/composables/useUpdate";
import UpdateDialog from "@/components/UpdateDialog.vue";
import type { CheckResult } from "@/api/updater";

defineProps<{
  status: Status | null;
  appIconUrl: string;
  repoUrl: string;
}>();

defineEmits<{
  openExternalLink: [url: string];
}>();

const currentYear = new Date().getFullYear();
const qqGroupNumber = "1085293418";
const qqCopied = ref(false);

async function copyQQGroup(event: Event) {
  event.stopPropagation();
  try {
    await navigator.clipboard.writeText(qqGroupNumber);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = qqGroupNumber;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    document.body.removeChild(ta);
  }
  qqCopied.value = true;
  setTimeout(() => {
    qqCopied.value = false;
  }, 2000);
}

// 检查更新对话框
const { pendingUpdate } = useUpdate();
const updateDialogOpen = ref(false);
const updateInitialResult = ref<CheckResult | null>(null);

function applyPendingUpdate() {
  if (pendingUpdate.value && !updateDialogOpen.value) {
    updateInitialResult.value = pendingUpdate.value;
    pendingUpdate.value = null;
    updateDialogOpen.value = true;
  }
}

// onMounted 处理启动前已有结果的情况
onMounted(applyPendingUpdate);

// watch 处理 Go 网络检查在挂载后才完成的情况
watch(pendingUpdate, applyPendingUpdate);

function openUpdateDialog() {
  updateInitialResult.value = null;
  updateDialogOpen.value = true;
}

function onUpdateDialogClose() {
  updateDialogOpen.value = false;
  updateInitialResult.value = null;
}
</script>

<template>
  <section class="section">
    <div class="section-header">
      <h2>关于</h2>
      <p class="section-desc">清风输入法 信息</p>
    </div>

    <div class="settings-card about-card" v-if="status">
      <!-- 应用标识 -->
      <div class="about-hero">
        <div class="about-icon-wrap">
          <img :src="appIconUrl" alt="清风输入法" />
        </div>
        <div class="about-info">
          <h3 class="about-name">{{ status.service.name }}</h3>
          <div class="about-version-row">
            <span class="about-version-badge">v{{ status.service.version }}</span>
            <button class="update-trigger-btn" @click="openUpdateDialog">检查更新</button>
          </div>
          <p class="about-desc">轻量、快速、可定制的开源中文输入法</p>
        </div>
      </div>

      <!-- 链接卡片 -->
      <div class="about-links">
        <button class="link-card" @click="$emit('openExternalLink', repoUrl)">
          <span class="link-card-icon icon-github" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
              <path
                d="M12 2C6.48 2 2 6.58 2 12.26c0 4.58 2.87 8.46 6.84 9.83.5.1.68-.22.68-.49 0-.24-.01-.87-.01-1.71-2.78.62-3.37-1.39-3.37-1.39-.45-1.2-1.1-1.52-1.1-1.52-.9-.64.07-.63.07-.63 1 .07 1.52 1.06 1.52 1.06.89 1.56 2.34 1.11 2.9.85.09-.67.35-1.11.63-1.37-2.22-.26-4.56-1.14-4.56-5.08 0-1.12.39-2.03 1.02-2.75-.1-.26-.44-1.3.1-2.71 0 0 .84-.27 2.75 1.03.8-.23 1.66-.35 2.51-.35.85 0 1.71.12 2.51.35 1.9-1.3 2.74-1.03 2.74-1.03.54 1.41.2 2.45.1 2.71.63.72 1.02 1.63 1.02 2.75 0 3.95-2.35 4.82-4.58 5.07.36.32.68.94.68 1.9 0 1.37-.01 2.47-.01 2.8 0 .27.18.6.69.49 3.97-1.37 6.83-5.25 6.83-9.83C22 6.58 17.52 2 12 2z"
              />
            </svg>
          </span>
          <div class="link-card-text">
            <span class="link-card-title">GitHub</span>
            <span class="link-card-desc">源码与文档</span>
          </div>
        </button>

        <button
          class="link-card"
          @click="$emit('openExternalLink', repoUrl + '/issues')"
        >
          <span class="link-card-icon icon-issues" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
              <path
                d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8zm-1-13h2v6h-2zm0 8h2v2h-2z"
              />
            </svg>
          </span>
          <div class="link-card-text">
            <span class="link-card-title">问题反馈</span>
            <span class="link-card-desc">报告 Bug 或建议</span>
          </div>
        </button>

        <button
          class="link-card"
          @click="$emit('openExternalLink', repoUrl + '/releases')"
        >
          <span class="link-card-icon icon-releases" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
              <path d="M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z" />
            </svg>
          </span>
          <div class="link-card-text">
            <span class="link-card-title">版本发布</span>
            <span class="link-card-desc">更新日志</span>
          </div>
        </button>

        <button
          class="link-card qq-card"
          @click="$emit('openExternalLink', 'https://qm.qq.com/q/u2A8FfafIs')"
        >
          <span class="link-card-icon icon-qq" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
              <path
                d="M21.395 15.035a40 40 0 0 0-.803-2.264l-1.079-2.695c.001-.032.014-.562.014-.836C19.526 4.632 17.351 0 12 0S4.474 4.632 4.474 9.241c0 .274.013.804.014.836l-1.08 2.695a39 39 0 0 0-.802 2.264c-1.021 3.283-.69 4.643-.438 4.673.54.065 2.103-2.472 2.103-2.472 0 1.469.756 3.387 2.394 4.771-.612.188-1.363.479-1.845.835-.434.32-.379.646-.301.778.343.578 5.883.369 7.482.189 1.6.18 7.14.389 7.483-.189.078-.132.132-.458-.301-.778-.483-.356-1.233-.646-1.846-.836 1.637-1.384 2.393-3.302 2.393-4.771 0 0 1.563 2.537 2.103 2.472.251-.03.581-1.39-.438-4.673"
              />
            </svg>
          </span>
          <div class="link-card-text">
            <span class="link-card-title">QQ 交流群</span>
            <span class="link-card-desc">{{ qqGroupNumber }}</span>
          </div>
          <span
            class="copy-btn"
            :class="{ copied: qqCopied }"
            @click="copyQQGroup($event)"
            :title="qqCopied ? '已复制' : '复制群号'"
            >{{ qqCopied ? "已复制" : "复制" }}</span
          >
        </button>
      </div>

      <!-- 版权 -->
      <div class="about-footer">
        <span
          >&copy; {{ currentYear }} WindInput Contributors &middot; MIT
          License</span
        >
      </div>
    </div>

    <UpdateDialog
      :open="updateDialogOpen"
      :initial-result="updateInitialResult"
      @close="onUpdateDialogClose"
    />
  </section>
</template>

<style scoped>
.about-card {
  padding: 32px 24px;
}

/* 顶部标识区 */
.about-hero {
  display: flex;
  align-items: center;
  gap: 20px;
  padding-bottom: 24px;
}
.about-icon-wrap {
  flex-shrink: 0;
}
.about-icon-wrap img {
  width: 80px;
  height: 80px;
  object-fit: contain;
}
.about-info {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.about-name {
  font-size: 22px;
  font-weight: 700;
  margin: 0;
  color: hsl(var(--foreground));
}
.about-version-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.about-version-badge {
  display: inline-block;
  font-size: 12px;
  font-weight: 600;
  color: hsl(var(--primary));
  background: hsl(var(--primary) / 0.1);
  padding: 2px 10px;
  border-radius: 999px;
  letter-spacing: 0.02em;
}
.update-trigger-btn {
  font-size: 11px;
  font-weight: 500;
  color: hsl(var(--primary));
  background: transparent;
  border: 1px solid hsl(var(--primary) / 0.45);
  border-radius: 4px;
  cursor: pointer;
  padding: 1px 8px;
  line-height: 1.6;
  transition: border-color 0.15s, background 0.15s;
  white-space: nowrap;
}
.update-trigger-btn:hover {
  border-color: hsl(var(--primary) / 0.75);
  background: hsl(var(--primary) / 0.06);
}
.about-desc {
  color: hsl(var(--muted-foreground));
  font-size: 13px;
  margin: 4px 0 0;
}

/* 链接卡片网格 */
.about-links {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.link-card {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 16px;
  border: 1px solid hsl(var(--border));
  border-radius: 12px;
  background: hsl(var(--card));
  cursor: pointer;
  text-align: left;
  position: relative;
  transition:
    border-color 0.15s,
    box-shadow 0.15s,
    transform 0.15s;
}
.link-card:hover {
  border-color: hsl(var(--ring) / 0.4);
  box-shadow: 0 4px 12px hsl(var(--ring) / 0.08);
  transform: translateY(-1px);
}
.link-card-icon {
  width: 36px;
  height: 36px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 10px;
  color: hsl(var(--primary-foreground));
  flex-shrink: 0;
}
.icon-github {
  background: hsl(var(--foreground));
}
.icon-issues {
  background: hsl(var(--warning));
}
.icon-releases {
  background: hsl(var(--success));
}
.icon-qq {
  background: #12b7f5;
}
.link-card-text {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
  flex: 1;
}
.link-card-title {
  font-size: 14px;
  font-weight: 600;
  color: hsl(var(--foreground));
}
.link-card-desc {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
}

/* QQ 卡片复制按钮 */
.copy-btn {
  font-size: 11px;
  color: hsl(var(--primary));
  background: hsl(var(--primary) / 0.1);
  padding: 2px 8px;
  border-radius: 4px;
  flex-shrink: 0;
  opacity: 0;
  transition:
    opacity 0.15s,
    background 0.15s;
  cursor: pointer;
}
.copy-btn:hover {
  background: hsl(var(--primary) / 0.15);
}
.copy-btn.copied {
  opacity: 1;
  color: hsl(var(--success));
  background: hsl(var(--success) / 0.15);
}
.qq-card:hover .copy-btn {
  opacity: 1;
}

/* 版权 */
.about-footer {
  text-align: center;
  padding-top: 24px;
  margin-top: 8px;
}
.about-footer span {
  font-size: 12px;
  color: hsl(var(--muted-foreground));
}
</style>
