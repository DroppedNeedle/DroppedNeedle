import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockPost = vi.fn();
const mockGet = vi.fn();

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
				get: (...args: unknown[]) => mockGet(...args),
				v3: {
					POST: (...args: unknown[]) => mockPost(...args),
					GET: (...args: unknown[]) => mockGet(...args)
				}
			}
		},
		ApiError: _ApiError
	};
});



import {
	reportPlexScrobble,
	reportPlexNowPlaying,
	reportPlexStopped,
	isPlexScrobbleEnabled,
	resetPlexScrobblePreference
} from './plexPlaybackApi';

describe('plexPlaybackApi', () => {
	beforeEach(() => {
		vi.resetAllMocks();
		resetPlexScrobblePreference();
	});

	describe('reportPlexScrobble', () => {
		it('calls scrobble endpoint when enabled', async () => {
			expect.assertions(2);
			mockGet.mockResolvedValueOnce({ scrobble_to_plex: true });
			mockPost.mockResolvedValueOnce(undefined);

			await reportPlexScrobble('12345');

			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/stop', { source: 'plex', track_id: '12345' });
			expect(mockGet).toHaveBeenCalledWith('/api/v3/settings/plex');
		});

		it('does not call scrobble when disabled', async () => {
			expect.assertions(1);
			mockGet.mockResolvedValueOnce({ scrobble_to_plex: false });

			await reportPlexScrobble('12345');

			expect(mockPost).not.toHaveBeenCalled();
		});

		it('warns on error without throwing', async () => {
			expect.assertions(1);
			const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
			mockGet.mockResolvedValueOnce({ scrobble_to_plex: true });
			mockPost.mockRejectedValueOnce(new Error('Network down'));

			await reportPlexScrobble('12345');

			expect(warnSpy).toHaveBeenCalled();
			warnSpy.mockRestore();
		});

		it('defaults to disabled when settings fetch fails', async () => {
			expect.assertions(1);
			mockGet.mockRejectedValueOnce(new Error('fetch failed'));

			await reportPlexScrobble('12345');

			expect(mockPost).not.toHaveBeenCalled();
		});
	});

	describe('reportPlexNowPlaying', () => {
		it('calls now-playing endpoint unconditionally', async () => {
			expect.assertions(1);
			mockPost.mockResolvedValueOnce(undefined);

			await reportPlexNowPlaying('67890');

			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/start', { source: 'plex', track_id: '67890' });
		});

		it('warns on error without throwing', async () => {
			expect.assertions(1);
			const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
			mockPost.mockRejectedValueOnce(new Error('timeout'));

			await reportPlexNowPlaying('67890');

			expect(warnSpy).toHaveBeenCalled();
			warnSpy.mockRestore();
		});
	});

	describe('isPlexScrobbleEnabled', () => {
		it('defaults to false before any load', () => {
			expect.assertions(1);
			expect(isPlexScrobbleEnabled()).toBe(false);
		});

		it('reflects loaded preference', async () => {
			expect.assertions(1);
			mockGet.mockResolvedValueOnce({ scrobble_to_plex: false });
			mockPost.mockResolvedValueOnce(undefined);

			await reportPlexScrobble('test');

			expect(isPlexScrobbleEnabled()).toBe(false);
		});
	});

	describe('resetPlexScrobblePreference', () => {
		it('resets cache so next call re-fetches', async () => {
			expect.assertions(2);
			mockGet.mockResolvedValueOnce({ scrobble_to_plex: false });
			await reportPlexScrobble('test');

			resetPlexScrobblePreference();

			mockGet.mockResolvedValueOnce({ scrobble_to_plex: true });
			mockPost.mockResolvedValueOnce(undefined);
			await reportPlexScrobble('test2');

			expect(mockGet).toHaveBeenCalledTimes(2);
			expect(mockPost).toHaveBeenCalledTimes(1);
		});
	});

	describe('preference caching', () => {
		it('fetches only once across multiple scrobble calls', async () => {
			expect.assertions(1);
			mockGet.mockResolvedValueOnce({ scrobble_to_plex: true });
			mockPost.mockResolvedValue(undefined);

			await reportPlexScrobble('a');
			await reportPlexScrobble('b');

			expect(mockGet).toHaveBeenCalledTimes(1);
		});
	});

	describe('reportPlexStopped', () => {
		it('calls stopped endpoint', async () => {
			expect.assertions(1);
			mockPost.mockResolvedValueOnce(undefined);

			await reportPlexStopped('99999');

			expect(mockPost).toHaveBeenCalledWith('/api/v3/playback/stop', { source: 'plex', track_id: '99999' });
		});

		it('warns on error without throwing', async () => {
			expect.assertions(1);
			const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
			mockPost.mockRejectedValueOnce(new Error('fail'));

			await reportPlexStopped('99999');

			expect(warnSpy).toHaveBeenCalled();
			warnSpy.mockRestore();
		});
	});
});
