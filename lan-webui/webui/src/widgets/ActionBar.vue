<template>
  <div v-if="actions && actions.length" class="ls-actions">
    <template v-for="action in actions" :key="action.id">
      <div v-if="action.fields && action.fields.length" class="ls-action-form">
        <a-button size="small" @click="toggle(action.id)">
          <Icon name="tune" :size="13" /> {{ action.label }}
        </a-button>
        <div v-if="open === action.id" class="ls-action-fields">
          <div v-for="field in action.fields" :key="field.key" class="ls-field">
            <label>{{ field.label }}</label>
            <textarea
              v-if="field.type === 'textarea'"
              v-model="values[`${action.id}:${field.key}`]"
              rows="3"
              class="ls-input"
            ></textarea>
            <input v-else v-model="values[`${action.id}:${field.key}`]" class="ls-input" />
          </div>
          <a-button type="primary" size="small" :loading="busy" @click="submit(action)">
            执行
          </a-button>
        </div>
      </div>
      <a-button v-else size="small" :loading="busy" @click="quick(action)">
        {{ action.label }}
      </a-button>
    </template>
  </div>
</template>

<script setup lang="ts">
import { reactive, ref } from 'vue'
import type { CapabilityAction } from '../capabilities'
import Icon from '../Icon.vue'

defineProps<{ actions?: CapabilityAction[]; busy?: boolean }>()
const emit = defineEmits<{
  (event: 'run', action: CapabilityAction, values: Record<string, string> | null): void
}>()

const open = ref<string | null>(null)
const values = reactive<Record<string, string>>({})

function toggle(id: string): void {
  open.value = open.value === id ? null : id
}

function quick(action: CapabilityAction): void {
  emit('run', action, null)
}

function submit(action: CapabilityAction): void {
  const out: Record<string, string> = {}
  for (const field of action.fields ?? []) {
    out[field.key] = values[`${action.id}:${field.key}`] ?? ''
  }
  emit('run', action, out)
  open.value = null
}
</script>
