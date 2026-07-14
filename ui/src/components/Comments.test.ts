import { describe, it, expect } from 'vitest';
import { receiptPill } from './Comments';

describe('receiptPill', () => {
  it('pending on a not-yet-done task carries no flag', () => {
    const p = receiptPill({ state: 'pending' }, false);
    expect(p.label).toBe('pending');
    expect(p.flag).toBeUndefined();
  });

  it('pending on a done task is flagged never-delivered (5.3 honesty rule)', () => {
    const p = receiptPill({ state: 'pending' }, true);
    expect(p.label).toBe('pending');
    expect(p.flag).toMatch(/never delivered/i);
  });

  it('delivered carries its timestamp and no flag, done or not', () => {
    const running = receiptPill({ state: 'delivered', atMs: 1000 }, false);
    expect(running.label).toContain('delivered');
    expect(running.flag).toBeUndefined();

    const done = receiptPill({ state: 'delivered', atMs: 1000 }, true);
    expect(done.flag).toBeUndefined();
  });

  it('read carries its timestamp and no flag', () => {
    const r = receiptPill({ state: 'read', atMs: 2000 }, true);
    expect(r.label).toContain('read');
    expect(r.flag).toBeUndefined();
  });

  it('delivered/read without an atMs still render a bare label', () => {
    expect(receiptPill({ state: 'delivered' }, false).label).toBe('delivered');
    expect(receiptPill({ state: 'read' }, false).label).toBe('read');
  });
});
