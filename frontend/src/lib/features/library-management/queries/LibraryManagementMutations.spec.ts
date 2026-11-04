import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('$lib/api/client', () => ({
	api: {
		v3: {
			POST: vi.fn().mockResolvedValue({}),
			PUT: vi.fn().mockResolvedValue({}),
			DELETE: vi.fn().mockResolvedValue({})
		}
	}
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));

import { api } from '$lib/api/client';
import type { Effect } from '$lib/queries/effects';
import {
	createLibraryManagementDuplicateResolutionMutation,
	createLibraryManagementTagEditPreviewMutation,
	createDeleteLibraryManagementProfileMutation,
	createDiscardLibraryManagementPreviewMutation,
	createExportLibraryManagementProfileMutation,
	createImportLibraryManagementProfileMutation,
	createPreviewLibraryManagementProfileImportMutation,
	createReissueLibraryManagementPreviewMutation,
	createResolveLibraryManagementImportBundleMutation,
	createUpdateLibraryManagementSettingsMutation
} from './LibraryManagementMutations.svelte';

type Method = 'POST' | 'PUT' | 'DELETE';

beforeEach(() => {
	vi.clearAllMocks();
});

describe('Library Management mutations', () => {
	it('send each write to its endpoint with the revisions it was given, and nothing added', async () => {
		const duplicateChoice = {
			source_job_id: 'job-1',
			source_plan_item_ordinal: 3,
			expected_source_operation_row_revision: 7,
			collision_kind: 'same_path_different_content',
			existing_root_id: 'root-1',
			existing_relative_path: 'Artist/Album/01.flac',
			action: 'keep_incoming_alternate',
			alternate_relative_path: 'Artist/Album/01 (2).flac',
			expected_settings_revision: 'settings',
			expected_policy_revision: 'policy',
			idempotency_key: 'resolve-once'
		};
		const importRequest = {
			content: 'DNLP1:code',
			reviewed_bundle_hash: 'hash',
			name: 'Shared profile',
			expected_settings_revision: 'rev-1'
		};
		const cases: {
			create: () => unknown;
			input: unknown;
			method: Method;
			args: unknown[];
			effects?: string[];
			answer?: unknown;
			returns?: Record<string, unknown>;
		}[] = [
			{
				create: createUpdateLibraryManagementSettingsMutation,
				input: { settings: { schema_version: 1 }, expected_settings_revision: 'rev-1' },
				method: 'PUT',
				args: [
					'/api/v3/settings/library-management',
					{ settings: { schema_version: 1 }, expected_settings_revision: 'rev-1' }
				],
				effects: ['catalogChanged']
			},
			// A profile delete carries its revision in the DELETE body.
			{
				create: createDeleteLibraryManagementProfileMutation,
				input: { profileId: 'profile/1', request: { expected_settings_revision: 'rev-2' } },
				method: 'DELETE',
				args: [
					'/api/v3/settings/library-management/profiles/profile%2F1',
					{ expected_settings_revision: 'rev-2' }
				]
			},
			{
				create: createExportLibraryManagementProfileMutation,
				input: { profileId: 'profile/1', request: { expected_settings_revision: 'rev-1' } },
				method: 'POST',
				args: [
					'/api/v3/settings/library-management/profiles/profile%2F1/export',
					{ expected_settings_revision: 'rev-1' }
				]
			},
			{
				create: createPreviewLibraryManagementProfileImportMutation,
				input: { content: 'DNLP1:code', expected_settings_revision: 'rev-1' },
				method: 'POST',
				args: [
					'/api/v3/settings/library-management/profile-imports/preview',
					{ content: 'DNLP1:code', expected_settings_revision: 'rev-1' }
				]
			},
			{
				create: createImportLibraryManagementProfileMutation,
				input: importRequest,
				method: 'POST',
				args: ['/api/v3/settings/library-management/profile-imports', importRequest],
				effects: ['catalogChanged']
			},
			// The explicit duplicate choice goes whole, with no default added.
			{
				create: createLibraryManagementDuplicateResolutionMutation,
				input: duplicateChoice,
				method: 'POST',
				args: ['/api/v3/library/management/duplicate-resolution-previews', duplicateChoice]
			},
			// List-valued tag edits go to the staged preview.
			{
				create: createLibraryManagementTagEditPreviewMutation,
				input: {
					local_track_id: 'track-1',
					mode: 'save_override',
					expected_settings_revision: 'settings-1',
					expected_policy_revision: 'policy-1',
					fields: [
						{ field_name: 'artist', value: ['Björk', 'PJ Harvey'] },
						{ field_name: 'genre', value: ['Art Pop', 'Electronic'] }
					]
				},
				method: 'POST',
				args: [
					'/api/v3/library/management/tag-edit-previews',
					{
						local_track_id: 'track-1',
						mode: 'save_override',
						expected_settings_revision: 'settings-1',
						expected_policy_revision: 'policy-1',
						fields: [
							{ field_name: 'artist', value: ['Björk', 'PJ Harvey'] },
							{ field_name: 'genre', value: ['Art Pop', 'Electronic'] }
						]
					}
				]
			},
			// A discard names the exact ready preview revision.
			{
				create: createDiscardLibraryManagementPreviewMutation,
				input: { jobId: 'preview/1', request: { expected_operation_row_revision: 7 } },
				method: 'POST',
				args: [
					'/api/v3/library/management/previews/preview%2F1/discard',
					{ expected_operation_row_revision: 7 }
				]
			},
			// Reissuing the sealed token and resolving a stuck bundle send no body.
			{
				create: createReissueLibraryManagementPreviewMutation,
				input: { jobId: 'preview/1' },
				method: 'POST',
				args: ['/api/v3/library/management/previews/preview%2F1/reissue']
			},
			{
				create: createResolveLibraryManagementImportBundleMutation,
				input: { bundleId: 'bundle/1' },
				method: 'POST',
				args: ['/api/v3/library/management/recovery/import-bundles/bundle%2F1/resolve']
			},
			{
				create: createResolveLibraryManagementImportBundleMutation,
				input: { bundleId: 'bundle-1' },
				method: 'POST',
				args: ['/api/v3/library/management/recovery/import-bundles/bundle-1/resolve'],
				answer: { bundle_id: 'bundle-1', state: 'resolved', verified_files: 4, total_files: 4 },
				returns: { verified_files: 4, total_files: 4 }
			}
		];
		for (const { create, input, method, args, effects, answer, returns } of cases) {
			vi.clearAllMocks();
			vi.mocked(api.v3[method]).mockResolvedValue((answer ?? {}) as never);
			create();
			const mutation = captured.current as {
				mutationFn: (input: unknown) => Promise<unknown>;
				effects: (data: unknown, input: unknown) => Effect[];
			};

			const result = await mutation.mutationFn(input);

			expect(api.v3[method], String(args[0])).toHaveBeenCalledWith(...args);
			if (effects) {
				expect(mutation.effects({}, input).map((effect) => effect.name)).toEqual(effects);
			}
			if (returns) expect(result).toMatchObject(returns);
		}
	});
});
