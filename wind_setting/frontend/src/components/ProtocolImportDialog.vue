<script setup lang="ts">
import { ref, watch } from "vue";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
} from "./ui/alert-dialog";
import { Button } from "./ui/button";
import {
  previewThemeFromURL,
  importThemeFromText,
  type ProtocolImportPayload,
  type ThemeURLPreview,
} from "../api/wails";
import { useToast } from "../composables/useToast";

const props = defineProps<{ payload: ProtocolImportPayload | null }>();
const emit = defineEmits<{ (e: "close"): void }>();

const { toast } = useToast();

const open = ref(false);
const loading = ref(false);
const errorMsg = ref("");
const preview = ref<ThemeURLPreview | null>(null);
const sourceHost = ref("");
const conflictName = ref(""); // 非空表示进入"是否覆盖"确认态

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

watch(
  () => props.payload,
  async (p) => {
    if (!p) return;
    open.value = true;
    errorMsg.value = "";
    preview.value = null;
    conflictName.value = "";
    if (!p.ok || !p.request) {
      errorMsg.value = p.error || "无效的导入链接";
      return;
    }
    sourceHost.value = hostOf(p.request.url);
    if (p.request.kind !== "theme") {
      errorMsg.value = `「${p.request.kind}」类型导入暂未支持`;
      return;
    }
    loading.value = true;
    try {
      const r = await previewThemeFromURL(p.request.url);
      if (!r.ok) {
        errorMsg.value = r.error_msg || "预览失败";
      } else {
        preview.value = r;
      }
    } catch (e: any) {
      errorMsg.value = String(e?.message || e);
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);

async function doImport(force: boolean) {
  if (!preview.value) return;
  loading.value = true;
  try {
    const res = await importThemeFromText(preview.value.yaml, force);
    if (res.conflict) {
      conflictName.value = res.theme_name || preview.value.name;
      return;
    }
    if (res.success) {
      toast(`主题「${res.theme_name}」已导入`, "success");
      close();
    } else {
      toast(res.error_msg || "导入失败", "error");
    }
  } catch (e: any) {
    toast(String(e?.message || e), "error");
  } finally {
    loading.value = false;
  }
}

function close() {
  open.value = false;
  emit("close");
}
</script>

<template>
  <AlertDialog :open="open">
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogTitle>导入主题</AlertDialogTitle>
        <AlertDialogDescription as="div">
          <template v-if="loading">正在加载…</template>
          <template v-else-if="errorMsg">
            <span class="text-destructive">{{ errorMsg }}</span>
          </template>
          <template v-else-if="conflictName">
            已存在主题「{{ conflictName }}」，是否覆盖？
          </template>
          <template v-else-if="preview">
            <div class="space-y-1 text-left">
              <div><b>名称：</b>{{ preview.name }}</div>
              <div v-if="preview.author"><b>作者：</b>{{ preview.author }}</div>
              <div v-if="preview.version">
                <b>版本：</b>{{ preview.version }}
              </div>
              <div v-if="preview.description">
                <b>说明：</b>{{ preview.description }}
              </div>
              <div class="text-muted-foreground break-all">
                <b>来源：</b>{{ sourceHost }}
              </div>
            </div>
          </template>
        </AlertDialogDescription>
      </AlertDialogHeader>
      <AlertDialogFooter>
        <Button variant="outline" :disabled="loading" @click="close"
          >取消</Button
        >
        <Button
          v-if="conflictName"
          :disabled="loading"
          @click="doImport(true)"
          >覆盖导入</Button
        >
        <Button
          v-else-if="preview && !errorMsg"
          :disabled="loading"
          @click="doImport(false)"
          >导入</Button
        >
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>
</template>
