import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';

import { launchRadio } from '$lib/features/player/playback/launchRadio';
import type { QueueItem } from '$lib/features/player/playback/types';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type FakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { dragSeekBar, expectSeekNear, playSeekableTrack, seekBar } from '$lib/test/seekBar';
import Player from './Player.svelte';

describe("the player bar's seek bar", () => {
	beforeEach(async () => {
		await playSeekableTrack();
		fakeApi({
			'GET /api/v3/library/tracks/{id}/lyrics': { is_synced: false, lines: [], text: '' }
		});
	});

	afterEach(() => {
		playerStore.stop();
		vi.restoreAllMocks();
	});

	it('seeks to where a drag lets go', async () => {
		await renderWithQuery(Player, {});
		await dragSeekBar(await seekBar(), 0.75);
		await expectSeekNear(150);
	});

	it('seeks on a click and on the arrow keys', async () => {
		await renderWithQuery(Player, {});
		const bar = await seekBar();
		const { width, height } = bar.getBoundingClientRect();
		await userEvent.click(bar, { position: { x: width / 2, y: height / 2 } } as never);
		await expectSeekNear(100);

		vi.mocked(playerStore.seekTo).mockClear();
		await userEvent.keyboard('{ArrowRight}');
		await expect.poll(() => vi.mocked(playerStore.seekTo).mock.calls.length).toBe(1);
		expect(vi.mocked(playerStore.seekTo).mock.calls[0][0]).toBeGreaterThan(100);
	});
});

