import { describe, expect, it } from 'vitest';
import { NavidromeFolderQueryKeyFactory } from './NavidromeFolderQueryKeyFactory';

describe('NavidromeFolderQueryKeyFactory', () => {
	it('keys preferences by user and catalogs by user plus scope', () => {
		expect(NavidromeFolderQueryKeyFactory.preferences('alice')).toEqual([
			'navidrome',
			'folder-preferences',
			'alice'
		]);
		expect(NavidromeFolderQueryKeyFactory.catalog('alice', 'selected-a')).toEqual([
			'navidrome',
			'catalog',
			'alice',
			'selected-a'
		]);
		expect(NavidromeFolderQueryKeyFactory.catalog('bob', 'selected-a')).not.toEqual(
			NavidromeFolderQueryKeyFactory.catalog('alice', 'selected-a')
		);
	});
});
