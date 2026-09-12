import { page } from '@vitest/browser/context';
import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import ArtistReleasePagination from './ArtistReleasePagination.svelte';

const loadMore = vi.fn();

beforeEach(() => vi.clearAllMocks());

it('loads another release page only after an explicit accessible action', async () => {
	const view = await render(ArtistReleasePagination, {
		loadedCount: 50,
		totalCount: 240,
		loading: false,
		onloadmore: loadMore
	});

	await expect.element(page.getByText('50 of 240 releases loaded')).toBeVisible();
	const button = page.getByRole('button', { name: 'Load more releases' });
	await expect.element(button).toBeEnabled();
	expect(loadMore).not.toHaveBeenCalled();

	await button.click();
	expect(loadMore).toHaveBeenCalledTimes(1);

	await view.rerender({
		loadedCount: 50,
		totalCount: 240,
		loading: true,
		onloadmore: loadMore
	});
	await expect.element(button).toBeDisabled();
	await expect.element(page.getByText('Loading releases…')).toBeVisible();
});
