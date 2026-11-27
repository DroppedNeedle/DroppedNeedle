import { page, userEvent } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import type { PlaylistListItemV3 } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import PlaylistsPage from './+page.svelte';

type Sync = RouteData<'GET /api/v3/playlists/{playlist_id}/sync'>;

const SYNC_OK: Sync = {
	syncable: true,
	enabled: true,
	whole_albums: false,
	grant: 'not_needed',
	progress: 'caught_up',
	songs_left: 0,
	hold: null,
	failure: null,
	spotify_off: false,
	checked_at: null
};

// Monday 30 November 2026, 00:00 UTC.
const NEXT = 1_795_996_800;

function playlist(id: string, name: string, overrides: Partial<PlaylistListItemV3> = {}) {
	return {
		id,
		name,
		track_count: 5,
		total_duration: 900,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: 1_767_225_600,
		updated_at: 1_767_312_000,
		is_public: false,
		is_owner: true,
		is_shared: false,
		can_edit: true,
		can_delete: true,
		owner_name: null,
		is_redacted: false,
		kind: 'own',
		rotation: null,
		...overrides
	} as PlaylistListItemV3;
}

const LISTS: PlaylistListItemV3[] = [
	playlist('road', 'Road trip', { updated_at: 1_767_400_000 }),
	playlist('liked', 'Liked Songs', { kind: 'imported', source_ref: 'spotify:liked' }),
	playlist('indie', 'Indie summer', {
		kind: 'imported',
		source_ref: 'spotify:abc',
		updated_at: 1_767_000_000
	}),
	playlist('mix', 'Your Weekly Mix', { kind: 'weekly_mix', source_ref: 'weekly_mix:u1' }),
	playlist('added', 'Added recently', {
		kind: 'rotation',
		can_edit: false,
		rotation: {
			idea: 'added_recently',
			decade: null,
			pinned: false,
			always_on: true,
			next_change_at: NEXT
		}
	}),
	playlist('sams', 'Wedding', {
		is_owner: false,
		can_edit: false,
		can_delete: false,
		is_public: true,
		owner_name: 'Sam'
	}),
	{ id: 'locked', track_count: 4, owner_name: 'Cara', can_delete: false, is_redacted: true }
];

function routes(lists: PlaylistListItemV3[], spotifyLinked = false) {
	return fakeApi({
		'GET /api/v3/playlists': { playlists: lists },
		'GET /api/v3/me/connections': {
			connections: spotifyLinked ? [{ service: 'spotify', enabled: true, username: 'me' }] : []
		},
		'GET /api/v3/me/rotation': { enabled: true, next_change_at: NEXT, waiting: [], thin: [] },
		'GET /api/v3/playlists/{playlist_id}/sync': (request) =>
			request.params.playlist_id === 'indie' ? { ...SYNC_OK, failure: 'playlist_gone' } : SYNC_OK
	});
}

const region = (name: string) => page.getByRole('region', { name });
const rowNames = () =>
	region('Your playlists')
		.getByRole('heading', { level: 3 })
		.elements()
		.map((el) => el.textContent?.trim());

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'u1',
			display_name: 'Me',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'me',
			username_display: 'Me'
		})
	);
});

describe('the Playlists page', () => {
	it('sorts the app-made lists, your own and shared ones into their sections, with no delete on a row', async () => {
		routes(LISTS);
		await renderWithQuery(PlaylistsPage, {});

		const made = region('Made for you');
		await expect.element(made.getByRole('link', { name: 'Your Weekly Mix' })).toBeVisible();
		await expect.element(made.getByText('Changes Mon')).toBeVisible();

		await expect.element(region('Your playlists').getByText('Road trip')).toBeVisible();
		expect(rowNames()).toEqual(['Road trip', 'Liked Songs', 'Indie summer']);

		// The source shows on an import; a sync shows only when it needs a look.
		await expect.element(region('Your playlists').getByText('Spotify').first()).toBeVisible();
		await expect
			.element(region('Your playlists').getByText('Sync stopped', { exact: true }))
			.toBeVisible();
		expect(region('Your playlists').getByText('Sync stopped').elements()).toHaveLength(1);

		const shared = region('Shared with you');
		await expect.element(shared.getByText('Wedding')).toBeVisible();
		await expect.element(shared.getByText('By Sam')).toBeVisible();
		await expect.element(shared.getByText('Private playlist')).toBeVisible();
		await expect.element(shared.getByText('4 songs · By Cara')).toBeVisible();

		await expect.element(page.getByRole('button', { name: 'Play Road trip' })).toBeInTheDocument();
		expect(page.getByRole('button', { name: /Delete/ }).elements()).toHaveLength(0);
	});

	it('finds and orders your playlists in the browser', async () => {
		routes(LISTS);
		await renderWithQuery(PlaylistsPage, {});
		await expect.element(region('Your playlists').getByText('Road trip')).toBeVisible();

		await page.getByRole('radio', { name: 'A-Z' }).click();
		expect(rowNames()).toEqual(['Indie summer', 'Liked Songs', 'Road trip']);

		await userEvent.fill(page.getByRole('searchbox', { name: 'Find a playlist' }), 'LIKED');
		await expect.poll(rowNames).toEqual(['Liked Songs']);

		await userEvent.fill(page.getByRole('searchbox', { name: 'Find a playlist' }), 'nothing');
		await expect.element(page.getByText('No playlist matches "nothing"')).toBeVisible();
	});

	it('imports from a file or, once Spotify is linked, from Spotify', async () => {
		routes(LISTS, true);
		await renderWithQuery(PlaylistsPage, {});

		await page.getByText('Import', { exact: true }).click();
		await expect.element(page.getByRole('menuitem', { name: 'From Spotify' })).toBeVisible();
		await page.getByRole('menuitem', { name: 'From a file' }).click();

		const dialog = page.getByRole('dialog', { name: 'Import a playlist file' });
		await expect.element(dialog).toBeVisible();
		await expect
			.element(dialog.getByRole('checkbox', { name: 'Only songs already in the library' }))
			.toBeVisible();
		await expect.element(dialog.getByRole('button', { name: 'Choose a file' })).toBeVisible();
	});

	it('keeps a way to create the first playlist and to delete an ownerless private one', async () => {
		routes([
			{ id: 'stray', track_count: 2, owner_name: null, can_delete: true, is_redacted: true }
		]);
		await renderWithQuery(PlaylistsPage, {});

		await expect
			.element(page.getByRole('button', { name: /Create your first playlist/ }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Delete private playlist with no owner' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: 'Made for you' }))
			.not.toBeInTheDocument();
	});
});
