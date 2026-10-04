import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { CrateTrack } from '$lib/types';
import Crate from './Crate.svelte';

const tracks: CrateTrack[] = [
	{
		track_file_id: 't1',
		title: 'Opener',
		album_name: 'First Light',
		artist_name: 'Aurora',
		album_mbid: 'rg1',
		cover_url: '/api/v3/covers/release-group/rg1?size=250',
		format: 'flac',
		year: 1994,
		duration_seconds: 200,
		reason: 'recent'
	},
	{
		track_file_id: 't2',
		title: 'Deep Cut',
		album_name: 'Lone Peak',
		artist_name: 'Boreal',
		album_mbid: null,
		cover_url: null,
		reason: 'surprise'
	}
];

async function renderCrate(
	overrides: Partial<{
		tracks: CrateTrack[];
		onPlay: (t: CrateTrack) => void;
		onQueue: (t: CrateTrack) => void;
	}> = {}
) {
	return await render(Crate, {
		tracks,
		isLoading: false,
		reducedMotion: true,
		onRefresh: vi.fn(),
		onPlay: vi.fn(),
		onQueue: vi.fn(),
		...overrides
	});
}

describe('Crate.svelte', () => {
	it('renders crate tracks with v3 covers', async () => {
		await renderCrate();
		await expect.element(page.getByText('Opener')).toBeInTheDocument();
		await expect.element(page.getByText('Deep Cut')).toBeInTheDocument();
		await expect
			.element(page.getByRole('img').first())
			.toHaveAttribute('src', '/api/v3/covers/release-group/rg1?size=250');
	});

	it('plays and queues through the callbacks', async () => {
		const onPlay = vi.fn();
		const onQueue = vi.fn();
		await renderCrate({ onPlay, onQueue });
		await (await page.getByRole('button', { name: 'Play now', exact: true }).all())[0].click();
		expect(onPlay).toHaveBeenCalledWith(expect.objectContaining({ track_file_id: 't1' }));
		await (await page.getByRole('button', { name: 'Add to queue', exact: true }).all())[0].click();
		expect(onQueue).toHaveBeenCalledWith(expect.objectContaining({ track_file_id: 't1' }));
	});
});
