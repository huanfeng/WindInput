<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import {
  AlertDialogTitle,
  type AlertDialogTitleProps,
  useForwardProps,
} from "radix-vue";
import { cn } from "@/lib/utils";

const props = defineProps<
  AlertDialogTitleProps & { class?: HTMLAttributes["class"] }
>();

const delegatedProps = computed(() => {
  const { class: _, ...rest } = props;
  return rest;
});

const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <AlertDialogTitle
    v-bind="forwardedProps"
    :class="cn('text-lg font-semibold', props.class)"
  >
    <slot />
  </AlertDialogTitle>
</template>
