<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import {
  AlertDialogCancel,
  type AlertDialogCancelProps,
  useForwardProps,
} from "radix-vue";
import { buttonVariants } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const props = defineProps<
  AlertDialogCancelProps & { class?: HTMLAttributes["class"] }
>();

const delegatedProps = computed(() => {
  const { class: _, ...rest } = props;
  return rest;
});

const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <AlertDialogCancel
    v-bind="forwardedProps"
    :class="
      cn(buttonVariants({ variant: 'outline' }), 'mt-2 sm:mt-0', props.class)
    "
  >
    <slot />
  </AlertDialogCancel>
</template>
