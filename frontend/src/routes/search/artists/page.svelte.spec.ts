import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { resetQueryCacheForUserSwitch } from '$lib/queries/QueryClient';
import ArtistSearchPageTestHarness from './ArtistSearchPageTestHarness.svelte';

const originalFetch = globalThis.fetch;

function jsonResponse(body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'content-type': 'application/json' }
	});
}

function v3Artist(title: string, id: string) {
	return {
		kind: 'artist',
		id: `local-${id}`,
		title,
		musicbrainz_id: id,
		in_library: false,
		requested: false,
		score: 80
	};
}

function bucketResponse(results: unknown[], offset = 0, status = 'ok') {
	return {
		bucket: 'artists',
		limit: 24,
		offset,
		results,
		top_result: null,
		status
	};
}

describe('dedicated artist search', () => {
	beforeEach(async () => {
		await resetQueryCacheForUserSwitch();
		authStore.setUser({
			id: 'artist-search-user',
			display_name: 'Artist Search User',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'artist-search-user',
			username_display: 'Artist Search User',
			providers: ['local']
		});
	});

	afterEach(async () => {
		globalThis.fetch = originalFetch;
		await resetQueryCacheForUserSwitch();
		authStore.clear();
	});

	it('replaces stale results on retry', async () => {
		let firstPageCalls = 0;
		const liveFirstPage = [
			v3Artist('Shared Artist', 'shared'),
			...Array.from({ length: 23 }, (_, index) =>
				v3Artist(`Live Artist ${index + 1}`, `live-${index + 1}`)
			)
		];

		globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (!url.startsWith('/api/v3/search/artists?')) {
				throw new Error(`Unexpected request: ${url}`);
			}
			if (url.includes('offset=24')) {
				return jsonResponse(bucketResponse([], 24));
			}

			firstPageCalls += 1;
			if (firstPageCalls === 1) {
				return jsonResponse(
					bucketResponse(
						[v3Artist('Shared Artist', 'shared'), v3Artist('Removed Cached Artist', 'cached-only')],
						0,
						'stale'
					)
				);
			}
			return jsonResponse(bucketResponse(liveFirstPage));
		}) as typeof fetch;

		await render(ArtistSearchPageTestHarness, { data: { query: 'muse' } });

		await expect.element(page.getByText('Removed Cached Artist')).toBeInTheDocument();
		await expect
			.element(
				page.getByText("MusicBrainz is unavailable, so we're showing cached artist results.")
			)
			.toBeInTheDocument();

		await page.getByRole('button', { name: 'Retry' }).click();

		await expect.element(page.getByText('Live Artist 23')).toBeInTheDocument();
		await expect.element(page.getByText('Removed Cached Artist')).not.toBeInTheDocument();
	});

	it('trims whitespace before the dedicated provider request', async () => {
		const requests: string[] = [];
		globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			requests.push(url);
			if (!url.startsWith('/api/v3/search/artists?')) {
				throw new Error(`Unexpected request: ${url}`);
			}
			return jsonResponse(bucketResponse([v3Artist('Muse', 'muse')]));
		}) as typeof fetch;

		await render(ArtistSearchPageTestHarness, { data: { query: '  Muse  ' } });

		await expect.element(page.getByText('Muse')).toBeInTheDocument();
		expect(requests[0]).toContain('q=Muse');
		expect(requests[0]).not.toContain('%20');
	});
});
