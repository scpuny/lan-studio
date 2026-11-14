<template>
  <component :is="widgetComponent" :component="component" :panel="panel" />
</template>

<script setup lang="ts">
import type { CapabilityPanel, WidgetKind } from '../capabilities'
import ActionList from './ActionList.vue'
import CodeEditor from './CodeEditor.vue'
import Form from './Form.vue'
import KvGrid from './KvGrid.vue'
import LogList from './LogList.vue'
import MetricGrid from './MetricGrid.vue'
import Table from './Table.vue'
import TextView from './TextView.vue'

const props = defineProps<{ component: string; panel: CapabilityPanel }>()

const REGISTRY: Record<WidgetKind, unknown> = {
  'kv-grid': KvGrid,
  'metric-grid': MetricGrid,
  table: Table,
  'log-list': LogList,
  'text-view': TextView,
  'code-editor': CodeEditor,
  form: Form,
  'action-list': ActionList,
}

// `kv-grid` is the safe default for any unknown widget kind so a manifest typo
// degrades to a readable key/value view rather than a blank card.
const widgetComponent = REGISTRY[props.panel.widget] ?? KvGrid
</script>
