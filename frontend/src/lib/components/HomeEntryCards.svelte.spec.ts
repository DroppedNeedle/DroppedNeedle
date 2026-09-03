import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	userId: 'admin-1',
	isAdmin: true,
	stats: {
		data: {
			total_albums: 40,
			total_artists: 18,
			total_tracks: 1234,
			total_size_bytes: 0,
			format_breakdown: {},
			review_count: 5483,
			local_only_count: 9,
			last_scan_at: null
		},
		isError: false
	} as Record<string, unknown>,
	activity: { data: { items: [] as Array<Record<string, unknown>> } } as Record<string, unknown>,
	localStats: { data: null } as Record<string, unknown>
}));

vi.mock('$lib/stores/integration', async () => {
	const { readable } = await import('svelte/store');
	return { integrationStore: readable({ loaded: true, localfiles: false }) };
});
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get user() {
			return { id: h.userId };
		},
		get isAdmin() {
			return h.isAdmin;
		},
		get isTrusted() {
			return false;
		}
	}
}));

vi.mock('$lib/queries/local/LocalQueries.svelte', () => ({
	getLocalStatsQuery: () => h.localStats,
	// re-exported key factory (DropImportMutations imports it via this module)
	LOCAL_KEYS: { root: ['local'] }
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryStatsQuery: () => h.stats
}));
vi.mock('$lib/queries/library/LibraryActivityQueries.svelte', () => ({
	getLibraryActivityQuery: () => h.activity
}));

import HomeEntryCards from './HomeEntryCards.svelte';

beforeEach(() => {
	h.userId = 'admin-1';
	h.isAdmin = true;
	h.stats = {
		data: {
			total_albums: 40,
			total_artists: 18,
			total_tracks: 1234,
			total_size_bytes: 0,
			format_breakdown: {},
			review_count: 5483,
			local_only_count: 9,
			last_scan_at: null
		},
		isError: false
	};
	h.activity = { data: { items: [] } };
	h.localStats = { data: null };
});

describe('HomeEntryCards review footer (issue 372)', () => {
	it('labels the count as review items and links the card to the review queue', async () => {
		render(HomeEntryCards);
		await expect.element(page.getByText('5,483 need review')).toBeVisible();
		expect(page.getByText('albums need review').all()).toHaveLength(0);
		await expect
			.element(page.getByRole('link', { name: /Review queue/ }))
			.toHaveAttribute('href', '/library/review');
	});

	it('keeps non-admins on the library link since review is admin-only', async () => {
		h.userId = 'user-1';
		h.isAdmin = false;
		render(HomeEntryCards);
		await expect.element(page.getByText('5,483 need review')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /Manage library/ }))
			.toHaveAttribute('href', '/library');
	});

	it('prefers the still-matching state over the review link while work runs', async () => {
		h.activity = {
			data: { items: [{ kind: 'identification', waiting_count: 5, deferred_count: 0 }] }
		};
		render(HomeEntryCards);
		await expect.element(page.getByText('5 still matching — 5,483 need a decision')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /Manage library/ }))
			.toHaveAttribute('href', '/library');
	});
});
