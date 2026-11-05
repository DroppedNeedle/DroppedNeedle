import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { QueueItem } from '$lib/features/player/playback/types';

// The queue never loads audio here: a source that does nothing.
vi.mock('$lib/features/player/playback/createSource', () => ({
	createPlaybackSource: () => ({
		type: 'local',
		load: async () => undefined,
		play: () => undefined,
		pause: () => undefined,
		seekTo: () => undefined,
		setVolume: () => undefined,
		getCurrentTime: () => 0,
		getDuration: () => 180,
		isSeekable: () => true,
		destroy: () => undefined,
		onStateChange: () => undefined,
		onReady: () => undefined,
		onError: () => undefined,
		onProgress: () => undefined
	})
}));

import '../../../../app.css';
import QueueList from './QueueList.svelte';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { skipByRating } from '$lib/features/player/state/skipByRating.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';

const RATINGS: Record<string, number> = { 'track-0': 5, 'track-1': 2, 'track-2': 4, 'track-4': 1 };

const item = (n: number): QueueItem => ({
	trackSourceId: `track-${n}`,
	trackName: `Song ${n}`,
	artistName: 'Artist',
	trackNumber: n + 1,
	albumId: 'album-1',
	albumName: 'Album',
	coverUrl: null,
	sourceType: 'local'
});

beforeEach(() => {
	localStorage.clear();
	queryClient.clear();
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

afterEach(() => {
	skipByRating.turnOff();
	playerStore.stop();
	queryClient.clear();
});

describe('the queue under a rating floor', () => {
	it('marks and dims the tracks it skips, keeps them playable and counts only the rest', async () => {
		fakeApi({
			'POST /api/v3/ratings/lookup': ({ body }) => {
				const ids = (body as { track_ids: string[] }).track_ids;
				const tracks: Record<string, number> = {};
				for (const id of ids) if (RATINGS[id]) tracks[id] = RATINGS[id];
				return { albums: {}, artists: {}, tracks };
			}
		});
		playerStore.appendQueueSilent([0, 1, 2, 3, 4].map(item));
		skipByRating.setMinStars(4);
		await renderWithQuery(QueueList, { oncleared: () => undefined }, { client: queryClient });

		const skipped = page.getByRole('button', { name: /Song 1/ });
		await expect.element(skipped.getByText('Skips')).toBeVisible();
		await expect.element(skipped).toHaveClass(/dn-queue-row--skips/);
		await expect
			.element(page.getByRole('button', { name: /Song 3/ }).getByText('Skips'))
			.not.toBeInTheDocument();
		await expect.element(page.getByText('2 upcoming')).toBeVisible();

		await skipped.click();
		await expect.poll(() => playerStore.currentIndex).toBe(1);
	});
});
