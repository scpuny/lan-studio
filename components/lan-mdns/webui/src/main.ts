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

const app = createApp(App)
for (const component of components) app.use(asPlugin(component))
app.mount('#app')
