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
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, isTrusted: true, user: { id: 'user-1' } },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));
vi.mock('$lib/features/requests/queries/RequestQueries.svelte', () => ({
	getActiveRequestCountQuery: () => ({ data: { count: 0 } }),
	getActiveRequestsQuery: () => ({ data: { items: [], count: 0 }, isFetching: false })
}));
vi.mock('$lib/features/downloads/queries/UpgradeQueries.svelte', () => ({
	getCutoffUnmetQuery: () => ({ data: undefined, isPending: false, isError: false }),
	createRequestUpgradeAlbumMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/features/requests/components/ActiveTab.svelte', emptyComponent);
vi.mock('$lib/features/requests/components/HistoryTab.svelte', emptyComponent);
vi.mock('$lib/features/requests/components/WantedTab.svelte', emptyComponent);
vi.mock('$lib/features/requests/components/ApprovalsTab.svelte', emptyComponent);
vi.mock('$lib/features/requests/components/AutoDownloadTab.svelte', emptyComponent);

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
