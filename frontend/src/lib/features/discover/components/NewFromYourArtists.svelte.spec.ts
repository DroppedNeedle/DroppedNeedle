import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { withBasePath } from '$lib/utils/basePath';
import type { NewRelease } from '../queries/newReleases.svelte';
import NewFromYourArtists from './NewFromYourArtists.svelte';

const KEEP = 'POST /api/v3/requests/albums';

function release(
	mbid: string,
	title: string,
	date: string,
	type: NonNullable<NewRelease['album']['release_type']>,
	marks: { in_library?: boolean; requested?: boolean } = {}
): NewRelease {
	return {
		album: {
			title,
			musicbrainz_id: mbid,
			artist_name: 'Someone',
			artist_id: `ar-${mbid}`,
			release_date: date,
			release_type: type,
			in_library: marks.in_library ?? false,
			requested: marks.requested ?? false
		}
	};
}

const ROWS: NewRelease[] = [
	release('rg-1', 'Owned One', '2026-11-20', 'album', { in_library: true }),
	{
		...release('rg-2', 'Asked For', '2026-11-12', 'ep', { requested: true }),
		secondary_kind: 'live'
	},
	{
		...release('rg-3', 'Plain Single', '2026-11-03', 'single'),
		reason: 'New from Someone, close to Radiohead.'
	}
];

function signIn() {
	authStore.setUser(
		toAuthUser({
			id: 'u1',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'ada'
		})
	);
}

describe('NewFromYourArtists.svelte', () => {
	it('marks an owned row with its chip and no Keep, and keeps the rest', async () => {
		const api = fakeApi({
			[KEEP]: () => ({
				success: true,
				message: 'Requested',
				musicbrainz_id: 'rg-3',
				status: 'pending'
			})
		});
		signIn();
		await renderWithQuery(NewFromYourArtists, {
			releases: ROWS,
			updatedAt: Date.UTC(2026, 10, 25) / 1000,
			href: withBasePath('/discover/new-releases')
		});

		const list = page.getByRole('list', { name: 'New from your artists' });
		const owned = list.getByRole('listitem').filter({ hasText: 'Owned One' });
		await expect.element(owned.getByText('Owned', { exact: true })).toBeVisible();
		await expect
			.element(list.getByRole('button', { name: 'Keep Owned One' }))
			.not.toBeInTheDocument();
		await expect.element(list.getByText('Requested')).toBeVisible();
		await expect
			.element(list.getByRole('button', { name: 'Keep Asked For' }))
			.not.toBeInTheDocument();
		await expect.element(list.getByText('- Single')).toBeVisible();
		await expect.element(list.getByText('Someone - EP - Live')).toBeVisible();
		await expect.element(list.getByText('New from Someone, close to Radiohead.')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /^All/ }))
			.toHaveAttribute('href', withBasePath('/discover/new-releases'));

		await list.getByRole('button', { name: 'Keep Plain Single' }).click();
		await expect.poll(() => api.calls(KEEP).length).toBe(1);
		expect(api.calls(KEEP)[0].body).toMatchObject({
			musicbrainz_id: 'rg-3',
			made_from: 'new_releases'
		});
	});
});
