import { describe, expect, it } from 'vitest';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';

describe('AuthQueryKeyFactory', () => {
	it('prefix is [auth]', () => {
		expect(AuthQueryKeyFactory.prefix).toEqual(['auth']);
	});

	it('importCandidates key includes provider and userId segments', () => {
		expect(AuthQueryKeyFactory.importCandidates('plex', 'admin-1')).toEqual([
			'auth',
			'import',
			'plex',
			'admin-1'
		]);
	});

	it('produces different keys for different users (no cross-user collision)', () => {
		const a = AuthQueryKeyFactory.importCandidates('plex', 'admin-1');
		const b = AuthQueryKeyFactory.importCandidates('plex', 'admin-2');
		expect(a).not.toEqual(b);
	});

	it('normalizes a missing userId to null', () => {
		expect(AuthQueryKeyFactory.importCandidates('jellyfin', undefined)).toEqual([
			'auth',
			'import',
			'jellyfin',
			null
		]);
	});
});
