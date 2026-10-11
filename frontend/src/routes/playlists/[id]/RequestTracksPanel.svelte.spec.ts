import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { PlaylistDetail, PlaylistTrack } from '$lib/api/playlists';
import type { MuxEventListener, MuxEventStream } from '$lib/queries/events/MuxEventStream';

const h = vi.hoisted(() => ({
	getTrackRequestSettings: vi.fn(),
	requestPlaylistTracks: vi.fn(),
	resolvePlaylistTracks: vi.fn(),
	chooseTrackHome: vi.fn(),
	toast: vi.fn()
}));

vi.mock('$lib/api/playlistTrackRequests', () => ({
	getTrackRequestSettings: h.getTrackRequestSettings,
	requestPlaylistTracks: h.requestPlaylistTracks,
	resolvePlaylistTracks: h.resolvePlaylistTracks,
	chooseTrackHome: h.chooseTrackHome
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));

import RequestTracksPanel from './RequestTracksPanel.svelte';

function fakeEvents() {
	const listeners = new Map<string, Set<MuxEventListener>>();
	const stream: MuxEventStream & { emit: (name: string, data: unknown) => void } = {
		connect: () => {},
		disconnect: () => {},
		isConnected: true,
		onConnect: () => () => {},
		on(name, listener) {
			const set = listeners.get(name) ?? new Set();
			set.add(listener);
			listeners.set(name, set);
			return () => set.delete(listener);
		},
		emit(name, data) {
			const event = new MessageEvent(name, { data: JSON.stringify(data) });
			for (const listener of listeners.get(name) ?? []) listener(event);
		}
	};
	return stream;
}

function track(id: string, extra: Partial<PlaylistTrack> = {}): PlaylistTrack {
	return {
		id,
		position: 0,
		track_name: `Song ${id}`,
		artist_name: 'Creedence Clearwater Revival',
		album_name: 'Chronicle',
		album_id: null,
		artist_id: null,
		track_source_id: null,
		cover_url: null,
		source_type: '',
		available_sources: null,
		format: null,
		track_number: null,
		disc_number: null,
		duration: null,
		created_at: '',
		plex_rating_key: null,
		library_file_id: null,
		...extra
	};
}

function playlist(tracks: PlaylistTrack[]): PlaylistDetail {
	return {
		id: 'pl-1',
		name: 'CCR',
		track_count: tracks.length,
		total_duration: null,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: '',
		updated_at: '',
		is_public: false,
		is_owner: true,
		owner_name: null,
		is_redacted: false,
		tracks
	};
}

const SUMMARY = {
	tracks: 3,
	owned: 1,
	chosen: 0,
	resolved: 2,
	flagged: 0,
	unresolved: 0,
	failed: 0,
	albums: 2,
	albums_by_tier: { album: 1, artist_compilation: 1 }
};

describe('RequestTracksPanel', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		localStorage.clear();
		h.getTrackRequestSettings.mockResolvedValue({
			mode: 'auto',
			compilation_policy: 'match_playlist'
		});
	});

	async function setup(
		tracks = [track('a'), track('b'), track('owned', { available_sources: ['x'] })]
	) {
		const events = fakeEvents();
		const onrefresh = vi.fn();
		const ontrackupdate = vi.fn();
		await render(RequestTracksPanel, {
			props: { playlist: playlist(tracks), onrefresh, ontrackupdate, events }
		});
		return { events, onrefresh, ontrackupdate };
	}

	it('counts only tracks without a source', async () => {
		await setup();
		await expect.element(page.getByText('2 tracks missing')).toBeInTheDocument();
	});

	it('auto mode requests at once and finishes on its own job event', async () => {
		h.requestPlaylistTracks.mockResolvedValue({
			playlist_id: 'pl-1',
			status: 'started',
			job: 'resolve_and_request',
			job_id: 'job-1'
		});
		const { events, onrefresh } = await setup();

		await page.getByRole('button', { name: 'Request all tracks' }).click();
		expect(h.requestPlaylistTracks).toHaveBeenCalledWith('pl-1', 'auto', 'match_playlist');
		await expect.element(page.getByRole('button', { name: 'Working…' })).toBeDisabled();

		// A replayed event from an earlier job must not end this one.
		events.emit('playlist_tracks_requested', {
			playlist_id: 'pl-1',
			job_id: 'old-job',
			result: { message: 'old' }
		});
		await expect.element(page.getByRole('button', { name: 'Working…' })).toBeInTheDocument();
		expect(onrefresh).not.toHaveBeenCalled();

		events.emit('playlist_tracks_requested', {
			playlist_id: 'pl-1',
			job_id: 'job-1',
			summary: { ...SUMMARY, flagged: 1 },
			result: { message: 'Track requests accepted: 2 started', status: 'pending' }
		});
		await expect
			.element(page.getByRole('button', { name: 'Request all tracks' }))
			.toBeInTheDocument();
		expect(onrefresh).toHaveBeenCalledOnce();
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Track requests accepted: 2 started. 1 track need a choice',
			type: 'success'
		});
	});

	it('review mode resolves, shows the album summary, then requests on confirm', async () => {
		h.getTrackRequestSettings.mockResolvedValue({ mode: 'review', compilation_policy: 'flag' });
		h.resolvePlaylistTracks.mockResolvedValue({
			playlist_id: 'pl-1',
			status: 'started',
			job: 'resolve',
			job_id: 'job-r'
		});
		h.requestPlaylistTracks.mockResolvedValue({
			playlist_id: 'pl-1',
			status: 'started',
			job: 'request',
			job_id: 'job-q'
		});
		const { events } = await setup();
		await expect.element(page.getByText('2 tracks missing')).toBeInTheDocument();
		await vi.waitFor(() => expect(h.getTrackRequestSettings).toHaveBeenCalled());

		await page.getByRole('button', { name: 'Request all tracks' }).click();
		expect(h.resolvePlaylistTracks).toHaveBeenCalledWith('pl-1', 'flag');
		events.emit('playlist_tracks_resolved', {
			playlist_id: 'pl-1',
			job_id: 'job-r',
			summary: SUMMARY
		});

		const dialog = page.getByRole('dialog');
		await expect.element(dialog.getByText('2 tracks from 2 albums')).toBeInTheDocument();
		await expect.element(dialog.getByText('Studio albums')).toBeInTheDocument();
		await expect.element(dialog.getByText("Artist's compilations")).toBeInTheDocument();
		await expect.element(dialog.getByText('1 already in your library')).toBeInTheDocument();
		expect(h.requestPlaylistTracks).not.toHaveBeenCalled();

		await dialog.getByRole('button', { name: 'Request' }).click();
		expect(h.requestPlaylistTracks).toHaveBeenCalledWith('pl-1', 'review');
	});

	it('remembers the last choice over the admin defaults', async () => {
		localStorage.setItem(
			'droppedneedle_track_request_choice_anon',
			JSON.stringify({ mode: 'auto', policy: 'studio' })
		);
		h.requestPlaylistTracks.mockResolvedValue({
			playlist_id: 'pl-1',
			status: 'started',
			job: 'resolve_and_request',
			job_id: 'j'
		});
		await setup();

		await page.getByRole('button', { name: 'Request all tracks' }).click();
		expect(h.requestPlaylistTracks).toHaveBeenCalledWith('pl-1', 'auto', 'studio');
		expect(h.getTrackRequestSettings).not.toHaveBeenCalled();
	});

	it('lets the owner pick an album for a flagged track', async () => {
		const flagged = track('f', {
			resolution_status: 'flagged',
			flag_options: [
				{
					kind: 'studio',
					recording_mbid: 'rec-st',
					release_group_mbid: 'rg-willy',
					title: 'Willy and the Poor Boys',
					tier: 'album'
				},
				{
					kind: 'compilation',
					recording_mbid: 'rec-fs',
					release_group_mbid: 'rg-chr',
					title: 'Chronicle',
					tier: 'artist_compilation'
				}
			]
		});
		const chosen = { ...flagged, resolution_status: 'chosen' as const };
		h.chooseTrackHome.mockResolvedValue(chosen);
		const { ontrackupdate } = await setup([flagged]);

		await expect
			.element(page.getByText('1 track is only on compilations: pick an album'))
			.toBeInTheDocument();
		await page.getByRole('button', { name: 'Studio: Willy and the Poor Boys' }).click();

		expect(h.chooseTrackHome).toHaveBeenCalledWith(
			'pl-1',
			'f',
			expect.objectContaining({ recording_mbid: 'rec-st', release_group_mbid: 'rg-willy' })
		);
		await vi.waitFor(() => expect(ontrackupdate).toHaveBeenCalledWith(chosen));
	});

	it('reports a failed job', async () => {
		h.requestPlaylistTracks.mockResolvedValue({
			playlist_id: 'pl-1',
			status: 'started',
			job: 'resolve_and_request',
			job_id: 'j'
		});
		const { events } = await setup();
		await page.getByRole('button', { name: 'Request all tracks' }).click();

		events.emit('playlist_tracks_failed', { playlist_id: 'pl-1', job_id: 'j', message: 'x' });

		await expect
			.element(page.getByRole('button', { name: 'Request all tracks' }))
			.toBeInTheDocument();
		expect(h.toast).toHaveBeenCalledWith({
			message: "Couldn't finish this playlist's tracks",
			type: 'error'
		});
	});
});
