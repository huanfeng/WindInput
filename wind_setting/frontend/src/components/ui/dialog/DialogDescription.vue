<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import {
  DialogDescription,
  type DialogDescriptionProps,
  useForwardProps,
} from "radix-vue";
import { cn } from "@/lib/utils";

const props = defineProps<
  DialogDescriptionProps & { class?: HTMLAttributes["class"] }
>();

const delegatedProps = computed(() => {
  const { class: _, ...rest } = props;
  return rest;
});

const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <DialogDescription
    v-bind="forwardedProps"
    :class="cn('text-sm text-muted-foreground', props.class)"
  >
    <slot />
  </DialogDescription>
</template>
