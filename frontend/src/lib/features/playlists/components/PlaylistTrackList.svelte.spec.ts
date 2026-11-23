import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { toPageDetail } from '$lib/features/playlists/queries/playlistV3Adapter';

import PlaylistTrackList from './PlaylistTrackList.svelte';

type Detail = RouteData<'GET /api/v3/playlists/{playlist_id}'>;
type Track = Detail['tracks'][number];

const GROUP = 'a0000000-0000-4000-8000-000000000001';

function track(id: string, name: string, overrides: Partial<Track> = {}): Track {
	return {
		id,
		position: 0,
		track_name: name,
		artist_name: 'Willow Fen',
		album_name: 'Harbour Lights',
		album_id: GROUP,
		artist_id: null,
		track_source_id: null,
		cover_url: null,
		source_type: '',
		available_sources: null,
		format: null,
		track_number: null,
		disc_number: null,
		duration: null,
		created_at: 1767225600,
		plex_rating_key: null,
		library_file_id: null,
		recording_mbid: null,
		...overrides
	};
}

function playlist(): ReturnType<typeof toPageDetail> {
	return toPageDetail({
		id: 'pl-1',
		name: 'Road Trip',
		track_count: 4,
		total_duration: null,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: 1767225600,
		updated_at: 1767225600,
		is_public: false,
		is_owner: false,
		is_shared: true,
		can_edit: true,
		can_delete: false,
		owner_name: 'Ada',
		is_redacted: false,
		tracks: [
			track('e1', 'Paper Lanterns'),
			track('e2', 'Salt Road'),
			track('e3', 'Lost Song', { album_id: null }),
			track('e4', 'On Jellyfin', { source_type: 'jellyfin', available_sources: ['jellyfin'] })
		]
	});
}

const STATE: RouteData<'GET /api/v3/playlists/{playlist_id}/request-state'> = {
	import: null,
	finding_albums: false,
	requestable_albums: 1,
	no_album_songs: 1,
	entries: [
		{ entry_id: 'e1', state: 'requestable' },
		{ entry_id: 'e2', state: 'awaiting_approval' },
		{ entry_id: 'e3', state: 'no_album_yet' }
	]
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'editor-1',
			display_name: 'Editor',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'editor',
			username_display: 'Editor'
		})
	);
});

describe('missing songs on a playlist', () => {
	it('shows what each missing song waits for and asks for a requestable album', async () => {
		const api = fakeApi({
			'GET /api/v3/playlists/{playlist_id}/request-state': STATE,
			'POST /api/v3/playlists/{playlist_id}/request-missing': {
				batch_id: null,
				requested: 1,
				in_library: 0,
				already_requested: 0,
				rejected_before: 0,
				over_cap: 0,
				over_quota: 0,
				needs_approval: true
			}
		});
		await renderWithQuery(PlaylistTrackList, {
			playlist: playlist(),
			ontrackchange: () => {},
			canRequest: true
		});

		await expect.element(page.getByText('Awaiting approval')).toBeVisible();
		await expect.element(page.getByText('No album found yet')).toBeVisible();
		await expect.element(page.getByText('Downloading')).not.toBeInTheDocument();

		await page.getByLabelText('More actions for Salt Road').click();
		await expect
			.element(page.getByRole('menuitem', { name: 'Request album' }))
			.not.toBeInTheDocument();
		await page.getByLabelText('More actions for Salt Road').click();

		await page.getByLabelText('More actions for Paper Lanterns').click();
		await expect
			.element(page.getByRole('menuitem', { name: 'Request just this song' }))
			.toBeVisible();
		await page.getByRole('menuitem', { name: 'Request album' }).click();

		await expect
			.poll(() => api.calls('POST /api/v3/playlists/{playlist_id}/request-missing').length)
			.toBe(1);
		const [call] = api.calls('POST /api/v3/playlists/{playlist_id}/request-missing');
		expect(call?.params).toEqual({ playlist_id: 'pl-1' });
		const body = call?.body as { idempotency_key: string; entry_id: string };
		expect(body.entry_id).toBe('e1');
		expect(body.idempotency_key).toMatch(/^[0-9a-f-]{36}$/);
	});

	it('shows a viewer no states and no request actions', async () => {
		const api = fakeApi({ 'GET /api/v3/playlists/{playlist_id}/request-state': STATE });
		await renderWithQuery(PlaylistTrackList, {
			playlist: playlist(),
			ontrackchange: () => {},
			readonly: true
		});

		await page.getByLabelText('More actions for Paper Lanterns').click();
		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		await expect
			.element(page.getByRole('menuitem', { name: 'Request album' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByText('Awaiting approval')).not.toBeInTheDocument();
		expect(api.calls('GET /api/v3/playlists/{playlist_id}/request-state')).toEqual([]);
	});
});
