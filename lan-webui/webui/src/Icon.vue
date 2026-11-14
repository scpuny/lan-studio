<template>
  <span class="ls-ico" :style="{ width: size + 'px', height: size + 'px' }" v-html="markup"></span>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { ICON_PATHS } from './icons'

const props = withDefaults(
  defineProps<{ name: string; size?: number; strokeWidth?: number }>(),
  { size: 17, strokeWidth: 1.6 },
)

const markup = computed(() => {
  const inner = ICON_PATHS[props.name] ?? ''
  return `<svg width="${props.size}" height="${props.size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="${props.strokeWidth}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${inner}</svg>`
})
</script>
