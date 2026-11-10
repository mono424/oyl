import type { Env } from './env';
import { getOwnedMaterial } from './service';
import { BadRequest, Surreal } from './surreal';

/** Records lecture exposure. Reaching a section is never evidence of mastery. */
export async function setLectureProgress(
  env: Env,
  ownerId: string,
  materialId: string,
  input: { through?: unknown; rev?: unknown },
  expectedThrough?: number,
): Promise<{ through: number }> {
  const { through, rev } = input ?? {};
  if (typeof through !== 'number' || !Number.isInteger(through) || typeof rev !== 'number' || !Number.isInteger(rev) || rev < 0) {
    throw new BadRequest('Progress requires an integer section and material revision.');
  }
  const material = await getOwnedMaterial(env, ownerId, materialId);
  if (material.status !== 'ready') throw new BadRequest('Wait until the lecture has finished processing before setting progress.');
  if (material.index_rev !== rev) throw new BadRequest('The lecture changed. Reload it and set your progress again.');
  if (through < -1 || through >= material.chunk_count) throw new BadRequest('Choose a section in this lecture, or Not started.');

  const rows = await Surreal.system(env).query<{ id: string }[]>(
    `UPDATE type::record($id) SET learned_through = $through
     WHERE owner = type::record($owner) AND index_rev = $rev AND status = 'ready'
       ${expectedThrough === undefined ? '' : 'AND (learned_through ?? -1) = $expected'}
     RETURN id`,
    { id: material.id, owner: ownerId, through, rev, expected: expectedThrough },
  );
  if (!rows?.length) throw new BadRequest('The lecture changed. Reload it and set your progress again.');
  return { through };
}
