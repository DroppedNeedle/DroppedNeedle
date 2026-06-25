import { describe, it, expect } from 'vitest';
import { mergeNowPlayingSessions } from './nowPlayingMerge';
import type { NowPlayingSession } from '$lib/types';

function session(overrides: Partial<NowPlayingSession>): NowPlayingSession {
	return {
		id: 'x',
		user_name: 'User',
		track_name: 'Track',
		artist_name: 'Artist',
		album_name: 'Album',
		cover_url: '',
		device_name: '',
		is_paused: false,
		...overrides
	};
}

describe('mergeNowPlayingSessions', () => {
	it("drops only the viewer's own web echo, keeping their connected apps", () => {
		const server = [
			session({ id: 'me:web', user_name: 'Me', source: 'local' }), // shown via local overlay
			session({ id: 'me:compat:manet', user_name: 'Me', source: 'manet' }), // my phone - keep
			session({ id: 'bob:web', user_name: 'Bob', source: 'local' })
		];
		const result = mergeNowPlayingSessions(null, server, 'me');
		expect(result.map((s) => s.id)).toEqual(['me:compat:manet', 'bob:web']);
	});

	it('prepends the local overlay and de-dupes the upstream echo of the local track', () => {
		const local = session({
			id: 'local-plex-1',
			source: 'plex',
			track_name: 'Song',
			user_name: 'Me',
			_isLocal: true
		});
		const server = [
			// the Plex poll echoes my own session (different username, same source+track)
			session({ id: 'plex:s1', source: 'plex', track_name: 'Song', user_name: 'MePlex' }),
			session({ id: 'plex:s2', source: 'plex', track_name: 'Other', user_name: 'Carol' })
		];
		const result = mergeNowPlayingSessions(local, server, 'me');
		expect(result[0]._isLocal).toBe(true);
		expect(result.map((s) => s.id)).toEqual(['local-plex-1', 'plex:s2']);
	});

	it('keeps other users, including redacted entries with identity + progress', () => {
		const server = [
			session({
				id: 'bob:web',
				user_name: 'Bob',
				track_name: '',
				artist_name: '',
				source: 'local',
				redacted: true,
				progress_ms: 1000,
				duration_ms: 2000
			})
		];
		const result = mergeNowPlayingSessions(null, server, 'me');
		expect(result).toHaveLength(1);
		expect(result[0].redacted).toBe(true);
		expect(result[0].user_name).toBe('Bob');
		expect(result[0].progress_ms).toBe(1000);
	});

	it('returns the feed unchanged with no local session and no viewer id', () => {
		const server = [session({ id: 'a:web' }), session({ id: 'b:web' })];
		const result = mergeNowPlayingSessions(null, server, undefined);
		expect(result).toHaveLength(2);
	});
});
