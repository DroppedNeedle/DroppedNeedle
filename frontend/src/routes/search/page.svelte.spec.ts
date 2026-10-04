import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { resetQueryCacheForUserSwitch } from '$lib/queries/QueryClient';
import SearchPageTestHarness from './SearchPageTestHarness.svelte';

const originalFetch = globalThis.fetch;

function jsonResponse(body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'content-type': 'application/json' }
	});
}

function v3Artist(title: string, id: string, mbid: string | null = id) {
	return {
		kind: 'artist',
		id: `local-${id}`,
		title,
		musicbrainz_id: mbid,
		in_library: false,
		requested: false,
		score: 90
	};
}

function unifiedResponse(
	artists: unknown[] = [],
	albums: unknown[] = [],
	overrides: Record<string, unknown> = {}
) {
	return {
		artists,
		albums,
		tracks: [],
		top_artist: null,
		top_album: null,
		top_track: null,
		artist_status: 'ok',
		album_status: 'ok',
		track_status: 'ok',
		...overrides
	};
}

describe('search result enrichment demand', () => {
	beforeEach(async () => {
		await resetQueryCacheForUserSwitch();
		authStore.setUser({
			id: 'search-user',
			display_name: 'Search User',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'search-user',
			username_display: 'Search User',
			providers: ['local']
		});
	});
	afterEach(async () => {
		globalThis.fetch = originalFetch;
		await resetQueryCacheForUserSwitch();
		authStore.clear();
	});

	it('renders usable primary results without enrichment traffic, then enriches on intent', async () => {
		const mockFetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			void init;
			const url = String(input);
			if (url.startsWith('/api/v3/search?')) {
				return jsonResponse(unifiedResponse([v3Artist('Muse', 'artist-1')]));
			}
			if (url === '/api/v3/search/enrich/batch') {
				return jsonResponse({
					artists: [{ musicbrainz_id: 'artist-1', listen_count: 100 }],
					albums: [],
					source: 'listenbrainz',
					degradations: []
				});
			}
			throw new Error(`Unexpected request: ${url}`);
		});
		globalThis.fetch = mockFetch as typeof fetch;

		await render(SearchPageTestHarness, { data: { query: 'muse' } });
		await expect.element(page.getByText('Muse')).toBeInTheDocument();
		await new Promise((resolve) => setTimeout(resolve, 250));

		const enrichmentCalls = () =>
			mockFetch.mock.calls.filter(([input]) => String(input) === '/api/v3/search/enrich/batch');
		expect(enrichmentCalls()).toHaveLength(0);
		expect(mockFetch).toHaveBeenCalledTimes(1);
		const searchCall = String(mockFetch.mock.calls[0][0]);
		expect(searchCall).toContain('limit_artists=6');
		expect(searchCall).toContain('limit_albums=24');

		await page.getByText('Muse').hover();
		await vi.waitFor(() => expect(enrichmentCalls()).toHaveLength(1));
		const posted = JSON.parse(String(enrichmentCalls()[0][1]?.body ?? '{}')) as {
			artists?: { musicbrainz_id?: string }[];
		};
		expect(posted.artists?.[0]?.musicbrainz_id).toBe('artist-1');
	});

	it('shows the provider notice when the artist bucket degrades', async () => {
		let finishSearch: ((response: Response) => void) | undefined;
		const mockFetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.startsWith('/api/v3/search?')) {
				return new Promise<Response>((resolve) => {
					finishSearch = resolve;
				});
			}
			throw new Error(`Unexpected request: ${url}`);
		});
		globalThis.fetch = mockFetch as typeof fetch;

		await render(SearchPageTestHarness, { data: { query: 'muse' } });

		await expect.element(page.getByLabelText('Loading top search results')).toBeInTheDocument();
		// The persisted-cache restore holds isFetching before the fetch
		// fires, so wait for the request itself before answering it.
		await vi.waitFor(() => expect(mockFetch).toHaveBeenCalled());

		finishSearch?.(
			jsonResponse(unifiedResponse([v3Artist('Muse', 'artist-1')], [], { artist_status: 'error' }))
		);
		await expect
			.element(page.getByText(/MusicBrainz artist search is temporarily unavailable/))
			.toBeInTheDocument();
		await expect.element(page.getByText('Muse')).toBeInTheDocument();
		await expect
			.element(page.getByLabelText('Artist search results'))
			.toHaveAttribute('aria-busy', 'false');
		await expect.element(page.getByLabelText('Loading top search results')).not.toBeInTheDocument();
	});

	it('keeps cached remote results visible during an outage', async () => {
		globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.startsWith('/api/v3/search?')) {
				return jsonResponse(
					unifiedResponse([v3Artist('Cached Muse', 'cached-artist')], [], {
						artist_status: 'stale'
					})
				);
			}
			throw new Error(`Unexpected request: ${url}`);
		}) as typeof fetch;

		await render(SearchPageTestHarness, { data: { query: 'cached muse' } });

		await expect.element(page.getByText('Cached Muse')).toBeInTheDocument();
		await expect
			.element(
				page.getByText(/showing cached artist results alongside any matches in your library/)
			)
			.toBeInTheDocument();
		await expect.element(page.getByText('No artists found')).not.toBeInTheDocument();
	});

	it('renders the top hit plus five artist cards from the six-row fetch', async () => {
		const rows = Array.from({ length: 6 }, (_, index) => v3Artist(`Artist ${index + 1}`, `a${index + 1}`));
		globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.startsWith('/api/v3/search?')) {
				return jsonResponse(unifiedResponse(rows, [], { top_artist: rows[0] }));
			}
			throw new Error(`Unexpected request: ${url}`);
		}) as typeof fetch;

		await render(SearchPageTestHarness, { data: { query: 'artist' } });

		for (const title of ['Artist 1', 'Artist 2', 'Artist 3', 'Artist 4', 'Artist 5', 'Artist 6']) {
			await expect.element(page.getByText(title)).toBeInTheDocument();
		}
		const searchCall = String(
			(globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0]
		);
		expect(searchCall).toContain('limit_artists=6');
	});

	it('keeps a top result visible when it falls outside the first six results', async () => {
		const rows = Array.from({ length: 6 }, (_, index) => v3Artist(`Artist ${index + 1}`, `a${index + 1}`));
		const topResult = v3Artist('Top Result Artist', 'top-result-artist');
		globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.startsWith('/api/v3/search?')) {
				return jsonResponse(unifiedResponse(rows, [], { top_artist: topResult }));
			}
			throw new Error(`Unexpected request: ${url}`);
		}) as typeof fetch;

		await render(SearchPageTestHarness, { data: { query: 'top result' } });

		await expect.element(page.getByText('Top Result Artist')).toBeInTheDocument();
		for (const title of ['Artist 1', 'Artist 2', 'Artist 3', 'Artist 4', 'Artist 5']) {
			await expect.element(page.getByText(title)).toBeInTheDocument();
		}
		await expect.element(page.getByText('Artist 6')).not.toBeInTheDocument();
	});

	it('keys unidentified rows by local id and skips their enrichment', async () => {
		const mockFetch = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.startsWith('/api/v3/search?')) {
				return jsonResponse(
					unifiedResponse([
						{
							kind: 'artist',
							id: 'local-artist',
							title: 'Local First',
							musicbrainz_id: null,
							in_library: true,
							requested: false,
							score: 90
						}
					])
				);
			}
			throw new Error(`Unexpected request: ${url}`);
		});
		globalThis.fetch = mockFetch as typeof fetch;

		await render(SearchPageTestHarness, { data: { query: 'local first' } });

		await expect.element(page.getByText('Local First')).toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: /Local First/ }))
			.toHaveAttribute('href', '/artist/local-artist');

		await page.getByText('Local First').hover();
		await new Promise((resolve) => setTimeout(resolve, 250));
		expect(
			mockFetch.mock.calls.filter(([input]) => String(input) === '/api/v3/search/enrich/batch')
		).toHaveLength(0);
	});
});
