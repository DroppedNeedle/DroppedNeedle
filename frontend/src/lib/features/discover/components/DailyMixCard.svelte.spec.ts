import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { DailyMix, MixTrack, MixTrackKind } from '../queries/types';
import DailyMixCard from './DailyMixCard.svelte';

function track(title: string, kind: MixTrackKind): MixTrack {
	const library = kind !== 'new';
	return {
		kind,
		title,
		artist_name: `By ${title}`,
		artist_mbid: `ar-${title}`,
		local_track_id: library ? `t-${title}` : undefined,
		duration_seconds: library ? 200 : undefined,
		album: {
			title: `Album ${title}`,
			musicbrainz_id: `rg-${title}`,
			in_library: library,
			requested: false
		},
		reason: library
			? undefined
			: { kind: 'your_artists', line: 'Close to Circa Survive.', artists: [] }
	};
}

function mix(tracks: MixTrack[], yours: number, fresh: number): DailyMix {
	return {
		id: 'u1:0',
		name: 'Circa Survive, La Dispute and more',
		artists: [],
		day: '2026-11-28',
		yours,
		new: fresh,
		tracks
	};
}

async function open() {
	await page.getByRole('button', { name: /Circa Survive, La Dispute and more/ }).click();
	return page.getByRole('list', { name: /in order/ });
}

describe('DailyMixCard.svelte', () => {
	it('says how much is yours and how much is new', async () => {
		fakeApi({});
		await renderWithQuery(DailyMixCard, {
			mix: mix([track('One', 'artist'), track('Two', 'new')], 28, 12)
		});
		await expect.element(page.getByText('28 yours, 12 new')).toBeVisible();
	});

	it('says only yours when nothing is new', async () => {
		fakeApi({});
		await renderWithQuery(DailyMixCard, { mix: mix([track('One', 'artist')], 40, 0) });
		await expect.element(page.getByText('40 yours', { exact: true })).toBeVisible();
	});

	it('marks only the tracks found by sound', async () => {
		fakeApi({});
		await renderWithQuery(DailyMixCard, {
			mix: mix([track('Own', 'artist'), track('Heard', 'sound'), track('Fresh', 'new')], 2, 1)
		});
		const list = await open();
		const row = (title: string) => list.getByRole('listitem').filter({ hasText: title });
		await expect.element(row('Heard').getByText('Found by sound')).toBeVisible();
		await expect.element(row('Own').getByText('Found by sound')).not.toBeInTheDocument();
		await expect.element(row('Fresh').getByText('Found by sound')).not.toBeInTheDocument();
		await expect.element(row('Fresh').getByText('Close to Circa Survive.')).toBeVisible();
		await expect
			.element(row('Fresh').getByRole('button', { name: 'Keep Album Fresh' }))
			.toBeVisible();
	});

	it('carries no sound mark on a mix built with analysis off', async () => {
		fakeApi({});
		await renderWithQuery(DailyMixCard, {
			mix: mix([track('Own', 'artist'), track('Other', 'artist')], 2, 0)
		});
		await open();
		await expect.element(page.getByText('Found by sound')).not.toBeInTheDocument();
	});
});
