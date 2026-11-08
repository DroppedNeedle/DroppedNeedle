import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';

import { playerStore } from '$lib/features/player/state/player.svelte';
import { fakeApi } from '$lib/test/api';
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
