import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type FakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

const h = vi.hoisted(() => ({ goto: vi.fn() }));

vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/album/x') } }));
vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		isPlaying: false,
		nowPlaying: null,
		currentQueueItem: null,
		addToQueue: vi.fn(),
		playNext: vi.fn(),
		playMultipleNext: vi.fn(),
		addMultipleToQueue: vi.fn()
	}
}));

import AlbumPage from './ProviderAlbumPage.svelte';
import AlbumRoute from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { integrationStore } from '$lib/stores/integration';
import { libraryStore } from '$lib/stores/library';
import { toAuthUser } from '$lib/queries/auth/types';

const albumId = '3f3a6d95-326e-4384-80b0-0744f20f24ff';
const localAlbumId = 'local-album-1';

type Basic = RouteData<'GET /api/v3/albums/{album_id}/basic'>;
type Status = RouteData<'GET /api/v3/library/albums/{id}/status'>;
type Plan = RouteData<'GET /api/v3/library/albums/{album_id}/edition/plan'>;
type History = RouteData<'GET /api/v3/library/albums/{album_id}/history'>;
type Fetch = RouteData<'GET /api/v3/albums/{album_id}/edition/acquire'>['latest'];
type LocalTrack = RouteData<'GET /api/v3/library/albums/{id}/tracks'>['items'][number];

// [disc, position, title, length in ms]
const songs: [number, number, string, number][] = [
	[1, 1, 'Infinite ❤️ Without Fulfillment', 95327],
	[1, 5, 'Circumambient', 223280],
	[2, 1, 'Ambrosia', 213093],
	[2, 5, 'Be a Body (Baarsden rework)', 204626]
];

function basic(id: string): Basic {
	return {
		title: 'Daylight Hours',
		musicbrainz_id: id,
		artist_name: 'Lumen',
		artist_id: 'artist-1',
		in_library: true,
		requested: false,
		cover_url: null,
		service_status: null,
		source: 'musicbrainz'
	};
}

function localTrack(i: number, albumIdOfFile: string): LocalTrack {
	const [disc, position, title, length] = songs[i];
	return {
		id: `file-${i}`,
		title,
		album_id: albumIdOfFile,
		album_title: 'Daylight Hours',
		album_artist_name: 'Lumen',
		artist_name: 'Lumen',
		disc_number: disc,
		track_number: position,
		duration_seconds: Math.round(length / 1000),
		file_size_bytes: 1,
		format: 'flac',
		cover_available: false,
		favorite: false
	};
}

// The release group the library does not hold: the status echoes the asked id.
function notHeld(id: string): Status {
	return { album_id: id, in_library: false, track_count: 0, tracks: [] };
}

function localAlbum(): RouteData<'GET /api/v3/library/albums/{id}'> {
	return {
		id: localAlbumId,
		title: 'Daylight Hours',
		artist_id: 'local-artist-1',
		artist_name: 'Lumen',
		release_group_mbid: albumId,
		cover_available: false,
		favorite: false,
		identity_state: 'matched',
		is_compilation: false,
		total_duration_seconds: 736,
		total_size_bytes: 4,
		track_count: 4
	};
}

function removed(id: string): RouteData<'DELETE /api/v3/library/tracks/{id}'> {
	return { id, removed_track_ids: [id] };
}

let status: (id: string) => Status;
let plan: Plan | null;
let history: History;
let editionFetch: Fetch;
let api: FakeApi;

