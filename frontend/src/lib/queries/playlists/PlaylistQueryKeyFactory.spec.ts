import { describe, expect, it } from 'vitest';
import { FavoriteQueryKeyFactory, PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';

describe('PlaylistQueryKeyFactory', () => {
	it('nests the list and every detail under the user root', () => {
		const root = PlaylistQueryKeyFactory.root('user-a');
		expect(PlaylistQueryKeyFactory.list('user-a').slice(0, root.length)).toEqual(root);
		expect(PlaylistQueryKeyFactory.detail('user-a', 'pl-1').slice(0, root.length)).toEqual(root);
	});

	it('differs per user (no cross-user collision)', () => {
		expect(PlaylistQueryKeyFactory.root('user-a')).not.toEqual(
			PlaylistQueryKeyFactory.root('user-b')
		);
		expect(PlaylistQueryKeyFactory.root(undefined)).toEqual(['playlists', null]);
	});
});

describe('FavoriteQueryKeyFactory', () => {
	it('scopes the list by user and kind', () => {
		expect(FavoriteQueryKeyFactory.list('user-a', 'album')).toEqual([
			'favorites',
			'user-a',
			'list',
			'album'
		]);
	});

	it('differs per user (no cross-user collision)', () => {
		expect(FavoriteQueryKeyFactory.list('user-a', null)).not.toEqual(
			FavoriteQueryKeyFactory.list('user-b', null)
		);
	});
});
