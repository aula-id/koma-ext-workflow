import { describe, it, expect, beforeEach, vi } from 'vitest';
import { Bridge, Snapshot } from './bridge';

describe('Bridge', () => {
  let bridge: Bridge;
  let mockKomaPanel: any;

  beforeEach(() => {
    mockKomaPanel = {
      send: vi.fn(),
      onPush: vi.fn((handler: Function) => {
        mockKomaPanel._pushHandler = handler;
      }),
      _pushHandler: null,
    };
    window.KomaPanel = mockKomaPanel;
    bridge = new Bridge();
  });

  describe('hello()', () => {
    it('should send hello with correct envelope', async () => {
      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [],
      };
      mockKomaPanel.send.mockResolvedValue({ snapshot });

      const result = await bridge.hello('0.1.0');

      const calls = mockKomaPanel.send.mock.calls;
      expect(calls.length).toBe(1);
      expect(calls[0][0]).toEqual({
        op: 'hello',
        uiVersion: '0.1.0',
      });
      expect(result).toEqual(snapshot);
    });

    it('should throw on invalid hello response', async () => {
      mockKomaPanel.send.mockResolvedValue({ invalid: 'response' });

      await expect(bridge.hello('0.1.0')).rejects.toThrow('Invalid hello response');
    });
  });

  describe('onSnapshot()', () => {
    it('should notify listeners on snapshot push', async () => {
      const listener = vi.fn();
      bridge.onSnapshot(listener);

      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [{ id: 'test-1', name: 'Test Project' }],
      };

      // Simulate push from daemon
      mockKomaPanel._pushHandler(snapshot);

      expect(listener).toHaveBeenCalledWith(snapshot);
    });

    it('should ignore pushes with lower seq numbers', async () => {
      const snapshot1: Snapshot = {
        kind: 'snapshot',
        seq: 2,
        projects: [],
      };
      const snapshot2: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [],
      };

      const listener = vi.fn();
      bridge.onSnapshot(listener);

      // First push with higher seq
      mockKomaPanel._pushHandler(snapshot1);
      expect(listener).toHaveBeenCalledTimes(1);

      // Second push with lower seq should be ignored
      mockKomaPanel._pushHandler(snapshot2);
      expect(listener).toHaveBeenCalledTimes(1);
    });

    it('should update seq on valid push', async () => {
      const listener = vi.fn();
      bridge.onSnapshot(listener);

      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 5,
        projects: [],
      };

      mockKomaPanel._pushHandler(snapshot);

      // Next push with equal or higher seq should work
      const snapshot2: Snapshot = {
        kind: 'snapshot',
        seq: 6,
        projects: [],
      };
      mockKomaPanel._pushHandler(snapshot2);

      expect(listener).toHaveBeenCalledTimes(2);
    });

    it('should allow unsubscribe via returned function', async () => {
      const listener = vi.fn();
      const unsubscribe = bridge.onSnapshot(listener);

      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [],
      };

      mockKomaPanel._pushHandler(snapshot);
      expect(listener).toHaveBeenCalledTimes(1);

      unsubscribe();

      mockKomaPanel._pushHandler({ ...snapshot, seq: 2 });
      expect(listener).toHaveBeenCalledTimes(1);
    });
  });

  describe('state()', () => {
    it('should send state request with optional project', async () => {
      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [],
      };
      mockKomaPanel.send.mockResolvedValue({ snapshot });

      await bridge.state('test-project');

      const calls = mockKomaPanel.send.mock.calls;
      expect(calls.length).toBe(1);
      expect(calls[0][0]).toEqual({
        op: 'state',
        project: 'test-project',
      });
    });

    it('should send state request without project when not provided', async () => {
      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [],
      };
      mockKomaPanel.send.mockResolvedValue({ snapshot });

      await bridge.state();

      const calls = mockKomaPanel.send.mock.calls;
      expect(calls.length).toBe(1);
      expect(calls[0][0]).toEqual({
        op: 'state',
      });
    });
  });

  describe('envelope shape', () => {
    it('should handle snapshot envelope with all fields', async () => {
      const listener = vi.fn();
      bridge.onSnapshot(listener);

      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [
          {
            id: 'proj-1',
            name: 'Project 1',
            phase: 'Running',
            tasks: [],
          },
        ],
        truncated: true,
      };

      mockKomaPanel._pushHandler(snapshot);

      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          kind: 'snapshot',
          seq: 1,
          truncated: true,
        })
      );
    });

    it('should ignore non-snapshot pushes', async () => {
      const listener = vi.fn();
      bridge.onSnapshot(listener);

      // Push non-snapshot payload
      mockKomaPanel._pushHandler({
        kind: 'other',
        seq: 1,
        projects: [],
      });

      expect(listener).not.toHaveBeenCalled();
    });
  });

  describe('error handling', () => {
    it('should handle listener exceptions gracefully', async () => {
      const goodListener = vi.fn();
      const badListener = vi.fn(() => {
        throw new Error('Listener error');
      });

      bridge.onSnapshot(badListener);
      bridge.onSnapshot(goodListener);

      const snapshot: Snapshot = {
        kind: 'snapshot',
        seq: 1,
        projects: [],
      };

      expect(() => {
        mockKomaPanel._pushHandler(snapshot);
      }).not.toThrow();

      // Good listener should still be called despite bad listener
      expect(goodListener).toHaveBeenCalledWith(snapshot);
    });
  });
});
