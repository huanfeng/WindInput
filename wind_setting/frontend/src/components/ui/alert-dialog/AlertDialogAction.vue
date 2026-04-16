<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import {
  AlertDialogAction,
  type AlertDialogActionProps,
  useForwardProps,
} from "radix-vue";
import { buttonVariants } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const props = defineProps<
  AlertDialogActionProps & { class?: HTMLAttributes["class"] }
>();

const delegatedProps = computed(() => {
  const { class: _, ...rest } = props;
  return rest;
});

const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <AlertDialogAction
    v-bind="forwardedProps"
    :class="cn(buttonVariants(), props.class)"
  >
    <slot />
  </AlertDialogAction>
</template>
