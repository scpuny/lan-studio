<template>
  <PanelShell
    :title="panel.title"
    :actions="panel.actions"
    :busy="busy"
    :error="error"
    @run="onRun"
  >
    <textarea v-model="draft" class="ls-editor" spellcheck="false" rows="18"></textarea>
    <p class="ls-note">写操作由管理员密码保护；保存前会经后端校验。</p>
  </PanelShell>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Message } from '@arco-design/web-vue'
import { componentApi, errorMessage } from '../api'
import type { CapabilityAction, CapabilityPanel } from '../capabilities'
import PanelShell from './PanelShell.vue'

const props = defineProps<{ component: string; panel: CapabilityPanel }>()
const api = componentApi(props.component)
const draft = ref<string>('')
const error = ref<string>('')
const busy = ref<boolean>(false)
const loaded = ref<boolean>(false)

async function load(): Promise<void> {
  error.value = ''
  busy.value = true
  try {
    const data = await api.get<{ raw: string }>(props.panel.endpoint.path)
    draft.value = data.raw ?? ''
    loaded.value = true
  } catch (caught) {
    error.value = errorMessage(caught)
  } finally {
    busy.value = false
  }
}

function download(text: string, filename: string): void {
  const blob = new Blob([text], { type: 'text/yaml;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  anchor.click()
  URL.revokeObjectURL(url)
}

async function onRun(
  action: CapabilityAction,
  values: Record<string, string> | null,
): Promise<void> {
  error.value = ''
  busy.value = true
  try {
    if (action.path === props.panel.endpoint.path && action.method === 'POST') {
      // Persist the edited document: the backend expects `{ config_text }`.
      await api.post(action.path, { config_text: draft.value })
      Message.success('配置已保存并热重载')
    } else if (action.method === 'GET') {
      const data = await api.get<{ config_text?: string }>(action.path)
      if (data && typeof data.config_text === 'string') {
        download(data.config_text, `${props.component}-snapshot.yaml`)
        Message.success('快照已导出')
      }
    } else {
      await api.post(action.path, values ?? {})
    }
    await load()
  } catch (caught) {
    error.value = errorMessage(caught)
    Message.error(error.value)
  } finally {
    busy.value = false
  }
}

onMounted(load)
</script>
