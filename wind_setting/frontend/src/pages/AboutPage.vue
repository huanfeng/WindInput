<script setup lang="ts">
import type { Status } from "../api/settings";

defineProps<{
  status: Status | null;
  appIconUrl: string;
  repoUrl: string;
}>();

defineEmits<{
  openExternalLink: [url: string];
}>();
</script>

<template>
  <section class="section">
    <div class="section-header">
      <h2>关于</h2>
      <p class="section-desc">清风输入法 信息</p>
    </div>

    <div class="settings-card about-card" v-if="status">
      <div class="about-simple">
        <div class="about-icon-wrap">
          <img :src="appIconUrl" alt="清风输入法" />
        </div>
        <div class="about-title">
          <h3>{{ status.service.name }}</h3>
          <p>{{ status.service.version }}</p>
        </div>
        <div class="about-links-inline">
          <button
            class="link-button modern-link"
            @click="$emit('openExternalLink', repoUrl)"
          >
            <span class="link-icon" aria-hidden="true">
              <svg
                viewBox="0 0 24 24"
                width="18"
                height="18"
                fill="currentColor"
              >
                <path
                  d="M12 2C6.48 2 2 6.58 2 12.26c0 4.58 2.87 8.46 6.84 9.83.5.1.68-.22.68-.49 0-.24-.01-.87-.01-1.71-2.78.62-3.37-1.39-3.37-1.39-.45-1.2-1.1-1.52-1.1-1.52-.9-.64.07-.63.07-.63 1 .07 1.52 1.06 1.52 1.06.89 1.56 2.34 1.11 2.9.85.09-.67.35-1.11.63-1.37-2.22-.26-4.56-1.14-4.56-5.08 0-1.12.39-2.03 1.02-2.75-.1-.26-.44-1.3.1-2.71 0 0 .84-.27 2.75 1.03.8-.23 1.66-.35 2.51-.35.85 0 1.71.12 2.51.35 1.9-1.3 2.74-1.03 2.74-1.03.54 1.41.2 2.45.1 2.71.63.72 1.02 1.63 1.02 2.75 0 3.95-2.35 4.82-4.58 5.07.36.32.68.94.68 1.9 0 1.37-.01 2.47-.01 2.8 0 .27.18.6.69.49 3.97-1.37 6.83-5.25 6.83-9.83C22 6.58 17.52 2 12 2z"
                />
              </svg>
            </span>
            <div class="link-text">
              <span class="link-title">GitHub 项目主页</span>
              <span class="link-desc">{{ repoUrl }}</span>
            </div>
          </button>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.about-card {
  padding: 24px;
}
.about-simple {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  text-align: center;
}
.about-icon-wrap {
  width: 96px;
  height: 96px;
  background: #f3f4f6;
  border-radius: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.04);
}
.about-icon-wrap img {
  width: 72px;
  height: 72px;
  object-fit: contain;
}
.about-title h3 {
  font-size: 22px;
  font-weight: 700;
  margin: 0;
  color: #111827;
}
.about-title p {
  color: #9ca3af;
  margin: 6px 0 0;
  font-size: 13px;
}
.about-links-inline {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 8px;
  width: 100%;
  max-width: 420px;
}
.link-button {
  border: 1px solid #e5e7eb;
  background: #fff;
  padding: 12px 14px;
  border-radius: 14px;
  color: #111827;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 12px;
  transition:
    border-color 0.15s,
    box-shadow 0.15s,
    transform 0.15s;
}
.link-button:hover {
  border-color: #cbd5f5;
  box-shadow: 0 8px 20px rgba(37, 99, 235, 0.08);
  transform: translateY(-1px);
}
.modern-link .link-icon {
  width: 36px;
  height: 36px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 12px;
  background: #111827;
  color: #fff;
  flex-shrink: 0;
}
.modern-link .link-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.modern-link .link-title {
  font-weight: 600;
  font-size: 14px;
  color: #111827;
}
.modern-link .link-desc {
  color: #6b7280;
  font-size: 12px;
}
</style>
