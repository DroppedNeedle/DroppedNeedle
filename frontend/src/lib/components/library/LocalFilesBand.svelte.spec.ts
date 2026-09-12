import { page } from '@vitest/browser/context';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$lib/queries/local/LocalQueries.svelte', () => ({
	getLocalStatsQuery: () => ({
		data: { total_tracks: 8, total_artists: 2, total_size_human: '1 GB' }
	}),
	getLocalRecentQuery: () => ({
		data: Array.from({ length: 8 }, (_, index) => ({
			musicbrainz_id: `album-${index}`,
			cover_url: null
		}))
	})
}));

vi.mock('$lib/utils/libraryTrackLoader.svelte', () => ({
	createLibraryTrackLoader: () => ({
		playAll: vi.fn(),
		shuffleAll: vi.fn()
	})
}));

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		appendQueueSilent: vi.fn(),
		playQueue: vi.fn(),
		regenerateShuffleOrder: vi.fn()
	}
}));

vi.mock('$lib/stores/playbackToast.svelte', () => ({
	playbackToast: { show: vi.fn() }
}));

import LocalFilesBand from './LocalFilesBand.svelte';

it('uses one managed 250px image for the blurred recent-album backdrop', async () => {
	await render(LocalFilesBand);

	expect(page.getByTestId('local-files-backdrop').all()).toHaveLength(1);
	expect(page.getByTestId('local-files-backdrop-image').all()).toHaveLength(1);
	await expect
		.element(page.getByTestId('local-files-backdrop-image'))
		.toHaveAttribute('data-src', '/api/v1/covers/release-group/album-0?size=250');
});
