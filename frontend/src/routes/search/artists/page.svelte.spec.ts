import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { searchStore } from '$lib/stores/search';
import ArtistSearchPage from './+page.svelte';

const originalFetch = globalThis.fetch;

function jsonResponse(body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'content-type': 'application/json' }
	});
}

function artist(title: string, id: string) {
	return {
		type: 'artist',
		title,
		musicbrainz_id: id,
		in_library: false,
		score: 80
	};
}

describe('dedicated artist search', () => {
	beforeEach(() => searchStore.clear());

	afterEach(() => {
		globalThis.fetch = originalFetch;
		searchStore.clear();
	});

	it('replaces stale results on retry', async () => {
		let firstPageCalls = 0;
		const liveFirstPage = [
			artist('Shared Artist', 'shared'),
			...Array.from({ length: 23 }, (_, index) =>
				artist(`Live Artist ${index + 1}`, `live-${index + 1}`)
			)
		];

		globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (!url.startsWith('/api/v1/search/artists?')) {
				throw new Error(`Unexpected request: ${url}`);
			}
			if (url.includes('offset=24')) {
				return jsonResponse({
					bucket: 'artists',
					limit: 24,
					offset: 24,
					results: [],
					top_result: null,
					status: 'ok'
				});
			}

			firstPageCalls += 1;
			if (firstPageCalls === 1) {
				return jsonResponse({
					bucket: 'artists',
					limit: 24,
					offset: 0,
					results: [
						artist('Shared Artist', 'shared'),
						artist('Removed Cached Artist', 'cached-only')
					],
					top_result: null,
					status: 'stale'
				});
			}
			return jsonResponse({
				bucket: 'artists',
				limit: 24,
				offset: 0,
				results: liveFirstPage,
				top_result: null,
				status: 'ok'
			});
		}) as typeof fetch;

		render(ArtistSearchPage, { data: { query: 'muse' } });

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
			if (!url.startsWith('/api/v1/search/artists?')) {
				throw new Error(`Unexpected request: ${url}`);
			}
			return jsonResponse({
				bucket: 'artists',
				limit: 24,
				offset: 0,
				results: [artist('Muse', 'muse')],
				top_result: null,
				status: 'ok'
			});
		}) as typeof fetch;

		render(ArtistSearchPage, { data: { query: '  Muse  ' } });

		await expect.element(page.getByText('Muse')).toBeInTheDocument();
		expect(requests[0]).toContain('q=Muse');
		expect(requests[0]).not.toContain('%20');
	});
});
