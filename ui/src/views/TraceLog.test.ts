import { describe, it, expect } from 'vitest';
import { formatTraceTime } from './TraceLog';

describe('formatTraceTime', () => {
  it('formats epoch millis as zero-padded HH:MM:SS', () => {
    expect(formatTraceTime(0)).toMatch(/^\d{2}:\d{2}:\d{2}$/);
    expect(formatTraceTime(Date.now())).toMatch(/^\d{2}:\d{2}:\d{2}$/);
  });

  it('zero-pads single-digit hours/minutes/seconds (timezone-independent)', () => {
    // Built from LOCAL components so the assertion holds in any timezone (formatTraceTime reads
    // the same local components back).
    const ts = new Date(2026, 0, 2, 3, 4, 5).getTime();
    expect(formatTraceTime(ts)).toBe('03:04:05');
  });
});
