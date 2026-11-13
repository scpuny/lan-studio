/// <reference types="vite/client" />

// Arco ships its `GlobalComponents` augmentation in a separate declaration file
// that `es/index.d.ts` does not reference, so it must be pulled in explicitly.
// Without it every `<a-button>` in a template would be an unknown element and
// silently lose prop checking.
/// <reference types="@arco-design/web-vue/es/components" />
