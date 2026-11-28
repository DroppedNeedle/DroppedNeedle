import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { pickSlip } from '$lib/features/discover/state/passLine.svelte';
import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';
import { apiError, fakeApi, type FakeRoutes, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import Discover from './+page.svelte';

const WEEK = 'GET /api/v3/discover/week';
const KEEP = 'POST /api/v3/requests/albums';
const LATER = 'PUT /api/v3/discover/later/{release_group_mbid}';
const TELL = 'POST /api/v3/discover/feedback';
type Week = RouteData<typeof WEEK>;
type Pick = NonNullable<Week['set']>['picks'][number];

const DAY = 86_400_000;
const WEEK_AT = Date.UTC(2026, 10, 20);

function pick(n: number, taste: number | null, state: Pick['state'] = 'open'): Pick {
	return {
		pick_no: n,
		album: {
			title: `Album ${n}`,
			musicbrainz_id: `rg-${n}`,
			artist_name: `Artist ${n}`,
			artist_id: `ar-${n}`,
			in_library: false,
			requested: false,
			year: 2010,
			release_type: 'album'
		},
		reason: {
			kind: 'your_artists',
			line: `Close to your artists, number ${n}.`,
			artists: [{ artist_mbid: 'seed-1', name: 'Poppy' }]
		},
		state,
		heard: false,
		taste_no: taste
	};
}

const TASTES = [
	{ name: 'Around Poppy', anchor_artist_mbid: 'seed-1', picks: 3, share: 0.35 },
	{ name: 'Around Circa Survive', anchor_artist_mbid: 'seed-2', picks: 3, share: 0.21 },
	{ name: 'Around Yes', anchor_artist_mbid: 'seed-3', picks: 1, share: 0.11 }
];
const TASTE_OF = [0, 0, 0, 1, 1, 1, 2, null];

function ready(
	picks: Pick[] = TASTE_OF.map((t, i) => pick(i + 1, t)),
	extra: Partial<NonNullable<Week['set']>> = {}
): Week {
	return {
		status: 'ready',
		set: {
			week_start_at: WEEK_AT,
			week_number: 47,
			built_at: WEEK_AT,
			next_set_at: WEEK_AT + 7 * DAY,
			counts: {
				plays_counted: 459,
				seeds: 23,
				shelf_artists: 169,
				shelf_albums: 223,
				linked: true,
				neighbours_found: 1256,
				known_left_out: 69,
				hubs_left_out: 4,
				history_from_at: WEEK_AT - 200 * DAY,
				history_to_at: WEEK_AT
			},
			tastes: TASTES,
			picks,
			...extra
		}
	};
}

function routes(week: Week | (() => Week)): FakeRoutes {
	return {
		[WEEK]: typeof week === 'function' ? week : () => week,
		'GET /api/v3/discover': () => ({
			sections: [],
			weekly_set_enabled: true,
			service_prompts: [],
			genre_artwork: {},
			genre_artwork_schema_version: '1',
			refreshing: false
		}),
		'GET /api/v3/discover/later': () => ({ items: [], next_cursor: null, total: 0 }),
		'GET /api/v3/discover/stated-artists': () => ({ items: [], limit: 30 }),
		'GET /api/v3/discover/prefs': () => ({
			set_day: 'friday',
			depth: 'next_door',
			app_playlists: true
		}),
		'GET /api/v3/me/section-prefs': () => ({ pages: {} }),
		'POST /api/v3/discover/activity': () => ({
			source_mode: 'official',
			source_id: 's',
			generation: 1
		}),
		'POST /api/v3/discover/picks/events': () => undefined,
		'GET /api/v3/discover/week/more': () => ({ items: [], next_cursor: null, total: null }),
		'GET /api/v3/discover/weeks': () => ({
			items: [],
			next_cursor: null,
			total: null,
			record: null
		})
	};
}

beforeEach(async () => {
	await page.viewport(1440, 900);
	localStorage.clear();
	pickSlip.dismiss();
	authStore.setUser(
		toAuthUser({
			id: 'u1',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'ada'
		})
	);
});

const PROPS = { params: {}, data: { primarySource: 'listenbrainz' as const, user: null } };

const pickCard = () => page.getByRole('article');
const bin = () => page.getByRole('region', { name: "This week's eight" });

describe('the Discover page', () => {
	it('shows pick one whole, the bin filed under the tastes and the five counts', async () => {
		fakeApi(routes(ready()));
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		await expect.element(pickCard().getByRole('heading', { name: 'Album 1' })).toBeVisible();
		await expect.element(pickCard().getByText('Close to your artists, number 1.')).toBeVisible();
		await expect.element(pickCard().getByText('No. 01 - Around Poppy')).toBeVisible();
		expect(pickCard().element().textContent).not.toContain('%');

		await expect
			.element(bin().getByRole('list', { name: 'Around Poppy, 35% of your listening' }))
			.toHaveTextContent(/Album 1.*Album 2.*Album 3/);
		await expect
			.element(bin().getByRole('list', { name: 'Around Circa Survive, 21% of your listening' }))
			.toHaveTextContent(/Album 4.*Album 5.*Album 6/);

		const receipt = page.getByRole('list', { name: 'What this set was made from' });
		await expect.element(receipt).toHaveTextContent(/459.*23.*1,256.*69.*8/);
	});

	it('Keep asks for the album from the weekly set and the row reads Kept', async () => {
		const api = fakeApi({
			...routes(ready()),
			[KEEP]: () => ({
				success: true,
				message: 'Requested',
				musicbrainz_id: 'rg-1',
				status: 'pending'
			})
		});
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		await pickCard().getByRole('button', { name: /^Keep/ }).click();
		await expect.element(pickCard().getByRole('heading', { name: 'Album 2' })).toBeVisible();
		expect(api.calls(KEEP).map((call) => call.body)).toMatchObject([
			{ musicbrainz_id: 'rg-1', made_from: 'weekly_set' }
		]);
		await expect.element(bin().getByRole('button', { name: /Album 1/ })).toHaveTextContent(/Kept/);
	});

	it('No moves past the pick and shows the slip with its three choices', async () => {
		const api = fakeApi({
			...routes(ready()),
			[TELL]: ({ body }) => ({
				target: 'album',
				target_mbid: 'rg-1',
				kind: (body as { kind: 'no' }).kind,
				release_group_mbid: 'rg-1',
				artist_mbid: 'ar-1',
				release_name: 'Album 1',
				artist_name: 'Artist 1',
				created_at: 1
			})
		});
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		await pickCard().getByRole('button', { name: /^No/ }).click();
		await expect.element(pickCard().getByRole('heading', { name: 'Album 2' })).toBeVisible();
		const slip = page.getByRole('status').filter({ hasText: 'Passed on Album 1.' });
		await expect.element(slip).toBeVisible();
		await expect.element(slip.getByRole('button', { name: 'Undo' })).toBeVisible();
		await expect.element(slip.getByRole('button', { name: 'Know it already' })).toBeVisible();
		await expect.element(slip.getByRole('button', { name: 'Not this artist' })).toBeVisible();
		await expect.element(slip.getByRole('button', { name: 'Not now' })).toBeVisible();
		expect(api.calls(TELL).map((call) => call.body)).toMatchObject([
			{ kind: 'no', release_group_mbid: 'rg-1', made_from: 'weekly_set' }
		]);
		await expect
			.element(bin().getByRole('button', { name: /Album 1/ }))
			.toHaveTextContent(/Passed/);
	});

	it('Keep over the request limit puts the pick on Later, with Undo', async () => {
		const api = fakeApi({
			...routes(ready()),
			[KEEP]: () =>
				apiError(429, 'QUOTA_EXCEEDED', 'Request limit reached', {
					limit: 5,
					window_days: 7,
					used: 5
				}),
			[LATER]: () => undefined
		});
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		await pickCard().getByRole('button', { name: /^Keep/ }).click();
		const slip = page.getByRole('status').filter({ hasText: 'On your Later pile.' });
		await expect.element(slip).toHaveTextContent('Your limit is 5 albums every 7 days, all used.');
		await expect.element(slip.getByRole('button', { name: 'Undo' })).toBeVisible();
		expect(api.calls(LATER).map((call) => call.params)).toEqual([{ release_group_mbid: 'rg-1' }]);
		await expect.element(bin().getByRole('button', { name: /Album 1/ })).toHaveTextContent(/Later/);
	});

	it('reads the three states after Keep in words, none of them kept', async () => {
		const picks = TASTE_OF.map((t, i) => pick(i + 1, t));
		picks[1] = pick(2, 0, 'awaiting_approval');
		picks[2] = pick(3, 0, 'refused');
		picks[3] = pick(4, 1, 'failed');
		fakeApi(routes(ready(picks)));
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		await expect
			.element(bin().getByRole('button', { name: /Album 2/ }))
			.toHaveTextContent(/Waiting/);
		await expect
			.element(bin().getByRole('button', { name: /Album 3/ }))
			.toHaveTextContent(/Refused/);
		await expect
			.element(bin().getByRole('button', { name: /Album 4/ }))
			.toHaveTextContent(/Didn't arrive/);
		for (const n of [2, 3, 4]) {
			expect(
				bin()
					.getByRole('button', { name: new RegExp(`Album ${n}`) })
					.element().textContent
			).not.toMatch(/Kept/);
		}
	});

	it('a short week of five says so with the server line', async () => {
		const picks = [0, 0, 1, 1, 2].map((t, i) => pick(i + 1, t));
		fakeApi(
			routes(
				ready(picks, {
					short_reason: 'short_history',
					short_line:
						'Your history is ten months long, and nothing from the charts is used to fill a set.'
				})
			)
		);
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		const five = page.getByRole('region', { name: "This week's five" });
		await expect.element(five).toBeVisible();
		await expect.element(five.getByText('Only 5 this week.')).toBeVisible();
		await expect.element(five.getByText(/Your history is ten months long/)).toBeVisible();
	});

	it.each([
		['being_set_out', { status: 'being_set_out', found_so_far: 5 } as Week, '5 of 8 found.'],
		['reading_history', { status: 'reading_history' } as Week, 'Reading your history.'],
		[
			'kept_last_week',
			{
				...ready(),
				status: 'kept_last_week',
				failing: { source: 'listenbrainz', since: WEEK_AT }
			} as Week,
			'ListenBrainz has not answered since'
		],
		['nothing_yet', { status: 'nothing_yet' } as Week, 'Name a few artists you love']
	])('shows the %s state and no album from elsewhere', async (_name, week, said) => {
		fakeApi(routes(week));
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		await expect.element(page.getByText(said).first()).toBeVisible();
		if (week.status !== 'kept_last_week') {
			await expect.element(page.getByRole('article')).not.toBeInTheDocument();
		}
	});

	it("ends with one link to the charts of the person's own source", async () => {
		fakeApi(routes(ready()));
		await renderWithQuery(
			Discover,
			{ params: {}, data: { primarySource: 'lastfm', user: null } },
			{ client: queryClient }
		);

		await expect.element(page.getByText('That is everything this week.')).toBeInTheDocument();
		const charts = page.getByRole('link', { name: 'What is popular on Last.fm' });
		await expect.element(charts).toHaveAttribute('href', '/popular');
		await expect
			.element(page.getByRole('link', { name: /What is popular on/ }))
			.toBeInTheDocument();
	});

	it('on a phone, puts the cover, the reason and the three buttons on the first screen', async () => {
		await page.viewport(390, 844);
		fakeApi(routes(ready()));
		await renderWithQuery(Discover, PROPS, { client: queryClient });

		const cover = pickCard().getByRole('link', { name: 'Open Album 1' });
		await expect.element(cover).toBeVisible();
		const inFirstScreen = (element: Element) => element.getBoundingClientRect().bottom <= 844;
		expect(inFirstScreen(cover.element())).toBe(true);
		expect(inFirstScreen(pickCard().getByText('Close to your artists, number 1.').element())).toBe(
			true
		);
		const bar = page.getByRole('group', { name: 'Answer this pick' });
		for (const name of ['No', 'Later', 'Keep']) {
			const button = bar.getByRole('button', { name, exact: true });
			await expect.element(button).toBeVisible();
			expect(inFirstScreen(button.element())).toBe(true);
		}
	});
});

/** An element that plays at once and never ends, so a sample stays on its clip. */
class SilentAudio extends EventTarget {
	src = '';
	preload = '';
	muted = false;
	volume = 1;
	currentTime = 0;
	duration = 30;
	ended = false;
	play() {
		return Promise.resolve();
	}
	pause() {}
	removeAttribute(name: string) {
		if (name === 'src') this.src = '';
	}
	load() {}
}

describe('the listening post', () => {
	const realAudio = window.Audio;
	beforeEach(() => {
		(window as unknown as { Audio: typeof SilentAudio }).Audio = SilentAudio;
	});
	afterEach(() => {
		deckSampler.stop();
		window.Audio = realAudio;
	});

	it('picks the week up where it was paused', async () => {
		fakeApi({
			...routes(ready()),
			'GET /api/v3/discover/album-preview': ({ query }) => ({
				provider: 'deezer',
				tracks: ['opener', 'best_known', 'deep_cut'].map((role, i) => ({
					title: `${query.get('album')} ${role}`,
					artist_name: 'Artist',
					preview_url: `https://cdn.example/${i}.mp3`,
					duration_s: 30,
					position: i + 1,
					role: role as 'opener',
					source: 'deezer' as const
				}))
			})
		});
		await renderWithQuery(Discover, PROPS, { client: queryClient });
		const post = page.getByRole('group', { name: 'Listening post' });

		await post.getByRole('button', { name: 'Play the week' }).click();
		await expect.element(post.getByRole('button', { name: 'Stop' })).toBeVisible();
		deckSampler.nextClip();
		await expect.poll(() => deckSampler.currentTrack?.title).toBe('Album 1 best_known');
		// paused from the floating player while away from the page
		deckSampler.pause();

		await post.getByRole('button', { name: 'Resume' }).click();

		await expect.poll(() => deckSampler.status).toBe('playing');
		expect(deckSampler.currentTrack?.title).toBe('Album 1 best_known');
		await expect.element(post.getByRole('button', { name: 'Stop' })).toBeVisible();
	});
});
