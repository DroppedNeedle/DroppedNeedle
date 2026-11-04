import { page, type Locator } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isHttpError } from '@sveltejs/kit';
import type { Component } from 'svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import type { RemoteSource } from '$lib/features/remote-library/queries/types';
import { remoteSources } from '$lib/features/remote-library/queries/remoteSources';
import { HomeQueryKeyFactory } from '$lib/queries/featureKeys';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { apiError, fakeApi, type FakeRoutes, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

// The pages of /library/{source}: what differs per media server runs for each.

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
	setQueryDataWithPersister(HomeQueryKeyFactory.integrationStatus(authStore.user?.id), {
		jellyfin: true,
		navidrome: true,
		plex: true
	});
});

afterEach(() => {
	vi.restoreAllMocks();
});

describe.each(SOURCES)('%s albums', (source) => {
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
});

describe.each(SOURCES.filter((source) => !remoteSources[source].capabilities.activity))(
	'%s activity',
	(source) => {
		it('is not a page', () => {
			let thrown: unknown;
			try {
				loadActivity({ params: { source } } as never);
			} catch (error) {
				thrown = error;
			}
			expect(isHttpError(thrown, 404)).toBe(true);
		});
	}
);

// The source only names the failure, so one server with every page stands for all.
describe('a library page whose read fails', () => {
	it('says what failed instead of showing it empty', async () => {
		const source: RemoteSource = 'plex';
		const { label } = remoteSources[source];
		const failing = apiError(500, 'INTERNAL_ERROR');
		const cases: {
			view: Component<never>;
			route: keyof FakeRoutes;
			failed: () => Locator;
			empty?: string;
		}[] = [
			{
				view: HubPage,
				route: 'GET /api/v3/remotes/{source}/hub',
				failed: () => page.getByText(`${label} library didn't load`),
				empty: 'No albums found.'
			},
			{
				view: AlbumsPage,
				route: 'GET /api/v3/remotes/{source}/albums',
				failed: () => page.getByText(`${label} albums didn't load`),
				empty: 'No albums found'
			},
			{
				view: ArtistsPage,
				route: 'GET /api/v3/remotes/{source}/artists',
				failed: () => page.getByText(`${label} artists didn't load`),
				empty: 'No artists found'
			},
			{
				view: TracksPage,
				route: 'GET /api/v3/remotes/{source}/tracks',
				failed: () => page.getByText(`${label} tracks didn't load`),
				empty: 'No tracks yet'
			},
			{
				view: PlaylistsPage,
				route: 'GET /api/v3/remotes/{source}/playlists',
				failed: () => page.getByText(`Couldn't load ${label} playlists`)
			},
			{
				view: PlaylistDetailPage,
				route: 'GET /api/v3/remotes/{source}/playlists/{id}',
				failed: () => page.getByRole('button', { name: /Try again|Retry/ }),
				empty: 'Low Tide 0'
			},
			{
				view: ActivityPage,
				route: 'GET /api/v3/remotes/{source}/history',
				failed: () => page.getByText("Listening history didn't load"),
				empty: 'No listening history is available yet.'
			}
		];
		for (const { view, route, failed, empty } of cases) {
			fakeApi({ ...remoteRoutes(), [route]: failing });
			const shown = await renderWithQuery(view, { data: pageData(source) } as never);

			await expect.element(failed()).toBeVisible();
			if (view === HubPage) {
				await expect.element(page.getByRole('button', { name: 'Try again' })).toBeVisible();
			}
			if (empty) await expect.element(page.getByText(empty)).not.toBeInTheDocument();
			shown.unmount();
		}
	});
});
