import { ref } from "vue";

const visible = ref(false);
const message = ref("");
const resolvePromise = ref<((value: boolean) => void) | null>(null);

export function useConfirm() {
  function confirm(msg: string): Promise<boolean> {
    message.value = msg;
    visible.value = true;
    return new Promise((resolve) => {
      resolvePromise.value = resolve;
    });
  }

  function handleConfirm() {
    visible.value = false;
    resolvePromise.value?.(true);
    resolvePromise.value = null;
  }

  function handleCancel() {
    visible.value = false;
    resolvePromise.value?.(false);
    resolvePromise.value = null;
  }

  return {
    confirmVisible: visible,
    confirmMessage: message,
    confirm,
    handleConfirm,
    handleCancel,
  };
}
