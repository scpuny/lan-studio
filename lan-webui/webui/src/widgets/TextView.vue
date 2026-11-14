<template>
  <PanelShell
    :title="panel.title"
    :actions="panel.actions"
    :busy="busy"
    :error="error"
    @run="onRun"
  >
    <pre v-if="text" class="ls-code">{{ text }}</pre>
    <a-empty v-else description="无数据" />
  </PanelShell>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { CapabilityAction, CapabilityPanel } from '../capabilities'
import { usePanel } from '../usePanel'
import PanelShell from './PanelShell.vue'

const props = defineProps<{ component: string; panel: CapabilityPanel }>()
const { data, error, busy, runAction } = usePanel(props.component, props.panel)

// Metrics / snapshots / exports are returned as raw text; object payloads fall
// back to a readable JSON rendering.
const text = computed(() => {
  if (typeof data.value === 'string') return data.value
  if (data.value === null || data.value === undefined) return ''
  return JSON.stringify(data.value, null, 2)
})

function onRun(action: CapabilityAction, values: Record<string, string> | null): void {
  void runAction(action, values ?? undefined)
}
</script>
