<template>
  <header class="ls-header">
    <div class="ls-brand">
      <span class="ls-dot" :class="status.running ? 'is-up' : 'is-down'"></span>
      <span>Lan Studio</span>
      <small>{{ status.component || 'lan-mdns' }} · {{ status.version || '—' }}</small>
    </div>

    <div class="ls-header-spacer"></div>

    <div class="ls-header-meta">
      <span>运行 {{ formatUptime(status.uptime) }}</span>
      <span>mDNS {{ status.group || '—' }}:{{ status.mdns_port || '—' }}</span>
      <span class="ls-inline">
        <span class="ls-dot" :class="status.security.password_set ? 'is-up' : 'is-warn'"></span>
        写保护{{ status.security.password_set ? '已启用' : '待初始化' }}
      </span>
      <span class="ls-inline">
        <a-switch v-model="dark" size="small" @change="applyTheme" />
        <span>深色</span>
      </span>
      <a-button size="small" @click="openAuth">{{ session.password ? '已解锁' : '管理员' }}</a-button>
    </div>
  </header>

  <main class="ls-body">
    <a-alert v-if="banner" type="warning" :show-icon="true" closable @close="banner = ''">
      {{ banner }}
    </a-alert>

    <a-tabs v-model:active-key="activeKey" type="line" size="large" :destroy-on-hide="false">
      <a-tab-pane key="overview" title="状态总览">
        <OverviewPage :active="activeKey === 'overview'" />
      </a-tab-pane>
      <a-tab-pane key="config" title="配置管理">
        <ConfigPage :active="activeKey === 'config'" @need-auth="openAuth" />
      </a-tab-pane>
      <a-tab-pane key="observe" title="可观测">
        <ObservePage :active="activeKey === 'observe'" />
      </a-tab-pane>
      <a-tab-pane key="logs" title="日志">
        <LogsPage :active="activeKey === 'logs'" />
      </a-tab-pane>
      <a-tab-pane key="alerts" title="告警设置">
        <AlertsPage :active="activeKey === 'alerts'" @goto="activeKey = $event" />
      </a-tab-pane>
      <a-tab-pane key="history" title="版本历史">
        <HistoryPage :active="activeKey === 'history'" @need-auth="openAuth" />
      </a-tab-pane>
    </a-tabs>
  </main>

  <a-modal
    v-model:visible="authVisible"
    :title="authTitle"
    :footer="false"
    :width="460"
    unmount-on-close
  >
    <div class="ls-stack">
      <p class="ls-note">
        写操作统一由管理员密码保护。密码以 PBKDF2-SHA256 摘要保存在数据目录（权限 0600），
        浏览器只在会话内保留明文，不会写入磁盘。
      </p>

      <div class="ls-field">
        <label>Admin Token（配置了 server.admin_token 时必填）</label>
        <a-input v-model="authForm.token" placeholder="留空表示未启用" allow-clear />
      </div>

      <template v-if="!session.passwordSet">
        <div class="ls-field">
          <label>设置管理员密码（至少 8 位）</label>
          <a-input-password v-model="authForm.password" placeholder="首次使用需先设置" />
        </div>
        <div class="ls-field">
          <label>确认密码</label>
          <a-input-password v-model="authForm.confirm" placeholder="再次输入" />
        </div>
        <a-button type="primary" long :loading="busy" @click="setupPassword">设置并解锁</a-button>
      </template>

      <template v-else>
        <div class="ls-field">
          <label>当前密码</label>
          <a-input-password v-model="authForm.password" placeholder="用于解锁写操作" />
        </div>
        <div class="ls-row">
          <a-button type="primary" :loading="busy" @click="unlock">解锁</a-button>
          <a-button @click="toggleChange">{{ changing ? '取消修改' : '修改密码' }}</a-button>
        </div>

        <template v-if="changing">
          <div class="ls-field">
            <label>新密码（至少 8 位）</label>
            <a-input-password v-model="authForm.newPassword" />
          </div>
          <div class="ls-field">
            <label>确认新密码</label>
            <a-input-password v-model="authForm.confirm" />
          </div>
          <a-button type="primary" status="warning" long :loading="busy" @click="changePassword">
            确认修改
          </a-button>
        </template>
      </template>
    </div>
  </a-modal>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { Message } from '@arco-design/web-vue'
