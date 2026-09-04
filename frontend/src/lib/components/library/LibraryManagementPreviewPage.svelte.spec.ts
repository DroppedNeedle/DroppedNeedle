import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	preview: {} as Record<string, unknown>,
	items: {} as Record<string, unknown>,
	apply: vi.fn(),
	discard: vi.fn(),
	resolve: vi.fn(),
	reissue: vi.fn(),
	goto: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: h.goto }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => ({
		data: {
			policy_revision: 'policy-1',
			library_roots: [
				{ id: 'root-1', label: 'Archive', path: '/secret/music', policy: 'automatic', rules: [] }
			]
		},
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibrarySearchQuery: () => ({ data: { artists: [], albums: [], tracks: [] } })
}));
vi.mock('$lib/queries/library-management/LibraryManagementEvents', () => ({
	createLibraryManagementEvents: () => ({ start: vi.fn(), stop: vi.fn() })
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementPreviewQuery: () => h.preview,
	getLibraryManagementPlanItemsQuery: () => h.items,
	getLibraryManagementSettingsQuery: () => ({
		data: { settings_revision: 'settings-1', recycle_bin_path: '' },
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	applyLibraryManagementPreviewMutation: () => ({ mutateAsync: h.apply, isPending: false }),
	discardLibraryManagementPreviewMutation: () => ({ mutateAsync: h.discard, isPending: false }),
	reissueLibraryManagementPreviewMutation: () => ({ mutateAsync: h.reissue, isPending: false }),
	createLibraryManagementDuplicateResolutionMutation: () => ({
		mutateAsync: h.resolve,
		isPending: false
	})
}));

import LibraryManagementPreviewPage from './LibraryManagementPreviewPage.svelte';

function detail(overrides: Record<string, unknown> = {}): Record<string, unknown> {
	return {
		job_id: 'preview-1',
		state: 'ready',
		phase: 'ready',
		mode: 'preview',
		origin: 'manual',
		profile_id: 'profile-1',
		profile_name: 'Picard-style Organizer',
		profile_revision: 'profile-revision-1',
		settings_revision: 'settings-1',
		policy_revision: 'policy-1',
		catalog_revision: 4,
		proposed_settings_revision: null,
		target_root_id: null,
		selection: { kind: 'tracks', ids: ['track-1'] },
		summary: {
			item_count: 2,
			bundle_count: 1,
			eligible_count: 1,
			warning_count: 0,
			blocked_count: 1,
			stale_count: 0,
			no_change_count: 0,
			tag_change_count: 1,
			artwork_change_count: 0,
			path_change_count: 1,
			sidecar_change_count: 0,
			estimated_temporary_bytes: 1024,
			expanded_track_count: 1,
			reasons: { PATH_COLLISION_DIFFERENT: 1 },
			roots: { 'root-1': 2 },
			formats: { flac: 2 },
			metadata_snapshot_ids: ['snapshot-1']
		},
		created_at: 1_800_000_000,
		updated_at: 1_800_000_000,
		expires_at: 1_900_000_000,
		expired: false,
		stale: false,
		stale_reasons: [],
		ready_for_confirmation: true,
		operation_row_revision: 7,
		operation_event_revision: 8,
		terminal_code: null,
		expected_work_count: 2,
		completed_count: 2,
		succeeded_count: 0,
		failed_count: 0,
		skipped_count: 0,
		control_request: 'none',
		...overrides
	};
}

const collisionItem = {
	ordinal: 0,
	bundle_ordinal: 0,
	local_album_id: 'album-1',
	local_track_id: 'track-1',
	source_root_id: 'root-1',
	source_relative_path: 'Incoming/track.flac',
	destination_root_id: 'root-1',
	destination_relative_path: 'Artist/Album/01 Track.flac',
	eligibility: 'blocked',
	reason_code: 'PATH_COLLISION_DIFFERENT',
	estimated_temporary_bytes: 1024,
	desired_document: {
		fields: [
			{ name: 'title', value: 'Track' },
			{ name: 'artist', value: ['Artist'] },
			{ name: 'album', value: 'Album' }
		]
	},
	artwork_choices: [],
	diff: {
		requires_write: true,
		tags_changed: true,
		path_changed: true,
		field_mutations: [
			{
				name: 'title',
				operation: 'set',
				before: 'Old title',
				after: 'Track',
				representation_loss: null
			}
		]
	},
	capability: { audio_format: 'flac', adapter: 'mutagen.flac', blockers: [], warnings: [] },
	collisions: [
		{
			classification: 'same_path_different_content',
			existing_root_id: 'root-1',
			existing_relative_path: 'Artist/Album/01 Track.flac'
		}
	]
};

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	h.preview = { data: detail(), isLoading: false, isError: false };
	h.items = {
		data: { pages: [{ items: [collisionItem], has_more: false, next_after_ordinal: null }] },
		isLoading: false,
		isError: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		fetchNextPage: vi.fn()
	};
	h.apply.mockResolvedValue({ id: 'preview-1' });
	h.reissue.mockResolvedValue({
		job_id: 'preview-1',
		preview_token: 'resumed-token',
		created_at: 1_800_000_000,
		expires_at: 1_900_000_000
	});
	h.discard.mockResolvedValue(
		detail({
			state: 'cancelled',
			ready_for_confirmation: false,
			terminal_code: 'PREVIEW_DISCARDED'
		})
	);
});