describe('a station in the player bar', () => {
	const GROUP = '11111111-2222-4333-8444-555555555555';
	const owned = (n: number) => ({
		track_name: `Song ${n}`,
		artist_name: 'Julie Christmas',
		artist_mbid: '',
		in_library: true,
		local_file_id: `t${n}`,
		file_format: 'mp3',
		duration_s: 200,
		reason: {
			kind: 'sounds_like' as const,
			line: 'Sounds close to Supernatural',
			seed_name: 'Supernatural',
			seed_track_id: 'seed',
			closeness: 'close' as const
		}
	});
	let api: FakeApi;

	beforeEach(async () => {
		await playSeekableTrack();
		api = fakeApi({
			'GET /api/v3/library/tracks/{id}/lyrics': { is_synced: false, lines: [], text: '' },
			'GET /api/v3/library/tracks/{id}/loudness': {},
			'GET /api/v3/library/tracks': { total: 0, next: null, items: [] },
			'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: {} },
			'POST /api/v3/discover/radio/plan': { title: 'Radio', tracks: [owned(1), owned(2)] },
			'POST /api/v3/requests/albums': {
				success: true,
				status: 'queued',
				message: 'Requested',
				musicbrainz_id: GROUP
			}
		});
		expect(await launchRadio({ seed_type: 'track', seed_id: 'seed' }, false)).toBe(true);
		await expect.poll(() => playerStore.playbackState).toBe('playing');
	});

	afterEach(() => {
		playerStore.stop();
		authStore.clear();
		vi.restoreAllMocks();
	});

	it('says why the track is on and steers by it', async () => {
		await renderWithQuery(Player, {});
		await expect.element(page.getByText('Sounds close to Supernatural')).toBeVisible();
		await expect.element(page.getByText('Found by sound')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Keep' })).not.toBeInTheDocument();

		await page.getByRole('button', { name: 'More like this' }).click();
		await expect
			.poll(() => api.calls('POST /api/v3/discover/radio/plan').length)
			.toBeGreaterThan(1);
		expect(api.calls('POST /api/v3/discover/radio/plan').at(-1)?.body).toMatchObject({
			seed_type: 'track',
			seed_id: 'seed',
			more_like: ['t1']
		});
	});

	it('skips a track said Not this to and keeps it off the station', async () => {
		const next = vi.spyOn(playerStore, 'nextTrack');
		await renderWithQuery(Player, {});
		await page.getByRole('button', { name: 'Not this' }).click();
		expect(next).toHaveBeenCalled();
		await expect
			.poll(() => api.calls('POST /api/v3/discover/radio/plan').length)
			.toBeGreaterThan(1);
		expect(api.calls('POST /api/v3/discover/radio/plan').at(-1)?.body).toMatchObject({
			not_this: ['t1']
		});
	});

	it('keeps an unowned track by asking for its album, from the radio', async () => {
		const unowned: QueueItem = {
			trackSourceId: 'vid',
			trackName: 'Hudsonville, MI 1956',
			artistName: 'La Dispute',
			trackNumber: 0,
			albumId: GROUP,
			albumName: 'Wildlife',
			coverUrl: null,
			sourceType: 'youtube',
			queueOrigin: 'context',
			playlistTrackId: 'radio:la dispute|hudsonville, mi 1956'
		};
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Ada',
				role: 'user',
				email: null,
				avatar_url: null,
				username: 'ada',
				username_display: 'Ada'
			})
		);
		vi.spyOn(playerStore, 'currentQueueItem', 'get').mockReturnValue(unowned);
		await renderWithQuery(Player, {});

		await page.getByRole('button', { name: 'Keep' }).click();
		await expect.element(page.getByRole('button', { name: 'Requested' })).toBeDisabled();
		await expect.poll(() => api.calls('POST /api/v3/requests/albums').length).toBe(1);
		expect(api.calls('POST /api/v3/requests/albums')[0].body).toMatchObject({
			musicbrainz_id: GROUP,
			album: 'Wildlife',
			made_from: 'radio'
		});
	});

	it('keeps an unowned song with no known album by asking for the song', async () => {
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Ada',
				role: 'user',
				email: null,
				avatar_url: null,
				username: 'ada',
				username_display: 'Ada'
			})
		);
		api.use({ 'POST /api/v3/requests/tracks': { status: 'queued' } });
		const song: QueueItem = {
			trackSourceId: 'vid',
			trackName: 'King Park',
			artistName: 'La Dispute',
			trackNumber: 0,
			albumId: '',
			albumName: '',
			coverUrl: null,
			sourceType: 'youtube',
			recordingIds: { youtube: '22222222-3333-4444-8555-666666666666' },
			queueOrigin: 'context',
			playlistTrackId: 'radio:la dispute|king park'
		};
		vi.spyOn(playerStore, 'currentQueueItem', 'get').mockReturnValue(song);
		await renderWithQuery(Player, {});

		await page.getByRole('button', { name: 'Keep' }).click();
		await expect.element(page.getByRole('button', { name: 'Requested' })).toBeDisabled();
		await expect.poll(() => api.calls('POST /api/v3/requests/tracks').length).toBe(1);
		expect(api.calls('POST /api/v3/requests/tracks')[0].body).toMatchObject({
			recording_mbid: '22222222-3333-4444-8555-666666666666',
			track_title: 'King Park'
		});
		// The song request route has no field for where it came from.
		expect(api.calls('POST /api/v3/requests/tracks')[0].body).not.toHaveProperty('made_from');
	});

	it('shows nothing of the station for a track queued beside it', async () => {
		const queued: QueueItem = {
			trackSourceId: 't9',
			trackName: 'Queued by hand',
			artistName: 'Someone',
			trackNumber: 1,
			albumId: 'album',
			albumName: 'Album',
			coverUrl: null,
			sourceType: 'local',
			reason: {
				kind: 'sounds_like',
				line: 'Sounds close to Supernatural',
				seed_name: 'Supernatural',
				seed_track_id: 'seed'
			}
		};
		vi.spyOn(playerStore, 'currentQueueItem', 'get').mockReturnValue(queued);
		await renderWithQuery(Player, {});
		await expect.element(page.getByRole('button', { name: 'Close player' })).toBeVisible();
		await expect.element(page.getByText('Sounds close to Supernatural')).not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: 'More like this' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Not this' })).not.toBeInTheDocument();
	});
});
