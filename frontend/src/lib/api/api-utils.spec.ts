import { beforeEach, describe, expect, it, vi } from 'vitest';

// Read through getters so each case can vary the SvelteKit base and the
// public env without re-importing the modules under test.
const mocks = vi.hoisted(() => ({
	base: '',
	publicEnv: {} as Record<string, string>
}));

vi.mock('$app/paths', () => ({
	get base() {
		return mocks.base;
	}
}));

vi.mock('$env/dynamic/public', () => ({
	env: mocks.publicEnv
}));

import { getApiUrl } from './api-utils';
import { withBasePath, withoutBasePath } from '$lib/utils/basePath';

function setPublicApiUrl(value?: string): void {
	delete mocks.publicEnv.PUBLIC_API_URL;
	if (value !== undefined) mocks.publicEnv.PUBLIC_API_URL = value;
}

beforeEach(() => {
	mocks.base = '';
	setPublicApiUrl();
});

const passthroughs = [
	'https://example.com/a?b=c',
	'data:image/png;base64,AAA',
	'blob:http://localhost/abc',
	'//cdn.example.com/x',
	'#section',
	'assets/song.mp3',
	''
];

describe('withBasePath', () => {
	it.each([
		['', '/api/v3/me', '/api/v3/me'],
		['/dn', '/api/v3/me', '/dn/api/v3/me'],
		['/dn', '/api/v3/stream?id=7#t=30', '/dn/api/v3/stream?id=7#t=30'],
		['/dn', '/dn/api/v3/me', '/dn/api/v3/me'], // already prefixed
		['/dn', '/dn/dn/api/v3/me', '/dn/api/v3/me'], // duplicated base collapses
		['/dn', '/dnextra', '/dn/dnextra'], // lookalike segment is not the base
		['/dn', '/', '/dn/'],
		['/music/app', '/api/v3/me', '/music/app/api/v3/me'],
		['/music/app', '/music/x', '/music/app/music/x'],
		['/music/app', '/music/application/x', '/music/app/music/application/x']
	])('base %j maps %j to %j', (base, input, expected) => {
		mocks.base = base;
		expect(withBasePath(input)).toBe(expected);
	});

	it.each(passthroughs)('leaves %j alone under a base', (input) => {
		mocks.base = '/dn';
		expect(withBasePath(input)).toBe(input);
	});
});

describe('withoutBasePath', () => {
	it.each([
		['', '/login', '/login'],
		['/dn', '/dn/login?next=%2Fartists#top', '/login?next=%2Fartists#top'],
		['/dn', '/dn', '/'],
		['/dn', '/dn/dn/login', '/dn/login'], // strips one occurrence only
		['/dn', '/dnextra/a', '/dnextra/a'],
		['/dn', '/other/deploy/login', '/other/deploy/login'],
		['/music/app', '/music/app/playlists', '/playlists'],
		['/music/app', '/music/application/x', '/music/application/x']
	])('base %j maps %j to %j', (base, input, expected) => {
		mocks.base = base;
		expect(withoutBasePath(input)).toBe(expected);
	});

	it('inverts withBasePath', () => {
		mocks.base = '/dn';
		for (const path of ['/', '/artists/42', '/settings?tab=ui', '/dnly/reports']) {
			expect(withoutBasePath(withBasePath(path))).toBe(path);
		}
	});
});

describe('getApiUrl', () => {
	it.each([
		[undefined, '', '/api/v3/me', '/api/v3/me'],
		[undefined, '/dn', '/api/v3/me', '/dn/api/v3/me'],
		['http://localhost:8688', '', '/api/v3/me', 'http://localhost:8688/api/v3/me'],
		['http://localhost:8688///', '', '/api/v3/me', 'http://localhost:8688/api/v3/me'],
		['http://localhost:8688/', '/dn', '/api/v3/me', 'http://localhost:8688/dn/api/v3/me'],
		['http://localhost:8688', '/dn', '//cdn.example/x.jpg', '//cdn.example/x.jpg'],
		['http://localhost:8688', '', 'https://cdn.example/img.png', 'https://cdn.example/img.png']
	])('origin %j and base %j map %j to %j', (origin, base, input, expected) => {
		setPublicApiUrl(origin);
		mocks.base = base;
		expect(getApiUrl(input)).toBe(expected);
	});

	it('is stable when applied twice', () => {
		mocks.base = '/dn';
		const relative = getApiUrl('/api/v3/me');
		expect(getApiUrl(relative)).toBe(relative);

		setPublicApiUrl('http://localhost:8688');
		const absolute = getApiUrl('/api/v3/me');
		expect(getApiUrl(absolute)).toBe(absolute);
	});
});
