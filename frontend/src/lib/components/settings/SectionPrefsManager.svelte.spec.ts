import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { SectionPrefsResponse } from '$lib/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const mockPrefs: SectionPrefsResponse = {
	pages: {
		home: [
			{
				key: 'trending_artists',
				title: 'Trending Artists',
				description: 'Artists trending across your music source right now.',
				zone: "What's Hot",
				enabled: true,
				available: true,
				requires: null
			},
			{
				key: 'weekly_exploration',
				title: 'Weekly Exploration',
				description: 'Your ListenBrainz weekly exploration playlist.',
				zone: 'For You',
				enabled: true,
				available: false,
				requires: 'listenbrainz'
			},
			{
				// deliberately NON-CONTIGUOUS zone (repeats "What's Hot" after "For You"):
				// grouping must merge it, not emit a duplicate {#each} key (crash regression)
				key: 'popular_albums',
				title: 'Popular Now',
				description: 'Albums popular across your music source this week.',
				zone: "What's Hot",
				enabled: true,
				available: true,
				requires: null
			}
		],
		discover: []
	}
};

const saveSectionPrefs = vi.fn().mockResolvedValue(undefined);

vi.mock('$lib/queries/section-prefs/SectionPrefsQuery.svelte', () => ({
	getSectionPrefsQuery: () => ({
		data: mockPrefs,
		isLoading: false,
		isError: false,
		refetch: vi.fn()
	}),
	saveSectionPrefs: (...args: unknown[]) => saveSectionPrefs(...args)
}));

import SectionPrefsManager from './SectionPrefsManager.svelte';

describe('SectionPrefsManager', () => {
	beforeEach(() => {
		saveSectionPrefs.mockClear();
	});

	it('renders sections grouped by zone with toggles', async () => {
		render(SectionPrefsManager, {
			props: { page: 'home', title: 'Home', description: 'Pick sections.' }
		} as Parameters<typeof render<typeof SectionPrefsManager>>[1]);

		await expect.element(page.getByText('Trending Artists')).toBeVisible();
		await expect.element(page.getByText("What's Hot")).toBeVisible();
		await expect.element(page.getByText('For You')).toBeVisible();
		await expect.element(page.getByText('3 of 3 sections shown')).toBeVisible();
		// non-contiguous zones merge into one group instead of crashing
		await expect.element(page.getByText('Popular Now')).toBeVisible();
	});

	it('unavailable sections show a connect link and a disabled toggle', async () => {
		render(SectionPrefsManager, {
			props: { page: 'home', title: 'Home', description: 'Pick sections.' }
		} as Parameters<typeof render<typeof SectionPrefsManager>>[1]);

		const connectLink = page.getByRole('link', { name: /Connect ListenBrainz/ });
		await expect.element(connectLink).toBeVisible();
		await expect.element(connectLink).toHaveAttribute('href', '/profile#scrobbling');
	});

	it('lastfm-requiring sections link to profile scrobbling', async () => {
		mockPrefs.pages.discover.push({
			key: 'similar_artists',
			title: 'Similar Artists',
			description: 'Artists similar to your Last.fm history.',
			zone: 'For You',
			enabled: true,
			available: false,
			requires: 'lastfm'
		});
		try {
			render(SectionPrefsManager, {
				props: { page: 'discover', title: 'Discover', description: 'Pick sections.' }
			} as Parameters<typeof render<typeof SectionPrefsManager>>[1]);

			const connectLink = page.getByRole('link', { name: /Connect Last\.fm/ });
			await expect.element(connectLink).toBeVisible();
			await expect.element(connectLink).toHaveAttribute('href', '/profile#scrobbling');
		} finally {
			mockPrefs.pages.discover.pop();
		}
	});

	it('toggling a section saves the page after the debounce', async () => {
		render(SectionPrefsManager, {
			props: { page: 'home', title: 'Home', description: 'Pick sections.' }
		} as Parameters<typeof render<typeof SectionPrefsManager>>[1]);

		const toggles = page.getByRole('checkbox');
		// first checkbox is the master toggle; second is Trending Artists
		await toggles.nth(1).click();

		await vi.waitFor(() => {
			expect(saveSectionPrefs).toHaveBeenCalledTimes(1);
		});
		expect(saveSectionPrefs).toHaveBeenCalledWith({
			page: 'home',
			sections: [
				{ key: 'trending_artists', enabled: false },
				{ key: 'weekly_exploration', enabled: true },
				{ key: 'popular_albums', enabled: true }
			]
		});
	});

	it('master toggle disables every section', async () => {
		render(SectionPrefsManager, {
			props: { page: 'home', title: 'Home', description: 'Pick sections.' }
		} as Parameters<typeof render<typeof SectionPrefsManager>>[1]);

		const toggles = page.getByRole('checkbox');
		await toggles.nth(0).click();

		await vi.waitFor(() => {
			expect(saveSectionPrefs).toHaveBeenCalledTimes(1);
		});
		expect(saveSectionPrefs).toHaveBeenCalledWith({
			page: 'home',
			sections: [
				{ key: 'trending_artists', enabled: false },
				{ key: 'weekly_exploration', enabled: false },
				{ key: 'popular_albums', enabled: false }
			]
		});
	});
});
