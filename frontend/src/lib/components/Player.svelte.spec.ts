import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LyricsData } from '$lib/queries/lyrics/LyricsQuery.svelte';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

vi.mock('$lib/player/createSource', () => ({
	createPlaybackSource: vi.fn(() => ({
		type: 'local' as const,
		load: vi.fn().mockResolvedValue(undefined),
		play: vi.fn(),
		pause: vi.fn(),
		seekTo: vi.fn(),
		setVolume: vi.fn(),
		getCurrentTime: vi.fn(() => 0),
		getDuration: vi.fn(() => 180),
		isSeekable: vi.fn(() => true),
		destroy: vi.fn(),
		onStateChange: vi.fn(),
		onReady: vi.fn(),
		onError: vi.fn(),
		onProgress: vi.fn()
	}))
}));

let mockQueryState: {
	isSuccess: boolean;
	isError: boolean;
	isLoading: boolean;
	isFetching: boolean;
	data: LyricsData | null | undefined;
};

vi.mock('$lib/queries/lyrics/LyricsQuery.svelte', () => ({
	getLyricsQuery: vi.fn(() => mockQueryState)
}));

import { playerStore } from '$lib/stores/player.svelte';
import Player from './Player.svelte';

function makeTrack(sourceType: 'navidrome' | 'jellyfin' | 'youtube' | 'local' | 'plex', id = 'v1') {
	return {
		trackSourceId: id,
		trackName: 'Test Track',
		artistName: 'Test Artist',
		trackNumber: 1,
		albumId: 'a1',
		albumName: 'Test Album',
		coverUrl: null,
		sourceType,
		streamUrl: `http://test/${id}.mp3`
	};
}

describe('Player.svelte lyrics button', () => {
	beforeEach(() => {
		playerStore.stop();
		mockQueryState = {
			isSuccess: false,
			isError: false,
			isLoading: false,
			isFetching: false,
			data: undefined
		};
	});

	it('shows lyrics button when query succeeds with lyrics data', async () => {
		mockQueryState = {
			isSuccess: true,
			isError: false,
			isLoading: false,
			isFetching: false,
			data: { text: 'Hello world', is_synced: false, lines: [] }
		};

		playerStore.playQueue([makeTrack('navidrome')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).toBeInTheDocument();
	});

	it('hides lyrics button when query succeeds with null (no lyrics)', async () => {
		mockQueryState = {
			isSuccess: true,
			isError: false,
			isLoading: false,
			isFetching: false,
			data: null
		};

		playerStore.playQueue([makeTrack('navidrome')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});

	it('hides lyrics button when query is loading', async () => {
		mockQueryState = {
			isSuccess: false,
			isError: false,
			isLoading: true,
			isFetching: true,
			data: undefined
		};

		playerStore.playQueue([makeTrack('navidrome')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});

	it('hides lyrics button when query errors', async () => {
		mockQueryState = {
			isSuccess: false,
			isError: true,
			isLoading: false,
			isFetching: false,
			data: undefined
		};

		playerStore.playQueue([makeTrack('navidrome')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});

	it('hides lyrics button for youtube source', async () => {
		mockQueryState = {
			isSuccess: false,
			isError: false,
			isLoading: false,
			isFetching: false,
			data: undefined
		};

		playerStore.playQueue([makeTrack('youtube')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});

	it('hides lyrics button for plex source', async () => {
		mockQueryState = {
			isSuccess: false,
			isError: false,
			isLoading: false,
			isFetching: false,
			data: undefined
		};

		playerStore.playQueue([makeTrack('plex')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});

	it('shows lyrics button for local source when embedded lyrics exist', async () => {
		mockQueryState = {
			isSuccess: true,
			isError: false,
			isLoading: false,
			isFetching: false,
			data: { text: 'Embedded lyrics', is_synced: false, lines: [] }
		};

		playerStore.playQueue([makeTrack('local')]);
		await render(Player);

		await expect.element(page.getByLabelText('Toggle lyrics')).toBeInTheDocument();
	});
});
