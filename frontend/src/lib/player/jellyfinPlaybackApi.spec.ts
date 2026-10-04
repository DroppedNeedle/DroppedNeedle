import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockPost = vi.fn();

vi.mock('$lib/api/client', () => {
	class _ApiError extends Error {
		status: number;
		code: string;
		details: unknown;
		constructor(status: number, code: string, message: string, details?: unknown) {
			super(message);
			this.status = status;
			this.code = code;
			this.details = details;
		}
	}
	return {
		api: {
			global: {
				post: (...args: unknown[]) => mockPost(...args),
				v3: { POST: (...args: unknown[]) => mockPost(...args) }
			}
		},
		ApiError: _ApiError
	};
});

import * as api from './jellyfinPlaybackApi';

describe('jellyfinPlaybackApi', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	describe('startSession', () => {
		it('starts a gateway session and returns its session key', async () => {
			mockPost.mockResolvedValueOnce({ accepted: true, session: 'u:web:item-456' });

			const result = await api.startSession('item-456');

			expect(result).toBe('u:web:item-456');
			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/start', {
				source: 'jellyfin',
				track_id: 'item-456'
			});
		});

		it('ignores a carried session id: v3 sessions are server-keyed', async () => {
			mockPost.mockResolvedValueOnce({ accepted: true, session: 'u:web:item-456' });

			await api.startSession('item-456', 'sess-existing');

			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/start', {
				source: 'jellyfin',
				track_id: 'item-456'
			});
		});

		it('throws on non-ok response', async () => {
			const { ApiError } = await import('$lib/api/client');
			mockPost.mockRejectedValueOnce(new ApiError(403, 'forbidden', 'Not allowed'));

			await expect(api.startSession('item-789')).rejects.toThrow(
				'Failed to start Jellyfin playback session'
			);
		});
	});

	describe('reportProgress', () => {
		it('sends POST with correct body and returns true on success', async () => {
			mockPost.mockResolvedValueOnce(undefined);

			const ok = await api.reportProgress('item-1', 'sess-1', 42.5, false);

			expect(ok).toBe(true);
			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/progress', {
				source: 'jellyfin',
				track_id: 'item-1',
				position_ms: 42500,
				is_paused: false
			});
		});

		it('returns false on network errors without throwing', async () => {
			mockPost.mockRejectedValueOnce(new Error('Network down'));

			await expect(api.reportProgress('item-1', 'sess-1', 10, false)).resolves.toBe(false);
		});

		it('returns false on non-ok responses without throwing', async () => {
			const { ApiError } = await import('$lib/api/client');
			mockPost.mockRejectedValueOnce(new ApiError(500, 'server_error', 'Internal error'));

			await expect(api.reportProgress('item-1', 'sess-1', 10, false)).resolves.toBe(false);
		});
	});

	describe('reportStop', () => {
		it('sends POST with correct body and returns true on success', async () => {
			mockPost.mockResolvedValueOnce(undefined);

			const ok = await api.reportStop('item-1', 'sess-1', 120.0);

			expect(ok).toBe(true);
			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/stop', {
				source: 'jellyfin',
				track_id: 'item-1',
				position_ms: 120000
			});
		});

		it('returns false on errors', async () => {
			mockPost.mockRejectedValueOnce(new Error('Network down'));

			await expect(api.reportStop('item-1', 'sess-1', 60)).resolves.toBe(false);
		});

		it('returns false on non-ok responses without throwing', async () => {
			const { ApiError } = await import('$lib/api/client');
			mockPost.mockRejectedValueOnce(new ApiError(502, 'bad_gateway', 'Bad gateway'));

			await expect(api.reportStop('item-1', 'sess-1', 60)).resolves.toBe(false);
		});
	});
});
