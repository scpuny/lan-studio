<template>
  <PanelShell
    :title="panel.title"
    :actions="panel.actions"
    :busy="busy"
    :error="error"
    @run="onRun"
  >
    <div v-if="rows.length" class="ls-kv">
      <div v-for="row in rows" :key="row.key" class="ls-kv-row">
        <span class="ls-kv-key">{{ row.key }}</span>
        <b class="ls-kpi-value">{{ row.value }}</b>
      </div>
    </div>
    <a-empty v-else description="无数据" />
  </PanelShell>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { CapabilityAction, CapabilityPanel } from '../capabilities'
import { toRows, usePanel } from '../usePanel'
import PanelShell from './PanelShell.vue'

const props = defineProps<{ component: string; panel: CapabilityPanel }>()
const { data, error, busy, runAction } = usePanel(props.component, props.panel)

const rows = computed(() => toRows(data.value))

function onRun(action: CapabilityAction, values: Record<string, string> | null): void {
  void runAction(action, values ?? undefined)
}
</script>
