import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	create: vi.fn(),
	apply: vi.fn(),
	discard: vi.fn(),
	undo: vi.fn(),
	preparations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	},
	estimate: {
		data: {
			album_count: 20,
			ready_album_count: 4,
			mapping_required_count: 12,
			exact_release_required_count: 4,
			selected_root_count: 0,
			queued_preparation_count: 0
		},
		isLoading: false,
		isError: false
	},
	findings: {
		data: {
			pages: [
				{
					current_counts_by_finding: {
						mapping_ready: 1,
						ready: 4,
						exact_release_required: 3,
						needs_review: 1
					} as Record<string, number>,
					refresh_required: false,
					items: [
						{
							id: 'finding-1',
							local_album_id: 'album-1',
							album_title: 'Juturna',
							album_artist_name: 'Circa Survive',
							album_year: 2005,
							cover_available: false,
							evidence_id: 'evidence-1',
							review_id: null,
							finding_code: 'mapping_ready',
							reason_code: 'EXACT_RELEASE_MAPPING_SUPPORTED',
							confidence: 'supported',
							apply_eligible: true,
							state: 'open',
							apply_result: null,
							updated_at: 10,
							row_revision: 1
						}
					] as Array<Record<string, unknown>>
				}
			]
		},
		isLoading: false,
		isError: false,
		hasNextPage: false
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1' } },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationQueries.svelte', () => ({
	getLibraryIdentityPreparationsQuery: () => h.preparations,
	getLibraryIdentityPreparationEstimateQuery: () => h.estimate,
	getLibraryIdentityPreparationFindingsQuery: () => h.findings
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationMutations.svelte', () => ({
	createLibraryIdentityPreparation: () => ({ mutateAsync: h.create, isPending: false }),
	applyLibraryIdentityPreparation: () => ({ mutateAsync: h.apply, isPending: false }),
	discardLibraryIdentityPreparation: () => ({ mutateAsync: h.discard, isPending: false }),
	undoLibraryAutomaticEdition: () => ({ mutateAsync: h.undo, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumDetailQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false
	})
}));

import LibraryManagementIdentityReadiness from './LibraryManagementIdentityReadiness.svelte';

const roots = [
	{ id: 'root-1', label: 'Archive', path: '/music', policy: 'automatic' as const, rules: [] }
];

function readyReport() {
	return {
		id: 'preparation-1',
		kind: 'repair',
		state: 'ready',
		expected_work_count: 20,
		completed_count: 20,
		succeeded_count: 20,
		failed_count: 0,
		skipped_count: 0,
		control_request: 'none',
		terminal_code: 'DRY_RUN_READY',
		row_revision: 7,
		event_revision: 2,
		created_at: 1,
		updated_at: 2,
		results: [],
		results_truncated: false,
		reidentification_candidates: [],
		repair_summary: {
			total_identities: 20,
			remaining_identities: 0,
			input_track_count: 100,
			playable_after_detach_track_count: 100,
			estimated_apply_changes: 12,
			catalog_snapshot_revision: 4,
			target_matcher_version: 'management-exact-release-v2',
			counts_by_finding: {
				mapping_ready: 12,
				ready: 4,
				exact_release_required: 3,
				needs_review: 1
			},
			counts_by_reason: {},
			album_counts_by_root: { 'root-1': 20 },
			provider_deferred_count: 0,
			failed_evidence_count: 0,
			purpose: 'management_readiness',
			ready_album_count: 4,
			mapping_candidate_count: 12,
			exact_release_required_count: 3,
			needs_review_count: 1
		}
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	h.create.mockResolvedValue({});
	h.apply.mockResolvedValue({});
	h.discard.mockResolvedValue({});
	h.undo.mockResolvedValue({});
	h.preparations = {
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	};
	h.findings.data.pages[0].current_counts_by_finding = {
		mapping_ready: 1,
		ready: 4,
		exact_release_required: 3,
		needs_review: 1
	};
	h.findings.data.pages[0].refresh_required = false;
	h.findings.data.pages[0].items[0] = {
		id: 'finding-1',
		local_album_id: 'album-1',
		album_title: 'Juturna',
		album_artist_name: 'Circa Survive',
		album_year: 2005,
		cover_available: false,
		evidence_id: 'evidence-1',
		review_id: null,
		finding_code: 'mapping_ready',
		reason_code: 'EXACT_RELEASE_MAPPING_SUPPORTED',
		confidence: 'supported',
		apply_eligible: true,
		state: 'open',
		apply_result: null,
		updated_at: 10,
		row_revision: 1
	};
});

