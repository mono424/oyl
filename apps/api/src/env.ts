import type { Workflow } from '@spooky-sync/backend';
import type { FetchParams } from './fetch/workflow';
import type { IngestParams } from './ingest';

export interface Env {
  AI: Ai;
  INGEST: Workflow<IngestParams>;
  /** One run of a source (fetch/workflow.ts). */
  FETCH: Workflow<FetchParams>;

  APP_ORIGIN: string;
  /** Public API origin used for OAuth issuer and MCP audience, when proxied. */
  API_ORIGIN?: string;
  /**
   * SurrealDB, set by sp00ky on a Worker backend (and by `spky dev`): the
   * https base URL (no /rpc), namespace and database. On the shared free
   * database also the project's namespace owner, with SPKY_DB_LEVEL=namespace.
   */
  SPKY_DB_URL?: string;
  SPKY_DB_NS?: string;
  SPKY_DB_NAME?: string;
  SPKY_DB_USER?: string;
  SPKY_DB_PASS?: string;
  /** `namespace` or `database`: where SPKY_DB_USER is defined. Unset: root. */
  SPKY_DB_LEVEL?: string;
  /**
   * Fallback for any SPKY_DB_* that is unset (see `dbConfig`): an older
   * .dev.vars, a Worker deployed with wrangler, or the login of a project's
   * own database, kept in the vault.
   */
  SURREAL_URL?: string;
  SURREAL_NS?: string;
  SURREAL_DB?: string;
  SURREAL_USER?: string;
  SURREAL_PASS?: string;
  SURREAL_AUTH_NS?: string;
  SURREAL_AUTH_DB?: string;

  EMBEDDING_MODEL: string;

  /**
   * SurrealDB Agent Memory Context API base, including /api/v1/{context_id}:
   * the course knowledge graph. Unset: no graph is built.
   */
  AGENT_MEMORY_URL?: string;
  AGENT_MEMORY_API_KEY?: string;

  /**
   * Seals the Canvas access tokens of sources (fetch/secret.ts). Any long
   * random string; changing it makes every saved token unreadable.
   */
  FETCHER_KEY?: string;

  /** sp00ky's job runner presents this (sp00ky.yml `apps.api.auth.token`) on /jobs/*. */
  JOB_TOKEN?: string;
}

/** Identity of the signed-in user behind a request. */
export interface Caller {
  userId: string;
  token: string;
}

export type AppEnv = { Bindings: Env; Variables: { caller: Caller } };