describe('LibraryManagementPreviewPage', () => {
	it('explains profile script failures without calling them path-length failures', async () => {
		h.preview = {
			data: detail({
				summary: {
					...(detail().summary as Record<string, unknown>),
					reasons: { SCRIPT_VALIDATION_FAILED: 1 }
				}
			}),
			isLoading: false,
			isError: false
		};
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								reason_code: 'SCRIPT_VALIDATION_FAILED',
								collisions: []
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Profile script could not safely process this file').last())
			.toBeVisible();
		await expect.element(page.getByText(/path exceeds/i)).not.toBeInTheDocument();
	});

	it('explains identity blockers and links back to identity readiness', async () => {
		h.preview = {
			data: detail({
				summary: {
					...(detail().summary as Record<string, unknown>),
					reasons: { TRACK_NOT_MAPPED: 12, RELEASE_NOT_SELECTED: 4 }
				}
			}),
			isLoading: false,
			isError: false
		};
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								source_relative_path: 'Trapeze/Hot Wire/08 Feel It Inside.mp3',
								destination_root_id: null,
								destination_relative_path: null,
								reason_code: 'TRACK_NOT_MAPPED',
								desired_document: {},
								capability: {
									audio_format: 'mp3',
									catalog_track_title: 'Feel It Inside',
									catalog_artist_name: 'Trapeze',
									catalog_album_title: 'Hot Wire',
									catalog_album_artist_name: 'Trapeze',
									catalog_disc_number: 1,
									catalog_track_number: 8,
									album_artwork_version: 7
								},
								collisions: []
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('16 files need identity preparation.')).toBeVisible();
		await expect.element(page.getByText(/Selecting a root chooses files/)).toBeVisible();
		await expect
			.element(
				page.getByRole('article').getByText('Exact edition selected; track map missing').first()
			)
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open identity readiness' }))
			.toHaveAttribute('href', '/library/management?tab=organize');
		await expect.element(page.getByText('TRACK NOT MAPPED')).not.toBeInTheDocument();
		await expect.element(page.getByRole('heading', { name: 'Hot Wire' })).toBeVisible();
		await expect.element(page.getByText('Trapeze · 1 file')).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Feel It Inside' })).toBeVisible();
		await expect
			.element(page.getByTestId('management-dossier-artwork'))
			.toHaveAttribute('data-src', '/api/v1/library/albums/album-1/artwork/cached?v=7');
		await expect.element(page.getByText('No root · No path')).not.toBeInTheDocument();
	});
	it('lists reasons with human labels and counts, most frequent first', async () => {
		h.preview = {
			data: detail({
				summary: {
					...(detail().summary as Record<string, unknown>),
					reasons: { RELEASE_NOT_SELECTED: 4, PATH_COLLISION_DIFFERENT: 12 }
				}
			}),
			isLoading: false,
			isError: false
		};
		const { container } = render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await expect.element(page.getByText('Files and changes')).toBeVisible();
		const reasonSelect = [...container.querySelectorAll('select')].find((select) =>
			select.textContent?.includes('All reasons')
		);
		expect([...(reasonSelect?.options ?? [])].map((option) => option.textContent?.trim())).toEqual([
			'All reasons',
			'Different file already at the destination (12)',
			'Exact MusicBrainz edition not chosen (4)'
		]);
	});
	it('heads each dossier with its top reason and reveals row reasons on selection', async () => {
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: []
							},
							{
								...collisionItem,
								ordinal: 1,
								local_track_id: 'track-2',
								source_relative_path: 'Incoming/track-two.flac',
								destination_relative_path: 'Artist/Album/02 Track Two.flac',
								desired_document: {
									fields: [
										{ name: 'title', value: 'Track Two' },
										{ name: 'artist', value: ['Artist'] },
										{ name: 'album', value: 'Album' }
									]
								},
								collisions: []
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await expect
			.element(page.getByText('Top reason: Different file already at the destination (1)'))
			.toBeVisible();
		await expect
			.element(
				page
					.getByRole('button', { name: 'Inspect exact diff for Track Two' })
					.getByText('Different file already at the destination')
			)
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Inspect exact diff for Track Two' }).click();
		await expect
			.element(
				page
					.getByRole('button', { name: 'Inspect exact diff for Track Two' })
					.getByText('Different file already at the destination')
			)
			.toBeVisible();
		const trackButton = page.getByRole('button', { name: 'Inspect exact diff for Track Two' });
		await expect.element(trackButton).toHaveAttribute('aria-controls', 'management-inspector');
		await expect.element(trackButton).toHaveAttribute('aria-expanded', 'true');
		const inspector = page.getByTestId('management-audit-inspector');
		await expect.element(inspector).toHaveAttribute('aria-live', 'polite');
		(document.activeElement as HTMLElement | null)?.blur?.();
		inspector
			.element()
			.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
		await expect.element(trackButton).toHaveFocus();
	});

	it('counts outcomes, roots, formats, and changes in every summary-backed filter', async () => {
		const { container } = render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await expect.element(page.getByText('Files and changes')).toBeVisible();
		const options = (label: string) =>
			[
				...([...container.querySelectorAll('select')].find((select) =>
					select.textContent?.includes(label)
				)?.options ?? [])
			].map((option) => option.textContent?.trim());
		expect(options('All outcomes')).toEqual([
			'All outcomes',
			'Eligible (1)',
			'Warning (0)',
			'Blocked (1)',
			'Stale (0)'
		]);
		expect(options('All roots')).toEqual(['All roots', 'Archive (2)']);
		expect(options('All formats')).toEqual(['All formats', 'FLAC (2)']);
		expect(options('All changes')).toEqual([
			'All changes',
			'Tags (1)',
			'Artwork (0)',
			'Path (1)',
			'Sidecars (0)',
			'No change (0)'
		]);
	});

	it('diagnoses a zero-appliable preview with top blockers, culprit release, and identity path', async () => {
		h.preview = {
			data: detail({
				summary: {
					...(detail().summary as Record<string, unknown>),
					eligible_count: 0,
					warning_count: 0,
					blocked_count: 16,
					reasons: { TRACK_NOT_MAPPED: 12, RELEASE_NOT_SELECTED: 4 },
					deferred_sources: { lrclib: 3 }
				}
			}),
			isLoading: false,
			isError: false
		};
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								ordinal: 5,
								bundle_ordinal: 2,
								reason_code: 'TRACK_NOT_MAPPED',
								collisions: []
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		const { container } = render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await expect.element(page.getByText('Nothing in this preview can be applied.')).toBeVisible();
		await expect.element(page.getByText(/Top blockers:/)).toBeVisible();
		await expect.element(page.getByText(/Start with Release 3/)).toBeVisible();
		await expect.element(page.getByText(/Deferred warnings:/)).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Open identity readiness' })).toBeVisible();
		await expect
			.element(page.getByText('16 files need identity preparation.'))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: /Filter to Exact edition/ }).click();
		await expect
			.element(page.getByText('Apply is disabled: no eligible files and no files with warnings.'))
			.toBeVisible();
		const reasonSelect = [...container.querySelectorAll('select')].find((select) =>
			select.textContent?.includes('All reasons')
		);
		expect(reasonSelect?.value).toBe('TRACK_NOT_MAPPED');
	});

	it('groups release files into compact dossiers with persistent inspection controls', async () => {
		const secondItem = {
			...collisionItem,
			ordinal: 1,
			local_track_id: 'track-2',
			source_relative_path: 'Incoming/track-two.flac',
			destination_relative_path: 'Artist/Album/02 Track Two.flac',
			eligibility: 'warning',
			reason_code: 'OPTIONAL_ENRICHMENT_DEFERRED',
			desired_document: {
				fields: [
					{ name: 'title', value: 'Track Two' },
					{ name: 'artist', value: ['Artist'] },
					{ name: 'album_artist', value: ['Artist'] },
					{ name: 'album', value: 'Album' },
					{ name: 'track_number', value: 2 }
				]
			},
			collisions: []
		};
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: []
							},
							secondItem
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByRole('heading', { name: 'Album' }).first()).toBeVisible();
		await expect.element(page.getByText('2 files')).toBeVisible();
		await page.getByRole('button', { name: 'Inspect exact diff for Track Two' }).click();
		await expect.element(page.getByRole('heading', { name: 'Track Two' })).toBeVisible();

		await page.getByRole('checkbox', { name: 'Show full paths' }).click();
		await expect
			.element(
				page
					.getByRole('button', { name: 'Inspect exact diff for Track Two' })
					.getByText('Archive · Incoming/track-two.flac')
			)
			.toBeVisible();

		await page.getByRole('checkbox', { name: 'Only exceptions' }).click();
		await expect
			.element(page.getByRole('button', { name: 'Inspect exact diff for Track', exact: true }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: 'Inspect exact diff for Track Two' }))
			.toBeVisible();

		await page.getByRole('button', { name: 'Collapse Album' }).click();
		await expect
			.element(page.getByRole('button', { name: 'Inspect exact diff for Track Two' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Expand Album' }).click();
		await expect
			.element(page.getByRole('button', { name: 'Inspect exact diff for Track Two' }))
			.toBeVisible();
	});

	it('counts only real tag changes and explains exact and rejected lyrics evidence', async () => {
		const exactItem = {
			...collisionItem,
			source_relative_path: 'Artist/Album/01 Exact Match.flac',
			destination_relative_path: 'Artist/Album/01 Exact Match.flac',
			eligibility: 'eligible',
			reason_code: null,
			collisions: [],
			desired_document: {
				fields: [
					{ name: 'title', value: 'Exact Match' },
					{ name: 'artist', value: ['Artist'] },
					{ name: 'album_artist', value: ['Artist'] },
					{ name: 'album', value: 'Album' },
					{ name: 'lyrics_plain', value: 'Pinned lyrics' }
				]
			},
			diff: {
				requires_write: true,
				tags_changed: true,
				path_changed: false,
				field_mutations: [
					{
						name: 'title',
						operation: 'unchanged',
						before: 'Exact Match',
						after: 'Exact Match',
						representation_loss: null
					},
					{
						name: 'lyrics_plain',
						operation: 'set',
						before: null,
						after: 'Pinned lyrics',
						representation_loss: null
					}
				],
				lyrics_projection: {
					status: 'available',
					provider_id: 101,
					provider_revision: 'lyrics-1',
					reason: null,
					plain_available: true,
					synced_available: true,
					plain_selected: true,
					synced_selected: true,
					preserve_existing: true
				}
			}
		};
		const mismatchItem = {
			...exactItem,
			ordinal: 1,
			local_track_id: 'track-2',
			source_relative_path: 'Artist/Album/02 Existing Lyrics.flac',
			destination_relative_path: 'Artist/Album/02 Existing Lyrics.flac',
			eligibility: 'warning',
			reason_code: 'OPTIONAL_ENRICHMENT_DEFERRED',
			desired_document: {
				fields: [
					{ name: 'title', value: 'Existing Lyrics' },
					{ name: 'artist', value: ['Artist'] },
					{ name: 'album_artist', value: ['Artist'] },
					{ name: 'album', value: 'Album' }
				]
			},
			diff: {
				requires_write: false,
				tags_changed: false,
				path_changed: false,
				field_mutations: [
					{
						name: 'title',
						operation: 'unchanged',
						before: 'Existing Lyrics',
						after: 'Existing Lyrics',
						representation_loss: null
					},
					{
						name: 'artist',
						operation: 'preserve',
						before: ['Artist'],
						after: ['Artist'],
						representation_loss: null
					}
				],
				lyrics_projection: {
					status: 'mismatch',
					provider_id: 102,
					provider_revision: 'lyrics-2',
					reason: 'LRCLIB returned a different recording signature.',
					plain_available: false,
					synced_available: false,
					plain_selected: true,
					synced_selected: false,
					preserve_existing: false
				}
			}
		};
		h.preview = {
			data: detail({
				summary: {
					...(detail().summary as Record<string, unknown>),
					item_count: 2,
					eligible_count: 1,
					warning_count: 1,
					blocked_count: 0,
					tag_change_count: 1,
					path_change_count: 0,
					reasons: { OPTIONAL_ENRICHMENT_DEFERRED: 1 }
				}
			}),
			isLoading: false,
			isError: false
		};
		h.items = {
			...h.items,
			data: {
				pages: [{ items: [exactItem, mismatchItem], has_more: false, next_after_ordinal: null }]
			}
		};

		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('1 tags', { exact: true })).toBeVisible();
		await expect.element(page.getByText('2 tags', { exact: true })).not.toBeInTheDocument();
		await expect.element(page.getByText('Exact match pinned')).toBeVisible();
		await expect.element(page.getByText(/LRCLIB matched the exact title/)).toBeVisible();
		await expect.element(page.getByText('Will be written')).toBeVisible();
		await expect.element(page.getByText('Existing lyrics preserved')).toBeVisible();

		await page.getByRole('button', { name: 'Inspect exact diff for Existing Lyrics' }).click();
		await expect.element(page.getByText('Signature mismatch')).toBeVisible();
		await expect
			.element(page.getByText('LRCLIB returned a different recording signature.'))
			.toBeVisible();
		await expect
			.element(page.getByText(/Any lyrics already in this file remain untouched/))
			.toBeVisible();
	});

	it('discloses native tags selected for explicit scrub', async () => {
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: [],
								diff: {
									...collisionItem.diff,
									path_changed: false,
									scrubbed_raw_tags: [
										{
											key: 'droppedneedle_acceptance_marker',
											value_kind: 'text',
											values: ['scrub-me-2026-08-07'],
											value_count: 1,
											truncated: false,
											sha256: 'a'.repeat(64)
										}
									]
								}
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};

		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('button', { name: 'Inspect exact diff for Track' }).click();
		await expect.element(page.getByText('Unmanaged tags to remove')).toBeVisible();
		await expect.element(page.getByText('droppedneedle_acceptance_marker')).toBeVisible();
		await expect.element(page.getByText('scrub-me-2026-08-07')).toBeVisible();
		await expect.element(page.getByText('Remove', { exact: true })).toBeVisible();
	});

	it('shows exact diffs and requires the private token plus typed apply confirmation', async () => {
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								artwork_choices: [
									{
										output_kind: 'external_art',
										image_type: 'front',
										blob_sha256: 'a'.repeat(64),
										source: 'cover_art_archive_release',
										format: 'jpeg',
										mime_type: 'image/jpeg',
										width: 1200,
										height: 1200,
										destination_relative_path: 'Artist/Album/cover.jpg'
									}
								]
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Organization preview' })).toBeVisible();
		await expect.element(page.getByText('1 tag change', { exact: true })).toBeVisible();
		await expect.element(page.getByText('1 path change', { exact: true })).toBeVisible();
		await expect.element(page.getByText('0 artwork changes', { exact: true })).toBeVisible();
		await expect.element(page.getByText('/secret/music')).not.toBeInTheDocument();
		await page.getByText('Inspect exact diff').click();
		await expect.element(page.getByText('Old title')).toBeVisible();
		await expect.element(page.getByText('Track', { exact: true }).first()).toBeVisible();
		await expect.element(page.getByText('External Art · Front')).toBeVisible();
		await expect
			.element(page.getByRole('img', { name: 'Front preview' }))
			.toHaveAttribute(
				'src',
				`/api/v1/library/management/previews/preview-1/items/0/artwork/${'a'.repeat(64)}`
			);
		await expect
			.element(page.getByText(/Cover Art Archive Release · 1,200 × 1,200 px/))
			.toBeVisible();
		await expect.element(page.getByText('Artist/Album/cover.jpg')).toBeVisible();

		await page.getByRole('button', { name: /Write tags and organize 1 file/ }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Apply this exact preview?' }))
			.toHaveFocus();
		await expect.element(page.getByRole('button', { name: 'Apply exact preview' })).toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('APPLY LIBRARY MANAGEMENT');
		await expect.element(page.getByRole('button', { name: 'Apply exact preview' })).toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Apply exact preview' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: expect.objectContaining({
				preview_token: 'private-token',
				expected_operation_row_revision: 7,
				confirmation: true
			})
		});
		expect(h.goto).toHaveBeenCalledWith('/library/management/operations/preview-1');
	});

	it('uses the new token when navigation reuses the page for another preview', async () => {
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'source-token'
		);
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:resolution-1',
			'resolution-token'
		);
		const view = render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await view.rerender({ jobId: 'resolution-1' });
		await page.getByRole('button', { name: /Write tags and organize 1 file/ }).click();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Apply exact preview' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'resolution-1',
			request: expect.objectContaining({ preview_token: 'resolution-token' })
		});
	});

	it('resumes apply after a browser restart by reissuing the sealed token', async () => {
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await vi.waitFor(() => expect(h.reissue).toHaveBeenCalledWith('preview-1'));
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize 1 file/ }))
			.toBeEnabled();
		await expect
			.element(page.getByText('The private apply token is not in this browser session.'))
			.not.toBeInTheDocument();
		expect(sessionStorage.getItem('droppedneedle:library-management:preview-token:preview-1')).toBe(
			'resumed-token'
		);

		await page.getByRole('button', { name: /Write tags and organize 1 file/ }).click();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Apply exact preview' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: expect.objectContaining({
				preview_token: 'resumed-token',
				expected_operation_row_revision: 7,
				confirmation: true
			})
		});
	});

	it('keeps apply disabled with a clause-specific reason and a token retry when reissue is denied', async () => {
		h.reissue.mockRejectedValueOnce(new Error('Forbidden'));
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await expect
			.element(page.getByText(/Apply is disabled: the private apply token is missing/))
			.toBeVisible();
		expect(h.reissue).toHaveBeenCalledWith('preview-1');
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.toBeDisabled();
		await page.getByRole('button', { name: 'Retry token resume' }).click();
		await vi.waitFor(() => expect(h.reissue).toHaveBeenCalledTimes(2));
	});

	it('shows the sealed metadata, artwork, and file-attribute state for recovery previews', async () => {
		h.preview = { data: detail({ mode: 'undo' }), isLoading: false, isError: false };
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: [],
								desired_document: {
									fields: [
										...collisionItem.desired_document.fields,
										{
											name: 'musicbrainz_release_group_id',
											value: '4b6276da-e7c7-36df-8771-34b92f774d3b'
										}
									]
								},
								capability: { audio_format: 'flac', restoration: true, album_artwork_version: 7 },
								artwork_choices: [
									{
										output_kind: 'external',
										blob_sha256: 'e'.repeat(64),
										destination_relative_path: 'Artist/Album/cover.png'
									}
								],
								diff: {
									requires_write: true,
									tags_changed: true,
									artwork_changed: true,
									path_changed: true,
									field_mutations: [
										{
											name: 'title',
											operation: 'set',
											before: 'The Fisherman Will Be Bewildered',
											after: 'The Fisherman Will Be Bewildered (H&D EP Version)',
											representation_loss: null
										}
									],
									restoration: {
										scope: 'operation_before_state',
										native_tags: {
											changed: true,
											current_primary_entries: 32,
											restored_primary_entries: 18,
											current_auxiliary_entries: 0,
											restored_auxiliary_entries: 0,
											current_encoded_primary: false,
											restored_encoded_primary: false,
											current_fingerprint: 'a'.repeat(64),
											restored_fingerprint: 'b'.repeat(64),
											changed_raw_keys: ['ARTIST', 'TITLE']
										},
										artwork: {
											changed: true,
											current: [
												{
													image_type: 'front',
													mime_type: 'image/jpeg',
													description: '',
													width: 1200,
													height: 1200,
													byte_size: 402600,
													sha256: 'c'.repeat(64)
												}
											],
											restored: [
												{
													image_type: 'front',
													mime_type: 'image/jpeg',
													description: '',
													width: 700,
													height: 700,
													byte_size: 206012,
													sha256: 'd'.repeat(64)
												}
											]
										},
										file_attributes: {
											changed: true,
											current_mtime_ns: '1800000000000000000',
											restored_mtime_ns: '1700000000000000000',
											current_permission_bits: 420,
											restored_permission_bits: 420
										}
									}
								}
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByText('Inspect exact diff').click();
		await expect.element(page.getByText('Sealed restoration snapshot')).toBeVisible();
		await expect.element(page.getByText('Operation before-state')).toBeVisible();
		await expect
			.element(page.getByText('The Fisherman Will Be Bewildered (H&D EP Version)'))
			.toBeVisible();
		await expect.element(page.getByText(/Primary entries/)).toBeVisible();
		await expect.element(page.getByText(/Native keys:/)).toBeVisible();
		await expect.element(page.getByText(/1,200 × 1,200 px/)).toBeVisible();
		await expect.element(page.getByText(/700 × 700 px/)).toBeVisible();
		await expect.element(page.getByText('0644 → 0644')).toBeVisible();
		await expect
			.element(page.getByTestId('management-dossier-artwork'))
			.toHaveAttribute(
				'data-src',
				`/api/v1/library/management/previews/preview-1/items/0/artwork/${'e'.repeat(64)}`
			);

		const artwork = page.getByTestId('management-dossier-artwork').element();
		const artworkFrame = page.getByTestId('management-dossier-art-frame').element();
		expect(artworkFrame.children).toHaveLength(1);
		expect(artwork.parentElement?.parentElement).toBe(artworkFrame);

		await expect
			.element(page.getByTestId('management-audit-layout'))
			.toHaveAttribute('data-reserve-sticky-footer', 'true');
		expect(page.getByTestId('management-audit-layout').element().getAttribute('style')).toContain(
			'--management-sticky-footer-height:'
		);
	});

	it('uses the catalog cover for an Undo preview that removes embedded artwork', async () => {
		h.preview = { data: detail({ mode: 'undo' }), isLoading: false, isError: false };
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: [],
								desired_document: {
									fields: [
										...collisionItem.desired_document.fields,
										{
											name: 'musicbrainz_release_group_id',
											value: '4b6276da-e7c7-36df-8771-34b92f774d3b'
										}
									]
								},
								artwork_choices: [],
								capability: {
									audio_format: 'mp3',
									restoration: true,
									album_artwork_version: 7
								},
								diff: { ...collisionItem.diff, artwork_changed: true }
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};

		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByTestId('management-dossier-artwork'))
			.toHaveAttribute('data-src', '/api/v1/library/albums/album-1/artwork/cached?v=7');
	});

	it('uses the pinned baseline identity and catalog cover when no artwork output is planned', async () => {
		h.preview = { data: detail({ mode: 'baseline_restore' }), isLoading: false, isError: false };
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: [],
								desired_document: {
									fields: [
										{ name: 'title', action: 'unchanged', value: 'She Loves Me So' },
										{ name: 'artist', action: 'unchanged', value: 'Anthony Green' },
										{ name: 'album', action: 'unchanged', value: 'Avalon' },
										{ name: 'album_artist', action: 'unchanged', value: 'Anthony Green' }
									]
								},
								artwork_choices: [],
								capability: { audio_format: 'flac', album_artwork_version: 7 }
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};

		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByRole('heading', { name: 'Avalon' })).toBeVisible();
		await expect.element(page.getByText('Anthony Green · 1 file')).toBeVisible();
		await expect
			.element(page.getByTestId('management-dossier-artwork'))
			.toHaveAttribute('data-src', '/api/v1/library/albums/album-1/artwork/cached?v=7');
	});

	it('never preselects a collision action and disables recycling without a configured path', async () => {
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await page.getByText('Inspect exact diff').click();
		await page.getByRole('button', { name: 'Choose resolution...' }).click();

		await expect
			.element(page.getByRole('heading', { name: 'Choose a collision resolution' }))
			.toHaveFocus();
		await expect.element(page.getByRole('radio', { name: /Keep existing/ })).not.toBeChecked();
		await expect
			.element(page.getByRole('radio', { name: /Keep incoming at an alternate/ }))
			.not.toBeChecked();
		await expect.element(page.getByRole('radio', { name: /Recycle existing/ })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Generate resolution preview' }))
			.toBeDisabled();
	});

	it('makes stale and expired plans impossible to apply', async () => {
		h.preview = {
			data: detail({ stale: true, expired: true, ready_for_confirmation: false }),
			isLoading: false,
			isError: false
		};
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await expect.element(page.getByText('This preview cannot be applied.')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.toBeDisabled();
	});

	it('confirms discard, forgets the apply token, and returns to the control room', async () => {
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('button', { name: 'Discard preview...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Discard this preview?' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Discard preview', exact: true }).click();

		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: { expected_operation_row_revision: 7 }
		});
		expect(
			sessionStorage.getItem('droppedneedle:library-management:preview-token:preview-1')
		).toBeNull();
		expect(h.goto).toHaveBeenCalledWith('/library/management?tab=organize');
	});

	it('renders a discarded audit plan without any write action', async () => {
		h.preview = {
			data: detail({
				state: 'cancelled',
				ready_for_confirmation: false,
				terminal_code: 'PREVIEW_DISCARDED'
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('Discarded', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('This preview is no longer awaiting confirmation.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: /Discard preview/ }))
			.not.toBeInTheDocument();
	});

	it('shows terminal planning failure instead of an endless planning state', async () => {
		h.preview = {
			data: detail({
				state: 'failed',
				phase: 'planning',
				ready_for_confirmation: false,
				terminal_code: 'PLANNING_FAILED',
				failed_count: 1
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('Preview planning failed.')).toBeVisible();
		await expect.element(page.getByText('Planning Failed')).toBeVisible();
		await expect.element(page.getByText(/Planning is still read-only/)).not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.not.toBeInTheDocument();
	});

	it('reports incremental read-only planning without claiming a zero-sized total', async () => {
		h.preview = {
			data: detail({
				state: 'running',
				phase: 'planning',
				ready_for_confirmation: false,
				expected_work_count: 0,
				completed_count: 0,
				summary: {
					...(detail().summary as Record<string, unknown>),
					item_count: 1000,
					bundle_count: 109
				}
			}),
			isLoading: false,
			isError: false
		};
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText(/1,000 files are planned so far/)).toBeVisible();
		await expect.element(page.getByText(/0 of 0 items inspected/)).not.toBeInTheDocument();
	});

	it.each([
		{
			mode: 'undo',
			button: 'Undo this operation for 1 file',
			title: 'Undo this operation from this exact preview?',
			confirm: 'Undo operation',
			detail: /does not restore the broader original baseline/
		},
		{
			mode: 'baseline_restore',
			button: 'Restore original state for 1 file',
			title: 'Restore these original baselines?',
			confirm: 'Restore original state',
			detail: /broader than Undo and leaves those files unmanaged/
		},
		{
			mode: 'duplicate_resolution',
			button: 'Apply collision resolution for 1 file',
			title: 'Apply this exact collision resolution?',
			confirm: 'Apply collision resolution',
			detail: /No destination is overwritten and no duplicate is deleted automatically/
		}
	])('uses consequence-specific confirmation copy for $mode', async (example) => {
		h.preview = { data: detail({ mode: example.mode }), isLoading: false, isError: false };
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('button', { name: example.button }).click();
		await expect.element(page.getByRole('heading', { name: example.title })).toHaveFocus();
		await expect.element(page.getByText(example.detail)).toBeVisible();
		await expect.element(page.getByRole('textbox', { name: 'Type CONFIRM' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: example.confirm, exact: true }))
			.toBeDisabled();
	});

	it('keeps activation previews read-only while exposing every file-level result', async () => {
		h.preview = {
			data: detail({ proposed_settings_revision: 'settings-2' }),
			isLoading: false,
			isError: false
		};
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('Activation dry run')).toBeVisible();
		await expect.element(page.getByText(/This page is read-only/)).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Library settings' }))
			.toHaveAttribute('href', '/settings?tab=library');
	});
});
