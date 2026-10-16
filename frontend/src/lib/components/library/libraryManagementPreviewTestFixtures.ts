// Shared by the preview page's specs: the mocked navigation, auth, query and
// mutation modules, and the preview they start from. Each spec mocks the
// modules with these objects, so they all drive one set of fakes.
import { vi } from 'vitest';

export const h = {
	preview: {} as Record<string, unknown>,
	items: {} as Record<string, unknown>,
	apply: vi.fn(),
	discard: vi.fn(),
	resolve: vi.fn(),
	reissue: vi.fn(),
	goto: vi.fn()
};

export const navigationMock = { goto: (...args: unknown[]) => h.goto(...args) };

export const authMock = {
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
};

export const policyQueriesMock = {
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
};

export const libraryQueriesMock = {
	getLibrarySearchQuery: () => ({ data: { artists: [], albums: [], tracks: [] } })
};

export const eventsMock = {
	createLibraryManagementEvents: () => ({ start: vi.fn(), stop: vi.fn() })
};

export const queriesMock = {
	getLibraryManagementPreviewQuery: () => h.preview,
	getLibraryManagementPlanItemsQuery: () => h.items,
	getLibraryManagementSettingsQuery: () => ({
		data: { settings_revision: 'settings-1', recycle_bin_path: '' },
		isLoading: false,
		isError: false
	})
};

export const mutationsMock = {
	applyLibraryManagementPreviewMutation: () => ({ mutateAsync: h.apply, isPending: false }),
	discardLibraryManagementPreviewMutation: () => ({ mutateAsync: h.discard, isPending: false }),
	reissueLibraryManagementPreviewMutation: () => ({ mutateAsync: h.reissue, isPending: false }),
	createLibraryManagementDuplicateResolutionMutation: () => ({
		mutateAsync: h.resolve,
		isPending: false
	})
};

export function detail(overrides: Record<string, unknown> = {}): Record<string, unknown> {
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

export const collisionItem = {
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

export function resetPreviewMocks(): void {
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
}
