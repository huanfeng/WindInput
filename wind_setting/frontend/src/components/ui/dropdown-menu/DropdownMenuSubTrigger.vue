<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import {
  DropdownMenuSubTrigger,
  type DropdownMenuSubTriggerProps,
  useForwardProps,
} from "radix-vue";
import { cn } from "@/lib/utils";

interface Props extends DropdownMenuSubTriggerProps {
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
  <DropdownMenuSubTrigger
    v-bind="forwardedProps"
    :class="
      cn(
        'flex cursor-default select-none items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none focus:bg-accent data-[state=open]:bg-accent [&>svg]:size-4 [&>svg]:shrink-0',
        inset && 'pl-8',
        props.class,
      )
    "
  >
    <slot />
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="ml-auto"
    >
      <path d="m9 18 6-6-6-6" />
    </svg>
  </DropdownMenuSubTrigger>
</template>
