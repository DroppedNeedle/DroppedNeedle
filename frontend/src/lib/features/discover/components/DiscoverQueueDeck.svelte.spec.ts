import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { setMusicBrainzSourceScope } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import DiscoverQueueDeck from './DiscoverQueueDeck.svelte';

const EVENTS = 'POST /api/v3/discover/picks/events';
type Card = NonNullable<RouteData<'GET /api/v3/discover/queue'>['items']>[number];

function card(n: number): Card {
	return {
		musicbrainz_id: `rg-${n}`,
		title: `Album ${n}`,
		artist_name: `Artist ${n}`,
		artist_id: `ar-${n}`,
		reason: { kind: 'your_artists', line: 'Close to Seed.', artists: [] },
		in_library: false
	};
}

describe('DiscoverQueueDeck.svelte', () => {
	it('reports the card on screen as shown, and no other card', async () => {
		const api = fakeApi({
			'POST /api/v3/discover/activity': {
				source_mode: 'brainzmash',
				source_id: 'source-a',
				generation: 1
			},
			'GET /api/v3/discover/queue/status': { status: 'ready', queue_id: 'q1', item_count: 3 },
			'GET /api/v3/discover/queue': { queue_id: 'q1', items: [card(1), card(2), card(3)] },
			'GET /api/v3/discover/queue/enrich/{release_group_mbid}': {
				tags: [],
				youtube_search_url: '',
				youtube_search_available: false
			},
			[EVENTS]: () => undefined
		});
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
		// The source the activity answers, so the deck sees no source switch.
		setMusicBrainzSourceScope({ source_mode: 'brainzmash', source_id: 'source-a', generation: 1 });
		const events = () => api.calls(EVENTS).map((call) => call.body);
		await renderWithQuery(DiscoverQueueDeck, { youtubeEnabled: false }, { client: queryClient });

		await expect.element(page.getByRole('link', { name: 'Album 1', exact: true })).toBeVisible();
		await vi.waitFor(() =>
			expect(events()).toEqual([{ release_group_mbid: 'rg-1', event: 'shown' }])
		);
		await page.getByRole('button', { name: 'Next' }).click();
		await expect.element(page.getByRole('link', { name: 'Album 2', exact: true })).toBeVisible();
		await vi.waitFor(() =>
			expect(events()).toEqual([
				{ release_group_mbid: 'rg-1', event: 'shown' },
				{ release_group_mbid: 'rg-2', event: 'shown' }
			])
		);
	});
});
