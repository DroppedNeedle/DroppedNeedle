import { page, userEvent } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { createRawSnippet } from 'svelte';
import SoundBench from './SoundBench.svelte';

type Similar = RouteData<'GET /api/v3/library/tracks/{id}/similar'>;

const BENCH = {
	seed: {
		track_id: 'seed',
		title: 'Supernatural',
		artist_name: 'Julie Christmas',
		album_id: 'al-seed',
		album_title: 'Ridiculous and Full of Blood',
		duration_seconds: 216
	},
	model: 'Test model 9',
	analysis_version: 7
};

const station = (
	id: string,
	title: string,
	artist: string,
	distance: number,
	closeness: 'very_close' | 'close' | 'further'
) => ({
	track_id: id,
	title,
	artist_name: artist,
	album_id: `al-${id}`,
	album_title: null,
	duration_seconds: 200,
	distance,
	closeness,
	shared_tags: ['rock']
});

const SIMILAR: Similar = {
	analysis_version: 1,
	tracks: [
		station('ash', 'The Ash', 'Julie Christmas', 0.021, 'very_close'),
		station('king', 'King Park', 'La Dispute', 0.074, 'close'),
		station('mariner', 'Mariner', 'Cult of Luna', 0.128, 'further')
	]
};

const measures = (shift: number) => ({
	measures: Array.from({ length: 200 }, (_, i) => Math.sin(i / 9) * 3 - 1 + shift)
});

describe('the sound bench', () => {
	beforeEach(() => {
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
	});

	afterEach(() => {
		playerStore.stop();
		authStore.clear();
		vi.restoreAllMocks();
	});

	it('sets the similar tracks out as stations at their distances, and tunes by the knob', async () => {
		fakeApi({
			'GET /api/v3/library/tracks/{id}/similar': SIMILAR,
			'GET /api/v3/library/tracks/{id}/sound': ({ params }) =>
				measures(params.id === 'seed' ? 0 : 0.5)
		});
		await renderWithQuery(SoundBench, { bench: BENCH });

		const stations = page.getByRole('group', { name: 'Tracks that sound like Supernatural' });
		await expect.element(stations).toBeVisible();
		const buttons = [...stations.element().querySelectorAll<HTMLElement>('button')];
		expect(buttons.map((b) => b.getAttribute('aria-label'))).toEqual([
			'The Ash by Julie Christmas, very close',
			'King Park by La Dispute, close',
			'Mariner by Cult of Luna, a little like it'
		]);
		expect(buttons.map((b) => Number(b.style.getPropertyValue('--at')).toFixed(3))).toEqual([
			(0.021 / 0.15).toFixed(3),
			(0.074 / 0.15).toFixed(3),
			(0.128 / 0.15).toFixed(3)
		]);
		await expect.element(page.getByText('Sounds very close to Supernatural')).toBeVisible();

		const knob = page.getByRole('slider', { name: 'Tune' });
		await expect.element(knob).toHaveAttribute('aria-valuetext', 'The Ash by Julie Christmas');
		await userEvent.click(knob);
		await userEvent.keyboard('{ArrowRight}');
		await expect.element(knob).toHaveAttribute('aria-valuetext', 'King Park by La Dispute');
		await expect.element(page.getByText('Sounds close to Supernatural')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'King Park by La Dispute, close' }))
			.toHaveAttribute('aria-current', 'true');

		// The meter and the scope are pictures named in words; no distance is printed.
		await expect
			.element(page.getByRole('img', { name: 'King Park: sounds close to Supernatural' }))
			.toBeVisible();
		// The model and version are the server's, as it sent them.
		await expect.element(page.getByText(/Sound model Test model 9/)).toBeVisible();
		await expect.element(page.getByText(/analysis version 7/)).toBeVisible();
		const bench = page.getByRole('region', { name: 'By ear' }).element();
		expect(bench.textContent ?? '').not.toMatch(/0\.\d/);
	});

	it('starts a station grown from the tuned track', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks/{id}/similar': SIMILAR,
			'GET /api/v3/library/tracks/{id}/sound': measures(0),
			'POST /api/v3/discover/radio/plan': {
				title: 'Radio: The Ash',
				tracks: [
					{
						track_name: 'The Ash',
						artist_name: 'Julie Christmas',
						artist_mbid: '',
						in_library: true,
						local_file_id: 'ash'
					}
				]
			}
		});
		vi.spyOn(playerStore, 'playQueue').mockImplementation(() => {});
		await renderWithQuery(SoundBench, { bench: BENCH });

		await page.getByRole('button', { name: 'Start radio' }).click();
		await expect
			.poll(() => api.calls('POST /api/v3/discover/radio/plan').length)
			.toBeGreaterThan(0);
		expect(api.calls('POST /api/v3/discover/radio/plan')[0].body).toMatchObject({
			seed_type: 'track',
			seed_id: 'ash'
		});

		const stop = page.getByRole('button', { name: 'Stop radio' });
		await expect.element(stop).toBeVisible();
		await stop.click();
		await expect.element(page.getByRole('button', { name: 'Start radio' })).toBeVisible();
	});

	it('is not drawn while sound analysis is off', async () => {
		fakeApi({
			'GET /api/v3/library/tracks/{id}/similar': apiError(409, 'SOUND_ANALYSIS_OFF'),
			'GET /api/v3/library/tracks/{id}/sound': apiError(409, 'SOUND_ANALYSIS_OFF')
		});
		const head = createRawSnippet(() => ({ render: () => '<h3>From Your Library</h3>' }));
		await renderWithQuery(SoundBench, { bench: BENCH, head });
		await expect.poll(() => document.querySelector('.dn-bench')).toBeNull();
		await expect.element(page.getByText('From Your Library')).not.toBeInTheDocument();
		await expect.element(page.getByRole('region', { name: 'By ear' })).not.toBeInTheDocument();
	});
});
