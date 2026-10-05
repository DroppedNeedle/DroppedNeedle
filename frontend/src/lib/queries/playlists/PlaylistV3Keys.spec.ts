import { describe, expect, it } from 'vitest';
import { FavoriteQueryKeyFactory, PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';

describe('PlaylistQueryKeyFactory.v3', () => {
	it('roots user-scoped v3 keys under the playlists prefix', () => {
		expect(PlaylistQueryKeyFactory.v3.root('user-a')).toEqual(['playlists', 'v3', 'user-a']);
	});

	it('differs per user (no cross-user collision)', () => {
		expect(PlaylistQueryKeyFactory.v3.list('user-a')).not.toEqual(
			PlaylistQueryKeyFactory.v3.list('user-b')
		);
	});

	it('scopes detail keys by playlist id', () => {
		expect(PlaylistQueryKeyFactory.v3.detail('user-a', 'pl-1')).toEqual([
			'playlists',
			'v3',
			'user-a',
			'detail',
			'pl-1'
		]);
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
