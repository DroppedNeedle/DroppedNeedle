import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const { apiGet, previewAction, deckMock, samplerStart, requestAlbum } = vi.hoisted(() => {
	const items = [
		{
			release_group_mbid: 'rg-1',
			album_name: 'The Bends',
			artist_name: 'The Verve',
			artist_mbid: 'artist-1',
			cover_url: null,
			recommendation_reason: 'Similar to Radiohead',
			is_wildcard: false,
			in_library: false,
			enrichment: {
				artist_mbid: 'artist-1',
				release_date: '1995-03-13',
				country: 'GB',
				tags: ['alt-rock', 'britpop'],
				youtube_url: null,
				youtube_search_url: 'https://youtube.example/search',
				youtube_search_available: true,
				artist_description: 'Formed in Wigan in 1989, the band went on to…',
				listen_count: 2100000
			}
		},
		{
			release_group_mbid: 'rg-2',
			album_name: 'Urban Hymns',
			artist_name: 'The Verve',
			artist_mbid: 'artist-1',
			cover_url: null,
			recommendation_reason: 'Similar to Radiohead',
			is_wildcard: false,
			in_library: false,
			enrichment: null
		}
	];
	return {
		apiGet: vi.fn(),
		previewAction: vi.fn(),
		deckMock: {
			phase: 'ready' as string,
			queue: items,
			currentIndex: 0,
			get current() {
				return this.queue[this.currentIndex];
			},
			get isLast() {
				return this.currentIndex >= this.queue.length - 1;
			},
			errorMessage: '',
			requestKey: 'source-a:1',
			replacing: false,
			init: vi.fn().mockResolvedValue(undefined),
			next: vi.fn(),
			previous: vi.fn(),
			jumpTo: vi.fn(),
			ignoreCurrent: vi.fn().mockResolvedValue(undefined),
			markCurrentRequested: vi.fn(),
			finish: vi.fn(),
			retryBuild: vi.fn(),
			buildNow: vi.fn(),
			destroy: vi.fn()
		},
		samplerStart: vi.fn().mockResolvedValue(undefined),
		requestAlbum: vi.fn().mockResolvedValue({ success: true })
	};
});

vi.mock('$lib/stores/discoverQueueDeck.svelte', () => ({
	discoverQueueDeck: deckMock
}));

vi.mock('$lib/stores/deckSampler.svelte', () => ({
	deckSampler: {
		status: 'idle',
		tracks: [],
		trackIndex: 0,
		currentTrack: null,
		provider: null,
		progress: 0,
		activeKey: '',
		start: (...args: unknown[]) => samplerStart(...args),
		stop: vi.fn()
	}
}));

vi.mock('$lib/stores/audioFocus.svelte', () => ({
	audioFocus: { claim: vi.fn(), release: vi.fn(), interrupt: vi.fn(), holder: null }
}));

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { isPlaying: false, pause: vi.fn() }
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestAlbum: () => ({
		mutateAsync: (input: unknown) => requestAlbum(input as { release_group_mbid: string })
	})
}));

vi.mock('$lib/stores/integration', async () => {
	const { readable } = await import('svelte/store');
	return {
		integrationStore: readable({ download_client: true, youtube: true, youtube_api: true })
	};
});

vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: apiGet,
			post: vi.fn().mockResolvedValue({})
		}
	}
}));
vi.mock('$lib/queries/discover/DiscoverDemand.svelte', () => ({
	getQueuePreviewMutation: () => ({ mutateAsync: previewAction })
}));

import DiscoverQueueDeck from './DiscoverQueueDeck.svelte';