function routes(): FakeApi {
	return fakeApi({
		'GET /api/v3/library/albums/{album_id}/removals': { items: [] },
		'GET /api/v3/albums/{album_id}/basic': ({ params }) => basic(params.album_id),
		'GET /api/v3/albums/{album_id}/tracks': {
			tracks: songs.map(([disc, position, title, length]) => ({
				disc_number: disc,
				position,
				title,
				length
			})),
			total_tracks: 4,
			total_length: 736326,
			selected_release_mbid: 'release-remaster'
		},
		'GET /api/v3/albums/{album_id}/more-by-artist': { artist_name: 'Lumen', albums: [] },
		'GET /api/v3/albums/{album_id}/similar': { albums: [], configured: false, source: 'lastfm' },
		'GET /api/v3/albums/{album_id}/editions': {
			items: [
				{
					release_mbid: 'release-1',
					date: '2012-02-21',
					country: 'US',
					track_count: 4,
					is_owned: false,
					is_pinned: false
				}
			],
			selected_release_mbid: 'release-1'
		},
		'GET /api/v3/library/albums/match/{mbid}': ({ params }) => ({
			items: params.mbid === albumId ? songs.map((_, i) => localTrack(i, localAlbumId)) : [],
			total: params.mbid === albumId ? 4 : 0,
			offset: 0,
			limit: 500
		}),
		'GET /api/v3/remotes/{source}/match': ({ params }) => ({
			found: params.source === 'navidrome',
			remote_album_id: params.source === 'navidrome' ? 'nav-1' : null,
			source: params.source === 'navidrome' ? 'navidrome' : 'jellyfin',
			tracks:
				params.source === 'navidrome'
					? songs.map(([disc, position, title, length], i) => ({
							id: `n${i + 1}`,
							title,
							track_number: position,
							disc_number: disc,
							duration_secs: Math.round(length / 1000),
							album_name: 'Daylight Hours',
							artist_name: 'Lumen',
							source: 'navidrome' as const
						}))
					: []
		}),
		'GET /api/v3/library/albums/{id}/status': ({ params }) => status(params.id),
		'GET /api/v3/library/albums/{id}': ({ params }) =>
			params.id === localAlbumId ? localAlbum() : apiError(404, 'NOT_FOUND', 'Unknown album'),
		'GET /api/v3/library/albums/{id}/copies': { items: [], total: 0, next: null },
		'GET /api/v3/library/albums/{album_id}/edition': () => ({
			album_id: localAlbumId,
			candidates: [],
			edition_kind: plan ? 'release' : 'none',
			files_by_itself: true,
			keeps_own_tags: false,
			identifying: false,
			identity_revision: 0,
			placing: null,
			reason: plan
				? { code: 'CHOSEN', message: '', action: '' }
				: { code: 'UNIDENTIFIED', message: 'Not matched yet.', action: '' },
			release_mbid: plan?.release_mbid ?? null,
			release_group_mbid: plan ? albumId : null,
			state: plan ? 'chosen' : 'unidentified',
			undo_available: false,
			writing: false
		}),
		'GET /api/v3/library/albums/{album_id}/edition/plan': () =>
			plan ?? apiError(404, 'NOT_FOUND', 'No edition chosen'),
		'GET /api/v3/library/albums/{album_id}/history': () => history,
		'GET /api/v3/albums/{album_id}/edition/acquire': () => ({ latest: editionFetch }),
		'GET /api/v3/albums/{album_id}/lastfm': { listeners: 0, playcount: 0, tags: [] },
		'GET /api/v3/albums/{album_id}/plugin-details': { links: [], tags: [] },
		'GET /api/v3/albums/{album_id}/purchase-options': {
			bandcamp_search_url: 'https://bandcamp.com/search?q=Daylight%20Hours',
			digital: [],
			free: [],
			physical: []
		},
		'GET /api/v3/youtube/link/{album_id}': undefined,
		'GET /api/v3/youtube/track-links/{album_id}': [],
		'GET /api/v3/covers/release-group/{release_group_id}': undefined,
		'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: {} },
		'GET /api/v3/library/retag-pairs': { pairs: [], total: 0 },
		'DELETE /api/v3/library/tracks/{id}': ({ params }) => removed(params.id)
	});
}

function signIn(role: 'admin' | 'user'): void {
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role,
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
}

async function renderPage(id = albumId) {
	const view = await renderWithQuery(AlbumPage, { data: { albumId: id } });
	await expect
		.element(page.getByRole('heading', { name: 'Daylight Hours', level: 1 }))
		.toBeVisible();
	return view;
}

const trackRow = (title: string) => page.getByRole('listitem').filter({ hasText: title }).first();

beforeEach(() => {
	localStorage.clear();
	h.goto.mockReset();
	status = notHeld;
	plan = null;
	history = { items: [] };
	editionFetch = null;
	integrationStore.reset();
	integrationStore.setStatus({ navidrome: true, localfiles: true, library: true });
	signIn('admin');
	api = routes();
});

