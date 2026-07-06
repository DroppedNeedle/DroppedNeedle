import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import WantedWatchCard from './WantedWatchCard.svelte';
import type { WantedWatchItem } from '$lib/queries/wanted/types';

function makeItem(overrides: Partial<WantedWatchItem> = {}): WantedWatchItem {
	return {
		release_group_mbid: '22222222-2222-2222-2222-222222222222',
		artist_name: 'Yan Qing',
		album_title: 'the arrival',
		kind: 'missing',
		state: 'watching',
		check_count: 4,
		next_check_at: Date.now() / 1000 + 3 * 86400,
		new_candidate_count: 0,
		created_at: Date.now() / 1000 - 86400,
		artist_mbid: null,
		year: 2026,
		cover_url: null,
		first_release_date: '2026-06-23',
		last_checked_at: Date.now() / 1000 - 3600,
		last_outcome: 'no_results',
		user_id: 'user-a',
		user_name: null,
		...overrides
	};
}

function renderCard(overrides: Partial<WantedWatchItem> = {}, props: Record<string, unknown> = {}) {
	return render(WantedWatchCard, {
		props: { item: makeItem(overrides), ...props }
	} as unknown as Parameters<typeof render<typeof WantedWatchCard>>[1]);
}

describe('WantedWatchCard.svelte', () => {
	it('shows the album, artist, and a live watching state line', async () => {
		renderCard();
		await expect.element(page.getByText('the arrival')).toBeVisible();
		await expect.element(page.getByText('Yan Qing • 2026')).toBeVisible();
		await expect.element(page.getByText('Watching')).toBeVisible();
		await expect.element(page.getByText('next check in 3 d')).toBeVisible();
		await expect.element(page.getByText('checked 4×')).toBeVisible();
	});

	it('shows Stop and Check now for a watching want and fires the callbacks', async () => {
		const onstop = vi.fn();
		const onresume = vi.fn();
		renderCard({}, { onstop, onresume });
		await page.getByTitle("Stop hunting for this - it won't be watched").click();
		expect(onstop).toHaveBeenCalledOnce();
		await page.getByTitle('Check for this album again soon').click();
		expect(onresume).toHaveBeenCalledOnce();
	});

	it('shows a Resume button for a dormant want', async () => {
		const onresume = vi.fn();
		renderCard({ state: 'dormant' }, { onresume });
		await expect.element(page.getByText('Dormant')).toBeVisible();
		await expect.element(page.getByText('paused after a year of looking')).toBeVisible();
		await page.getByText('Resume').click();
		expect(onresume).toHaveBeenCalledOnce();
	});

	it('shows a stopped state with Resume', async () => {
		renderCard({ state: 'stopped' }, { onresume: vi.fn() });
		await expect.element(page.getByText('Stopped')).toBeVisible();
		await expect.element(page.getByText('Resume')).toBeVisible();
	});

	it('shows a fulfilled state with no actions', async () => {
		renderCard({ state: 'fulfilled' }, { onstop: vi.fn(), onresume: vi.fn() });
		await expect.element(page.getByText('Found')).toBeVisible();
		await expect.element(page.getByText('in your library')).toBeVisible();
		expect(page.getByText('Stop').elements()).toHaveLength(0);
		expect(page.getByText('Resume').elements()).toHaveLength(0);
	});

	it('shows the new-candidates badge and fires onseen when clicked', async () => {
		const onseen = vi.fn();
		renderCard({ new_candidate_count: 2 }, { onseen });
		const badge = page.getByText('2 new candidates to review');
		await expect.element(badge).toBeVisible();
		// the badge is a real link to the album page - block the navigation so the
		// test page survives, then verify the mark-seen callback still fired
		document.addEventListener('click', (e) => e.preventDefault(), { capture: true, once: true });
		await badge.click();
		expect(onseen).toHaveBeenCalledOnce();
	});

	it('notes when a partial want is finding missing tracks', async () => {
		renderCard({ kind: 'partial' });
		await expect.element(page.getByText('finding missing tracks')).toBeVisible();
	});

	it('hides mutating buttons when no callbacks are given (non-owner)', async () => {
		renderCard();
		expect(page.getByText('Stop').elements()).toHaveLength(0);
		expect(page.getByText('Check now').elements()).toHaveLength(0);
	});
});
