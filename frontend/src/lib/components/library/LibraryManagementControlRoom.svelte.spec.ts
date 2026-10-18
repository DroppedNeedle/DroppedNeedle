import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	discard: vi.fn(),
	reissue: vi.fn(),
	resolveImportBundle: vi.fn(),
	goto: vi.fn(),
	replaceState: vi.fn(),
	apiGet: vi.fn(),
	apiPost: vi.fn(),
	toast: vi.fn(),
	invalidate: vi.fn(),
	appPage: {
		url: new URL('https://music.example.test/library/management#management-controls'),
		state: {}
	},
	operations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	},
	recovery: {
		data: {
			recoverable_bundle_count: 0,
			nonterminal_journal_count: 0,
			needs_attention_count: 0,
			cleanup_pending_count: 0,
			oldest_updated_at: null,
			state_counts: {},
			needs_attention_bundles: [] as Array<{ bundle_id: string }>
		},
		isLoading: false,
		isError: false
	},
	identityPreparations: {
		data: { pages: [{ items: [] as Array<Record<string, unknown>> }] },
		isLoading: false,
		isError: false
	},
	identityEstimate: {
		data: {
			album_count: 12,
			ready_album_count: 4,
			mapping_required_count: 6,
			exact_release_required_count: 2,
			selected_root_count: 0,
			queued_preparation_count: 0
		},
		isLoading: false,
		isError: false
	},
	admin: true,
	takeovers: {
		data: undefined as
			| {
					total: number;
					handing_back: number;
					items: Array<{
						album_id: string;
						album_title: string;
						artist_name: string | null;
						cause: string;
						noticed_at: number;
					}>;
			  }
			| undefined,
		isPending: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	},
	handBack: vi.fn(),
	handBackAll: vi.fn(),
	retagPairs: {
		data: undefined as { total: number; pairs: Array<Record<string, unknown>> } | undefined,
		isPending: false,
		isError: false,
		error: null,
		refetch: vi.fn()
	}
}));

vi.mock('$app/navigation', () => ({ goto: h.goto, replaceState: h.replaceState }));
vi.mock('$app/state', () => ({ page: h.appPage }));
vi.mock('$lib/api/client', () => ({
	api: { global: { get: h.apiGet, post: h.apiPost } },
	ApiError: class ApiError extends Error {},
	TransportError: class TransportError extends Error {}
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: h.toast } }));
vi.mock('$lib/queries/library-management/LibraryManagementInvalidation', () => ({
	invalidateLibraryManagementSurfaces: h.invalidate
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get isAdmin() {
			return h.admin;
		},
		user: { id: 'admin-1' }
	}
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => ({
		data: {
			policy_revision: 'policy-1',
			library_roots: [
				{ id: 'root-1', label: 'Archive', path: '/music', policy: 'automatic', rules: [] }
			]
		},
		isLoading: false,
		isError: false
	}),
	getLibraryPolicyTreeQuery: () => ({
		data: { policy_revision: 'policy-1', roots: [] },
		isSuccess: true,
		isLoading: false,
		isError: false
	}),
	getLibraryRestorableRootsQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibrarySearchQuery: () => ({ data: { artists: [], albums: [], tracks: [] } }),
	getLibraryAlbumDetailQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryMembershipQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistsInfiniteQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistThumbsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryStatsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRecentlyAddedQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumCopiesQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumTracksQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistDetailQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistAlbumsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryArtistAppearancesQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryScanScheduleQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryAlbumStatusQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getAlbumSearchQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));
