<script setup lang="ts">
import { Toaster as Sonner, type ToasterProps } from "vue-sonner";

const props = withDefaults(defineProps<ToasterProps & { class?: string }>(), {
  position: "top-center",
  duration: 3000,
  expand: false,
  richColors: true,
});
</script>

<template>
  <Sonner
    v-bind="props"
    :class="props.class"
    :toastOptions="{
      classes: {
        toast:
          'group toast border bg-card text-foreground shadow-lg rounded-lg px-4 py-3 text-sm flex items-center gap-2',
        title: 'font-medium',
        description: 'text-muted-foreground text-xs',
        success:
          'border-green-200 bg-green-50 text-green-900 dark:border-green-800 dark:bg-green-950 dark:text-green-100',
        error:
          'border-red-200 bg-red-50 text-red-900 dark:border-red-800 dark:bg-red-950 dark:text-red-100',
        info: 'border-blue-200 bg-blue-50 text-blue-900 dark:border-blue-800 dark:bg-blue-950 dark:text-blue-100',
      },
    }"
  />
</template>

<style>
/* 确保 Sonner 容器 fixed 定位，居中，不影响文档流 */
[data-sonner-toaster] {
  position: fixed !important;
  top: 16px !important;
  left: 50% !important;
  transform: translateX(-50%) !important;
  z-index: 9999 !important;
  pointer-events: none;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}
[data-sonner-toaster] [data-sonner-toast] {
  pointer-events: auto;
  width: auto !important;
  max-width: 360px;
  position: relative !important;
  margin-bottom: 8px;
}
</style>
