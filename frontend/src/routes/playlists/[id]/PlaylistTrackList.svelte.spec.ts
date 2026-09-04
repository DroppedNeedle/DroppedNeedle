import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import PlaylistTrackList from './PlaylistTrackList.svelte';
import type { PlaylistDetail, PlaylistTrack } from '$lib/api/playlists';

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		isPlaying: false,
		currentQueueItem: null,
		isPlayerVisible: false,
		nowPlaying: null
	}
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('$lib/api/playlists', () => ({
	removeTrackFromPlaylist: vi.fn(),
	removeTracksFromPlaylist: vi.fn(),
	updatePlaylistTrack: vi.fn(),
	reorderPlaylistTrack: vi.fn()
}));

const MBID = '123e4567-e89b-12d3-a456-426614174000';
const GUID = '2e7af1e07ade413fe92c03d06ab9a4c0';

function makeTrack(overrides: Partial<PlaylistTrack> = {}): PlaylistTrack {
	return {
		id: 'trk-1',
		position: 0,
		track_name: 'Test Track',
		artist_name: 'Test Artist',
		album_name: 'Test Album',
		album_id: null,
		artist_id: null,
		track_source_id: 'vid-1',
		cover_url: null,
		source_type: 'local',
		available_sources: ['local'],
		format: 'flac',
		track_number: 1,
		disc_number: null,
		duration: 200,
		created_at: '2026-01-01T00:00:00Z',
		plex_rating_key: null,
		library_file_id: null,
		...overrides
	};
}

function renderList() {
	const playlist: PlaylistDetail = {
		id: 'pl-1',
		name: 'Link Test Playlist',
		track_count: 3,
		total_duration: 600,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-02T00:00:00Z',
		is_public: false,
		is_owner: true,
		owner_name: null,
		is_redacted: false,
		tracks: [
			makeTrack({
				id: 'trk-mbid',
				position: 0,
				track_name: 'Mbid Track',
				album_name: 'Mbid Album',
				album_id: MBID
			}),
			makeTrack({
				id: 'trk-guid',
				position: 1,
				track_name: 'Guid Track',
				album_name: 'Guid Album',
				album_id: GUID
			}),
			makeTrack({
				id: 'trk-none',
				position: 2,
				track_name: 'Unlinked Track',
				album_name: 'Unlinked Album',
				album_id: null
			})
		]
	};
	return render(PlaylistTrackList, {
		props: { playlist, ontrackchange: vi.fn(), readonly: true }
	} as Parameters<typeof render<typeof PlaylistTrackList>>[1]);
}

describe('PlaylistTrackList album links', () => {
	it('links track title and album name for a valid MBID album_id', async () => {
		renderList();
		await expect
			.element(page.getByRole('link', { name: 'Mbid Track' }))
			.toHaveAttribute('href', `/album/${MBID}`);
		await expect
			.element(page.getByRole('link', { name: 'Mbid Album' }))
			.toHaveAttribute('href', `/album/${MBID}`);
	});

	it('renders plain text with no album link for a 32-hex GUID album_id', async () => {
		renderList();
		await expect.element(page.getByText('Guid Track')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Guid Track' })).not.toBeInTheDocument();
		await expect.element(page.getByRole('link', { name: 'Guid Album' })).not.toBeInTheDocument();
	});

	it('renders plain text with no album link when album_id is null', async () => {
		renderList();
		await expect.element(page.getByText('Unlinked Track')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Unlinked Track' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Unlinked Album' }))
			.not.toBeInTheDocument();
	});
});
