import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
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
	isFetching: boolean;
	data: LyricsData | null | undefined;
};

vi.mock('$lib/queries/lyrics/LyricsQuery.svelte', () => ({
	getLyricsQuery: vi.fn(() => mockQueryState)
}));

import { playerStore } from '$lib/stores/player.svelte';
import Turntable from './Turntable.svelte';

const callbacks = {
	onDropPlay: vi.fn(),
	onDropAlbum: vi.fn(),
	onPlayAll: vi.fn(),
	onShuffleAll: vi.fn(),
	onSurprise: vi.fn(),
	onOpenQueue: vi.fn()
};

function playLocalTrack() {
	playerStore.playQueue([
		{
			trackSourceId: 'file-1',
			trackName: 'So It Goes',
			artistName: 'Anthony Green',
			trackNumber: 1,
			albumId: 'album-1',
			albumName: 'Boom. Done.',
			coverUrl: null,
			sourceType: 'local',
			streamUrl: '/api/v1/stream/local/file-1'
		}
	]);
}

describe('Turntable lyrics', () => {
	beforeEach(() => {
		playerStore.stop();
		mockQueryState = {
			isSuccess: true,
			isError: false,
			isFetching: false,
			data: { text: 'First line\nSecond line', is_synced: false, lines: [] }
		};
	});

	it('opens the shared lyrics panel for a local track', async () => {
		playLocalTrack();
		render(Turntable, callbacks);

		const toggle = page.getByLabelText('Toggle lyrics');
		await expect.element(toggle).toBeInTheDocument();
		await toggle.click();

		await expect.element(page.getByRole('dialog', { name: 'Lyrics' })).toBeInTheDocument();
		await expect.element(page.getByText('First line')).toBeInTheDocument();
	});

	it('hides the lyrics action when the local file has none', async () => {
		mockQueryState.data = null;
		playLocalTrack();
		render(Turntable, callbacks);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});
});

describe('Turntable deck cover', () => {
	const deckMbid = 'b1392450-e666-3926-a536-22c65f834433';
	const proxyCover = `/api/v1/covers/release-group/${deckMbid}?size=250`;
	const remoteCover = 'https://r2.theaudiodb.com/images/media/album/thumb/abc123.jpg';

	function playDeckTrack(overrides: { coverUrl?: string | null; coverRemoteUrl?: string | null }) {
		playerStore.playQueue([
			{
				trackSourceId: 'file-9',
				trackName: 'Deck Song',
				artistName: 'Deck Artist',
				trackNumber: 1,
				albumId: deckMbid,
				albumName: 'Guard Album',
				coverUrl: proxyCover,
				coverRemoteUrl: null,
				sourceType: 'local',
				streamUrl: '/api/v1/stream/local/file-9',
				...overrides
			}
		]);
	}

	beforeEach(() => {
		playerStore.stop();
		mockQueryState = {
			isSuccess: true,
			isError: false,
			isFetching: false,
			data: { text: 'First line\nSecond line', is_synced: false, lines: [] }
		};
	});

	it('renders the covers proxy when the queued remote cover is a local path', async () => {
		playDeckTrack({ coverRemoteUrl: proxyCover });
		render(Turntable, callbacks);

		await expect.element(page.getByAltText('Guard Album')).toHaveAttribute('src', proxyCover);
	});

	it('renders the remote branch for https covers', async () => {
		playDeckTrack({ coverUrl: remoteCover, coverRemoteUrl: remoteCover });
		render(Turntable, callbacks);

		await expect
			.element(page.getByAltText('Guard Album'))
			.toHaveAttribute('src', `${remoteCover}/small`);
	});
});