import { clearPassword, errorMessage, get, post, session, storeCredentials } from './api'
import { emptyStatus } from './defaults'
import { formatUptime, usePolling } from './poll'
import type { PasswordResult, StatusData, VerifyResult } from './types'
import AlertsPage from './pages/AlertsPage.vue'
import ConfigPage from './pages/ConfigPage.vue'
import HistoryPage from './pages/HistoryPage.vue'
import LogsPage from './pages/LogsPage.vue'
import ObservePage from './pages/ObservePage.vue'
import OverviewPage from './pages/OverviewPage.vue'

interface AuthForm {
  token: string
  password: string
  newPassword: string
  confirm: string
}

const activeKey = ref('overview')
const banner = ref('')
const status = reactive<StatusData>(emptyStatus())

const dark = ref(false)
const authVisible = ref(false)
const changing = ref(false)
const busy = ref(false)
const authForm = reactive<AuthForm>({ token: '', password: '', newPassword: '', confirm: '' })

const authTitle = computed(() => (session.passwordSet ? '管理员凭据' : '初始化管理员密码'))

function applyTheme(): void {
  document.body.setAttribute('arco-theme', dark.value ? 'dark' : 'light')
  try {
    localStorage.setItem('lanstudio.dark', dark.value ? '1' : '0')
  } catch {
    /* ignore */
  }
}

async function refreshStatus(): Promise<void> {
  try {
    const data = await get<StatusData>('/api/status')
    Object.assign(status, data)
    session.tokenRequired = data.security.token_required
    session.passwordSet = data.security.password_set
    session.ready = true
    banner.value = ''
  } catch (error) {
    banner.value = `无法连接管理接口：${errorMessage(error)}`
  }
}

function openAuth(): void {
  authForm.token = session.token
  authForm.password = session.password
  authForm.newPassword = ''
  authForm.confirm = ''
  changing.value = false
  authVisible.value = true
}

async function setupPassword(): Promise<void> {
  if (authForm.password.length < 8) {
    Message.warning('管理员密码至少 8 位')
    return
  }
  if (authForm.password !== authForm.confirm) {
    Message.warning('两次输入的密码不一致')
    return
  }
  busy.value = true
  try {
    storeCredentials(authForm.token, '')
    await post<PasswordResult>('/api/auth/password', { new_password: authForm.password })
    storeCredentials(authForm.token, authForm.password)
    await refreshStatus()
    Message.success('管理员密码已设置，写操作已解锁')
    authVisible.value = false
  } catch (error) {
    Message.error(errorMessage(error))
  } finally {
    busy.value = false
  }
}

async function unlock(): Promise<void> {
  if (!authForm.password) {
    Message.warning('请输入管理员密码')
    return
  }
  busy.value = true
  try {
    storeCredentials(authForm.token, authForm.password)
    await post<VerifyResult>('/api/auth/verify', { password: authForm.password })
    Message.success('已解锁写操作')
    authVisible.value = false
  } catch (error) {
    clearPassword()
    storeCredentials(authForm.token, '')
    Message.error(errorMessage(error))
  } finally {
    busy.value = false
  }
}

function toggleChange(): void {
  changing.value = !changing.value
  authForm.newPassword = ''
  authForm.confirm = ''
}

async function changePassword(): Promise<void> {
  if (authForm.newPassword.length < 8) {
    Message.warning('新密码至少 8 位')
    return
  }
  if (authForm.newPassword !== authForm.confirm) {
    Message.warning('两次输入的新密码不一致')
    return
  }
  busy.value = true
  try {
    storeCredentials(authForm.token, authForm.password)
    await post<PasswordResult>('/api/auth/password', {
      current_password: authForm.password,
      new_password: authForm.newPassword,
    })
    storeCredentials(authForm.token, authForm.newPassword)
    authForm.password = authForm.newPassword
    changing.value = false
    Message.success('管理员密码已更新')
    authVisible.value = false
  } catch (error) {
    Message.error(errorMessage(error))
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
  void refreshStatus()
})

usePolling(refreshStatus, 5000)
</script>
