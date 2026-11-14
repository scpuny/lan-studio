<template>
  <PanelShell
    :title="panel.title"
    :actions="panel.actions"
    :busy="busy"
    :error="error"
    @run="onRun"
  >
    <div v-if="lines.length" class="ls-loglist">
      <div v-for="(line, index) in lines" :key="index" class="ls-logline">{{ line }}</div>
    </div>
    <a-empty v-else description="无数据" />
  </PanelShell>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { CapabilityAction, CapabilityPanel } from '../capabilities'
import { toLines, usePanel } from '../usePanel'
import PanelShell from './PanelShell.vue'

const props = defineProps<{ component: string; panel: CapabilityPanel }>()
const { data, error, busy, runAction } = usePanel(props.component, props.panel)

const lines = computed(() => toLines(data.value))

function onRun(action: CapabilityAction, values: Record<string, string> | null): void {
  void runAction(action, values ?? undefined)
}
</script>
