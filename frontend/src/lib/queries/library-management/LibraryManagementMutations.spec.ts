import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));
const invalidate = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));

vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			delete: vi.fn().mockResolvedValue({}),
			post: vi.fn().mockResolvedValue({}),
			put: vi.fn().mockResolvedValue({})
		}
	}
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('./LibraryManagementInvalidation', () => ({
	invalidateLibraryManagementSurfaces: invalidate
}));

import { api } from '$lib/api/client';
import {
	createLibraryManagementDuplicateResolutionMutation,
	createLibraryManagementTagEditPreviewMutation,
	deleteLibraryManagementProfileMutation,
	discardLibraryManagementPreviewMutation,
	updateLibraryManagementSettingsMutation
} from './LibraryManagementMutations.svelte';

function currentMutation<TInput, TOutput = unknown>() {
	return captured.current as {
		mutationFn: (input: TInput) => Promise<TOutput>;
		onSuccess?: (data: TOutput, input: TInput) => Promise<void> | void;
	};
}

beforeEach(() => {
	vi.clearAllMocks();
});

describe('Library Management mutations', () => {
	it('updates revision-guarded settings and invalidates persisted surfaces', async () => {
		updateLibraryManagementSettingsMutation();
		const mutation = currentMutation<{
			settings: Record<string, unknown>;
			expected_settings_revision: string;
		}>();
		const request = { settings: { schema_version: 1 }, expected_settings_revision: 'rev-1' };

		await mutation.mutationFn(request);
		await mutation.onSuccess?.({}, request);

		expect(api.global.put).toHaveBeenCalledWith('/api/v1/settings/library-management', request);
		expect(invalidate).toHaveBeenCalledOnce();
	});

	it('sends profile delete revisions in the DELETE JSON body', async () => {
		deleteLibraryManagementProfileMutation();
		const mutation = currentMutation<{
			profileId: string;
			request: { expected_settings_revision: string };
		}>();
		const input = {
			profileId: 'profile/1',
			request: { expected_settings_revision: 'rev-2' }
		};

		await mutation.mutationFn(input);

		expect(api.global.delete).toHaveBeenCalledWith(
			'/api/v1/settings/library-management/profiles/profile%2F1',
			{ body: input.request }
		);
	});

	it('posts the complete explicit duplicate choice without adding a default', async () => {
		createLibraryManagementDuplicateResolutionMutation();
		const mutation = currentMutation<Record<string, unknown>>();
		const request = {
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

		await mutation.mutationFn(request);

		expect(api.global.post).toHaveBeenCalledWith(
			'/api/v1/library/management/duplicate-resolution-previews',
			request
		);
		expect(request).not.toHaveProperty('recycle');
	});

	it('posts list-valued tag edits to the staged preview endpoint', async () => {
		createLibraryManagementTagEditPreviewMutation();
		const mutation = currentMutation<Record<string, unknown>>();
		const request = {
			local_track_id: 'track-1',
			mode: 'save_override',
			expected_settings_revision: 'settings-1',
			expected_policy_revision: 'policy-1',
			fields: [
				{ field_name: 'artist', value: ['Björk', 'PJ Harvey'] },
				{ field_name: 'genre', value: ['Art Pop', 'Electronic'] }
			]
		};

		await mutation.mutationFn(request);

		expect(api.global.post).toHaveBeenCalledWith(
			'/api/v1/library/management/tag-edit-previews',
			request
		);
	});

	it('discards only the exact current ready preview revision', async () => {
		discardLibraryManagementPreviewMutation();
		const mutation = currentMutation<{
			jobId: string;
			request: { expected_operation_row_revision: number };
		}>();
		const input = {
			jobId: 'preview/1',
			request: { expected_operation_row_revision: 7 }
		};

		await mutation.mutationFn(input);

		expect(api.global.post).toHaveBeenCalledWith(
			'/api/v1/library/management/previews/preview%2F1/discard',
			input.request
		);
	});
});
