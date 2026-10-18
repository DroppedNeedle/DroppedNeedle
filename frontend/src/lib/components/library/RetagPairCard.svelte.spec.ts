import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { RetagPair } from '$lib/queries/library/LibraryRetagPairQueries.svelte';

const h = vi.hoisted(() => {
	class ApiError extends Error {
		constructor(
			readonly status: number,
			message: string,
			readonly code = ''
		) {
			super(message);
		}
	}
	return {
		ApiError,
		join: vi.fn(),
		dismiss: vi.fn(),
		goto: vi.fn(),
		invalidate: vi.fn()
	};
});

vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$lib/api/client', () => ({ ApiError: h.ApiError }));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: { user: { id: 'admin-1' } } }));
vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: h.invalidate }));
vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: (_user: () => string, group: () => string) => ({
		get data() {
			const release = group() === 'rg-old' ? 'rel-old' : 'rel-new';
			const country = group() === 'rg-old' ? 'GB' : 'US';
			return {
				items: [{ release_mbid: release, date: '2011-03-01', country, media_formats: ['CD'] }]
			};
		}
	})
}));
vi.mock('$lib/queries/library/LibraryRetagPairQueries.svelte', () => ({
	joinRetagPair: () => ({ mutateAsync: h.join, isPending: false }),
	dismissRetagPair: () => ({ mutateAsync: h.dismiss, isPending: false })
}));

import RetagPairCard from './RetagPairCard.svelte';

function side(
	id: string,
	title: string,
	edition: Partial<NonNullable<RetagPair['old_album']['edition']>> | null
) {
	return {
		id,
		title,
		album_artist_name: 'Artist',
		track_count: id === 'old' ? 12 : 11,
		identity_revision: id === 'old' ? 3 : 7,
		edition: edition
			? {
					chosen: true,
					custom: false,
					release_group_mbid: `rg-${id}`,
					release_mbid: `rel-${id}`,
					...edition
				}
			: null
	};
}

function pair(overrides: Partial<RetagPair> = {}): RetagPair {
	return {
		old_album: side('old', 'Blue Hour', {}),
		new_album: side('new', 'Blue Hour (Remaster)', {}),
		reason: 'chosen_matches_differ',
		editions_differ: true,
		recorded_at: 0,
		...overrides
	};
}

beforeEach(() => {
	h.join.mockReset().mockResolvedValue({ album_id: 'new' });
	h.dismiss.mockReset().mockResolvedValue({ dismissed: true });
	h.goto.mockReset();
	h.invalidate.mockReset();
});

describe('RetagPairCard', () => {
	it('asks which edition to keep before a merge of two chosen editions', async () => {
		render(RetagPairCard, { pair: pair() });

		await expect
			.element(
				page.getByText('have different editions chosen, so DroppedNeedle did not join them.', {
					exact: false
				})
			)
			.toBeVisible();
		const merge = page.getByRole('button', { name: 'Merge into one album' });
		await expect.element(merge).toBeDisabled();

		await page.getByRole('radio', { name: 'Keep the 2011 GB CD edition (from Blue Hour)' }).click();
		await expect.element(merge).toBeEnabled();
		await expect
			.element(page.getByText('the album is left unmanaged after the merge', { exact: false }))
			.toBeVisible();
		await merge.click();

		await vi.waitFor(() =>
			expect(h.join).toHaveBeenCalledWith({
				oldAlbumId: 'old',
				newAlbumId: 'new',
				keepEditionOf: 'old',
				expectedIdentityRevisions: { old: 3, new: 7 }
			})
		);
	});

	it('says a split album gives its edition to the named album only', async () => {
		render(RetagPairCard, { pair: pair({ reason: 'tracks_elsewhere', editions_differ: false }) });

		await expect
			.element(
				page.getByText("Blue Hour's edition goes to Blue Hour (Remaster) only.", { exact: false })
			)
			.toBeVisible();
		await expect.element(page.getByRole('radio')).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Merge into one album' }).click();
		await vi.waitFor(() =>
			expect(h.join).toHaveBeenCalledWith(expect.objectContaining({ keepEditionOf: null }))
		);
	});

	it('warns before the click that a custom edition is dropped for good', async () => {
		render(RetagPairCard, {
			pair: pair({
				reason: 'custom_edition',
				editions_differ: false,
				old_album: side('old', 'Blue Hour', { custom: true })
			})
		});

		await expect
			.element(
				page.getByText("Merging drops it: it does not carry over and can't be put back.", {
					exact: false
				})
			)
			.toBeVisible();
	});

	it('says new files beside a hands-off album are left alone, merged or not', async () => {
		render(RetagPairCard, {
			pair: pair({
				reason: 'replaced_hands_off',
				editions_differ: false,
				old_album: side('old', 'Blue Hour', null),
				new_album: side('new', 'Retagged', null)
			})
		});

		await expect
			.element(
				page.getByText(
					'Nothing shows they are one album, so DroppedNeedle has left Retagged alone.',
					{
						exact: false
					}
				)
			)
			.toBeVisible();
		await expect
			.element(
				page.getByText('Retagged stays left unmanaged until you choose "Manage it again".', {
					exact: false
				})
			)
			.toBeVisible();
	});

	it('keeps them apart with a dismiss', async () => {
		render(RetagPairCard, { pair: pair() });

		await page.getByRole('button', { name: 'Keep them apart' }).click();

		await vi.waitFor(() =>
			expect(h.dismiss).toHaveBeenCalledWith({ oldAlbumId: 'old', newAlbumId: 'new' })
		);
		expect(h.join).not.toHaveBeenCalled();
	});

	it('shows a 409 in place and refetches the pairs', async () => {
		h.join.mockRejectedValue(
			new h.ApiError(
				409,
				"An album's edition changed since this was shown. Look again.",
				'STALE_REVISION'
			)
		);
		render(RetagPairCard, { pair: pair({ reason: 'tracks_elsewhere', editions_differ: false }) });

		await page.getByRole('button', { name: 'Merge into one album' }).click();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent("An album's edition changed since this was shown. Look again.");
		expect(h.invalidate).toHaveBeenCalledWith({
			queryKey: expect.arrayContaining(['retag-pairs'])
		});
	});

	it('goes to the album that stays when the page showed the other one', async () => {
		render(RetagPairCard, {
			pair: pair({ reason: 'tracks_elsewhere', editions_differ: false }),
			currentAlbumId: 'old'
		});

		await page.getByRole('button', { name: 'Merge into one album' }).click();

		await vi.waitFor(() =>
			expect(h.goto).toHaveBeenCalledWith(expect.stringMatching(/\/album\/new$/))
		);
	});
});
