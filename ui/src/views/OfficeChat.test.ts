import { describe, it, expect } from 'vitest';
import { isFolded, noticeStatus } from './OfficeChat';

describe('isFolded', () => {
  it('is false when officeSummary is absent or blank', () => {
    expect(isFolded({})).toBe(false);
    expect(isFolded({ officeSummary: '' })).toBe(false);
    expect(isFolded({ officeSummary: '   ' })).toBe(false);
  });

  it('is true once officeSummary carries content (transcript folded at least once)', () => {
    expect(isFolded({ officeSummary: 'decisions: use postgres' })).toBe(true);
  });
});

describe('noticeStatus', () => {
  it('paused takes priority over sent', () => {
    expect(noticeStatus({ sent: true, paused: true })).toBe('paused');
  });

  it('sent when not paused and sent', () => {
    expect(noticeStatus({ sent: true, paused: false })).toBe('sent');
  });

  it('queued when neither sent nor paused', () => {
    expect(noticeStatus({ sent: false, paused: false })).toBe('queued');
  });
});
