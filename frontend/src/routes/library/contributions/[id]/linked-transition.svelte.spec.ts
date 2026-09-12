import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryContribution } from '$lib/types';
import { LibraryContributionQueryKeyFactory } from '$lib/queries/libraryContributions/LibraryContributionQueryKeyFactory';
import {
	invalidateQueriesWithPersister,
	resetQueryCacheForUserSwitch
} from '$lib/queries/QueryClient';

const h = vi.hoisted(() => ({
	get: vi.fn(),
	invalidateCatalog: vi.fn().mockResolvedValue(undefined),
	authStore: { user: { id: 'user-1' }, isTrusted: true }
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/stores/authStore.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/stores/authStore.svelte')>()),
	authStore: h.authStore
}));
vi.mock('$lib/api/client', () => ({ api: { global: { get: h.get } } }));
vi.mock('$lib/queries/library/LibraryCatalogInvalidation', () => ({
	invalidateLibraryCatalog: (...args: unknown[]) => h.invalidateCatalog(...args)
}));
// The persister behind the sanctioned query helpers must stay inert in tests.
vi.mock('idb-keyval', () => ({
	clear: vi.fn().mockResolvedValue(undefined),
	del: vi.fn().mockResolvedValue(undefined),
	entries: vi.fn().mockResolvedValue([]),
	get: vi.fn().mockResolvedValue(undefined),
	set: vi.fn().mockResolvedValue(undefined),
	// Inert UseStore: persistence drops writes, like the get/set stubs above.
	createStore: vi.fn(() => vi.fn(async () => {}))
}));

import LinkedTransitionHarness from './LinkedTransitionHarness.svelte';

function contribution(
	state: LibraryContribution['state'],
	rowRevision: number
): LibraryContribution {
	return {
		id: 'contribution-1',
		local_album_id: 'album-1',
		created_by_user_id: 'curator-1',
		updated_by_user_id: 'curator-1',
		state,
		album_row_revision: 1,
		input_revision: `input-${rowRevision}`,
		local_snapshot: {
			schema_version: 1,
			local_album_id: 'album-1',
			local_artist_id: 'artist-1',
			album_row_revision: 1,
			input_revision: 'input-1',
			title: 'Basement Pressing',
			album_artist_name: 'Signal Path',
			artist_kind: 'group',
			musicbrainz_artist_id: null,
			musicbrainz_release_group_id: null,
			musicbrainz_release_id: null,
			release_date: '2024',
			year: 2024,
			is_compilation: false,
			captured_at: 1_700_000_000,
			media: [
				{
					position: 1,
					title: null,
					tracks: [
						{
							local_track_id: 'track-1',
							disc_number: 1,
							track_number: 1,
							title: 'First Track',
							artist_name: 'Signal Path',
							duration_seconds: 180,
							duration_reliable: true
						}
					]
				}
			]
		},
		draft: {
			schema_version: 1,
			title: { value: 'Basement Pressing', source: 'local' },
			artist_credit: { value: 'Signal Path', source: 'local' },
			release_date: { value: '2024', source: 'local' },
			country: { value: null, source: 'local' },
			label: { value: null, source: 'local' },
			catalogue_number: { value: null, source: 'local' },
			barcode: { value: null, source: 'local' },
			packaging: { value: null, source: 'local' },
			media: [
				{
					position: 1,
					title: { value: null, source: 'local' },
					format: { value: null, source: 'local' },
					tracks: [
						{
							local_track_id: 'track-1',
							disc_number: 1,
							track_number: 1,
							title: { value: 'First Track', source: 'local' },
							artist_name: { value: 'Signal Path', source: 'local' },
							duration_seconds: 180
						}
					]
				}
			]
		},
		source_selection: { schema_version: 1, sources: [], alignments: [] },
		provider_snapshot_expires_at: null,
		discogs_source: null,
		duplicate_result: null,
		duplicate_checked_at: null,
		result_release_mbid: null,
		result_source: null,
		result_received_at: null,
		seeded_at: null,
		terminal_at: null,
		created_at: 1,
		updated_at: 1,
		row_revision: rowRevision,
		input_is_current: true,
		validation: [],
		next_actions: state === 'linked' ? [] : ['edit_draft', 'run_duplicate_check', 'cancel']
	};
}

const DETAIL_KEY = LibraryContributionQueryKeyFactory.detail('user-1', 'contribution-1');
const refreshDetail = () => invalidateQueriesWithPersister({ queryKey: DETAIL_KEY });

beforeEach(async () => {
	vi.clearAllMocks();
	await resetQueryCacheForUserSwitch();
});

describe('contribution linked-transition catalog guard', () => {
	it('sweeps the catalog exactly once when polling observes the link landing', async () => {
		h.get.mockResolvedValue(contribution('verifying', 1));
		const screen = await render(LinkedTransitionHarness, {
			props: { data: { contributionId: 'contribution-1', primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('verifying', { exact: true })).toBeVisible();
		expect(h.invalidateCatalog).not.toHaveBeenCalled();

		h.get.mockResolvedValue(contribution('linked', 2));
		await refreshDetail();
		await expect.element(page.getByText('linked', { exact: true })).toBeVisible();
		expect(h.invalidateCatalog).toHaveBeenCalledOnce();

		// further linked-state updates (refetches, revision bumps) never re-sweep
		await refreshDetail();
		await refreshDetail();
		await expect.element(page.getByText('linked', { exact: true })).toBeVisible();
		expect(h.invalidateCatalog).toHaveBeenCalledOnce();

		await screen.unmount();
	});

	it('never sweeps on revisits of an already-linked contribution', async () => {
		h.get.mockResolvedValue(contribution('linked', 5));
		const first = await render(LinkedTransitionHarness, {
			props: { data: { contributionId: 'contribution-1', primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByText('linked', { exact: true })).toBeVisible();

		// leave and come back - the repeat-visit pathology must stay at zero sweeps
		await first.unmount();
		await render(LinkedTransitionHarness, {
			props: { data: { contributionId: 'contribution-1', primarySource: 'listenbrainz' } }
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByText('linked', { exact: true })).toBeVisible();
		await refreshDetail();

		expect(h.invalidateCatalog).not.toHaveBeenCalled();
	});
});