describe('album detail page remote match', () => {
	it('sends the album title and artist so Navidrome can match by name', async () => {
		await renderPage();
		const matchRoute = 'GET /api/v3/remotes/{source}/match' as const;
		await vi.waitFor(() => expect(api.calls(matchRoute).length).toBeGreaterThan(0));
		const query = api.calls(matchRoute)[0].url.searchParams;
		expect([query.get('mbid'), query.get('title'), query.get('artist')]).toEqual([
			albumId,
			'Daylight Hours',
			'Lumen'
		]);
	});
});

describe('album detail page track removal', () => {
	const removeRoute = 'DELETE /api/v3/library/tracks/{id}' as const;
	const matchRoute = 'GET /api/v3/library/albums/match/{mbid}' as const;

	async function openRemoveFileDialog(): Promise<void> {
		await trackRow('Infinite ❤️ Without Fulfillment').getByLabelText('More actions').click();
		await page.getByRole('menuitem', { name: 'Remove file' }).click();
		await expect.element(page.getByRole('dialog')).toBeVisible();
	}

	it('removes the file and refetches only that album', async () => {
		await renderPage();
		await openRemoveFileDialog();
		const before = api.calls(matchRoute).length;

		await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click();

		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		expect(api.calls(removeRoute).map((call) => call.params.id)).toEqual(['file-0']);
		await vi.waitFor(() => expect(api.calls(matchRoute).length).toBe(before + 1));
		expect(api.calls(matchRoute).every((call) => call.params.mbid === albumId)).toBe(true);
	});

	it('cancel closes the dialog without removing the file', async () => {
		await renderPage();
		await openRemoveFileDialog();

		await page.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();

		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		expect(api.calls(removeRoute)).toHaveLength(0);
	});

	it('surfaces a removal failure in the dialog and refetches nothing', async () => {
		await renderPage();
		await openRemoveFileDialog();
		const before = api.calls(matchRoute).length;
		api.use({ [removeRoute]: apiError(409, 'CONFLICT', 'Busy') });

		await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click();

		await expect.element(page.getByRole('alert')).toHaveTextContent("Couldn't remove this file");
		await expect.element(page.getByRole('dialog')).toBeVisible();
		expect(api.calls(matchRoute)).toHaveLength(before);
	});

	it('drops the dialog on navigation and never refetches the old album', async () => {
		const view = await renderPage();
		await openRemoveFileDialog();
		let finishRemoval: () => void = () => {};
		api.use({
			[removeRoute]: ({ params }) =>
				new Promise((resolve) => (finishRemoval = () => resolve(removed(params.id))))
		});
		await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click();
		await vi.waitFor(() => expect(api.calls(removeRoute)).toHaveLength(1));

		await view.rerender({ data: { albumId: '5b0f0f11-1111-4111-8111-111111111111' } });
		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
		const oldAlbumReads = () =>
			api.calls().filter((call) => call.method === 'GET' && call.url.pathname.endsWith(albumId))
				.length;
		const before = oldAlbumReads();

		finishRemoval();
		await vi.waitFor(() => expect(view.client.isMutating()).toBe(0));
		expect(oldAlbumReads()).toBe(before);
	});
});

describe('matched album edition controls', () => {
	function holdLocalCopy(): void {
		status = (id) =>
			id === albumId
				? { album_id: localAlbumId, in_library: true, track_count: 0, tracks: [] }
				: notHeld(id);
	}

	it('shows the edition plate of the copy the release group holds', async () => {
		holdLocalCopy();
		await renderPage();

		await expect
			.element(page.getByRole('button', { name: /^Edition: No edition yet\./ }))
			.toBeVisible();
	});

	it('shows no edition plate when the library holds no copy', async () => {
		await renderPage();

		await expect.element(page.getByRole('button', { name: /^Edition:/ })).not.toBeInTheDocument();
	});
});

