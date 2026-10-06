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

	it('uses a null segment when no userId is present', () => {
		expect(LidarrImportQueryKeyFactory.candidates(undefined)).toEqual([
			'lidarr-import',
			'candidates',
			null
		]);
	});

	it('keeps the config key user-agnostic', () => {
		expect(LidarrImportQueryKeyFactory.config()).toEqual(['lidarr-import', 'config']);
	});
});
