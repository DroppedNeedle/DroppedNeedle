import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import PlaylistDiscoveryModal from './PlaylistDiscoveryModal.svelte';

const SUGGEST = 'POST /api/v3/discover/playlist-suggestions';
type Answer = RouteData<typeof SUGGEST>;

function answer(titles: string[], fallback: string | null = null): Answer {
	return {
		playlist_id: 'pl1',
		profile: { artist_mbids: [], genre_distribution: {}, track_count: 30 },
		suggestions: {
			title: 'Suggestions',
			type: 'albums',
			fallback_message: fallback,
			items: titles.map((title) => ({
				kind: 'album' as const,
				title,
				artist_name: 'Someone',
				musicbrainz_id: null,
				in_library: false,
				requested: false
			}))
		}
	};
}

function open() {
	return renderWithQuery(PlaylistDiscoveryModal, {
		open: true,
		playlistId: 'pl1',
		playlistName: 'From Jellyfin'
	});
}

describe('PlaylistDiscoveryModal.svelte', () => {
	it("shows the server's reason when the answer has no albums", async () => {
		fakeApi({ [SUGGEST]: answer([], "The music source you selected isn't set up yet.") });
		await open();
		await expect
			.element(page.getByText("The music source you selected isn't set up yet."))
			.toBeVisible();
		await expect.element(page.getByText(/No more suggestions left/)).not.toBeInTheDocument();
	});

	it("shows the server's reason, with no retry, when the playlist has no artists to go on", async () => {
		const reason = 'This playlist has no artist data to base suggestions on';
		fakeApi({ [SUGGEST]: apiError(400, 'INVALID_INPUT', reason) });
		await open();
		await expect.element(page.getByText(reason)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Try Again' })).not.toBeInTheDocument();
	});

	it('says no more suggestions are left once every result is ignored', async () => {
		fakeApi({ [SUGGEST]: answer(['First', 'Second']) });
		await open();
		await expect.element(page.getByText('First')).toBeVisible();
		await page.getByLabelText('Ignore suggestion').first().click();
		await page.getByLabelText('Ignore suggestion').first().click();
		await expect.element(page.getByText(/No more suggestions left/)).toBeVisible();
	});
});
