import type { Surreal } from './surreal';

const RECONCILE_DELAYS_MS = [0, 250, 500, 1000];

export async function createMaterial(
  user: Surreal,
  sql: string,
  vars: Record<string, unknown> & { id: string },
): Promise<unknown[]> {
  try {
    return await user.query<unknown[]>(sql, vars);
  } catch (error) {
    if (!(error instanceof Error) || !/_00_material_mutation\b[\s\S]*remote HTTP request:\s*50[234]\b/i.test(error.message)) throw error;
    // The sync hook can fail after CREATE commits. Allow a short visibility
    // window, reading only the exact id with the caller's permissions.
    const outcomes: string[] = [];
    for (const delay of RECONCILE_DELAYS_MS) {
      if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
      try {
        const rows = await user.query<unknown[]>(
          'SELECT id FROM type::record($id) WHERE owner = $auth.id',
          { id: vars.id },
        );
        if (rows?.length) return rows;
        outcomes.push('not_visible');
      } catch {
        outcomes.push('read_failed');
      }
    }
    console.warn('[mcp] material create reconciliation exhausted', { materialId: vars.id, outcomes });
    throw error;
  }
}
