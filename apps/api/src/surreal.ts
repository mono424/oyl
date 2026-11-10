import type { Env } from './env';

/**
 * Minimal SurrealDB client over HTTP JSON-RPC (`POST /rpc`). Two identities:
 *
 * - `system`: the database login from `dbConfig` (SPKY_DB_USER / SPKY_DB_PASS,
 *   or SURREAL_USER / SURREAL_PASS), for the server-only work (writing chunks,
 *   assets, calls, transcripts). Every query run this way filters by owner
 *   explicitly.
 * - `as(token)`: a user's own session token, so SurrealDB applies that user's
 *   table permissions.
 *
 * Values cross as JSON: record ids are `table:key` strings (turn them into
 * records with `type::record($id)` in the query) and bytes go in and out as
 * base64 (`encoding::base64::decode/encode`).
 */
export class Surreal {
  constructor(
    private readonly db: DbConfig,
    private readonly authorization: string,
  ) {}

  static system(env: Env) {
    const db = dbConfig(env);
    if (!db.user) throw new Error('SurrealDB login missing: set SPKY_DB_USER and SPKY_DB_PASS (or SURREAL_USER and SURREAL_PASS)');
    return new Surreal(db, `Basic ${btoa(`${db.user}:${db.pass ?? ''}`)}`);
  }

  static as(env: Env, token: string) {
    return new Surreal(dbConfig(env), `Bearer ${token}`);
  }

  /** Runs one or more statements and returns every statement's result. */
  async queryAll(sql: string, vars: Record<string, unknown> = {}): Promise<unknown[]> {
    const names = Object.keys(vars);
    let bindings = vars;
    let bindingStatements = 0;
    if (names.length) {
      for (const name of names) {
        if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) throw new Error(`Invalid SurrealDB parameter name: ${name}`);
      }
      let transport = '__surreal_transport';
      while (names.includes(transport) || names.includes(`${transport}_json`)) transport += '_';
      // JSON-RPC guesses record types from strings such as "Source: 01" and
      // can truncate their contents. Decode JSON inside the query to retain
      // literal strings; explicit type::record casts still create references.
      // The envelope starts with "{", so RPC leaves it a string. Sending JSON
      // directly avoids expanding already-base64 PDF/image data a second time.
      // Omit null object fields as before, so option<T> fields receive NONE.
      const json = JSON.stringify(vars, (_key, value) => value === null ? undefined : value);
      bindings = { [`${transport}_json`]: json };
      const prefix = [
        `LET $${transport} = encoding::json::decode($${transport}_json);`,
        ...names.map((name) => `LET $${name} = $${transport}[${JSON.stringify(name)}];`),
      ];
      bindingStatements = prefix.length;
      sql = `${prefix.join('\n')}\n${sql}`;
    }
    const res = await fetch(`${this.db.url}/rpc`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json',
        'surreal-ns': this.db.ns,
        'surreal-db': this.db.db,
        ...(this.authorization.startsWith('Basic ') && this.db.authNs ? { 'surreal-auth-ns': this.db.authNs } : {}),
        ...(this.authorization.startsWith('Basic ') && this.db.authDb ? { 'surreal-auth-db': this.db.authDb } : {}),
        authorization: this.authorization,
      },
      body: JSON.stringify({ id: 1, method: 'query', params: [sql, bindings] }),
    });
    if (res.status === 401 || res.status === 403) throw new SurrealAuthError(await res.text());
    const text = await res.text();
    let body: { result?: { status: string; result: unknown }[]; error?: { message?: string } };
    try {
      body = JSON.parse(text);
    } catch {
      throw new Error(`SurrealDB answered ${res.status}: ${text.slice(0, 300)}`);
    }
    if (body.error) {
      const msg = body.error.message ?? JSON.stringify(body.error);
      if (/authentication|token|expired/i.test(msg)) throw new SurrealAuthError(msg);
      throw new Error(`SurrealDB: ${msg}`);
    }
    const results = body.result ?? [];
    // In a failed transaction every other statement reports that it was not
    // executed; the cause is the one that says something else.
    const errors = results.filter((r) => r.status !== 'OK');
    const failed = errors.find((r) => !/not executed due to a failed transaction/i.test(String(r.result))) ?? errors[0];
    if (failed) throw new Error(`SurrealDB: ${String(failed.result)}`);
    return results.slice(bindingStatements).map((r) => r.result);
  }

  /** Runs the statements and returns the result of the last one. */
  async query<T = unknown>(sql: string, vars: Record<string, unknown> = {}): Promise<T> {
    const all = await this.queryAll(sql, vars);
    return all[all.length - 1] as T;
  }
}

