<template>
  <section class="ls-panel">
    <header class="ls-panel-head">
      <h3>{{ title }}</h3>
      <span v-if="busy" class="ls-spin" aria-hidden="true"></span>
      <span v-if="error" class="ls-panel-err">{{ error }}</span>
    </header>
    <div class="ls-panel-body">
      <slot />
    </div>
    <ActionBar :actions="actions" :busy="busy" @run="(a, v) => emit('run', a, v)" />
  </section>
</template>

<script setup lang="ts">
import type { CapabilityAction } from '../capabilities'
import ActionBar from './ActionBar.vue'

defineProps<{
  title: string
  actions?: CapabilityAction[]
  busy?: boolean
  error?: string
}>()
const emit = defineEmits<{
  (event: 'run', action: CapabilityAction, values: Record<string, string> | null): void
}>()
</script>
