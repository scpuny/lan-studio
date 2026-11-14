import { createApp } from 'vue'
import type { Plugin } from 'vue'
import {
  Alert,
  Button,
  ConfigProvider,
  Empty,
  Input,
  InputNumber,
  InputPassword,
  Modal,
  Option,
  Progress,
  Select,
  Space,
  Spin,
  Switch,
  Table,
  TabPane,
  Tabs,
  Tag,
  Tooltip,
} from '@arco-design/web-vue'
import '@arco-design/web-vue/dist/arco.css'
import './styles/app.css'
import App from './App.vue'
import { installDemoMode } from './demo'

// Only the controls the console actually uses are registered: importing the
// whole library would bloat the embedded single file for no benefit.
const components = [
  Alert,
  Button,
  ConfigProvider,
  Empty,
  Input,
  InputNumber,
  InputPassword,
  Modal,
  Option,
  Progress,
  Select,
  Space,
  Spin,
  Switch,
  Table,
  TabPane,
  Tabs,
  Tag,
  Tooltip,
]

// Arco components expose `install(app, options?)`, which `app.use` accepts at
// runtime but Vue models as a callable `FunctionPlugin`. One documented cast
// here keeps the cast out of every call site.
const asPlugin = (component: unknown): Plugin => component as Plugin

// Opt-in visual-review shim: `?demo=1` serves realistic fixtures instead of a
// live backend. No-op in production / render-check. Must run before mount so
// the first `/api/status` fetch is already intercepted.
installDemoMode()

const app = createApp(App)
for (const component of components) app.use(asPlugin(component))
app.mount('#app')
