<template>
  <a-config-provider size="large">
    <div class="ls-app">
      <aside class="ls-rail">
        <div class="ls-brand">
          <div class="ls-brand-mark"><Icon name="broadcast" :size="19" /></div>
          <div>
            <div class="ls-brand-name">Lan Studio</div>
            <div class="ls-brand-sub">统一控制台</div>
          </div>
        </div>

        <div class="ls-nav-label">组件</div>
        <select
          v-if="components.length"
          class="ls-component-select"
          :value="activeComponentName"
          @change="onComponentChange"
        >
          <option v-for="item in components" :key="item.name" :value="item.name">
            {{ item.label }}
          </option>
        </select>

        <template v-if="activeComponent">
          <div class="ls-nav-label">模块</div>
          <nav class="ls-nav">
            <button
              v-for="mod in modules"
              :key="mod.id"
              class="ls-nav-item"
              :class="{ on: activeModule && mod.id === activeModule.id }"
              type="button"
              @click="selectModule(mod.id)"
            >
              <Icon :name="mod.icon" />
              <span>{{ mod.title }}</span>
            </button>
          </nav>
        </template>

        <div class="ls-rail-foot">
          <div class="ls-rail-stat">
            <span>当前组件</span>
            <b class="ls-mono">{{ activeComponent ? activeComponent.label : '—' }}</b>
          </div>
          <div class="ls-rail-stat">
            <span>代理端口</span>
            <b class="ls-mono">{{ activeComponent ? activeComponent.port : '—' }}</b>
          </div>
          <div class="ls-rail-stat">
            <span>控制台</span>
            <b class="ls-inline">
              <span class="ls-dot" :class="session.token || session.password ? 'is-up' : 'is-down'"></span>
              {{ session.token || session.password ? '已配置凭据' : '未配置凭据' }}
            </b>
          </div>
        </div>
      </aside>

      <main class="ls-main">
        <header class="ls-topbar">
          <div v-if="activeComponent && activeModule">
            <div class="ls-eyebrow">
              <span class="ls-eyebrow-dot"></span>
              <span>{{ activeComponent.label }} / {{ activeModule.title }}</span>
            </div>
            <h1 class="ls-page-title">{{ activeModule.title }}</h1>
            <p class="ls-page-sub">{{ activeComponent.label }} · 端口 {{ activeComponent.port }}</p>
          </div>
          <div v-else class="ls-page-title">Lan Studio 统一控制台</div>

          <div class="ls-top-actions">
            <span class="ls-status-pill">
              <span class="ls-dot" :class="session.token || session.password ? 'is-up' : 'is-down'"></span>
              <span>{{ session.token || session.password ? '已配置凭据' : '匿名' }}</span>
            </span>
            <button class="ls-icon-btn" :title="dark ? '切换浅色' : '切换深色'" @click="toggleTheme">
              <Icon :name="dark ? 'sun' : 'moon'" />
            </button>
            <a-button size="small" @click="openAuth">
              <Icon name="lock" :size="14" />
              {{ session.token || session.password ? '凭据' : '管理员' }}
            </a-button>
          </div>
        </header>

        <a-alert
          v-if="banner"
          type="warning"
          :show-icon="true"
          closable
          @close="banner = ''"
        >
          {{ banner }}
        </a-alert>

        <div v-if="activeModule" class="ls-view">
          <PanelView
            v-for="panel in activeModule.panels"
            :key="panel.id"
            :component="activeComponent!.name"
            :panel="panel"
          />
        </div>
        <a-empty v-else description="该组件没有可显示的模块" />
      </main>

      <a-modal
        v-model:visible="authVisible"
        title="控制台凭据"
        :footer="false"
        :width="460"
        unmount-on-close
      >
        <div class="ls-stack">
          <p class="ls-note">
            写操作统一由管理员密码保护。凭据仅保存在本会话（sessionStorage），不会写入磁盘，
            并随同源代理转发到各组件；凭据由目标组件校验。
          </p>
          <div class="ls-field">
            <label>Admin Token（组件配置了 server.admin_token 时必填）</label>
            <a-input v-model="authForm.token" placeholder="留空表示未启用" allow-clear />
          </div>
          <div class="ls-field">
            <label>管理员密码</label>
            <a-input-password v-model="authForm.password" placeholder="用于解锁写操作" />
          </div>
          <a-button type="primary" long :loading="busy" @click="applyCredentials">
            保存凭据
          </a-button>
        </div>
      </a-modal>
    </div>
  </a-config-provider>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { Message } from '@arco-design/web-vue'
import { errorMessage, session, storeCredentials } from './api'
import { getManifest, visibleModules } from './capabilities'
import type { CapabilityComponent, CapabilityModule } from './capabilities'
import Icon from './Icon.vue'
import PanelView from './widgets/PanelView.vue'

const manifest = getManifest()
const components = manifest.components

const activeComponentName = ref<string>(components[0]?.name ?? '')
const activeComponent = computed<CapabilityComponent | null>(() =>
  components.find((component) => component.name === activeComponentName.value) ?? components[0] ?? null,
)
const modules = computed<CapabilityModule[]>(() =>
  activeComponent.value ? visibleModules(manifest, activeComponent.value) : [],
)

function pickDefaultModule(list: CapabilityModule[]): CapabilityModule | null {
  if (!list.length) return null
  return list.reduce(
    (best, module) =>
      (module.panels?.length ?? 0) > (best.panels?.length ?? 0) ? module : best,
    list[0]!,
  )
}

const activeModuleId = ref<string>('')
const activeModule = computed<CapabilityModule | null>(() => {
  const list = modules.value
  if (!list.length) return null
  if (activeModuleId.value) {
    const found = list.find((module) => module.id === activeModuleId.value)
    if (found) return found
  }
  return pickDefaultModule(list)
})

function onComponentChange(event: Event): void {
  const target = event.target as HTMLSelectElement
  activeComponentName.value = target.value
  activeModuleId.value = ''
}

function selectModule(id: string): void {
  activeModuleId.value = id
}

const dark = ref(false)
const authVisible = ref(false)
const busy = ref(false)
const banner = ref('')
const authForm = reactive<{ token: string; password: string }>({ token: '', password: '' })

function applyTheme(): void {
  document.body.setAttribute('arco-theme', dark.value ? 'dark' : 'light')
  try {
    localStorage.setItem('lanstudio.dark', dark.value ? '1' : '0')
  } catch {
    /* ignore */
  }
}

function toggleTheme(): void {
  dark.value = !dark.value
  applyTheme()
}

function openAuth(): void {
  authForm.token = session.token
  authForm.password = session.password
  authVisible.value = true
}

async function applyCredentials(): Promise<void> {
  busy.value = true
  try {
    storeCredentials(authForm.token, authForm.password)
    banner.value = ''
    authVisible.value = false
    Message.success('凭据已更新，写操作将随代理转发到各组件')
  } catch (error) {
    banner.value = `凭据保存失败：${errorMessage(error)}`
  } finally {
    busy.value = false
  }
}

onMounted(() => {
  try {
    dark.value = localStorage.getItem('lanstudio.dark') === '1'
  } catch {
    dark.value = false
  }
  applyTheme()
})
</script>
