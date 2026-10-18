import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const { location, emptyComponent } = vi.hoisted(() => ({
	location: { url: new URL('http://localhost/requests?tab=upgrades') },
	emptyComponent: () => {
		const C = function () {};
		C.prototype = {};
		return { default: C };
	}
}));

vi.mock('$app/state', () => ({ page: location }));
vi.mock('$app/navigation', () => ({ replaceState: vi.fn() }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true, user: { id: 'user-1' } },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));
vi.mock('$lib/queries/requests/RequestQueries.svelte', () => ({
	getActiveRequestCountQuery: () => ({ data: { count: 0 } })
}));
vi.mock('$lib/queries/downloads/UpgradeQueries.svelte', () => ({
	getCutoffUnmetQuery: () => ({ data: undefined, isPending: false, isError: false }),
	requestUpgradeAlbum: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('./ActiveTab.svelte', emptyComponent);
vi.mock('./HistoryTab.svelte', emptyComponent);
vi.mock('./WantedTab.svelte', emptyComponent);
vi.mock('./ApprovalsTab.svelte', emptyComponent);
vi.mock('./AutoDownloadTab.svelte', emptyComponent);

import RequestsPage from './+page.svelte';

describe('requests route page', () => {
	it('opens the tab named in the URL', async () => {
		await render(RequestsPage);

		await expect
			.element(page.getByRole('tab', { name: /Upgrades/ }))
			.toHaveAttribute('aria-selected', 'true');
		await expect.element(page.getByText('Upgrades are off')).toBeInTheDocument();
	});
});
