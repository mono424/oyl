import {
  column,
  type Connector,
  ConnectorError,
  compileVersion,
  execute,
  type MappingVersionInput,
  parseVersion,
  type Plan,
  readSnapshot,
  type Row,
  type RowSet,
  type Table,
  type TableRef,
  TargetSchema,
} from '@hockeymask/core';
import { BadRequest } from '../surreal';

/**
 * What a source has to offer and what TalkToBolo imports from it, as a
 * hockeymask mapping. Connectors (canvas.ts, web.ts) only read: they list
 * every file, page or link they find as a `SourceRow`. The mapping decides
 * which of them become lecture material and in what shape (an `ImportRow`),
 * and the runner (run.ts) writes those. Every fetcher has a template mapping
 * built from its settings; a custom one (`fetcher.mapping`) replaces it.
 */

/** One thing a source lists. The same shape for every connector. */
export interface SourceRow {
  [field: string]: string | number | null;
  /** Stable across runs: `file:123`, `page:45`, or a link's URL. */
  id: string;
  name: string;
  /** `file` (Canvas), `page` (a Canvas page, or the web page itself), `link` (on a web page). */
  kind: 'file' | 'page' | 'link';
  content_type: string | null;
  /** Lowercase file extension of the URL's path, without the dot. */
  ext: string | null;
  /** Where the runner downloads it from. */
  url: string;
  /** Where a person opens it. */
  link: string | null;
  /** Changes when the item does (Canvas `updated_at`). Null if the source does not say. */
  updated_at: string | null;
  /** The Canvas module or the page heading it is listed under. */
  module: string | null;
  size: number | null;
}

/** One lecture material to import, the target of every mapping. */
export interface ImportRow {
  external_id: string;
  title: string;
  kind: 'pdf' | 'markdown' | 'tex';
  url: string;
  link: string | null;
  version: string | null;
}

const field = (name: string, logicalType: 'string', nullable: boolean, description: string) =>
  ({ name, logicalType, nullable, description });

export const TARGETS = TargetSchema.parse({
  name: 'talktobolo',
  entities: [
    {
      name: 'import',
      description: 'A lecture material imported from a source.',
      identity: ['external_id'],
      fields: [
        field('external_id', 'string', false, 'Stable id of the item in its source.'),
        field('title', 'string', false, 'Title of the new material.'),
        field('kind', 'string', false, '`pdf`, `markdown` (a page) or `tex` (LaTeX source).'),
        field('url', 'string', false, 'Where to download it.'),
        field('link', 'string', true, 'Where a person opens it.'),
        field('version', 'string', true, 'Changes when the item changes; null if unknown.'),
      ],
    },
  ],
});

const text = { type: 'string' };
const maybe = (type: 'string' | 'number') => ({ type: [type, 'null'] });
export const SOURCE_ROW_SCHEMA = {
  type: 'object',
  properties: {
    id: text, name: text, kind: text, content_type: maybe('string'), ext: maybe('string'), url: text,
    link: maybe('string'), updated_at: maybe('string'), module: maybe('string'), size: maybe('number'),
  },
  required: ['id', 'name', 'kind', 'content_type', 'ext', 'url', 'link', 'updated_at', 'module', 'size'],
};

export interface TemplateOptions {
  system: 'canvas' | 'web';
  /** Comma-separated words; an item is taken when its name or URL contains one. Blank: everything. */
  include?: string | null;
  /** Import pages (Canvas pages, the web page itself) as Markdown too, not only PDFs. */
  importPages?: boolean;
  /** Import LaTeX sources (.tex) too. */
  importTex?: boolean;
}

const escapeRegex = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

export const includeWords = (include: string | null | undefined) =>
  (include ?? '').split(',').map((w) => w.trim().toLowerCase()).filter(Boolean);

/** The mapping a fetcher uses unless it has its own: PDFs (and pages), filtered by name. */
export function templateMapping(opts: TemplateOptions): MappingVersionInput {
  const pdf = { any: { of: [{ path: 'content_type', eq: 'application/pdf' }, { path: 'ext', eq: 'pdf' }] } };
  const tex = { path: 'ext', eq: 'tex' };
  const kinds = { any: { of: [pdf, ...(opts.importPages ? [{ path: 'kind', eq: 'page' }] : []), ...(opts.importTex ? [tex] : [])] } };
  const words = includeWords(opts.include);
  const conds = [kinds, ...(words.length ? [{ path: 'match_text', matches: words.map(escapeRegex).join('|') }] : [])];
  return {
    id: `${opts.system}-lectures`,
    name: opts.system === 'canvas' ? 'bCourses lecture files' : 'Web page lecture files',
    expectedSchema: { items: SOURCE_ROW_SCHEMA },
    nodes: [
      { id: 'items', op: 'source', system: opts.system, entity: 'items' },
      {
        id: 'prepared',
        op: 'map',
        in: 'items',
        keep: 'all',
        fields: [{ key: 'match_text', pipeline: [{ op: 'extract', path: ['name', 'url'] }, { op: 'concat', separator: ' ' }, { op: 'lower' }] }],
      },
      { id: 'wanted', op: 'filter', in: 'prepared', cond: { all: { of: conds } } },
      {
        id: 'imports',
        op: 'map',
        in: 'wanted',
        fields: [
          { key: 'import.external_id', pipeline: [{ op: 'extract', path: 'id' }] },
          {
            key: 'import.title',
            pipeline: [
              { op: 'extract', path: 'name' },
              { op: 'replace', pattern: '\\.([pP][dD][fF]|[tT][eE][xX])$', with: '', regex: true },
              { op: 'trim' },
              { op: 'blank_to_null' },
              { op: 'default', value: 'Untitled' },
            ],
          },
          {
            key: 'import.kind',
            pipeline: [{ op: 'when', cases: [{ cond: { path: 'kind', eq: 'page' }, then: 'markdown' }, { cond: tex, then: 'tex' }], else: 'pdf' }],
          },
          { key: 'import.url', pipeline: [{ op: 'extract', path: 'url' }] },
          { key: 'import.link', pipeline: [{ op: 'extract', path: 'link' }] },
          { key: 'import.version', pipeline: [{ op: 'extract', path: 'updated_at' }] },
        ],
      },
      { id: 'out', op: 'sink', in: 'imports', target: 'import', key: ['import.external_id'] },
    ],
  };
}

