import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import SearchSuggestionsTestHarness from './SearchSuggestionsTestHarness.svelte';
import type { SuggestResultV3 } from '$lib/queries/search/SearchV3Adapters';
import { authStore } from '$lib/stores/authStore.svelte';
import { resetQueryCacheForUserSwitch } from '$lib/queries/QueryClient';

const mockRows: SuggestResultV3[] = [
	{
		kind: 'artist',
		id: 'artist-1',
		title: 'Muse',
		musicbrainz_id: 'mbid-artist-1',
		score: 95
	},
	{
		kind: 'album',
		id: 'album-1',
		title: 'Origin of Symmetry',
		artist: 'Muse',
		musicbrainz_id: 'mbid-album-1',
		score: 90
	}
];

function makeResponse(body: unknown, status = 200): Response {
	const json = JSON.stringify(body);
	return new Response(json, {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

// The typeahead reads one endpoint now: any other request (the old v1
// suggest, the old local fan-out) fails the test outright.
function mockFetchSuccess(results: SuggestResultV3[] = mockRows) {
	return vi.fn().mockImplementation((input: RequestInfo | URL) => {
		const url = String(input);
		if (url.startsWith('/api/v3/search/suggest?')) {
			return Promise.resolve(makeResponse({ results, status: 'ok' }));
		}
		throw new Error(`Unexpected request: ${url}`);
	});
}

async function renderComponent(props: Record<string, unknown> = {}) {
	const options = {
		props: { query: '', onSearch: vi.fn(), onSelect: vi.fn(), ...props }
	};
	return await render(
		SearchSuggestionsTestHarness,
		options as unknown as Parameters<typeof render<typeof SearchSuggestionsTestHarness>>[1]
	);
}

describe('SearchSuggestions.svelte', () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(async () => {
		originalFetch = globalThis.fetch;
		await resetQueryCacheForUserSwitch();
		authStore.setUser({
			id: 'suggest-user',
			display_name: 'Suggest User',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'suggest-user',
			username_display: 'Suggest User',
			providers: ['local']
		});
		vi.useFakeTimers({ shouldAdvanceTime: true });
	});

	afterEach(async () => {
		globalThis.fetch = originalFetch;
		await resetQueryCacheForUserSwitch();
		authStore.clear();
		vi.useRealTimers();
	});

	it('should debounce and only fire one fetch for rapid input', async () => {
		const fetchSpy = mockFetchSuccess();
		globalThis.fetch = fetchSpy;

		await renderComponent();

		const input = page.getByRole('searchbox');
		await input.fill('m');
		await vi.advanceTimersByTimeAsync(100);
		await input.fill('mu');
		await vi.advanceTimersByTimeAsync(100);
		await input.fill('mus');
		await vi.advanceTimersByTimeAsync(400);

		await vi.waitFor(() => {
			const suggestionCalls = fetchSpy.mock.calls.filter(([input]) =>
				String(input).startsWith('/api/v3/search/suggest?')
			);
			expect(suggestionCalls).toHaveLength(1);
		});
	});

	it('should ignore stale responses when a newer request is pending', async () => {
		let callCount = 0;
		globalThis.fetch = vi.fn().mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (!url.startsWith('/api/v3/search/suggest?')) {
				throw new Error(`Unexpected request: ${url}`);
			}
			callCount++;
			const currentCall = callCount;
			if (currentCall === 1) {
				return new Promise((resolve) =>
					setTimeout(
						() =>
							resolve(
								makeResponse({
									results: [
										{
											kind: 'artist' as const,
											id: 'stale-1',
											title: 'StaleResult',
											score: 50
										}
									],
									status: 'ok'
								})
							),
						300
					)
				);
			}
			return Promise.resolve(
				makeResponse({
					results: [
						{
							kind: 'artist' as const,
							id: 'fresh-1',
							title: 'FreshResult',
							score: 80
						}
					],
					status: 'ok'
				})
			);
		});

		await renderComponent();

		const input = page.getByRole('searchbox');

		await input.fill('ab');
		await vi.advanceTimersByTimeAsync(310);

		await input.fill('abc');
		await vi.advanceTimersByTimeAsync(310);

		await vi.advanceTimersByTimeAsync(400);

		const stale = page.getByText('StaleResult');
		await expect.element(stale).not.toBeInTheDocument();

		const fresh = page.getByText('FreshResult');
		await expect.element(fresh).toBeInTheDocument();
	});

	it('drops track rows and over-fetches to keep the list full', async () => {
		const fetchSpy = mockFetchSuccess([
			{
				kind: 'track',
				id: 'track-1',
				title: 'Hysteria',
				artist: 'Muse',
				musicbrainz_id: null,
				score: 99
			},
			mockRows[0]
		]);
		globalThis.fetch = fetchSpy;

		await renderComponent();

		const input = page.getByRole('searchbox');
		await input.fill('mus');
		await vi.advanceTimersByTimeAsync(400);

		await expect.element(page.getByRole('option').first()).toHaveTextContent('Muse');
		await expect.element(page.getByText('Hysteria')).not.toBeInTheDocument();
		await vi.waitFor(() => {
			const calls = fetchSpy.mock.calls.map(([input]) => String(input));
			expect(calls).toHaveLength(1);
			expect(calls[0]).toContain('limit=10');
		});
	});
});
