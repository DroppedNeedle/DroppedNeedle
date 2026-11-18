import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { fakeApi, type FakeRoutes } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import SharePlaylistModal from './SharePlaylistModal.svelte';

const MEMBERS = 'GET /api/v3/playlists/{playlist_id}/members';
const SET_MEMBERS = 'PUT /api/v3/playlists/{playlist_id}/members';
const VISIBILITY = 'PATCH /api/v3/playlists/{playlist_id}/visibility';

const summary = (isPublic: boolean) => ({
	id: 'p1',
	name: 'Road Trip',
	track_count: 0,
	total_duration: null,
	cover_urls: [],
	custom_cover_url: null,
	source_ref: null,
	created_at: 1767225600,
	updated_at: 1767225600,
	is_public: isPublic,
	is_owner: true,
	is_shared: false,
	can_delete: true,
	owner_name: 'Ada',
	is_redacted: false
});

/** Ada's playlist, shared with `members`, among Ada, Bob and Cara. */
function serve(members: { user_id: string; display_name: string }[], routes: FakeRoutes = {}) {
	return fakeApi({
		'GET /api/v3/users/directory': {
			users: [
				{ id: 'u-ada', display_name: 'Ada' },
				{ id: 'u-bob', display_name: 'Bob' },
				{ id: 'u-cara', display_name: 'Cara' }
			]
		},
		[MEMBERS]: { members },
		[SET_MEMBERS]: ({ body }) => ({
			members: (body as { user_ids: string[] }).user_ids.map((user_id) => ({
				user_id,
				display_name: user_id
			}))
		}),
		[VISIBILITY]: ({ body }) => summary((body as { is_public: boolean }).is_public),
		...routes
	});
}

async function open(isPublic: boolean) {
	const saved: boolean[] = [];
	const view = await renderWithQuery(SharePlaylistModal, {
		playlistId: 'p1',
		isPublic,
		onsaved: (nowPublic: boolean) => saved.push(nowPublic)
	});
	(view.component as unknown as { showModal: () => void }).showModal();
	await expect.element(page.getByRole('dialog')).toBeVisible();
	return saved;
}

describe('SharePlaylistModal.svelte', () => {
	beforeEach(() => {
		authStore.setUser({
			id: 'u-ada',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'ada',
			providers: []
		});
	});
	afterEach(() => authStore.clear());

	it('shares a private playlist with the people added by name, never the owner', async () => {
		const api = serve([]);
		const saved = await open(false);
		await page.getByText('Specific people').click();
		await page.getByLabelText('Add people').fill('a');
		await expect.element(page.getByRole('button', { name: 'Add Cara' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Add Ada' })).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Add Cara' }).click();
		await expect.element(page.getByRole('button', { name: 'Remove Cara' })).toBeVisible();
		await page.getByRole('button', { name: 'Save' }).click();

		await expect.poll(() => saved).toEqual([false]);
		expect(api.calls(SET_MEMBERS).map((call) => call.body)).toEqual([{ user_ids: ['u-cara'] }]);
		expect(api.calls(VISIBILITY)).toHaveLength(0);
	});

	it('opens on whom it is shared with, and Only me unshares a public one', async () => {
		const api = serve([{ user_id: 'u-bob', display_name: 'Bob' }]);
		const saved = await open(true);
		await expect.element(page.getByRole('radio', { name: /Everyone/ })).toBeChecked();
		await page.getByText('Only me').click();
		await page.getByRole('button', { name: 'Save' }).click();

		await expect.poll(() => saved).toEqual([false]);
		expect(api.calls(VISIBILITY).map((call) => call.body)).toEqual([{ is_public: false }]);
		expect(api.calls(SET_MEMBERS).map((call) => call.body)).toEqual([{ user_ids: [] }]);
	});
});