/**
 * Compiles a mapping against the import target. A custom mapping reads the
 * connector's rows through `source` nodes of entity `items`; their schema is
 * always the connector's, whatever the document says.
 */
export function compileMapping(input: unknown): Plan {
  let version;
  try {
    const doc = typeof input === 'string' ? JSON.parse(input) : input;
    if (!doc || typeof doc !== 'object') throw new Error('not an object');
    const nodes = Array.isArray((doc as { nodes?: unknown }).nodes) ? (doc as { nodes: { id?: unknown; op?: unknown; entity?: unknown }[] }).nodes : [];
    const schemas: Record<string, unknown> = { ...((doc as { expectedSchema?: object }).expectedSchema ?? {}) };
    for (const n of nodes) if (n.op === 'source' && n.entity === 'items' && typeof n.id === 'string') schemas[n.id] = SOURCE_ROW_SCHEMA;
    version = parseVersion({ ...doc, expectedSchema: schemas });
  } catch (err) {
    throw new BadRequest(`The mapping is not a valid hockeymask document: ${err instanceof Error ? err.message : String(err)}`);
  }
  const compiled = compileVersion(version, { targets: TARGETS });
  if (!compiled.ok) {
    const issues = compiled.issues.slice(0, 5).map((i) => ('node' in i && i.node ? `${i.node}: ${i.message}` : i.message));
    throw new BadRequest(`The mapping does not compile: ${issues.join('; ')}`);
  }
  if (!compiled.plan.nodes.some((n) => n.op === 'sink' && n.params.target === 'import')) {
    throw new BadRequest('The mapping must end in a sink to `import`.');
  }
  return compiled.plan;
}

/** Runs a compiled mapping over a connector's rows. Issues are worth showing on the run. */
export async function runMapping(plan: Plan, rows: SourceRow[], now = new Date()): Promise<{ imports: ImportRow[]; issues: string[] }> {
  const snapshot = await readSnapshot(plan, async (s) => (s.entity === 'items' ? rows : []));
  const result = execute(plan, snapshot, { now: now.toISOString() });
  const issues = result.issues.slice(0, 20).map((i) => `${i.node}: ${i.message}`);
  const sink = result.sinks.find((s) => s.write?.target === 'import');
  if (!sink || sink.status !== 'write' || !sink.write) {
    throw new BadRequest(`The mapping produced nothing to import (${result.status}${issues.length ? `: ${issues[0]}` : ''}).`);
  }
  const imports = sink.write.rows.map((r) => {
    const get = (k: string) => {
      const v = r[`import.${k}`] ?? r[k];
      return v === null || v === undefined ? null : String(v);
    };
    const kind = get('kind') === 'markdown' ? 'markdown' : get('kind') === 'tex' ? 'tex' : 'pdf';
    return {
      external_id: get('external_id') ?? '',
      title: (get('title') ?? 'Untitled').slice(0, 200),
      kind,
      url: get('url') ?? '',
      link: get('link'),
      version: get('version'),
    } satisfies ImportRow;
  });
  return { imports: imports.filter((i) => i.external_id && i.url), issues };
}

const COLUMNS = [
  column({ name: 'id', logicalType: 'string', sourceType: 'string', nullable: false }),
  column({ name: 'name', logicalType: 'string', sourceType: 'string', nullable: false }),
  column({ name: 'kind', logicalType: 'string', sourceType: 'string', nullable: false }),
  column({ name: 'content_type', logicalType: 'string', sourceType: 'string' }),
  column({ name: 'ext', logicalType: 'string', sourceType: 'string' }),
  column({ name: 'url', logicalType: 'string', sourceType: 'url', nullable: false }),
  column({ name: 'link', logicalType: 'string', sourceType: 'url' }),
  column({ name: 'updated_at', logicalType: 'timestamp', sourceType: 'iso8601' }),
  column({ name: 'module', logicalType: 'string', sourceType: 'string' }),
  column({ name: 'size', logicalType: 'integer', sourceType: 'bytes' }),
];

/** A connector with one table, `items` (SourceRow), read in one go. */
export function rowsConnector(system: string, read: () => Promise<SourceRow[]>): Connector {
  const table: Table = { name: 'items', label: 'Lecture items', description: null, rowCount: null, columns: COLUMNS };
  return {
    system,
    dialect: 'none',
    async listTables(): Promise<TableRef[]> {
      return [table];
    },
    async describe(name: string): Promise<Table> {
      if (name !== 'items') throw new ConnectorError(`${system} has no table '${name}'.`);
      return table;
    },
    async sample(name: string, limit = 20): Promise<RowSet> {
      await this.describe(name);
      const rows = await read();
      return { columns: COLUMNS, rows: rows.slice(0, limit) as Row[], totalRows: rows.length };
    },
    async *read(name: string): AsyncIterable<Row[]> {
      await this.describe(name);
      yield (await read()) as Row[];
    },
    async query(): Promise<RowSet> {
      throw new ConnectorError(`${system} cannot run queries.`);
    },
  };
}
