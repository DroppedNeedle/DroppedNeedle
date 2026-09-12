import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render } from 'vitest-browser-svelte';

// toggle is gated on linked accounts; mock the query to control them
let mockConnections: { service: string }[] = [];
vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({
		get data() {
			return { connections: mockConnections };
		}
	})
}));

import SourceSwitcher from './SourceSwitcher.svelte';
import { musicSourceStore, type MusicSource } from '$lib/stores/musicSource';
import { PAGE_SOURCE_KEYS } from '$lib/constants';

describe('SourceSwitcher.svelte', () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: () => Promise.resolve({ source: 'listenbrainz' })
		});
		mockConnections = [];
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
		mockConnections = [];
		localStorage.removeItem(PAGE_SOURCE_KEYS.home);
	});

	it('renders nothing when only ListenBrainz is linked', async () => {
		mockConnections = [{ service: 'listenbrainz' }];
		const { container } = await render(SourceSwitcher, {
			props: { pageKey: 'home' }
		} as Parameters<typeof render<typeof SourceSwitcher>>[1]);
		await vi.waitFor(() => {
			expect(container.querySelectorAll('button').length).toBe(0);
		});
	});

	it('renders nothing when only Last.fm is linked', async () => {
		mockConnections = [{ service: 'lastfm' }];
		const { container } = await render(SourceSwitcher, {
			props: { pageKey: 'home' }
		} as Parameters<typeof render<typeof SourceSwitcher>>[1]);
		await vi.waitFor(() => {
			expect(container.querySelectorAll('button').length).toBe(0);
		});
	});

	it('renders nothing when neither service is linked', async () => {
		mockConnections = [];
		const { container } = await render(SourceSwitcher, {
			props: { pageKey: 'home' }
		} as Parameters<typeof render<typeof SourceSwitcher>>[1]);
		await vi.waitFor(() => {
			expect(container.querySelectorAll('button').length).toBe(0);
		});
	});

	it('renders switcher buttons when both services are linked', async () => {
		mockConnections = [{ service: 'listenbrainz' }, { service: 'lastfm' }];
		await render(SourceSwitcher, {
			props: { pageKey: 'home' }
		} as Parameters<typeof render<typeof SourceSwitcher>>[1]);

		await expect.element(page.getByRole('button', { name: 'ListenBrainz' })).toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Last.fm' })).toBeInTheDocument();
	});

	it('defaults to ListenBrainz as active source', async () => {
		mockConnections = [{ service: 'listenbrainz' }, { service: 'lastfm' }];
		await render(SourceSwitcher, {
			props: { pageKey: 'home' }
		} as Parameters<typeof render<typeof SourceSwitcher>>[1]);

		const lbBtn = page.getByRole('button', { name: 'ListenBrainz' });
		await vi.waitFor(() => {
			expect(lbBtn.element().className).toContain('btn-primary');
		});
	});

	it('calls onSourceChange when switching source', async () => {
		mockConnections = [{ service: 'listenbrainz' }, { service: 'lastfm' }];
		const onSourceChange = vi.fn<(source: MusicSource) => void>();
		await render(SourceSwitcher, {
			props: { pageKey: 'home', onSourceChange }
		} as unknown as Parameters<typeof render<typeof SourceSwitcher>>[1]);

		await page.getByRole('button', { name: 'Last.fm' }).click();
		await vi.waitFor(() => {
			expect(onSourceChange).toHaveBeenCalledWith('lastfm');
		});
	});

	it('updates page source when switching source', async () => {
		mockConnections = [{ service: 'listenbrainz' }, { service: 'lastfm' }];
		await render(SourceSwitcher, {
			props: { pageKey: 'home' }
		} as Parameters<typeof render<typeof SourceSwitcher>>[1]);

		await page.getByRole('button', { name: 'Last.fm' }).click();
		await vi.waitFor(() => {
			expect(musicSourceStore.getPageSource('home')).toBe('lastfm');
		});
	});
});
