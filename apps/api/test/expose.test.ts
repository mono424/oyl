import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const root = new URL('../../../', import.meta.url);
const read = (path: string) => readFileSync(new URL(path, root), 'utf8');

/** The public routes in sp00ky.yml. Any other path falls through to the static app. */
function exposed(): string[] {
  const yml = read('sp00ky.yml');
  const block = yml.slice(yml.indexOf('expose:'));
  const items: string[] = [];
  for (const line of block.split('\n').slice(1)) {
    const item = /^\s*-\s+(\/\S*)/.exec(line);
    if (item) items.push(item[1]);
    else if (line.trim() && !line.trim().startsWith('#')) break;
  }
  return items;
}

const matches = (pattern: string, path: string) =>
  pattern.endsWith('*') ? path.startsWith(pattern.slice(0, -1)) : path === pattern;

test('every route the web app and AI clients call is exposed by the deploy', () => {
  const index = read('apps/api/src/index.ts');
  const routes = new Set<string>(['/mcp', '/oauth/register', '/.well-known/oauth-authorization-server']);
  for (const m of index.matchAll(/app\.(get|post|put|patch|delete|all|route)\(\s*'(\/[^']*)'/g)) {
    // Job paths and the health check are private on purpose.
    if (!m[2].startsWith('/api/') && m[2] !== '/mcp') continue;
    // app.route mounts a whole router under its path.
    routes.add(m[2].replace(/:[^/]+/g, 'x') + (m[1] === 'route' ? '/x' : ''));
  }
  assert.ok(routes.has('/api/keys'));
  assert.ok(routes.has('/api/documents/x/images'));
  const patterns = exposed();
  for (const route of routes) {
    assert.ok(patterns.some((p) => matches(p, route)), `${route} is not under expose in sp00ky.yml, so it never reaches the API`);
  }
});
