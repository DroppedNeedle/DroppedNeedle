import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import PlaylistCard from './PlaylistCard.svelte';
import type { PlaylistSummary } from '$lib/api/playlists';

const basePlaylist: PlaylistSummary = {
	id: 'pl-1',
	name: 'My Playlist',
	track_count: 5,
	total_duration: 1234,
	cover_urls: ['a.jpg', 'b.jpg'],
	custom_cover_url: null,
	source_ref: null,
	created_at: '2026-01-01T00:00:00Z',
	updated_at: '2026-01-02T00:00:00Z',
	is_public: false,
	is_owner: true,
	owner_name: null,
	is_redacted: false
};

async function renderCard(playlist: PlaylistSummary = basePlaylist) {
	return await render(PlaylistCard, {
		props: { playlist }
	} as Parameters<typeof render<typeof PlaylistCard>>[1]);
}

describe('PlaylistCard.svelte', () => {
	it('renders playlist name', async () => {
		await renderCard();
		await expect.element(page.getByText('My Playlist')).toBeInTheDocument();
	});

	it('renders track count and duration subtitle', async () => {
		await renderCard();
		await expect.element(page.getByText(/5 tracks/)).toBeInTheDocument();
		await expect.element(page.getByText(/20 min/)).toBeInTheDocument();
	});

	it('renders singular "track" for 1 track', async () => {
		const single: PlaylistSummary = { ...basePlaylist, track_count: 1 };
		await renderCard(single);
		await expect.element(page.getByText(/1 track(?!s)/)).toBeInTheDocument();
	});

	it('links to the correct playlist detail page', async () => {
		await renderCard();
		const link = page.getByRole('link', { name: /Open My Playlist/ });
		await expect.element(link).toBeInTheDocument();
		expect(await link.element()).toHaveAttribute('href', '/playlists/pl-1');
	});

	it('omits duration from subtitle when total_duration is null', async () => {
		const noDuration: PlaylistSummary = { ...basePlaylist, total_duration: null };
		await renderCard(noDuration);
		const subtitle = page.getByText(/5 tracks/);
		await expect.element(subtitle).toBeInTheDocument();
		const el = await subtitle.element();
		expect(el.textContent).not.toContain(' - ');
	});
});