describe('choosing the edition to request', () => {
	const requestRoute = 'POST /api/v3/requests/albums' as const;

	// A request marks the album requested in the shared library store.
	beforeEach(() => libraryStore.removeMbid(albumId));

	function editions(): void {
		api.use({
			'GET /api/v3/albums/{album_id}/editions': {
				items: [
					{
						release_mbid: 'release-remaster',
						disambiguation: 'remaster',
						date: '2012-02-21',
						country: 'US',
						track_count: 4,
						is_owned: false,
						is_pinned: false
					},
					{
						release_mbid: 'release-deluxe',
						disambiguation: 'deluxe',
						date: '2013-05-01',
						country: 'GB',
						track_count: 2,
						is_owned: false,
						is_pinned: false
					}
				]
			},
			'GET /api/v3/albums/{album_id}/editions/{release_mbid}/tracks': {
				tracks: [
					{ disc_number: 1, position: 1, title: 'Ambrosia', length: 213093 },
					{ disc_number: 1, position: 2, title: 'Circumambient', length: 223280 }
				],
				total_tracks: 2
			},
			'GET /api/v3/requests/active': { count: 0, items: [] },
			'GET /api/v3/downloads/tasks': { items: [], page: 1, page_size: 100 },
			'GET /api/v3/albums/{album_id}/basic': ({ params }) => ({
				...basic(params.album_id),
				in_library: false
			}),
			[requestRoute]: {
				success: true,
				message: 'Request accepted',
				musicbrainz_id: albumId,
				status: 'pending'
			}
		});
	}

	it('requests the edition a curator picked', async () => {
		editions();
		await renderPage();

		await page.getByRole('button', { name: /^To get: Remaster\./ }).click();
		const sheet = page.getByRole('dialog');
		await sheet.getByRole('button', { name: /Deluxe/ }).click();
		await expect.element(sheet.getByText('Circumambient')).toBeVisible();
		await sheet.getByRole('button', { name: 'Choose this edition' }).first().click();
		await expect.element(sheet).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Add to Library' }).click();

		await vi.waitFor(() => expect(api.calls(requestRoute)).toHaveLength(1));
		expect(api.calls(requestRoute)[0].body).toMatchObject({
			musicbrainz_id: albumId,
			release_mbid: 'release-deluxe'
		});
	});

	it('names the edition on its way once the download has started', async () => {
		editions();
		api.use({
			'GET /api/v3/albums/{album_id}/basic': ({ params }) => ({
				...basic(params.album_id),
				in_library: false,
				requested: true
			}),
			'GET /api/v3/requests/active': {
				count: 1,
				items: [
					{
						musicbrainz_id: albumId,
						artist_name: 'Lumen',
						album_title: 'Daylight Hours',
						requested_at: 1,
						status: 'downloading',
						request_kind: 'album',
						origin: 'user',
						requester_count: 1,
						task_id: 'task-1',
						release_mbid: 'release-deluxe'
					}
				]
			}
		});
		await renderPage();

		await expect.element(page.getByRole('group', { name: 'Downloading: Deluxe.' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /^Downloading:/ }))
			.not.toBeInTheDocument();
	});

	it('shows a plain user no picker', async () => {
		editions();
		signIn('user');
		await renderPage();

		await expect.element(page.getByRole('button', { name: 'Add to Library' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: /^To get:/ })).not.toBeInTheDocument();
	});
});

