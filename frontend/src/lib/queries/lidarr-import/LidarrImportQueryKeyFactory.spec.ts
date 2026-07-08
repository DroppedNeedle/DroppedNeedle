import { describe, expect, it } from 'vitest';

import { LidarrImportQueryKeyFactory } from './LidarrImportQueryKeyFactory';

describe('LidarrImportQueryKeyFactory', () => {
	it('scopes the candidates key by userId (cross-user cache-leak guard)', () => {
		expect(LidarrImportQueryKeyFactory.candidates('user-a')).toEqual([
			'lidarr-import',
			'candidates',
			'user-a'
		]);
		expect(LidarrImportQueryKeyFactory.candidates('user-b')).not.toEqual(
			LidarrImportQueryKeyFactory.candidates('user-a')
		);
	});

	it('falls back to anon when no userId is present', () => {
		expect(LidarrImportQueryKeyFactory.candidates(undefined)).toEqual([
			'lidarr-import',
			'candidates',
			'anon'
		]);
	});

	it('keeps config + status keys user-agnostic', () => {
		expect(LidarrImportQueryKeyFactory.config()).toEqual(['lidarr-import', 'config']);
		expect(LidarrImportQueryKeyFactory.status()).toEqual(['lidarr-import', 'status']);
	});
});
