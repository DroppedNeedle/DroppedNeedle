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
import { launchLocalPlayback } from '$lib/player/launchLocalPlayback';
import { getCoverUrl } from '$lib/utils/errorHandling';
import { playSourceTrack } from '../../../routes/album/[id]/albumPlaybackHandlers';
import type { LocalTrackInfo } from '$lib/types';

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
		await render(Turntable, callbacks);

		const toggle = page.getByLabelText('Toggle lyrics');
		await expect.element(toggle).toBeInTheDocument();
		await toggle.click();

		await expect.element(page.getByRole('dialog', { name: 'Lyrics' })).toBeInTheDocument();
		await expect.element(page.getByText('First line')).toBeInTheDocument();
	});

	it('hides the lyrics action when the local file has none', async () => {
		mockQueryState.data = null;
		playLocalTrack();
		await render(Turntable, callbacks);

		await expect.element(page.getByLabelText('Toggle lyrics')).not.toBeInTheDocument();
	});
});

describe('Turntable deck cover', () => {
	const deckMbid = 'b1392450-e666-3926-a536-22c65f834433';
	const proxyCover = `/api/v1/covers/release-group/${deckMbid}?size=250`;
	const localCover = `/api/v1/library/albums/${deckMbid}/artwork/local`;
	const remoteCover = 'https://r2.theaudiodb.com/images/media/album/thumb/abc123.jpg';

	function playDeckTrack(overrides: {
		coverUrl?: string | null;
		coverRemoteUrl?: string | null;
		sourceType?: 'local' | 'navidrome';
	}) {
		playerStore.playQueue([
			{
				trackSourceId: 'file-9',
				trackName: 'Deck Song',
				artistName: 'Deck Artist',
				trackNumber: 1,
				albumId: deckMbid,
				albumName: 'Guard Album',
				coverUrl: localCover,
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

	it('renders stable local album artwork when the queued remote cover is a local path', async () => {
		playDeckTrack({ coverRemoteUrl: proxyCover });
		await render(Turntable, callbacks);

		await expect
			.element(page.getByAltText('Guard Album'))
			.toHaveAttribute('src', `/api/v1/library/albums/${deckMbid}/artwork/local`);
	});

	it('preserves provider artwork when local audio has no explicit local album identity', async () => {
		playDeckTrack({ coverUrl: remoteCover, coverRemoteUrl: remoteCover });
		await render(Turntable, callbacks);
		await expect
			.element(page.getByAltText('Guard Album'))
			.toHaveAttribute('src', `${remoteCover}/small`);
	});

	it('keeps provider-detail identity through the actual local launcher and Turntable', async () => {
		const track: LocalTrackInfo = {
			track_file_id: 'file-9',
			title: 'Song',
			track_number: 1,
			disc_number: 1,
			size_bytes: 1,
			format: 'FLAC'
		};
		playSourceTrack(
			'local',
			1,
			1,
			'Song',
			{
				musicbrainz_id: deckMbid,
				title: 'Provider detail',
				artist_name: 'Artist',
				artist_id: 'artist',
				in_library: true,
				cover_url: null
			},
			null,
			{
				found: true,
				musicbrainz_id: deckMbid,
				tracks: [track],
				total_size_bytes: 1,
				primary_format: 'FLAC'
			},
			null,
			null,
			[]
		);
		await vi.waitFor(() => expect(playerStore.nowPlaying?.sourceType).toBe('local'));
		expect(playerStore.nowPlaying?.albumId).toBe(deckMbid);
		await render(Turntable, callbacks);
		await expect.element(page.getByAltText('Provider detail')).toHaveAttribute('src', proxyCover);
	});

	it('keeps explicit native DTO artwork through launcher and repeated player normalization', async () => {
		const track: LocalTrackInfo = {
			track_file_id: 'file-9',
			title: 'Song',
			track_number: 1,
			disc_number: 1,
			size_bytes: 1,
			format: 'FLAC'
		};
		launchLocalPlayback([track], 0, false, {
			albumId: deckMbid,
			albumName: 'Native DTO album',
			artistName: 'Artist',
			coverUrl: localCover
		});
		await vi.waitFor(() => expect(playerStore.nowPlaying?.albumId).toBe(deckMbid));
		const np = playerStore.nowPlaying!;
		expect(getCoverUrl(getCoverUrl(np.coverUrl, np.albumId), np.albumId)).toBe(localCover);
		await render(Turntable, callbacks);
		await expect.element(page.getByAltText('Native DTO album')).toHaveAttribute('src', localCover);
	});

	it('preserves remote artwork for provider playback', async () => {
		playDeckTrack({ coverUrl: remoteCover, coverRemoteUrl: remoteCover, sourceType: 'navidrome' });
		await render(Turntable, callbacks);

		await expect
			.element(page.getByAltText('Guard Album'))
			.toHaveAttribute('src', `${remoteCover}/small`);
	});
});
