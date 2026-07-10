import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type {
	Concert,
	ConcertsResponse,
	FollowedArtist,
	NewReleasesResponse
} from '$lib/queries/following/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' }, isAdmin: false },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

let artistsData: FollowedArtist[] = [];
let releasesData: NewReleasesResponse = { items: [], total: 0 };
let concertsData: ConcertsResponse = { configured: true, items: [], total: 0 };

vi.mock('$lib/queries/following/FollowQueries.svelte', () => ({
	getFollowedArtistsQuery: () => ({ data: artistsData, isPending: false }),
	getRecentReleasesQuery: () => ({ data: releasesData, isPending: false }),
	getConcertsQuery: () => ({ data: concertsData, isPending: false })
}));

import FollowingHub from './+page.svelte';

function artist(n: number): FollowedArtist {
	return {
		mbid: `mbid-${n}`,
		name: `Artist ${n}`,
		auto_download: false,
		auto_download_state: 'none',
		followed_at: n
	};
}

const GIG: Concert = {
	artist_mbid: 'mbid-1',
	artist_name: 'beabadoobee',
	event_name: 'The Powerlines Tour',
	local_date: '2026-11-17',
	status: 'scheduled',
	source: 'ticketmaster',
	source_event_id: 'tm-1',
	matched_city: 'Manchester',
	venue_name: 'AO Arena',
	city: 'Manchester',
	ticket_url: 'https://tickets.example/x',
	distance_km: 3.2
};

describe('Following hub digest', () => {
	it('renders the three sections with counts in the headers', async () => {
		artistsData = [artist(1), artist(2)];
		releasesData = {
			items: [
				{
					release_group_mbid: 'rg-1',
					title: 'This Is How Tomorrow Moves',
					artist_name: 'beabadoobee',
					artist_mbid: 'mbid-1'
				}
			],
			total: 9
		};
		concertsData = { configured: true, items: [GIG], total: 11 };
		render(FollowingHub);

		// hero subtitle also contains "New releases", so scope to the section heading
		await expect.element(page.getByRole('heading', { name: /New releases/ })).toBeVisible();
		await expect.element(page.getByText('(9)')).toBeVisible();
		await expect.element(page.getByText('This Is How Tomorrow Moves')).toBeVisible();
		await expect.element(page.getByText('Coming up')).toBeVisible();
		await expect.element(page.getByText('(11)')).toBeVisible();
		await expect.element(page.getByText('AO Arena · Manchester · 2 mi')).toBeVisible();
		// the hero eyebrow also says "Your artists", so scope to the section heading
		await expect.element(page.getByRole('heading', { name: /Your artists/ })).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Tickets' })).toBeVisible();
	});

	it('shows the quiet-month state when nothing released in the window', async () => {
		artistsData = [artist(1)];
		releasesData = { items: [], total: 0 };
		concertsData = { configured: true, items: [], total: 0 };
		render(FollowingHub);
		await expect
			.element(page.getByText('Nothing released in the last 30 days - quiet month.'))
			.toBeVisible();
	});

	it('marks releases already in the library with a tick', async () => {
		artistsData = [artist(1)];
		releasesData = {
			items: [
				{
					release_group_mbid: 'rg-owned',
					title: 'Grabbed Album',
					artist_name: 'beabadoobee',
					artist_mbid: 'mbid-1',
					in_library: true
				}
			],
			total: 1
		};
		concertsData = { configured: true, items: [], total: 0 };
		render(FollowingHub);
		await expect.element(page.getByText('In your library')).toBeInTheDocument();
	});

	it('invites to follow artists when following nobody', async () => {
		artistsData = [];
		releasesData = { items: [], total: 0 };
		concertsData = { configured: true, items: [], total: 0 };
		render(FollowingHub);
		await expect.element(page.getByText("You're not following anyone yet")).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Discover artists' })).toBeVisible();
	});

	it('prompts for cities when configured but no gigs match', async () => {
		artistsData = [artist(1)];
		releasesData = { items: [], total: 0 };
		concertsData = { configured: true, items: [], total: 0 };
		render(FollowingHub);
		await expect.element(page.getByText('Pick your cities to see gigs near you')).toBeVisible();
	});

	it('shows an overflow chip past twelve artists', async () => {
		artistsData = Array.from({ length: 15 }, (_, i) => artist(i));
		releasesData = { items: [], total: 0 };
		concertsData = { configured: true, items: [], total: 0 };
		render(FollowingHub);
		await expect.element(page.getByRole('link', { name: '+3' })).toBeVisible();
	});
});
