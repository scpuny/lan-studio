<template>
  <PanelShell
    :title="panel.title"
    :actions="panel.actions"
    :busy="busy"
    :error="error"
    @run="onRun"
  >
    <a-table
      v-if="rows.length"
      :data="rows"
      :columns="columns"
      :pagination="false"
      size="small"
      :bordered="{ wrapper: false, cell: false }"
      row-key="__idx"
      class="ls-table"
    />
    <a-empty v-else description="无数据" />
  </PanelShell>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { CapabilityAction, CapabilityPanel } from '../capabilities'
import { firstArray, usePanel } from '../usePanel'
import PanelShell from './PanelShell.vue'

const props = defineProps<{ component: string; panel: CapabilityPanel }>()
const { data, error, busy, runAction } = usePanel(props.component, props.panel)

const rows = computed(() =>
  firstArray(data.value).map((item, index) => {
    const obj = (typeof item === 'object' && item !== null ? item : { value: item }) as Record<
      string,
      unknown
    >
    return { ...obj, __idx: index }
  }),
)

// Prefer the manifest-declared columns; otherwise derive from the first row.
const columns = computed(() => {
  if (props.panel.columns && props.panel.columns.length) {
    return props.panel.columns.map((column) => ({ title: column.label, dataIndex: column.key }))
  }
  const first = rows.value[0]
  if (!first) return []
  return Object.keys(first)
    .filter((key) => key !== '__idx')
    .map((key) => ({ title: key, dataIndex: key }))
})

function onRun(action: CapabilityAction, values: Record<string, string> | null): void {
  void runAction(action, values ?? undefined)
}
</script>
