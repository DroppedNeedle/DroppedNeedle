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
		stale_input_count: 0,
		stale_sample_relative_paths: [],
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

		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
		const view = await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await vi.waitFor(() => expect(h.reissue).toHaveBeenCalledWith({ jobId: 'preview-1' }));
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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await expect
			.element(page.getByText(/Apply is disabled: the private apply token is missing/))
			.toBeVisible();
		expect(h.reissue).toHaveBeenCalledWith({ jobId: 'preview-1' });
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.toBeDisabled();
		await page.getByRole('button', { name: 'Retry token resume' }).click();
		await vi.waitFor(() => expect(h.reissue).toHaveBeenCalledTimes(2));
	});

	it('never preselects a collision action and disables recycling without a configured path', async () => {
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

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
