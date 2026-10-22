import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type FakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', () => ({
	page: { params: { id: 'local-album-1' }, url: new URL('http://localhost/album/local-album-1') }
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { playQueue: vi.fn(), addToQueue: vi.fn(), playNext: vi.fn() }
}));

import LocalAlbumPage from './LocalAlbumPage.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const album: RouteData<'GET /api/v3/library/albums/{id}'> = {
	id: 'local-album-1',
	title: 'Local Only Album',
	artist_id: 'local-artist-1',
	artist_name: 'Local Artist',
	identity_state: 'local_only',
	is_compilation: false,
	track_count: 1,
	total_duration_seconds: 181,
	total_size_bytes: 1024,
	format: 'flac',
	year: 2026,
	cover_available: false,
	favorite: false,
	date_added: 1
};

const track: RouteData<'GET /api/v3/library/albums/{id}/tracks'>['items'][number] = {
	id: 'local-track-1',
	title: 'Unmatched Song',
	album_id: album.id,
	album_title: album.title,
	artist_id: album.artist_id,
	artist_name: album.artist_name,
	album_artist_id: album.artist_id,
	album_artist_name: album.artist_name,
	disc_number: 1,
	track_number: 1,
	year: 2026,
	genre: 'Electronic',
	duration_seconds: 181,
	format: 'flac',
	bit_rate: 900000,
	sample_rate: 48000,
	bit_depth: 24,
	channels: 2,
	file_size_bytes: 1024,
	date_added: 1,
	cover_available: false,
	favorite: false
};

function fakeLibrary(options: { downloadAllowed: boolean }): FakeApi {
	return fakeApi({
		'GET /api/v3/library/albums/{id}': album,
		'GET /api/v3/library/albums/{id}/tracks': { items: [track], total: 1, offset: 0, limit: 100 },
		'GET /api/v3/download/access': { allowed: options.downloadAllowed },
		'GET /api/v3/library/albums/{album_id}/edition': {
			album_id: album.id,
			candidates: [],
			edition_kind: 'none',
			files_by_itself: true,
			keeps_own_tags: false,
			identifying: false,
			identity_revision: 0,
			placing: null,
			reason: { code: 'UNIDENTIFIED', message: 'Not matched yet.', action: '' },
			release_mbid: null,
			release_group_mbid: null,
			state: 'unidentified',
			undo_available: false,
			writing: false
		},
		'GET /api/v3/library/albums/{album_id}/history': { items: [] },
		'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: {} }
	});
}

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
});

describe('local album page track menu', () => {
	it('hides the button and omits the menu Download item when restricted', async () => {
		expect.assertions(4);
		fakeLibrary({ downloadAllowed: false });
		await renderWithQuery(LocalAlbumPage, { albumId: album.id });

		await expect.element(page.getByText('Unmatched Song')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Download album/ }))
			.not.toBeInTheDocument();
		await (await page.getByLabelText('More actions').all())[0].click();
		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download' }).elements()).toHaveLength(0);
	});
});

describe('local album page and its edition', () => {
	it("shows a file's note on its row and the album history below the tracks", async () => {
		const api = fakeLibrary({ downloadAllowed: true });
		api.use({
			'GET /api/v3/library/albums/{album_id}/edition': {
				album_id: album.id,
				candidates: [],
				edition_kind: 'release',
				files_by_itself: true,
				keeps_own_tags: false,
				identifying: false,
				identity_revision: 0,
				placing: null,
				reason: { code: 'CHOSEN', message: '', action: '' },
				release_mbid: 'release-1',
				release_group_mbid: 'group-1',
				state: 'chosen',
				undo_available: false,
				writing: false
			},
			'GET /api/v3/library/albums/{album_id}/edition/plan': {
				album_id: album.id,
				artist_name: album.artist_name,
				title: album.title,
				release_group_mbid: 'group-1',
				release_mbid: 'release-1',
				identity_revision: 0,
				is_current: true,
				same_album: true,
				edition: { title: album.title, disc_count: 1, track_count: 1, formats: ['CD'] },
				kept_count: 1,
				missing_count: 0,
				other_version_count: 0,
				duplicate_count: 0,
				extra_count: 0,
				files: [
					{
						local_track_id: track.id,
						title: track.title,
						disc_number: 1,
						track_number: 1,
						edition_disc_number: 1,
						edition_position: 1,
						fate: 'keep',
						length_ms: 181000,
						match_written: false,
						matched_by_hand: false,
						note: { code: 'TRACK_ID_CORRECTED', message: '', action: '' }
					}
				],
				tracks: [
					{
						disc_number: 1,
						position: 1,
						title: track.title,
						length_ms: 181000,
						local_track_id: track.id,
						recording_mbid: 'rec-1',
						release_track_mbid: 'track-1'
					}
				]
			},
			'GET /api/v3/library/albums/{album_id}/history': {
				items: [
					{
						kind: 'tracks_continued',
						at: Date.UTC(2026, 9, 12),
						by: null,
						track_count: 1,
						track_titles: ['Unmatched Song'],
						other_album_title: null,
						undo_id: null
					}
				]
			}
		});
		await renderWithQuery(LocalAlbumPage, { albumId: album.id });

		const row = page.getByRole('listitem').filter({ hasText: 'Unmatched Song' }).first();
		await expect
			.element(
				row.getByText(
					"This file's track id belonged to another edition; it will be corrected when tags are written"
				)
			)
			.toBeVisible();
		const history = page.getByRole('heading', { name: 'Album history' });
		await expect.element(history).toBeVisible();
		const tracks = page.getByRole('heading', { name: 'Tracks', exact: true }).element();
		expect(
			tracks.compareDocumentPosition(history.element()) & Node.DOCUMENT_POSITION_FOLLOWING
		).toBeTruthy();
	});
});