describe('LibraryManagementIdentityReadiness', () => {
	it('requires a second confirmation before accepting catalog-only mappings', async () => {
		h.preparations = {
			data: { pages: [{ items: [readyReport()] }] },
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementIdentityReadiness, { roots });

		await expect.element(page.getByText('Juturna')).toBeVisible();
		await expect.element(page.getByText('Circa Survive')).toBeVisible();
		await expect.element(page.getByText('2005')).toBeVisible();
		await expect.element(page.getByText(/Exact track map verified/)).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Mappings ready/ }))
			.toHaveTextContent('1');
		await expect
			.element(page.getByRole('link', { name: 'Open release' }))
			.toHaveAttribute('href', '/album/album-1');
		await page.getByRole('button', { name: 'Accept mappings...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Accept exact-release mappings?' }))
			.toHaveFocus();
		await expect
			.element(page.getByText(/This writes only verified MusicBrainz identities/))
			.toBeVisible();
		await page.getByRole('button', { name: 'Accept identities' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'preparation-1',
			expectedRevision: 7
		});
	});

	it('can dismiss a ready report without changing identities or files', async () => {
		h.preparations = {
			data: { pages: [{ items: [readyReport()] }] },
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementIdentityReadiness, { roots });

		await page.getByRole('button', { name: 'Dismiss report' }).click();
		await expect.element(page.getByRole('heading', { name: 'Dismiss this report?' })).toHaveFocus();
		await page
			.getByRole('dialog')
			.getByRole('button', { name: 'Dismiss report', exact: true })
			.click();

		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'preparation-1',
			expectedRevision: 7
		});
	});

	it('confirms the bulk edition accept with the apply mutation', async () => {
		h.preparations = {
			data: { pages: [{ items: [readyReport()] }] },
			isLoading: false,
			isError: false
		};
		h.findings.data.pages[0].current_counts_by_finding = {
			mapping_ready: 1,
			ready: 4,
			exact_release_required: 0,
			exact_release_suggested: 2,
			needs_review: 1
		};
		await render(LibraryManagementIdentityReadiness, { roots });

		await page.getByRole('button', { name: 'Accept editions (2)...' }).click();
		await page.getByRole('button', { name: 'Accept identities' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'preparation-1',
			expectedRevision: 7
		});
	});

	it('shows the undo action on an auto-accepted finding and fires it once', async () => {
		h.preparations = {
			data: { pages: [{ items: [readyReport()] }] },
			isLoading: false,
			isError: false
		};
		h.findings.data.pages[0].items[0] = {
			id: 'finding-auto',
			local_album_id: 'album-auto',
			album_title: 'Juturna',
			album_artist_name: 'Circa Survive',
			album_year: 2005,
			cover_available: false,
			evidence_id: 'evidence-auto',
			review_id: null,
			finding_code: 'exact_release_auto_accepted',
			reason_code: 'EXACT_EDITION_AUTO_ACCEPTED',
			confidence: 'complete',
			apply_eligible: false,
			state: 'applied',
			apply_result: 'EDITION_AUTO_ACCEPTED',
			suggested_edition: null,
			automatic_undo: {
				expected_album_revision: 4,
				expected_identity_revision: 2
			},
			updated_at: 10,
			row_revision: 3
		};
		await render(LibraryManagementIdentityReadiness, { roots });
		await page.getByRole('button', { name: /Auto-accepted/ }).click();

		await expect.element(page.getByText('Edition accepted automatically')).toBeVisible();
		const undo = page.getByRole('button', { name: 'Undo' });
		await expect.element(undo).toBeVisible();
		await undo.click();

		expect(h.undo).toHaveBeenCalledTimes(1);
		expect(h.undo).toHaveBeenCalledWith({
			albumId: 'album-auto',
			expectedAlbumRevision: 4,
			expectedIdentityRevision: 2
		});
	});
});
