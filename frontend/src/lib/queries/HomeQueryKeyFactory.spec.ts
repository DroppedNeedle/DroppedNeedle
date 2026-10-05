import { describe, expect, it } from 'vitest';
import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';

describe('HomeQueryKeyFactory (AMU-5)', () => {
	it('home key includes the userId and MusicBrainz source identity dimensions', () => {
		expect(HomeQueryKeyFactory.home('user-a')).toEqual([
			'home',
			'user-a',
			{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 }
		]);
	});

	it('produces different keys for different users (no cross-user collision)', () => {
		const a = HomeQueryKeyFactory.home('user-a');
		const b = HomeQueryKeyFactory.home('user-b');
		expect(a).not.toEqual(b);
	});

	it('scopes integration status by user', () => {
		expect(HomeQueryKeyFactory.integrationStatus('user-a')).toEqual([
			'home',
			'user-a',
			'integration-status'
		]);
		expect(HomeQueryKeyFactory.integrationStatus('user-a')).not.toEqual(
			HomeQueryKeyFactory.integrationStatus('user-b')
		);
	});
});
