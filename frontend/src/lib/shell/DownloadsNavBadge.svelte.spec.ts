import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import DownloadsNavBadge from './DownloadsNavBadge.svelte';

function summary(active: number, held: number) {
	return {
		revision: 1,
		active_count: active,
		held_count: held,
		failed_count: 0,
		landed_release_group_mbids: []
	};
}

describe('DownloadsNavBadge', () => {
	beforeEach(() => {
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Uma',
				role: 'user',
				email: null,
				avatar_url: null,
				username: 'uma',
				username_display: 'Uma'
			})
		);
	});

	afterEach(() => authStore.clear());

	it('shows the held count once nothing is active', async () => {
		fakeApi({ 'GET /api/v3/downloads/activity-summary': summary(0, 3) });
		await renderWithQuery(DownloadsNavBadge, {});

		await expect.element(page.getByLabelText('3 held for you to check')).toHaveTextContent('3');
	});

	it('shows the active count while anything is active', async () => {
		fakeApi({ 'GET /api/v3/downloads/activity-summary': summary(2, 3) });
		await renderWithQuery(DownloadsNavBadge, {});

		await expect.element(page.getByLabelText('2 active downloads')).toHaveTextContent('2');
		await expect.element(page.getByLabelText('3 held for you to check')).not.toBeInTheDocument();
	});
});
