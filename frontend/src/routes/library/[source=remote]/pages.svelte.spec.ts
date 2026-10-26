import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isHttpError } from '@sveltejs/kit';
import { toAuthUser } from '$lib/queries/auth/types';
import type { RemoteSource } from '$lib/queries/remotes/types';
import { remoteSources } from '$lib/queries/remotes/remoteSources';
import { authStore } from '$lib/stores/authStore.svelte';
import { playerStore } from '$lib/stores/player.svelte';
import { apiError, fakeApi, type FakeRoutes, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

// One spec per page kind of /library/{source}, each run for every media server.

const location = vi.hoisted(() => ({
	url: new URL('http://localhost/'),
	params: { id: 'pl-1' } as Record<string, string>
}));
vi.mock('$app/state', () => ({ page: location }));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import HubPage from './+page.svelte';
import AlbumsPage from './albums/+page.svelte';
import ArtistsPage from './artists/+page.svelte';
import TracksPage from './tracks/+page.svelte';
import PlaylistsPage from './playlists/+page.svelte';
import PlaylistDetailPage from './playlists/[id]/+page.svelte';
import ActivityPage from './activity/+page.svelte';
import { load as loadActivity } from './activity/+page';

// Canned answers for every remote read the library pages make, one server at a time.

const SOURCES: RemoteSource[] = ['plex', 'jellyfin', 'navidrome'];

type Album = RouteData<'GET /api/v3/remotes/{source}/albums'>['items'][number];
type Artist = RouteData<'GET /api/v3/remotes/{source}/artists'>['items'][number];
type Track = RouteData<'GET /api/v3/remotes/{source}/tracks'>['items'][number];

const album = (source: RemoteSource, i = 0): Album => ({
	source,
	id: `${source}-al-${i}`,
	title: `Night Garden ${i}`,
	artist_name: 'Signal Path',
	year: 2020 + i,
	track_count: 9,
	image_url: null,
	release_group_mbid: null
});

const artist = (source: RemoteSource, i = 0): Artist => ({
	source,
	id: `${source}-ar-${i}`,
	name: `Mara Vell ${i}`,
	album_count: 2,
	image_url: null,
	artist_mbid: null
});

const track = (source: RemoteSource, i = 0): Track => ({
	source,
	id: `${source}-tr-${i}`,
	title: `Low Tide ${i}`,
	album_name: 'Night Garden 0',
	artist_name: 'Signal Path',
	duration_secs: 200,
	track_number: i + 1,
	disc_number: 1,
	part_key: `/library/parts/${i}/file.flac`
});

function signIn() {
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
}

/** The layout's data for a page under /library/{source}. */
const pageData = (source: RemoteSource) => ({
	remote: remoteSources[source],
	primarySource: 'library' as const,
	user: null
});

/** Every remote read, answered for the server named in the path. */
function remoteRoutes(): FakeRoutes {
	const src = (params: Record<string, string>) => params.source as RemoteSource;
	const page = <T>(items: T[]) => ({ items, total: items.length, limit: 48, offset: 0 });
	return {
		'GET /api/v3/remotes/{source}/hub': ({ params }) => ({
			source: src(params),
			stats: { total_albums: 1, total_artists: 1, total_tracks: 1 },
			genres: ['Indie'],
			recently_played: [],
			recently_added: [album(src(params))],
			all_albums_preview: [album(src(params))],
			favorites: [],
			favorite_artists: [],
			most_played_artists: []
		}),
		'GET /api/v3/remotes/{source}/favorites': { albums: [], artists: [], tracks: [] },
		'GET /api/v3/remotes/{source}/artists/index': { index: [] },
		'GET /api/v3/remotes/{source}/connection': ({ params }) => ({
			source: src(params),
			connected: true,
			account_mode: 'shared',
			account_label: 'admin'
		}),
		'GET /api/v3/remotes/{source}/playlists': ({ params }) => ({
			items: [
				{
					source: src(params),
					id: 'pl-1',
					name: 'Morning run',
					track_count: 1,
					duration_secs: 200,
					is_smart: false,
					is_imported: false,
					image_url: null
				}
			]
		}),
		'GET /api/v3/remotes/{source}/playlists/{id}': ({ params }) => ({
			playlist: {
				source: src(params),
				id: params.id,
				name: 'Morning run',
				track_count: 1,
				duration_secs: 200,
				is_smart: false,
				is_imported: false,
				image_url: null
			},
			tracks: [track(src(params))]
		}),
		'GET /api/v3/remotes/{source}/discovery': ({ params }) => ({ source: src(params), hubs: [] }),
		'GET /api/v3/remotes/{source}/history': ({ params }) => ({
			source: src(params),
			total: 0,
			items: []
		}),
		'GET /api/v3/remotes/{source}/random': page([]),
		'GET /api/v3/remotes/{source}/albums': ({ params }) => page([album(src(params))]),
		'GET /api/v3/remotes/{source}/artists': ({ params }) => page([artist(src(params))]),
		'GET /api/v3/remotes/{source}/tracks': ({ params }) => page([track(src(params))]),
		'GET /api/v3/remotes/{source}/recent': [],
		'GET /api/v3/remotes/{source}/genres': ['Indie'],
		'GET /api/v3/remotes/{source}/moods': [],
		'GET /api/v3/remotes/{source}/filters': { years: [], tags: [], studios: [] },
		'GET /api/v3/remotes/{source}/stats': { total_albums: 1, total_artists: 1, total_tracks: 1 },
		'GET /api/v3/remotes/navidrome/folders': {
			mode: 'all',
			folder_ids: [],
			available_folders: [],
			source_available: true,
			stale_folder_ids: []
		},
		'GET /api/v3/remotes/{source}/analytics': {
			top_artists: [],
			top_albums: [],
			top_tracks: [],
			total_listens: 1240,
			listens_last_7_days: 61,
			listens_last_30_days: 240,
			total_hours: 81.5,
			is_complete: true,
			entries_analyzed: 1240
		}
	};
}

beforeEach(() => {
	localStorage.clear();
	location.url = new URL('http://localhost/');
	signIn();
});

afterEach(() => {
	vi.restoreAllMocks();
});

describe.each(SOURCES)('%s hub', (source) => {
	const label = remoteSources[source].label;

	it('shows the library with its counts and albums', async () => {
		fakeApi(remoteRoutes());
		await renderWithQuery(HubPage, { data: pageData(source) } as never);

		await expect.element(page.getByRole('heading', { name: `${label} Library` })).toBeVisible();
		await expect.element(page.getByText('Browse Albums')).toBeVisible();
		await expect.element(page.getByText('Night Garden 0').first()).toBeVisible();
	});

	it('says the library failed instead of showing it empty', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/hub': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(HubPage, { data: pageData(source) } as never);

		await expect.element(page.getByText(`${label} library didn't load`)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Try again' })).toBeVisible();
		await expect.element(page.getByText('No albums found.')).not.toBeInTheDocument();
	});
});

describe.each(SOURCES)('%s albums', (source) => {
	const label = remoteSources[source].label;

	it('lists albums and loads the next page from where the last ended', async () => {
		const api = fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/albums': ({ query }) =>
				query.get('offset') === '1'
					? { items: [album(source, 2)], total: 2, limit: 48, offset: 1 }
					: { items: [album(source, 1)], total: 2, limit: 48, offset: 0 }
		});
		await renderWithQuery(AlbumsPage, { data: pageData(source) } as never);

		await expect.element(page.getByRole('button', { name: 'Open Night Garden 1' })).toBeVisible();
		await page.getByRole('button', { name: 'Show more' }).click();
		await expect.element(page.getByRole('button', { name: 'Open Night Garden 2' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Show more' })).not.toBeInTheDocument();
		const first = api.calls('GET /api/v3/remotes/{source}/albums')[0];
		expect(first?.params.source).toBe(source);
		expect(first?.query.get('sort_by')).toBe(remoteSources[source].sorts.albums[0].value);
	});

	it('opens a deep-linked sort', async () => {
		const added = remoteSources[source].sorts.albumsAdded;
		location.url = new URL(`http://localhost/library/${source}/albums?sort=${added}`);
		const api = fakeApi(remoteRoutes());
		await renderWithQuery(AlbumsPage, { data: pageData(source) } as never);

		await expect.element(page.getByRole('button', { name: 'Open Night Garden 0' })).toBeVisible();
		const call = api.calls('GET /api/v3/remotes/{source}/albums')[0];
		expect(call?.query.get('sort_by')).toBe(added);
		expect(call?.query.get('sort_order')).toBe('desc');
	});

	it('says the albums failed instead of listing none', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/albums': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(AlbumsPage, { data: pageData(source) } as never);

		await expect.element(page.getByText(`${label} albums didn't load`)).toBeVisible();
		await expect.element(page.getByText('No albums found')).not.toBeInTheDocument();
	});
});

describe.each(SOURCES)('%s artists', (source) => {
	const label = remoteSources[source].label;

	it('lists artists, starting from a deep-linked search', async () => {
		location.url = new URL(`http://localhost/library/${source}/artists?search=Mara`);
		const api = fakeApi(remoteRoutes());
		await renderWithQuery(ArtistsPage, { data: pageData(source) } as never);

		await expect.element(page.getByRole('heading', { name: `${label} Artists` })).toBeVisible();
		await expect.element(page.getByText('Mara Vell 0')).toBeVisible();
		await expect.element(page.getByRole('textbox', { name: 'Search artists' })).toHaveValue('Mara');
		expect(api.calls('GET /api/v3/remotes/{source}/artists')[0]?.query.get('search')).toBe('Mara');
	});

	it('says the artists failed instead of listing none', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/artists': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(ArtistsPage, { data: pageData(source) } as never);

		await expect.element(page.getByText(`${label} artists didn't load`)).toBeVisible();
		await expect.element(page.getByText('No artists found')).not.toBeInTheDocument();
	});
});

