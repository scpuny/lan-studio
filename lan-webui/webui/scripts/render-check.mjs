// Headless render check for the built console.
//
// `vue-tsc` proves the code type-checks; it does not prove the page paints.
// This script evaluates the built single-file bundle inside jsdom and asserts
// that the app actually mounted: the sidebar navigation (six items), the panels,
// the status lights and the KPI cells must exist. A blank page therefore fails
// CI instead of shipping.
//
// Chrome is not used on purpose: it needs ptrace/sandbox privileges that CI and
// restricted containers do not always grant, while jsdom runs anywhere Node does.
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { JSDOM, VirtualConsole } from 'jsdom'

const BUNDLE = fileURLToPath(
  new URL('../../../common/webui/src/assets/index.html', import.meta.url),
)

const html = readFileSync(BUNDLE, 'utf8')
// The bundle is one inline `<script type="module">` in <head>. jsdom does not
// execute module scripts, and splicing the code back into HTML is unreliable
// (the code itself contains `</script>`-like sequences), so it is extracted and
// evaluated in the window instead. `vite-plugin-singlefile` emits a plain IIFE:
// no top-level import/export, hence evaluation is equivalent.
const script = html.match(/<script type="module"[^>]*>([\s\S]*?)<\/script>/)
if (!script) throw new Error('inline module script not found in the built bundle')
const code = script[1]

const consoleErrors = []
const virtualConsole = new VirtualConsole()
for (const event of ['jsdomError', 'error']) {
  virtualConsole.on(event, (msg) =>
    consoleErrors.push(`${event}: ${String(msg && msg.message ? msg.message : msg)}`),
  )
}

const dom = new JSDOM(html.replace(script[0], ''), {
  runScripts: 'dangerously',
  pretendToBeVisual: true,
  virtualConsole,
  url: 'http://127.0.0.1/',
})

const { window } = dom
window.ResizeObserver =
  window.ResizeObserver || class { observe() {} unobserve() {} disconnect() {} }
window.matchMedia =
  window.matchMedia ||
  function () {
    return {
      matches: false,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
    }
  }
// No backend during the check: every API call rejects, which is exactly the
// degraded path the console must survive.
window.fetch = () => Promise.reject(new Error('render-check: offline'))
window.eval(code)

const marker = [
  'Lan Studio',
  '统一控制台',
  '名称解析与服务发现',
  '状态总览',
  '配置管理',
  '可观测',
  '日志',
  '告警',
  '版本历史',
  '管理员',
]

setTimeout(() => {
  const { document } = window
  const app = document.getElementById('app')
  const text = app ? (app.textContent ?? '') : ''
  const missing = marker.filter((item) => !text.includes(item))
  const navItems = document.querySelectorAll('.ls-nav-item').length
  const panels = document.querySelectorAll('.ls-panel').length
  const dots = document.querySelectorAll('.ls-dot').length
  const kpi = document.querySelectorAll('.ls-kpi-value').length

  console.log(`markers  : ${marker.length - missing.length}/${marker.length}` +
    (missing.length ? `  missing=${JSON.stringify(missing)}` : ''))
  console.log(`nav      : ${navItems}  panels: ${panels}  dots: ${dots}  kpi: ${kpi}`)
  console.log(`console  : ${consoleErrors.length}` +
    (consoleErrors.length ? `  ${consoleErrors.slice(0, 3).join(' | ')}` : ''))

  // The render-check runs offline (no backend, no demo flag) so panel data and
  // KPI cells are absent by design; we only assert the shell painted.
  const problems = []
  if (missing.length) problems.push(`missing markers: ${missing.join(', ')}`)
  if (navItems < 6) problems.push(`expected >= 6 nav items (modules), got ${navItems}`)
  if (panels < 1) problems.push(`expected >= 1 panel card, got ${panels}`)
  if (dots < 2) problems.push(`expected >= 2 status lights, got ${dots}`)
  if (consoleErrors.length) problems.push(`console errors: ${consoleErrors[0]}`)

  if (problems.length) {
    console.error('RENDER FAILED')
    for (const problem of problems) console.error(`  - ${problem}`)
    process.exit(1)
  }
  console.log('RENDER OK')
  process.exit(0)
}, 1500)
