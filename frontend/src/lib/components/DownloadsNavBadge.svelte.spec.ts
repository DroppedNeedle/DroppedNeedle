import { page } from '@vitest/browser/context';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$lib/queries/downloads/DownloadQueries.svelte', () => ({
	getDownloadActivitySummaryQuery: () => ({
		data: {
			revision: 4,
			active_count: 3,
			held_count: 0,
			failed_count: 0,
			landed_release_group_mbids: []
		}
	})
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

vi.mock('$lib/stores/library', () => ({
	libraryStore: { addMbid: vi.fn() }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import DownloadsNavBadge from './DownloadsNavBadge.svelte';

it('renders the active count with an accessible label', async () => {
	await render(DownloadsNavBadge);

	await expect.element(page.getByLabelText('3 active downloads')).toHaveTextContent('3');
});