describe.each(SOURCES)('%s tracks', (source) => {
	const remote = remoteSources[source];

	it('plays the page from the chosen track', async () => {
		const playQueue = vi.spyOn(playerStore, 'playQueue').mockImplementation(() => {});
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/tracks': {
				items: [track(source, 0), track(source, 1)],
				total: 2,
				limit: 48,
				offset: 0
			}
		});
		await renderWithQuery(TracksPage, { data: pageData(source) } as never);

		await page.getByRole('button', { name: 'Play Low Tide 1' }).click();
		expect(playQueue).toHaveBeenCalledOnce();
		const [items, start, shuffle] = playQueue.mock.calls[0];
		expect(items.map((item) => [item.sourceType, item.trackSourceId])).toEqual(
			[track(source, 0), track(source, 1)].map((t) => [source, remote.playKey(t)])
		);
		expect(start).toBe(1);
		expect(shuffle).toBe(false);
	});

	it('says the tracks failed instead of listing none', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/tracks': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(TracksPage, { data: pageData(source) } as never);

		await expect.element(page.getByText(`${remote.label} tracks didn't load`)).toBeVisible();
		await expect.element(page.getByText('No tracks yet')).not.toBeInTheDocument();
	});
});

describe.each(SOURCES)('%s playlists', (source) => {
	const label = remoteSources[source].label;

	it('lists the playlists linking to their pages', async () => {
		fakeApi(remoteRoutes());
		await renderWithQuery(PlaylistsPage, { data: pageData(source) } as never);

		await expect
			.element(page.getByRole('link', { name: /Morning run/ }))
			.toHaveAttribute('href', `/library/${source}/playlists/pl-1`);
	});

	it('says the playlists failed instead of listing none', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/playlists': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(PlaylistsPage, { data: pageData(source) } as never);

		await expect.element(page.getByText(`Couldn't load ${label} playlists`)).toBeVisible();
	});
});

