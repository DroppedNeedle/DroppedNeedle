import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({ id: 'idx1' });
const testMutate = vi.fn().mockResolvedValue({
	valid: true,
	version: '0.1',
	message: 'DS OK - text search (no audio-search)',
	supports_audio_search: false,
	category_count: 8
});

let indexersData: unknown[] = [];

vi.mock('$lib/queries/downloads/IndexerQueries.svelte', () => ({
	getIndexersQuery: () => ({ data: indexersData, isLoading: false, isError: false }),
	saveIndexerMutation: () => ({ mutateAsync: saveMutate, mutate: saveMutate, isPending: false }),
	deleteIndexerMutation: () => ({ mutateAsync: vi.fn().mockResolvedValue({}), isPending: false }),
	reorderIndexersMutation: () => ({ mutate: vi.fn(), isPending: false }),
	testIndexerMutation: () => ({ mutateAsync: testMutate, isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import SettingsIndexers from './SettingsIndexers.svelte';

describe('SettingsIndexers.svelte', () => {
	it('shows the empty state teaching bring-your-own when no indexers exist', async () => {
		indexersData = [];
		render(SettingsIndexers);
		await expect.element(page.getByText('No indexers yet')).toBeInTheDocument();
		// "bring your own" only appears in the empty state (the header says "add your own").
		await expect.element(page.getByText(/bring your own/)).toBeInTheDocument();
	});

	it('opens an add form and saves a new indexer', async () => {
		indexersData = [];
		render(SettingsIndexers);
		await page.getByRole('button', { name: 'Add indexer' }).click();
		await page.getByPlaceholder('https://indexer.example/api').fill('https://idx.test/api');
		await page.getByRole('button', { name: 'Save' }).click();
		expect(saveMutate).toHaveBeenCalledWith(
			expect.objectContaining({ url: 'https://idx.test/api', type: 'newznab' })
		);
	});

	it('lists a configured indexer with a masked key and a Test that reports audio-search', async () => {
		indexersData = [
			{
				id: 'idx1',
				type: 'newznab',
				name: 'DrunkenSlug',
				url: 'https://drunkenslug.com/api',
				api_key: 'indexer****',
				categories: [3000, 3010, 3040],
				enabled: true,
				priority: 1
			}
		];
		render(SettingsIndexers);
		await expect.element(page.getByText('DrunkenSlug', { exact: true })).toBeInTheDocument();
		// Expand and run Test - the result line reports the text-search fallback.
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('button', { name: 'Test' }).click();
		// The result line reports the caps verdict (text-search fallback for DrunkenSlug).
		await expect.element(page.getByText(/DS OK - text search/)).toBeInTheDocument();
	});

	it('offers a "did you mean /api" fix when the site URL is pasted and applies it', async () => {
		indexersData = [];
		// First Test (site URL) returns the suggestion; the retry after applying it passes.
		testMutate.mockResolvedValueOnce({
			valid: false,
			message: "That's the site's homepage, not the API endpoint.",
			suggested_url: 'https://idx.test/api',
			supports_audio_search: false,
			category_count: 0
		});
		render(SettingsIndexers);
		await page.getByRole('button', { name: 'Add indexer' }).click();
		await page.getByPlaceholder('https://indexer.example/api').fill('https://idx.test');
		await page.getByRole('button', { name: 'Test' }).click();
		// The suggestion button appears; clicking it re-tests against the /api URL.
		await page.getByRole('button', { name: 'Use https://idx.test/api' }).click();
		expect(testMutate).toHaveBeenLastCalledWith(
			expect.objectContaining({ url: 'https://idx.test/api' })
		);
	});
});
