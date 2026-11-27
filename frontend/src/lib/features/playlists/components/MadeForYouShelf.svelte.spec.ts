import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import type { PlaylistSummary } from '$lib/api/playlists';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import MadeForYouShelf from './MadeForYouShelf.svelte';

type Status = RouteData<'GET /api/v3/me/rotation'>;

// Monday 30 November 2026, 00:00 UTC.
const NEXT = 1_795_996_800;

const STATUS: Status = {
	enabled: true,
	next_change_at: NEXT,
	waiting: ['five_stars', 'lossless'],
	thin: ['never_played']
};

function list(
	id: string,
	name: string,
	rotation: NonNullable<PlaylistSummary['rotation']> | null
): PlaylistSummary & { kind: 'rotation' | 'weekly_mix' } {
	return {
		id,
		name,
		track_count: 50,
		total_duration: 3000,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: '2026-11-23T00:00:00.000Z',
		updated_at: '2026-11-23T00:00:00.000Z',
		is_public: false,
		is_owner: true,
		is_shared: false,
		can_edit: false,
		can_delete: true,
		owner_name: 'Owner',
		is_redacted: false,
		kind: rotation ? 'rotation' : 'weekly_mix',
		rotation
	};
}

const MIX = list('pl-mix', 'Your Weekly Mix', null);

const LISTS = [
	list('pl-added', 'Added recently', {
		idea: 'added_recently',
		decade: null,
		pinned: false,
		always_on: true,
		next_change_at: NEXT
	}),
	list('pl-90s', 'The 1990s', {
		idea: 'decade',
		decade: 1990,
		pinned: false,
		always_on: false,
		next_change_at: NEXT
	}),
	list('pl-deep', 'Deep cuts', {
		idea: 'deep_cuts',
		decade: null,
		pinned: true,
		always_on: false,
		next_change_at: NEXT
	})
];

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'owner-1',
			display_name: 'Owner',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'owner',
			username_display: 'Owner'
		})
	);
});

describe('the Made for you shelf', () => {
	it('shows the Weekly Mix first, each list on rotation with its change day, and the ideas waiting', async () => {
		fakeApi({ 'GET /api/v3/me/rotation': STATUS });
		await renderWithQuery(MadeForYouShelf, { lists: [...LISTS, MIX] });

		await expect.element(page.getByRole('heading', { name: 'Made for you' })).toBeVisible();
		const tiles = page.getByRole('link');
		await expect.element(tiles.first()).toHaveAccessibleName('Your Weekly Mix');
		await expect.element(page.getByText('Each week')).toBeVisible();
		expect(page.getByText('Changes Mon').elements()).toHaveLength(3);
		await expect.element(page.getByRole('link', { name: 'The 1990s' })).toBeVisible();
		await expect.element(page.getByText('2 waiting')).toBeVisible();
		await expect.element(page.getByText('Five stars · Lossless')).toBeVisible();
		await expect.element(page.getByText('Skipped while thin: Never played')).toBeVisible();
	});

	it('keeps the Weekly Mix and drops the lists on rotation while they are off', async () => {
		const api = fakeApi({ 'GET /api/v3/me/rotation': { ...STATUS, enabled: false } });
		await renderWithQuery(MadeForYouShelf, { lists: [...LISTS, MIX] });

		await expect.poll(() => api.calls('GET /api/v3/me/rotation').length).toBe(1);
		await expect.element(page.getByRole('link', { name: 'Your Weekly Mix' })).toBeVisible();
		await expect.element(page.getByText('Added recently')).not.toBeInTheDocument();
		await expect.element(page.getByText('2 waiting')).not.toBeInTheDocument();
	});

	it('is absent while it has nothing', async () => {
		const api = fakeApi({ 'GET /api/v3/me/rotation': { ...STATUS, enabled: false } });
		await renderWithQuery(MadeForYouShelf, { lists: LISTS });

		await expect.poll(() => api.calls('GET /api/v3/me/rotation').length).toBe(1);
		await expect
			.element(page.getByRole('heading', { name: 'Made for you' }))
			.not.toBeInTheDocument();
	});
});
