import { describe, expect, it } from 'vitest';

import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';

describe('RemoteQueryKeyFactory', () => {
	it('nests every key under the shared remotes prefix', () => {
		const prefix = [...RemoteQueryKeyFactory.all];
		for (const key of [
			RemoteQueryKeyFactory.hub('userA', 'plex'),
			RemoteQueryKeyFactory.stats('userA', 'jellyfin'),
			RemoteQueryKeyFactory.random('userA', 'navidrome', {}),
			RemoteQueryKeyFactory.discovery('userA', 'plex', 10),
			RemoteQueryKeyFactory.folders('userA')
		]) {
			expect([...key].slice(0, prefix.length)).toEqual(prefix);
		}
	});

	it('carries userId and source segments on every key', () => {
		expect(RemoteQueryKeyFactory.hub('userA', 'plex')).toContain('userA');
		expect(RemoteQueryKeyFactory.hub('userA', 'plex')).toContain('plex');
		expect(RemoteQueryKeyFactory.stats('userA', 'jellyfin')).toContain('jellyfin');
		expect(RemoteQueryKeyFactory.stats(undefined, 'jellyfin')).toContain(null);
	});

	it('isolates sources and users from each other', () => {
		expect(RemoteQueryKeyFactory.hub('userA', 'plex')).not.toEqual(
			RemoteQueryKeyFactory.hub('userA', 'jellyfin')
		);
		expect(RemoteQueryKeyFactory.hub('userA', 'plex')).not.toEqual(
			RemoteQueryKeyFactory.hub('userB', 'plex')
		);
	});

	it('keys browse pages separately by params', () => {
		expect(RemoteQueryKeyFactory.random('userA', 'navidrome', { genre: 'Jazz' })).not.toEqual(
			RemoteQueryKeyFactory.random('userA', 'navidrome', { genre: 'Rock' })
		);
	});

	it('lets one source-prefix sweep clear that adapter without touching others', () => {
		const plexPrefix = [...RemoteQueryKeyFactory.source('userA', 'plex')];
		const hub = [...RemoteQueryKeyFactory.hub('userA', 'plex')];
		const jelly = [...RemoteQueryKeyFactory.hub('userA', 'jellyfin')];
		expect(hub.slice(0, plexPrefix.length)).toEqual(plexPrefix);
		expect(jelly.slice(0, plexPrefix.length)).not.toEqual(plexPrefix);
	});

	it('nests the navidrome folders key under the navidrome source prefix', () => {
		const navidromePrefix = [...RemoteQueryKeyFactory.source('userA', 'navidrome')];
		const folders = [...RemoteQueryKeyFactory.folders('userA')];
		expect(folders.slice(0, navidromePrefix.length)).toEqual(navidromePrefix);
	});
});
