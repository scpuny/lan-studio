import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { viteSingleFile } from 'vite-plugin-singlefile'

// The console ships as ONE html file that `common-webui` embeds with
// `include_str!`, so the runtime never reads an external asset (PRD 3.7 / F-I01).
// `cssCodeSplit: false` + `assetsInlineLimit` keep CSS and every asset inline;
// types are checked by `vue-tsc` before `vite build` runs (see package.json).
export default defineConfig({
  plugins: [vue(), viteSingleFile({ removeViteModuleLoader: true })],
  build: {
    outDir: fileURLToPath(new URL('../../../common/webui/src/assets', import.meta.url)),
    emptyOutDir: false,
    cssCodeSplit: false,
    assetsInlineLimit: 100 * 1024 * 1024,
    reportCompressedSize: false,
    target: 'es2020',
  },
})
