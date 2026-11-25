import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import ServiceHealthIndicator from './ServiceHealthIndicator.svelte';

type Problems = RouteData<'GET /api/v3/admin/problems'>;
type Health = RouteData<'GET /api/v3/system/health'>;

function signIn(role: 'admin' | 'user') {
	authStore.setUser(
		toAuthUser({
			id: `${role}-1`,
			display_name: role,
			role,
			email: null,
			avatar_url: null,
			username: role,
			username_display: role
		})
	);
}

const broken: Problems = {
	checked_at: 1_000,
	problems: [
		{
			kind: 'download_client_down',
			severity: 'broken',
			subject: 'slskd',
			count: 1,
			since_at: null,
			accept_folder: null,
			stale: false
		}
	]
};

const degraded: Health = {
	degraded: [
		{
			service: 'musicbrainz',
			capability: 'metadata',
			severity: 'degraded',
			message: 'MusicBrainz is slow.',
			fallback: null,
			degraded_seconds: 5
		}
	]
};

describe('the header mark', () => {
	afterEach(() => authStore.clear());

	it('shows an admin what needs them and links to the Health tab', async () => {
		signIn('admin');
		fakeApi({
			'GET /api/v3/admin/problems': broken,
			'GET /api/v3/system/health': { degraded: [] }
		});
		await renderWithQuery(ServiceHealthIndicator, {});

		const mark = page.getByRole('button', { name: '1 need you' });
		await expect.element(mark).toBeInTheDocument();
		await mark.click();
		await expect.element(page.getByText("slskd isn't answering.")).toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'See all problems' }))
			.toHaveAttribute('href', expect.stringContaining('/settings?tab=health'));
	});

	it('shows a plain user only the outside services', async () => {
		signIn('user');
		const api = fakeApi({ 'GET /api/v3/system/health': degraded });
		await renderWithQuery(ServiceHealthIndicator, {});

		await expect
			.element(page.getByRole('button', { name: 'Service status: 1 degraded' }))
			.toBeInTheDocument();
		await expect.element(page.getByText('need you')).not.toBeInTheDocument();
		expect(api.calls('GET /api/v3/admin/problems')).toHaveLength(0);
	});
});