export class SurrealAuthError extends Error {}

interface DbConfig {
  /** HTTP base URL, without /rpc. */
  url: string;
  ns: string;
  db: string;
  user?: string;
  pass?: string;
  /** Sign-in level of `user`: its namespace and database, none for root. */
  authNs?: string;
  authDb?: string;
}

/**
 * Where SurrealDB is and who the API signs in as. On a sp00ky Worker backend
 * the platform sets SPKY_DB_*: always the coordinates, and on the shared free
 * database also the project's namespace owner (SPKY_DB_LEVEL=namespace).
 * SURREAL_* fills in what is missing: an older .dev.vars, a Worker deployed
 * with wrangler, or the vault login of a project's own database. A login is
 * taken whole from one side, so a platform user never meets a vault password.
 */
export function dbConfig(env: Env): DbConfig {
  const url = env.SPKY_DB_URL || env.SURREAL_URL;
  const ns = env.SPKY_DB_NS || env.SURREAL_NS;
  const db = env.SPKY_DB_NAME || env.SURREAL_DB;
  if (!url || !ns || !db) throw new Error('SurrealDB not configured: set SPKY_DB_URL, SPKY_DB_NS and SPKY_DB_NAME (or SURREAL_URL, SURREAL_NS and SURREAL_DB)');
  const base = url.replace(/\/+$/, '').replace(/^ws(s?):\/\//, 'http$1://').replace(/\/rpc$/, '');
  if (env.SPKY_DB_USER) {
    const level = env.SPKY_DB_LEVEL;
    return {
      url: base, ns, db, user: env.SPKY_DB_USER, pass: env.SPKY_DB_PASS,
      authNs: level === 'namespace' || level === 'database' ? ns : undefined,
      authDb: level === 'database' ? db : undefined,
    };
  }
  return {
    url: base, ns, db, user: env.SURREAL_USER, pass: env.SURREAL_PASS,
    authNs: env.SURREAL_AUTH_NS || undefined, authDb: env.SURREAL_AUTH_DB || undefined,
  };
}

/**
 * Signs in through a record access method and returns the session token, e.g.
 * the `agent` access with an API key.
 */
export async function signinRecord(env: Env, access: string, params: Record<string, string>): Promise<string | null> {
  const { url, ns, db } = dbConfig(env);
  const res = await fetch(`${url}/signin`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({ ns, db, ac: access, ...params }),
  });
  if (!res.ok) return null;
  const body = (await res.json()) as { token?: string };
  return body.token ?? null;
}

/**
 * Runs a write again when it lost a race for a row, as a delete does against the
 * sync node while the app has the document open. Only for one statement or one
 * transaction: those fail whole, so nothing is done twice.
 */
export async function retryConflicts<T>(write: () => Promise<T>, attempts = 4): Promise<T> {
  for (let attempt = 1; ; attempt++) {
    try {
      return await write();
    } catch (error) {
      if (attempt >= attempts || !(error instanceof Error) || !/Transaction conflict:.*can be retried/i.test(error.message)) throw error;
      await new Promise((resolve) => setTimeout(resolve, 150 * attempt));
    }
  }
}

/** Record ids pass through JSON as strings; this rejects anything else. */
export function recordId(table: string, raw: unknown): string {
  const s = String(raw ?? '');
  const id = s.includes(':') ? s : `${table}:${s}`;
  if (!id.startsWith(`${table}:`) || !/^[a-z_]+:[A-Za-z0-9_]+$/.test(id)) {
    throw new BadRequest(`Not a ${table} id: ${s}`);
  }
  return id;
}

/** An error whose message is safe to show to the caller. */
export class UserError extends Error {
  constructor(
    message: string,
    readonly status: 400 | 403 | 404 | 409 | 503 = 400,
  ) {
    super(message);
  }
}
export class BadRequest extends UserError {}
export class NotFound extends UserError {
  constructor(message: string) {
    super(message, 404);
  }
}
export class Conflict extends UserError {
  constructor(message: string) {
    super(message, 409);
  }
}

export const toBase64 = (bytes: Uint8Array): string => {
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
};

export const fromBase64 = (b64: string): Uint8Array => {
  const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4);
  const bin = atob(padded);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
};
