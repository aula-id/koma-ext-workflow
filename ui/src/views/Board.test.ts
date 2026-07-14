import { describe, it, expect } from 'vitest';
import { guardCardMove } from './Board';

describe('guardCardMove', () => {
  it('allows backlog -> todo (groom)', () => {
    expect(guardCardMove('backlog', 'todo', false)).toEqual({ legal: true });
  });

  it('rejects backlog -> onprogress (no direct edge)', () => {
    const result = guardCardMove('backlog', 'onprogress', false);
    expect(result.legal).toBe(false);
    expect(result.reason).toBeTruthy();
  });

  it('rejects todo -> anything (dispatch is kernel-driven, not a manual drag)', () => {
    expect(guardCardMove('todo', 'onprogress', false).legal).toBe(false);
    expect(guardCardMove('todo', 'backlog', false).legal).toBe(false);
  });

  it('allows onprogress -> review (manual send-to-review)', () => {
    expect(guardCardMove('onprogress', 'review', false)).toEqual({ legal: true });
  });

  it('requires killWorker when dragging onprogress -> todo', () => {
    const withoutKill = guardCardMove('onprogress', 'todo', false);
    expect(withoutKill.legal).toBe(false);
    expect(withoutKill.requiresKillWorker).toBe(true);

    const withKill = guardCardMove('onprogress', 'todo', true);
    expect(withKill).toEqual({ legal: true });
  });

  it('allows review -> done and review -> todo', () => {
    expect(guardCardMove('review', 'done', false)).toEqual({ legal: true });
    expect(guardCardMove('review', 'todo', false)).toEqual({ legal: true });
  });

  it('allows parked -> todo (unpark)', () => {
    expect(guardCardMove('parked', 'todo', false)).toEqual({ legal: true });
  });

  it('rejects any move out of done (terminal state)', () => {
    expect(guardCardMove('done', 'todo', false).legal).toBe(false);
    expect(guardCardMove('done', 'backlog', false).legal).toBe(false);
  });

  it('rejects a no-op move to the same column when not a legal edge', () => {
    expect(guardCardMove('done', 'done', false).legal).toBe(false);
  });
});