vi.mock('$lib/queries/library-management/LibraryManagementEvents', () => ({
	createLibraryManagementEvents: () => ({ start: vi.fn(), stop: vi.fn() })
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementSettingsQuery: () => ({
		data: { root_assignments: [], profiles: [], settings_revision: 'settings-1' },
		isLoading: false,
		isError: false
	}),
	getLibraryManagementOperationsQuery: () => ({
		...h.operations
	}),
	getLibraryManagementRecoveryQuery: () => h.recovery,
	getLibraryManagementSettingsQueryOptions: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	getLibraryManagementTagEditorQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryManagementPresetDiffQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false
	}),
	getLibraryManagementActivationPreviewQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false
	}),
	getLibraryManagementPreviewQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryManagementPlanItemsQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryManagementOperationQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryManagementOperationResultsQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false
	}),
	getLibraryManagementAutomaticSkippedQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false
	}),
	getLibraryManagementActivationHealthQuery: () => ({
		data: undefined,
		isLoading: false,
		isError: false
	}),
	getLibraryManagementTakeoversQuery: () => h.takeovers,
	TAKEOVERS_POLL_MS: 5_000
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	controlLibraryManagementOperationMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	handBackLibraryAlbumMutation: () => ({ mutateAsync: h.handBack, isPending: false }),
	handBackAllLibraryAlbumsMutation: () => ({ mutateAsync: h.handBackAll, isPending: false }),
	handBackRefusalMessage: () => null,
	discardLibraryManagementPreviewMutation: () => ({ mutateAsync: h.discard, isPending: false }),
	reissueLibraryManagementPreviewMutation: () => ({ mutateAsync: h.reissue, isPending: false }),
	resolveLibraryManagementImportBundleMutation: () => ({
		mutateAsync: h.resolveImportBundle,
		isPending: false
	}),
	createLibraryManagementPreviewMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLibraryManagementBaselineRestorePreviewMutation: () => ({
		mutateAsync: vi.fn(),
		isPending: false
	}),
	updateLibraryManagementSettingsMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	previewLibraryManagementSettingsImpactMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	validateLibraryManagementSettingsMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	copyLibraryManagementProfileMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	deleteLibraryManagementProfileMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	exportLibraryManagementProfileMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	previewLibraryManagementProfileImportMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	importLibraryManagementProfileMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createLibraryManagementActivationPreviewMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	confirmLibraryManagementActivationMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createLibraryManagementTagEditPreviewMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	applyLibraryManagementPreviewMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createLibraryManagementUndoPreviewMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	createLibraryManagementDuplicateResolutionMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	previewLibraryManagementBaselinePurgeMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	}),
	purgeLibraryManagementBaselinesMutation: () => ({
		mutateAsync: vi.fn(),
		mutate: vi.fn(),
		isPending: false
	})
}));
vi.mock('$lib/queries/library/LibraryRetagPairQueries.svelte', () => ({
	getRetagPairsQuery: () => h.retagPairs,
	joinRetagPair: () => ({ mutateAsync: vi.fn(), isPending: false }),
	dismissRetagPair: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/albums/EditionQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<object>()),
	getAlbumEditionsQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationQueries.svelte', () => ({
	getLibraryIdentityPreparationsQuery: () => h.identityPreparations,
	getLibraryIdentityPreparationEstimateQuery: () => h.identityEstimate,
	getLibraryIdentityPreparationFindingsQuery: () => ({
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryIdentityPreparationMutations.svelte', () => ({
	createLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	applyLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	discardLibraryIdentityPreparation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	undoLibraryAutomaticEdition: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	controlLibraryRun: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false }),
	controlIdentification: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryRepairQueries.svelte', () => ({
	getLibraryRepairsQuery: () => ({ data: { pages: [{ items: [] }] }, isLoading: false }),
	getLibraryRepairEstimateQuery: () => ({ data: undefined, isLoading: false, isError: false }),
	getLibraryRepairFindingsQuery: () => ({
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false,
		hasNextPage: false
	})
}));
vi.mock('$lib/queries/library/LibraryRepairMutations.svelte', () => ({
	createLibraryRepair: () => ({ mutateAsync: vi.fn(), isPending: false }),
	applyLibraryRepair: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

import LibraryManagementControlRoom from './LibraryManagementControlRoom.svelte';

function failedOperation(
	id: string,
	overrides: Record<string, unknown> = {},
	operationOverrides: Record<string, unknown> = {}
): Record<string, unknown> {
	return {
		operation: {
			id,
			state: 'failed',
			terminal_code: 'STALE_INPUT',
			row_revision: 3,
			updated_at: 1_800_000_001,
			succeeded_count: 0,
			failed_count: 0,
			skipped_count: 0,
			...operationOverrides
		},
		profile_name: 'Picard-style Organizer',
		mode: 'preview',
		phase: 'planning',
		selection: { kind: 'albums', ids: ['album-1'] },
		...overrides
	};
}

function historyWith(items: Array<Record<string, unknown>>): {
	data: { pages: Array<{ items: Array<Record<string, unknown>> }> };
	isLoading: boolean;
	isError: boolean;
} {
	return { data: { pages: [{ items }] }, isLoading: false, isError: false };
}

function recoveryDataWith(overrides: Partial<typeof h.recovery.data> = {}): typeof h.recovery.data {
	return {
		recoverable_bundle_count: 0,
		nonterminal_journal_count: 0,
		needs_attention_count: 0,
		cleanup_pending_count: 0,
		oldest_updated_at: null,
		state_counts: {},
		needs_attention_bundles: [],
		...overrides
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	h.appPage.url = new URL('https://music.example.test/library/management#management-controls');
	h.operations = { data: { pages: [{ items: [] }] }, isLoading: false, isError: false };
	h.recovery.data = recoveryDataWith();
	h.recovery.isLoading = false;
	h.recovery.isError = false;
	h.admin = true;
	h.retagPairs.data = undefined;
	h.resolveImportBundle.mockResolvedValue({
		bundle_id: 'bundle-1',
		state: 'resolved',
		verified_files: 4,
		total_files: 4,
		note: 'Import bundle marked as handled (4/4 files verified).'
	});
	h.apiPost.mockResolvedValue({});
	h.reissue.mockResolvedValue({ job_id: 'reissued-1', preview_token: 'token-1' });
	h.invalidate.mockResolvedValue(undefined);
	h.identityPreparations = {
		data: { pages: [{ items: [] }] },
		isLoading: false,
		isError: false
	};
	h.discard.mockResolvedValue({});
	h.takeovers.data = undefined;
	h.handBack.mockResolvedValue({ handed_back: true });
	h.handBackAll.mockResolvedValue({ albums: 2 });
});

function takenOver(id: string, title: string) {
	return {
		album_id: id,
		album_title: title,
		artist_name: 'Signal Artist',
		cause: 'files_changed',
		noticed_at: Date.UTC(2026, 9, 1, 12)
	};
}

describe('albums left unmanaged', () => {
	const summary = /left unmanaged because/;

	it('says nothing when no album is left unmanaged', async () => {
		h.takeovers.data = { total: 0, handing_back: 0, items: [] };
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Ready previews')).toBeVisible();
		await expect.element(page.getByText(summary)).not.toBeInTheDocument();
	});

	it('lists the albums with one button each and no bulk button for one', async () => {
		h.takeovers.data = { total: 1, handing_back: 0, items: [takenOver('album-1', 'Kept Mine')] };
		await render(LibraryManagementControlRoom);

		await page
			.getByText('1 album is left unmanaged because its files were changed outside DroppedNeedle')
			.click();
		await expect.element(page.getByRole('link', { name: 'Kept Mine' })).toBeVisible();
		await page.getByRole('button', { name: 'Manage it again' }).click();

		expect(h.handBack).toHaveBeenCalledWith({ albumId: 'album-1' });
		await expect.element(page.getByRole('button', { name: /Manage all/ })).not.toBeInTheDocument();
	});

	it('hands every album back with one button, saying what that overwrites', async () => {
		h.takeovers.data = {
			total: 2,
			handing_back: 0,
			items: [takenOver('album-1', 'Kept Mine'), takenOver('album-2', 'Also Mine')]
		};
		await render(LibraryManagementControlRoom);

		await page
			.getByText(
				'2 albums are left unmanaged because their files were changed outside DroppedNeedle'
			)
			.click();
		await expect
			.element(
				page.getByText(
					'DroppedNeedle will retag and rename the files of all 2 albums from their editions.'
				)
			)
			.toBeVisible();
		await page.getByRole('button', { name: 'Manage all 2 again' }).click();

		expect(h.handBackAll).toHaveBeenCalledOnce();
		expect(h.handBack).not.toHaveBeenCalled();
	});

	it('says how many albums wait their turn to be handed back', async () => {
		h.takeovers.data = { total: 0, handing_back: 3, items: [] };
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('3 albums are being handed back.')).toBeVisible();
	});
});

function retagPair(id: string) {
	const album = (albumId: string, title: string) => ({
		id: albumId,
		title,
		album_artist_name: 'Signal Artist',
		track_count: 10,
		identity_revision: 1,
		edition: null
	});
	return {
		old_album: album(`${id}-old`, `Old ${id}`),
		new_album: album(`${id}-new`, `New ${id}`),
		reason: 'tracks_elsewhere',
		editions_differ: false,
		recorded_at: 0
	};
}

describe('albums that might be one album retagged', () => {
	it('says nothing when no pair is waiting', async () => {
		h.retagPairs.data = { total: 0, pairs: [] };
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Ready previews')).toBeVisible();
		await expect.element(page.getByText(/might (each )?be one album/)).not.toBeInTheDocument();
	});

	it('counts the pairs and opens to their cards', async () => {
		h.retagPairs.data = { total: 2, pairs: [retagPair('a'), retagPair('b')] };
		await render(LibraryManagementControlRoom);

		await page.getByText('2 pairs of albums might each be one album retagged').click();
		await expect
			.element(page.getByRole('article', { name: 'Might be one album: Old a and New a' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('article', { name: 'Might be one album: Old b and New b' }))
			.toBeVisible();
	});
});

describe('LibraryManagementControlRoom', () => {
	it('fails closed visually when recovery diagnostics are unavailable', async () => {
		h.recovery.isError = true;
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Status unavailable')).toBeVisible();
		await expect
			.element(page.getByRole('alert').getByText('Recovery status is unavailable'))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Preview organization...' }))
			.toBeDisabled();
	});

	it('confirms and discards a ready preview directly from its review card', async () => {
		h.operations = {
			data: {
				pages: [
					{
						items: [
							{
								operation: {
									id: 'preview-1',
									state: 'ready',
									row_revision: 7,
									updated_at: 1_800_000_000,
									failed_count: 0
								},
								profile_name: 'Picard-style Organizer',
								mode: 'preview',
								phase: 'ready'
							}
						]
					}
				]
			},
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Discard preview for Picard-style Organizer' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Discard this preview?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/No music file, tag, baseline/)).toBeVisible();
		await page.getByRole('button', { name: 'Discard preview', exact: true }).click();

		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: { expected_operation_row_revision: 7 }
		});
	});

	it('bulk-retries only the stale members with one toast summary', async () => {
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Retry 2 stale' }).click();

		await expect.element(page.getByText('Retried 2 of 2 stale previews.')).toBeVisible();
		expect(h.reissue).toHaveBeenCalledTimes(2);
		expect(h.reissue).toHaveBeenNthCalledWith(1, { jobId: 'stale-1', silent: true });
		expect(h.reissue).toHaveBeenNthCalledWith(2, { jobId: 'stale-2', silent: true });
		expect(h.toast).toHaveBeenCalledTimes(1);
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Retried 2 of 2 stale previews.',
			type: 'success'
		});
	});

	it('never bulk-retries real failures mixed in with stale attempts', async () => {
		h.operations = historyWith([
			failedOperation('stale-1'),
			failedOperation('real-1', {}, { terminal_code: 'PLANNING_FAILED', failed_count: 1 })
		]);
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Retry 1 stale' }).click();

		await expect.element(page.getByText('Retried 1 of 1 stale preview.')).toBeVisible();
		expect(h.reissue).toHaveBeenCalledTimes(1);
		expect(h.reissue).toHaveBeenCalledWith({ jobId: 'stale-1', silent: true });
		expect(h.toast).toHaveBeenCalledTimes(1);
	});

	it('bulk-dismisses stale previews with their expected revisions', async () => {
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await page.getByRole('button', { name: 'Dismiss 2 stale' }).click();

		await expect.element(page.getByText('Dismissed 2 of 2 stale previews.')).toBeVisible();
		expect(h.discard).toHaveBeenCalledTimes(2);
		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'stale-1',
			request: { expected_operation_row_revision: 3 },
			silent: true
		});
		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'stale-2',
			request: { expected_operation_row_revision: 3 },
			silent: true
		});
		expect(h.toast).toHaveBeenCalledTimes(1);
	});

	it('hides bulk stale actions from non-admins', async () => {
		h.admin = false;
		h.operations = historyWith([failedOperation('stale-1'), failedOperation('stale-2')]);
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('2 failed attempts · same album')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Retry .* stale/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: /Dismiss .* stale/ }))
			.not.toBeInTheDocument();
	});

	it('marks a stuck import bundle as handled and refreshes recovery state', async () => {
		h.recovery.data = recoveryDataWith({
			needs_attention_count: 1,
			needs_attention_bundles: [{ bundle_id: 'bundle-1' }]
		});
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Some file writes need you')).toBeVisible();
		await page.getByRole('button', { name: 'Mark bundle-1 as handled' }).click();

		await vi.waitFor(() => expect(h.resolveImportBundle).toHaveBeenCalledTimes(1));
		expect(h.resolveImportBundle).toHaveBeenCalledWith({ bundleId: 'bundle-1' });
		await vi.waitFor(() => expect(h.invalidate).toHaveBeenCalledOnce());
		expect(h.toast).toHaveBeenCalledWith({
			message: 'Import bundle marked as handled (4/4 files verified).',
			type: 'success'
		});
	});

	it('hides the import bundle resolve action from non-admins', async () => {
		h.admin = false;
		h.recovery.data = recoveryDataWith({
			needs_attention_count: 1,
			needs_attention_bundles: [{ bundle_id: 'bundle-1' }]
		});
		await render(LibraryManagementControlRoom);

		await expect.element(page.getByText('Some file writes need you')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Mark bundle-1 as handled' }))
			.not.toBeInTheDocument();
	});
});
