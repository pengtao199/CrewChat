/**
 * Renderer-only Vite config, ported from AionUi's electron.vite.config.ts
 * (Apache-2.0, see NOTICE.md). Electron's main/preload targets are dropped so
 * the UI builds as a plain static web app that our Python backend serves.
 */
import { readFileSync } from 'fs';
import { resolve } from 'path';
import UnoCSS from 'unocss/vite';
import { defineConfig } from 'vite';

import unoConfig from './uno.config.ts';

const rootPackageJson = JSON.parse(readFileSync(resolve(__dirname, 'package.json'), 'utf-8')) as {
  version?: string;
};

const BACKEND = process.env.CREWCHAT_BACKEND ?? 'http://127.0.0.1:8000';

const rendererRoot = resolve(__dirname, 'packages/desktop/src/renderer');
const desktopSrcRoot = resolve(__dirname, 'packages/desktop/src');

/** Wraps named @icon-park/react imports with IconParkHOC, as upstream does. */
function iconParkPlugin() {
  return {
    name: 'vite-plugin-icon-park',
    enforce: 'pre' as const,
    transform(source: string, id: string) {
      if (!id.endsWith('.tsx') || id.includes('node_modules')) return null;
      if (!source.includes('@icon-park/react')) return null;
      const transformedSource = source.replace(
        /import\s+\{\s+([a-zA-Z, ]*)\s+\}\s+from\s+['"]@icon-park\/react['"](;?)/g,
        function (str, match) {
          if (!match) return str;
          const components = match.split(',');
          const importComponent = str.replace(
            match,
            components.map((key: string) => `${key} as _${key.trim()}`).join(', ')
          );
          const hoc = `import IconParkHOC from '@renderer/components/IconParkHOC';
          ${components.map((key: string) => `const ${key.trim()} = IconParkHOC(_${key.trim()})`).join(';\n')}`;
          return importComponent + ';' + hoc;
        }
      );
      if (transformedSource !== source)
        return { code: transformedSource, map: null } as { code: string; map: null };
      return null;
    }
  };
}

export default defineConfig(({ mode }) => {
  const isDevelopment = mode === 'development';

  return {
    root: rendererRoot,
    base: './',
    publicDir: resolve(__dirname, 'public'),
    appType: 'mpa',
    server: {
      port: 5173,
      hmr: { host: 'localhost' },
      // 开发期把 /api 和 /ws 代理到我们的 Python 后端（生产由后端直接服务静态文件）
      proxy: {
        '/api': { target: BACKEND, changeOrigin: true },
        '/ws': { target: BACKEND, ws: true, changeOrigin: true }
      }
    },
    resolve: {
      alias: {
        '@': desktopSrcRoot,
        '@common': resolve(__dirname, 'packages/desktop/src/common'),
        '@renderer': rendererRoot,
        streamdown: resolve(__dirname, 'node_modules/streamdown/dist/index.js'),
        // Electron 仅供桌面壳使用；web 构建下所有调用点都有 window.electronAPI 守卫，
        // 因此把 electron 与其 sentry 渲染器包统一指向空实现，避免打包 Electron 代码。
        electron: resolve(__dirname, 'stubs/electron.ts'),
        '@sentry/electron/renderer': resolve(__dirname, 'stubs/sentry-renderer.ts')
      },
      extensions: ['.ts', '.tsx', '.js', '.jsx', '.css'],
      dedupe: [
        'react',
        'react-dom',
        'react-router-dom',
        '@codemirror/state',
        '@codemirror/view',
        '@codemirror/language',
        '@lezer/highlight'
      ]
    },
    plugins: [UnoCSS(unoConfig), iconParkPlugin()],
    build: {
      target: 'es2022',
      outDir: resolve(__dirname, 'dist'),
      emptyOutDir: true,
      sourcemap: isDevelopment,
      minify: !isDevelopment,
      reportCompressedSize: false,
      chunkSizeWarningLimit: 1500,
      cssCodeSplit: true,
      rollupOptions: {
        input: { index: resolve(rendererRoot, 'index.html') },
        external: ['node:crypto', 'crypto'],
        onwarn(warning, warn) {
          if (warning.code === 'EVAL') return;
          warn(warning);
        },
        output: {
          manualChunks(id: string) {
            if (!id.includes('node_modules')) return undefined;
            if (
              id.includes('/react-dom/') ||
              id.includes('/react/') ||
              id.includes('/@arco-design/') ||
              id.includes('/react-markdown/') ||
              id.includes('/remark-') ||
              id.includes('/rehype-') ||
              id.includes('/unified/') ||
              id.includes('/mdast-') ||
              id.includes('/hast-') ||
              id.includes('/micromark') ||
              id.includes('/react-syntax-highlighter/') ||
              id.includes('/refractor/') ||
              id.includes('/highlight.js/') ||
              id.includes('/monaco-editor/') ||
              id.includes('/@monaco-editor/') ||
              id.includes('/codemirror/') ||
              id.includes('/@codemirror/') ||
              id.includes('/katex/') ||
              id.includes('/wavedrom/')
            )
              return 'vendor';
            if (id.includes('/@icon-park/')) return 'vendor-icons';
            if (id.includes('/diff2html/')) return 'vendor-diff';
            return undefined;
          }
        }
      }
    },
    define: {
      'process.env.NODE_ENV': JSON.stringify(mode),
      'process.env.env': JSON.stringify(process.env.env),
      'process.env.AIONUI_MULTI_INSTANCE': JSON.stringify(process.env.AIONUI_MULTI_INSTANCE ?? ''),
      'process.env.SENTRY_DSN': JSON.stringify(process.env.SENTRY_DSN ?? ''),
      __APP_VERSION__: JSON.stringify(rootPackageJson.version ?? '0.0.0'),
      __IS_DISCONTINUED_BUILD__: JSON.stringify(false),
      global: 'globalThis'
    },
    optimizeDeps: {
      include: [
        'react',
        'react-dom',
        'react-router-dom',
        'react-i18next',
        'i18next',
        '@arco-design/web-react',
        '@icon-park/react',
        'react-markdown',
        'react-syntax-highlighter',
        'react-virtuoso',
        'classnames',
        'swr',
        'eventemitter3',
        'katex',
        'diff2html',
        'remark-gfm',
        'remark-math',
        'remark-breaks',
        'rehype-raw',
        'rehype-katex',
        'wavedrom',
        '@uiw/react-codemirror',
        '@codemirror/lang-markdown',
        '@codemirror/language'
      ]
    }
  };
});