describe('DiscoverQueueDeck', () => {
	beforeEach(() => {
		deckMock.phase = 'ready';
		deckMock.currentIndex = 0;
		deckMock.requestKey = 'source-a:1';
		vi.clearAllMocks();
		apiGet.mockResolvedValue({ used: 0, limit: 100, remaining: 100 });
		previewAction.mockReset();
	});

	it('renders the current item with reason, links, meta and tags', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await expect.element(page.getByText('Similar to Radiohead')).toBeVisible();
		const albumLink = page.getByRole('link', { name: 'The Bends', exact: true });
		await expect.element(albumLink).toHaveAttribute('href', '/album/rg-1');
		const artistLink = page.getByRole('link', { name: 'The Verve', exact: true });
		await expect.element(artistLink).toHaveAttribute('href', '/artist/artist-1');
		await expect.element(page.getByText('1995')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'alt-rock' })).toBeVisible();
		await expect.element(page.getByText('1 / 2')).toBeVisible();
		await expect
			.element(page.getByTestId('discover-primary-cover'))
			.toHaveAttribute(
				'srcset',
				'/api/v1/covers/release-group/rg-1?size=250 250w, /api/v1/covers/release-group/rg-1?size=500 500w'
			);
	});

	it('Next advances the deck', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await page.getByRole('button', { name: /^Next$/ }).click();
		expect(deckMock.next).toHaveBeenCalledTimes(1);
	});

	it('Not for me ignores the current item', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await page.getByRole('button', { name: /Not for me/ }).click();
		expect(deckMock.ignoreCurrent).toHaveBeenCalledTimes(1);
	});

	it('Request files an album request and marks it', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await page.getByRole('button', { name: /^Request$/ }).click();
		await vi.waitFor(() => {
			expect(requestAlbum).toHaveBeenCalledWith({
				release_group_mbid: 'rg-1',
				artist_name: 'The Verve',
				album_title: 'The Bends',
				artist_mbid: 'artist-1'
			});
		});
		expect(deckMock.markCurrentRequested).toHaveBeenCalled();
	});

	it('Sample album starts the sampler for the current item', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await page.getByRole('button', { name: /Sample album/ }).click();
		expect(samplerStart).toHaveBeenCalledWith('rg-1', 'The Verve', 'The Bends');
	});

	it('filmstrip jump navigates to the clicked item', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await page.getByRole('tab', { name: /Urban Hymns/ }).click();
		expect(deckMock.jumpTo).toHaveBeenCalledWith(1);
	});

	it('building phase shows the equalizer state', async () => {
		deckMock.phase = 'building';
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await expect.element(page.getByText('Building your personalised queue…')).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Build now instead/ })).toBeVisible();
	});

	it('error phase offers retry', async () => {
		deckMock.phase = 'error';
		await render(DiscoverQueueDeck, { youtubeEnabled: true });

		await page.getByRole('button', { name: /Retry/ }).click();
		expect(deckMock.retryBuild).toHaveBeenCalled();
	});

	it('keeps unresolved video and external search reachable without automatic lookup', async () => {
		await render(DiscoverQueueDeck, { youtubeEnabled: false });
		await expect.element(page.getByRole('button', { name: 'Play music video' })).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Search YouTube' }))
			.toHaveAttribute('href', 'https://youtube.example/search');
		expect(previewAction).not.toHaveBeenCalled();
		expect(apiGet).not.toHaveBeenCalled();
	});
	it('keeps an enriched direct video available when the integration is disabled', async () => {
		const enrichment = deckMock.queue[0].enrichment as { youtube_url: string | null };
		enrichment.youtube_url = 'https://www.youtube-nocookie.com/embed/direct-video';

		try {
			await render(DiscoverQueueDeck, { youtubeEnabled: false });

			await expect.element(page.getByRole('button', { name: 'Play music video' })).toBeVisible();
			await page.getByRole('button', { name: 'Play music video' }).click();
			await expect.element(page.getByRole('button', { name: 'Close video' })).toBeVisible();
			expect(previewAction).not.toHaveBeenCalled();
			expect(apiGet).not.toHaveBeenCalled();
		} finally {
			enrichment.youtube_url = null;
		}
	});

	it('shows inline failure and retries the explicit preview action', async () => {
		previewAction.mockRejectedValueOnce(new Error('provider failed')).mockResolvedValueOnce({
			status: 'not_found',
			youtube_url: null,
			youtube_search_url: 'https://youtube.example/manual'
		});
		await render(DiscoverQueueDeck, { youtubeEnabled: true });
		await page.getByRole('button', { name: 'Play music video' }).click();
		await expect.element(page.getByRole('status')).toHaveTextContent('Video lookup failed');
		await page.getByRole('button', { name: 'Retry video' }).click();
		await expect.element(page.getByRole('status')).toHaveTextContent('No video found');
		await expect
			.element(page.getByRole('link', { name: 'Search YouTube' }))
			.toHaveAttribute('href', 'https://youtube.example/manual');
	});

	it('does not publish a preview after its source changes', async () => {
		const pending = Promise.withResolvers<{
			status: string;
			youtube_url: string | null;
			youtube_search_url: string | null;
		}>();
		previewAction.mockReturnValue(pending.promise);
		await render(DiscoverQueueDeck, { youtubeEnabled: true });
		await page.getByRole('button', { name: 'Play music video' }).click();
		await expect.element(page.getByRole('button', { name: 'Finding video…' })).toBeDisabled();
		deckMock.requestKey = 'source-b:2';
		pending.resolve({
			status: 'not_found',
			youtube_url: null,
			youtube_search_url: 'https://stale.example'
		});
		await pending.promise;
		await expect
			.element(page.getByRole('link', { name: 'Search YouTube' }))
			.toHaveAttribute('href', 'https://youtube.example/search');
		expect(previewAction).toHaveBeenCalledWith({ mbid: 'rg-1', signal: expect.any(AbortSignal) });
	});
});
