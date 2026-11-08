import { afterEach, beforeEach, describe, it, vi } from 'vitest';

import { playerStore } from '$lib/features/player/state/player.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { dragSeekBar, expectSeekNear, playSeekableTrack, seekBar } from '$lib/test/seekBar';
import Turntable from './Turntable.svelte';

const noop = () => {};

describe("the deck's seek bar", () => {
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
		await renderWithQuery(Turntable, {
			stats: null,
			reducedMotion: true,
			onDropPlay: noop,
			onDropAlbum: noop,
			onPlayAll: noop,
			onShuffleAll: noop,
			onSurprise: noop,
			onOpenQueue: noop
		});
		await dragSeekBar(await seekBar(), 0.75);
		await expectSeekNear(150);
	});
});
