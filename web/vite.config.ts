import { defineConfig, loadEnv, type HtmlTagDescriptor, type Plugin, type ProxyOptions } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { mockBackend } from './mock/plugin.ts';

const root = fileURLToPath(new URL('.', import.meta.url));

/** The map's code is a dynamic import, so the first API requests need not
 *  wait for it, but every page load needs it: the page fetches it alongside
 *  the entry rather than once the entry has run. */
function preloadMap(): Plugin {
  return {
    name: 'wx-preload-map',
    apply: 'build',
    transformIndexHtml: {
      order: 'post',
      handler(_html, ctx) {
        if (!ctx.bundle || !ctx.filename.endsWith('index.html')) return;
        const chunk = Object.values(ctx.bundle).find((c) => c.type === 'chunk' && c.facadeModuleId?.endsWith('/src/map/mapController.ts'));
        if (!chunk) return;
        const tag: HtmlTagDescriptor = { tag: 'link', attrs: { rel: 'modulepreload', crossorigin: true, href: `/${chunk.fileName}` }, injectTo: 'head' };
        return [tag];
      },
    },
  };
}

export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, root, ''), ...process.env };
  const mock = env.VITE_MOCK === '1';
  const backend = env.WX_BACKEND || 'http://127.0.0.1:8080';
  const proxy: Record<string, ProxyOptions> | undefined = mock
    ? undefined
    : {
        '/api': { target: backend, changeOrigin: false },
        '/tiles': { target: backend, changeOrigin: false },
        '/basemap': { target: backend, changeOrigin: false },
        '/healthz': { target: backend, changeOrigin: false },
        '/ws': { target: backend.replace(/^http/, 'ws'), ws: true },
      };

  return {
    plugins: [react(), preloadMap(), mock ? mockBackend({ repoRoot: fileURLToPath(new URL('..', import.meta.url)) }) : null],
    server: {
      port: Number(env.PORT || 5180),
      strictPort: true,
      host: env.HOST || '127.0.0.1',
      proxy,
    },
    preview: {
      port: Number(env.PORT || 5181),
      strictPort: true,
      host: env.HOST || '127.0.0.1',
      proxy,
    },
    build: {
      target: 'es2022',
      // Source maps only when asked: `npm run build -- --sourcemap`.
      sourcemap: false,
      chunkSizeWarningLimit: 1500,
      rolldownOptions: {
        input: {
          main: fileURLToPath(new URL('index.html', import.meta.url)),
          status: fileURLToPath(new URL('status.html', import.meta.url)),
        },
      },
    },
    worker: { format: 'es' },
    test: {
      include: ['src/**/*.test.ts', 'mock/**/*.test.ts'],
      environment: 'node',
    },
  };
});
