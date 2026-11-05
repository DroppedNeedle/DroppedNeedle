import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	page: { url: new URL('http://localhost/library/albums') },
	goto: vi.fn()
}));
vi.mock('$app/state', () => ({ page: h.page }));
vi.mock('$app/navigation', () => ({ goto: h.goto }));

import RememberedFiltersTestHarness from './RememberedFiltersTestHarness.svelte';

const KEY = 'library.filters.albums';

describe('rememberFilters', () => {
	beforeEach(() => {
		localStorage.clear();
		h.goto.mockReset();
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
});
