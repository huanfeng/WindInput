<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import { SelectLabel, type SelectLabelProps, useForwardProps } from "radix-vue";
import { cn } from "@/lib/utils";

const props = defineProps<
  SelectLabelProps & { class?: HTMLAttributes["class"] }
>();

const delegatedProps = computed(() => {
  const { class: _, ...rest } = props;
  return rest;
});

const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <SelectLabel
    v-bind="forwardedProps"
    :class="cn('px-2 py-1.5 text-sm font-semibold', props.class)"
  >
    <slot />
  </SelectLabel>
</template>
