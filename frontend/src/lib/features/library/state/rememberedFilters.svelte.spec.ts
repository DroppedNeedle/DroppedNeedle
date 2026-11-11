import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	page: { url: new URL('http://localhost/library/albums') },
	goto: vi.fn(),
	auth: { user: { id: 'user-1' } as { id: string } | null },
	resets: [] as (() => void)[]
}));
vi.mock('$app/state', () => ({ page: h.page }));
vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: h.auth }));
vi.mock('$lib/utils/userSessionCleanup', () => ({
	registerUserSessionReset: (reset: () => void) => {
		h.resets.push(reset);
		return () => {};
	}
}));

import RememberedFiltersTestHarness from './RememberedFiltersTestHarness.svelte';
import { keepFilter, keptFilter } from './rememberedFilters.svelte';

const KEY = 'library.filters.user-1.albums';

describe('rememberFilters', () => {
	beforeEach(() => {
		localStorage.clear();
		h.goto.mockReset();
		h.auth.user = { id: 'user-1' };
	});

	it('keeps the filters the address names', async () => {
		h.page.url = new URL('http://localhost/library/albums?rating=4&formats=flac&q=x');
		render(RememberedFiltersTestHarness);

		await vi.waitFor(() =>
			expect(JSON.parse(localStorage.getItem(KEY) ?? '{}')).toEqual({
				rating: '4',
				formats: 'flac'
			})
		);
		expect(h.goto).not.toHaveBeenCalled();
	});

	it('brings the kept filters back to a visit that names none', async () => {
		localStorage.setItem(KEY, JSON.stringify({ rating: '3' }));
		h.page.url = new URL('http://localhost/library/albums?q=night');
		render(RememberedFiltersTestHarness);

		await vi.waitFor(() => expect(h.goto).toHaveBeenCalledOnce());
		const [url] = h.goto.mock.calls[0];
		expect(String(url)).toBe('http://localhost/library/albums?q=night&rating=3');
	});

	it('lets a link that names a filter win over the kept ones', async () => {
		localStorage.setItem(KEY, JSON.stringify({ rating: '3', formats: 'mp3' }));
		h.page.url = new URL('http://localhost/library/albums?formats=flac');
		render(RememberedFiltersTestHarness);

		await vi.waitFor(() =>
			expect(JSON.parse(localStorage.getItem(KEY) ?? '{}')).toEqual({ formats: 'flac' })
		);
		expect(h.goto).not.toHaveBeenCalled();
	});

	it("keeps one person's filters from the next, and forgets them on sign-out", async () => {
		localStorage.setItem(KEY, JSON.stringify({ rating: '3' }));
		localStorage.setItem('other', 'kept');
		h.auth.user = { id: 'user-2' };
		h.page.url = new URL('http://localhost/library/albums');
		render(RememberedFiltersTestHarness);

		await vi.waitFor(() => expect(h.resets).toHaveLength(1));
		expect(h.goto).not.toHaveBeenCalled();
		for (const reset of h.resets) reset();
		expect(localStorage.getItem(KEY)).toBeNull();
		expect(localStorage.getItem('other')).toBe('kept');
	});
});

describe('keptFilter', () => {
	const SORT_KEY = 'library.filters.user-1.album-sort';

	beforeEach(() => {
		localStorage.clear();
		h.auth.user = { id: 'user-1' };
	});

	it('keeps one value where the next visit reads it before rendering', () => {
		keepFilter('album-sort', 'sort', 'artist');

		expect(localStorage.getItem(SORT_KEY)).toBe('{"sort":"artist"}');
		expect(keptFilter('album-sort', 'sort')).toBe('artist');
	});

	it('forgets the value set to null', () => {
		keepFilter('album-sort', 'sort', 'artist');
		keepFilter('album-sort', 'sort', null);

		expect(keptFilter('album-sort', 'sort')).toBeNull();
		expect(localStorage.getItem(SORT_KEY)).toBeNull();
	});

	it('keeps nothing without a signed-in user', () => {
		h.auth.user = null;
		keepFilter('album-sort', 'sort', 'artist');

		expect(keptFilter('album-sort', 'sort')).toBeNull();
	});
});
