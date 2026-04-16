import { toast as sonnerToast } from "vue-sonner";

export interface ToastContext {
  toast: (
    message: string,
    type?: "success" | "error",
    duration?: number,
  ) => void;
}

export function useToast(): ToastContext {
  return {
    toast(
      message: string,
      type: "success" | "error" = "success",
      duration = 3000,
    ) {
      if (type === "error") {
        sonnerToast.error(message, { duration });
      } else {
        sonnerToast.success(message, { duration });
      }
    },
  };
}

// Keep provideToast for backward compat during migration
export function provideToast(): ToastContext {
  return useToast();
}
