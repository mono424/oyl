import { cpSync, createReadStream, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, extname, join, normalize, resolve } from 'node:path';
import { defineConfig, type Plugin } from 'vite';
import solid from 'vite-plugin-solid';
import tailwindcss from '@tailwindcss/vite';
import wasm from 'vite-plugin-wasm';

/**
 * TikZJax's TeX (src/lib/tikz.ts) under /tikzjax/, straight from the package:
 * served in dev, copied into the build. Only what the worker loads.
 */
function tikzjax(): Plugin {
  const dist = join(dirname(createRequire(import.meta.url).resolve('@drgrice1/tikzjax/package.json')), 'dist');
  const files = ['run-tex.js', 'tex.wasm.gz', 'core.dump.gz', 'fonts.css', 'fonts', 'tex_files'];
  const types: Record<string, string> = { '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2' };
  let outDir = 'dist';
  return {
    name: 'tikzjax',
    configResolved(config) {
      outDir = resolve(config.root, config.build.outDir);
    },
    configureServer(server) {
      server.middlewares.use('/tikzjax', (req, res) => {
        const path = normalize(decodeURIComponent((req.url ?? '').split('?')[0])).replace(/^[/\\]+/, '');
        const file = join(dist, path);
        let found = false;
        try {
          found = files.includes(path.split(/[/\\]/)[0]) && statSync(file).isFile();
        } catch {
          // Not found.
        }
        if (!found) {
          res.statusCode = 404;
          return res.end();
        }
        // The .gz files are read as they are: no Content-Encoding.
        res.setHeader('Content-Type', types[extname(file)] ?? 'application/octet-stream');
        createReadStream(file).pipe(res);
      });
    },
    writeBundle() {
      for (const f of files) cpSync(join(dist, f), join(outDir, 'tikzjax', f), { recursive: true });
    },
  };
}

export default defineConfig({
  // wasm() is required by @spooky-sync/core, which bundles the loro-crdt,
  // SQLite and SurrealDB WASM modules. Their top-level await needs no plugin:
  // the build targets esnext.
  plugins: [solid(), tailwindcss(), wasm(), tikzjax()],
  server: {
    port: 5180,
    strictPort: true,
  },
  build: {
    target: 'esnext',
  },
  optimizeDeps: {
    // Served raw: esbuild pre-bundling breaks each module's own `.wasm` /
    // worker URL lookup (`new URL(..., import.meta.url)`), and the local
    // engine dies behind the loading screen.
    exclude: [
      '@surrealdb/wasm',
      '@surrealdb/wasm-native',
      '@sqlite.org/sqlite-wasm',
      'surrealdb',
      '@spooky-sync/client-solid',
      '@spooky-sync/core',
    ],
    // pino is reached through @spooky-sync/core and is CJS: pre-bundle just it
    // so its default export interop works when core is served raw.
    include: ['@spooky-sync/core > pino'],
    esbuildOptions: {
      target: 'esnext',
    },
  },
  esbuild: {
    supported: {
      'top-level-await': true,
    },
  },
});
