import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { NativeTrackListItem } from '$lib/types';
import { Download, ListPlus } from 'lucide-svelte';

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { playQueue: vi.fn() }
}));

import LocalAlbumTrackList from './LocalAlbumTrackList.svelte';
import { closeAllMenus, type MenuItem } from '$lib/components/ContextMenu.svelte';

function track(
	id: string,
	title: string,
	artistId: string,
	artistName: string,
	fileSizeBytes = 1000
): NativeTrackListItem {
	return {
		id,
		title,
		album_id: 'compilation-1',
		album_title: 'Night Signals',
		artist_id: artistId,
		artist_name: artistName,
		album_artist_id: 'various-artists',
		album_artist_name: 'Various Artists',
		musicbrainz_recording_id: null,
		musicbrainz_release_group_id: null,
		musicbrainz_artist_id: null,
		musicbrainz_album_artist_id: null,
		disc_number: 1,
		track_number: id === 'track-1' ? 1 : 2,
		year: 2026,
		genre: 'Electronic',
		duration_seconds: 180,
		format: 'flac',
		bit_rate: null,
		sample_rate: 44100,
		bit_depth: 16,
		channels: 2,
		file_size_bytes: fileSizeBytes,
		date_added: 1,
		cover_available: false,
		current_tier: 'lossless',
		below_cutoff: false
	};
}

describe('LocalAlbumTrackList', () => {
	it('keeps compilation track credits linked to their stable local artists', async () => {
		await render(LocalAlbumTrackList, {
			props: {
				tracks: [
					track('track-1', 'Northbound', 'artist-north', 'North Signal'),
					track('track-2', 'Southbound', 'artist-south', 'South Signal')
				]
			}
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('link', { name: 'North Signal' }))
			.toHaveAttribute('href', '/artist/artist-north');
		await expect
			.element(page.getByRole('link', { name: 'South Signal' }))
			.toHaveAttribute('href', '/artist/artist-south');
	});

	it('uses the MusicBrainz artist route when the track credit is linked', async () => {
		const linked = track('track-1', 'Northbound', 'artist-north', 'North Signal');
		linked.musicbrainz_artist_id = 'provider-north';
		await render(LocalAlbumTrackList, {
			props: { tracks: [linked] }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('link', { name: 'North Signal' }))
			.toHaveAttribute('href', '/artist/provider-north');
	});

	it('shows formatBytes sizes per row', async () => {
		expect.assertions(2);
		await render(LocalAlbumTrackList, {
			props: {
				tracks: [
					track('track-1', 'Northbound', 'artist-north', 'North Signal', 12582912),
					track('track-2', 'Southbound', 'artist-south', 'South Signal', 20971520)
				]
			}
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('12 MB')).toBeVisible();
		await expect.element(page.getByText('20 MB')).toBeVisible();
	});

	it('shows the absence marker for zero-byte rows', async () => {
		expect.assertions(2);
		await render(LocalAlbumTrackList, {
			props: { tracks: [track('track-1', 'Northbound', 'artist-north', 'North Signal', 0)] }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('Northbound')).toBeVisible();
		expect(page.getByText('—', { exact: true }).elements()).toHaveLength(1);
	});

	it('mounts one menu per row from the builder and clicks through', async () => {
		expect.assertions(4);
		closeAllMenus();
		const download = vi.fn();
		const builder = (t: NativeTrackListItem): MenuItem[] => [
			{ label: 'Add to Queue', icon: ListPlus, onclick: vi.fn() },
			{ label: 'Download', icon: Download, onclick: () => download(t.id) }
		];
		await render(LocalAlbumTrackList, {
			props: {
				tracks: [
					track('track-1', 'Northbound', 'artist-north', 'North Signal'),
					track('track-2', 'Southbound', 'artist-south', 'South Signal')
				],
				getTrackMenuItems: builder
			}
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('Northbound')).toBeVisible();
		const triggers = await page.getByLabelText('More actions').all();
		expect(triggers).toHaveLength(2);

		await triggers[1].click();
		await expect.element(page.getByRole('menuitem', { name: 'Download' })).toBeVisible();
		await page.getByRole('menuitem', { name: 'Download' }).click();
		expect(download).toHaveBeenCalledWith('track-2');
	});

	it('renders no menu without a builder', async () => {
		expect.assertions(2);
		await render(LocalAlbumTrackList, {
			props: { tracks: [track('track-1', 'Northbound', 'artist-north', 'North Signal')] }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('Northbound')).toBeVisible();
		expect(page.getByLabelText('More actions').elements()).toHaveLength(0);
	});
});
