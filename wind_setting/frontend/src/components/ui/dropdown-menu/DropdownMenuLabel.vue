<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import {
  DropdownMenuLabel,
  type DropdownMenuLabelProps,
  useForwardProps,
} from "radix-vue";
import { cn } from "@/lib/utils";

interface Props extends DropdownMenuLabelProps {
  class?: HTMLAttributes["class"];
  inset?: boolean;
}

const props = defineProps<Props>();

const delegatedProps = computed(() => {
  const { class: _, inset: _i, ...rest } = props;
  return rest;
});

const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <DropdownMenuLabel
    v-bind="forwardedProps"
    :class="
      cn('px-2 py-1.5 text-sm font-semibold', inset && 'pl-8', props.class)
    "
  >
    <slot />
  </DropdownMenuLabel>
</template>
