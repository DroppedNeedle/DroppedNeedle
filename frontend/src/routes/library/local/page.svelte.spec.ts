import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	local: '11111111-1111-4111-8111-111111111111',
	provider: '22222222-2222-4222-8222-222222222222'
}));
vi.mock('$lib/queries/local/LocalQueries.svelte', () => {
	const empty = { data: { items: [] }, isLoading: false, isFetching: false, refetch: vi.fn() };
	return {
		getLocalRecentQuery: () => ({
			...empty,
			data: [
				{
					local_id: h.local,
					musicbrainz_id: h.provider,
					name: 'Local shelf album',
					artist_name: 'Artist',
					track_count: 1,
					total_size_bytes: 1
				}
			]
		}),
		getLocalAlbumsQuery: () => empty,
		getLocalSuggestionsQuery: () => empty,
		getLocalDecadesQuery: () => empty,
		getLocalStatsQuery: () => ({ data: null }),
		getLocalSearchQuery: () => ({ ...empty, data: { albums: [], tracks: [] } })
	};
});
vi.mock('$lib/queries/lyrics/LyricsQuery.svelte', () => ({
	getLyricsQuery: () => ({ isSuccess: false })
}));
import ListeningRoom from './+page.svelte';

describe('Listening Room local artwork', () => {
	it('uses the explicit stable DTO ID for shelf artwork even when the compatibility token differs', async () => {
		await render(ListeningRoom);
		await expect
			.element(page.getByAltText('Local shelf album'))
			.toHaveAttribute('data-src', `/api/v1/library/albums/${h.local}/artwork/local`);
	});
});