describe('the page agrees with the edition', () => {
	// After choosing the remaster: Circumambient's file is another version
	// (3:30 against the edition's 3:43), the others sit on their tracks.
	function chooseRemaster(): void {
		status = (id) =>
			id === albumId
				? {
						album_id: localAlbumId,
						in_library: true,
						track_count: 4,
						tracks: songs.map((_, i) => ({
							...localTrack(i, localAlbumId),
							below_cutoff: false,
							current_tier: 'lossless'
						}))
					}
				: notHeld(id);
		plan = {
			album_id: localAlbumId,
			artist_name: 'Lumen',
			title: 'Daylight Hours',
			release_group_mbid: albumId,
			release_mbid: 'release-remaster',
			identity_revision: 0,
			is_current: true,
			same_album: true,
			edition: { title: 'Daylight Hours', disc_count: 2, track_count: 4, formats: ['CD'] },
			kept_count: 3,
			missing_count: 1,
			other_version_count: 1,
			duplicate_count: 0,
			extra_count: 0,
			files: songs.map(([disc, position, title], i) => ({
				local_track_id: `file-${i}`,
				title,
				disc_number: disc,
				track_number: position,
				edition_disc_number: disc,
				edition_position: position,
				fate: title === 'Circumambient' ? 'other_version' : 'keep',
				length_ms: title === 'Circumambient' ? 210000 : 200000,
				match_written: false,
				matched_by_hand: false,
				note: null
			})),
			tracks: songs.map(([disc, position, title, length], i) => ({
				disc_number: disc,
				position,
				title,
				length_ms: length,
				local_track_id: title === 'Circumambient' ? null : `file-${i}`,
				recording_mbid: `rec-${i}`,
				release_track_mbid: `track-${i}`
			}))
		};
	}

	for (const role of ['admin', 'user'] as const) {
		it(`shows the chosen edition's counts and gaps to ${role === 'admin' ? 'an admin' : 'a plain user'}`, async () => {
			signIn(role);
			chooseRemaster();
			await renderPage();

			const missing = trackRow('Circumambient');
			await expect.element(missing.getByText('Missing', { exact: true })).toBeVisible();
			await expect.element(missing.getByText('3:43')).toBeVisible();
			await expect.element(missing.getByText('Your version 3:30')).toBeVisible();
			await expect.element(trackRow('Ambrosia').getByText('3:20')).toBeVisible();
			await expect.element(page.getByText('3/4', { exact: true })).toBeVisible();
			await expect.element(page.getByText('Edition complete')).not.toBeInTheDocument();
			await expect
				.element(page.getByText('Another version of track 5', { exact: true }))
				.toBeVisible();
		});
	}

	it('says which missing tracks an edition fetch is getting, and which it failed to get', async () => {
		chooseRemaster();
		editionFetch = {
			task_id: 'task-1',
			release_mbid: 'release-remaster',
			state: 'failed',
			tracks: [{ disc: 1, position: 5, state: 'failed' }],
			replace_versions: true,
			can_retry: true,
			can_stop: false
		};
		await renderPage();

		const missing = trackRow('Circumambient');
		await expect.element(missing.getByText('Failed', { exact: true })).toBeVisible();
		await expect.element(missing.getByText('Missing', { exact: true })).not.toBeInTheDocument();
	});

	it("shows a file's note on its row", async () => {
		chooseRemaster();
		const files = plan?.files ?? [];
		files[2].note = { code: 'TRACK_ID_CORRECTED', message: '', action: '' };
		files.push({
			...files[0],
			local_track_id: 'file-other',
			title: 'Genesis',
			fate: 'extra',
			edition_position: null,
			note: { code: 'TRACK_ID_ELSEWHERE', message: '', action: '' }
		});
		await renderPage();

		await expect
			.element(
				trackRow('Ambrosia').getByText(
					"This file's track id belonged to another edition; it will be corrected when tags are written"
				)
			)
			.toBeVisible();
		const leftOut = page.getByRole('listitem').filter({ hasText: 'Genesis' });
		await expect.element(leftOut.getByText('Not on this edition')).toBeVisible();
		await expect
			.element(leftOut.getByText("This file's track id belongs to another edition of this album"))
			.toBeVisible();
	});

	it('shows the album history below the track list', async () => {
		chooseRemaster();
		history = {
			items: [
				{
					kind: 'tracks_continued',
					at: Date.UTC(2026, 9, 12),
					by: null,
					track_count: 1,
					track_titles: ['Ambrosia'],
					other_album_title: null,
					undo_id: null
				}
			]
		};
		await renderPage();

		const historyHeading = page.getByRole('heading', { name: 'Album history' });
		await expect.element(historyHeading).toBeVisible();
		const tracks = page.getByRole('heading', { name: 'Tracks', exact: true }).element();
		expect(
			tracks.compareDocumentPosition(historyHeading.element()) & Node.DOCUMENT_POSITION_FOLLOWING
		).toBeTruthy();
	});

	it('never reads the release group id as a local album', async () => {
		chooseRemaster();
		await renderWithQuery(AlbumRoute, { data: { albumId } });
		await expect
			.element(page.getByRole('heading', { name: 'Daylight Hours', level: 1 }))
			.toBeVisible();

		const localReads = api.calls('GET /api/v3/library/albums/{id}').map((call) => call.params.id);
		expect(localReads).toContain(localAlbumId);
		expect(localReads).not.toContain(albumId);
	});
});
