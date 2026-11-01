import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import SpotifyPlaylists from './+page.svelte';

describe('Spotify playlists while Spotify is limiting requests', () => {
	it('says when to try again', async () => {
		fakeApi({
			'GET /api/v3/acquire/spotify/playlists': apiError(
				503,
				'PROVIDER_UNAVAILABLE',
				'An outside service is not answering. Try again later.',
				{ provider: 'spotify', retry_after: 30 }
			)
		});
		await renderWithQuery(SpotifyPlaylists, {});

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Spotify is not answering. Try again in 30 seconds.');
	});
});
