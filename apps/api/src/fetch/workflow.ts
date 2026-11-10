import { WorkflowEntrypoint, type WorkflowEvent, type WorkflowStep } from '@spooky-sync/backend';
import type { Env } from '../env';
import { UserError } from '../surreal';
import { CanvasAuthError } from './canvas';
import { finishRun, importItem, planRun, type RunOutcome, type RunPlan, runError } from './run';

export interface FetchParams {
  fetcherId: string;
  /** The `fetch_run` row this run reports to. */
  runId: string;
}

const RETRY = { retries: { limit: 2, delay: '20 seconds', backoff: 'exponential' }, timeout: '10 minutes' } as const;
const ITEM_RETRY = { retries: { limit: 2, delay: '30 seconds', backoff: 'exponential' }, timeout: '10 minutes' } as const;

type Failed = { error: string; disable: boolean };

/**
 * One check of a source, durably: read and map it (one step), import each
 * new or changed item (a step each, so a failed download retries alone), then
 * record the outcome on the run and the source. Errors the student can act
 * on (a refused token, a page that is gone) are not retried; they end the run
 * or the item with their message. Nothing returned from a step holds a token.
 */
export class FetchSource extends WorkflowEntrypoint<Env, FetchParams> {
  async run(event: WorkflowEvent<FetchParams>, step: WorkflowStep) {
    const { fetcherId, runId } = event.payload;
    const finish = (o: RunOutcome) => step.do('finish', RETRY, () => finishRun(this.env, fetcherId, runId, o));

    let plan: RunPlan | Failed | null;
    try {
      plan = await step.do('read', RETRY, async (): Promise<RunPlan | Failed | null> => {
        try {
          return await planRun(this.env, fetcherId);
        } catch (err) {
          if (err instanceof UserError) return { error: err.message, disable: err instanceof CanvasAuthError };
          throw err;
        }
      });
    } catch (err) {
      console.error('[fetch] read', fetcherId, err);
      plan = { error: runError(err), disable: false };
    }
    if (!plan) return { gone: true };
    if ('error' in plan) {
      await finish({ status: 'failed', found: 0, created: 0, updated: 0, failed: 0, deferred: 0, error: plan.error, detail: [plan.error], disable: plan.disable });
      return { failed: plan.error };
    }

    let created = 0;
    let updated = 0;
    const problems: string[] = [];
    for (const [i, item] of plan.items.entries()) {
      try {
        const r = await step.do(`item ${i}: ${item.external_id.slice(0, 80)}`, ITEM_RETRY, async (): Promise<{ done: string } | { error: string }> => {
          try {
            return { done: await importItem(this.env, fetcherId, item) };
          } catch (err) {
            if (err instanceof UserError) return { error: err.message };
            throw err;
          }
        });
        if ('error' in r) problems.push(`${item.title}: ${r.error}`);
        else if (r.done === 'created') created++;
        else if (r.done === 'updated') updated++;
      } catch (err) {
        console.error('[fetch] item', fetcherId, item.external_id, err);
        problems.push(`${item.title}: could not be imported. It is tried again at the next check.`);
      }
    }

    const failed = problems.length;
    const status = failed && !created && !updated && plan.items.length ? 'failed' : failed ? 'partial' : 'ok';
    await finish({
      status, found: plan.found, created, updated, failed, deferred: plan.deferred,
      detail: [...problems, ...plan.issues],
      ...(failed ? { error: failed === 1 ? problems[0] : `${failed} items could not be imported.` } : {}),
    });
    return { created, updated, failed, deferred: plan.deferred };
  }
}