describe.each(SOURCES)('%s playlist', (source) => {
	it('shows the playlist and its tracks', async () => {
		const api = fakeApi(remoteRoutes());
		await renderWithQuery(PlaylistDetailPage, { data: pageData(source) } as never);

		await expect.element(page.getByRole('heading', { name: 'Morning run' })).toBeVisible();
		await expect.element(page.getByText('Low Tide 0')).toBeVisible();
		expect(api.calls('GET /api/v3/remotes/{source}/playlists/{id}')[0]?.params).toEqual({
			source,
			id: 'pl-1'
		});
	});

	it('says the playlist failed instead of showing it empty', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/playlists/{id}': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(PlaylistDetailPage, { data: pageData(source) } as never);

		await expect.element(page.getByRole('button', { name: /Try again|Retry/ })).toBeVisible();
		await expect.element(page.getByText('Low Tide 0')).not.toBeInTheDocument();
	});
});

describe.each(SOURCES)('%s activity', (source) => {
	const remote = remoteSources[source];

	if (!remote.capabilities.activity) {
		it('is not a page', () => {
			let thrown: unknown;
			try {
				loadActivity({ params: { source } } as never);
			} catch (error) {
				thrown = error;
			}
			expect(isHttpError(thrown, 404)).toBe(true);
		});
		return;
	}

	it('shows the listening totals', async () => {
		fakeApi(remoteRoutes());
		await renderWithQuery(ActivityPage, { data: pageData(source) } as never);

		expect(loadActivity({ params: { source } } as never)).toBeUndefined();
		await expect.element(page.getByText('Total Listens')).toBeVisible();
		await expect.element(page.getByText('1,240')).toBeVisible();
	});

	it('says the history failed instead of showing it empty', async () => {
		fakeApi({
			...remoteRoutes(),
			'GET /api/v3/remotes/{source}/history': apiError(500, 'INTERNAL_ERROR')
		});
		await renderWithQuery(ActivityPage, { data: pageData(source) } as never);

		await expect.element(page.getByText("Listening history didn't load")).toBeVisible();
		await expect
			.element(page.getByText('No listening history is available yet.'))
			.not.toBeInTheDocument();
	});
});
