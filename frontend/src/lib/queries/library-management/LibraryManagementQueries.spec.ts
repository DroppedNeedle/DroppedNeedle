import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createInfiniteQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((options: Record<string, unknown>) => options)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn().mockResolvedValue({}) } }
}));

import { api } from '$lib/api/client';
import { LibraryManagementQueryKeyFactory } from './LibraryManagementQueryKeyFactory';
import type { LibraryManagementResultPageResponse } from './types';
import {
	getLibraryManagementActivationHealthQuery,
	getLibraryManagementActivationPreviewQuery,
	getLibraryManagementOperationQuery,
	getLibraryManagementOperationResultsQuery,
	getLibraryManagementOperationsQuery,
	getLibraryManagementPlanItemsQuery,
	getLibraryManagementPresetDiffQuery,
	getLibraryManagementPreviewQuery,
	getLibraryManagementProfileQuery,
	getLibraryManagementRecoveryQuery,
	getLibraryManagementSettingsQueryOptions,
	getLibraryManagementTagEditorQuery
} from './LibraryManagementQueries.svelte';

const mockGet = vi.mocked(api.global.get);

interface QueryContext {
	signal: AbortSignal;
	pageParam?: string | number;
}

async function callQueryFn(options: unknown, context: QueryContext): Promise<unknown> {
	return (options as { queryFn: (value: QueryContext) => Promise<unknown> }).queryFn(context);
}

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
});

describe('LibraryManagementQueryKeyFactory', () => {
	it('isolates every persisted domain by user', () => {
		expect(LibraryManagementQueryKeyFactory.settings('admin-a')).not.toEqual(
			LibraryManagementQueryKeyFactory.settings('admin-b')
		);
		expect(LibraryManagementQueryKeyFactory.preview('admin-a', 'job-1')).toEqual([
			'library-management',
			'admin-a',
			'previews',
			'job-1'
		]);
	});

	it('normalizes pageable filters without putting cursors in the history identity', () => {
		const first = LibraryManagementQueryKeyFactory.operations('admin-a', {
			cursor: 'page-1',
			state: 'succeeded'
		});
		const second = LibraryManagementQueryKeyFactory.operations('admin-a', {
			cursor: 'page-2',
			state: 'succeeded'
		});
		expect(first).toEqual(second);
		expect(first).not.toEqual(
			LibraryManagementQueryKeyFactory.operations('admin-a', { state: 'failed' })
		);
		expect(first).not.toEqual(
			LibraryManagementQueryKeyFactory.operations('admin-a', {
				state: 'succeeded',
				rootId: 'root-1'
			})
		);
	});

	it('keeps every plan-item filter in the persisted query identity', () => {
		const base = LibraryManagementQueryKeyFactory.previewItems('admin-a', 'job-1', {
			artistId: 'artist-1',
			collisionClass: 'normalized_path_collision',
			hasRepresentationLoss: true
		});
		expect(base).not.toEqual(
			LibraryManagementQueryKeyFactory.previewItems('admin-a', 'job-1', {
				artistId: 'artist-2',
				collisionClass: 'normalized_path_collision',
				hasRepresentationLoss: true
			})
		);
		expect(LibraryManagementQueryKeyFactory.tagEditor('admin-a', 'track-1')).toEqual([
			'library-management',
			'admin-a',
			'tag-editor',
			'track-1'
		]);
	});
});

describe('Library Management query endpoints', () => {
	it('forwards the abort signal for settings', async () => {
		const signal = new AbortController().signal;
		await callQueryFn(getLibraryManagementSettingsQueryOptions('admin-a'), { signal });
		expect(mockGet).toHaveBeenCalledWith('/api/v1/settings/library-management', { signal });
	});

	it('polls an activation preview until it is ready or terminal', () => {
		const options = getLibraryManagementActivationPreviewQuery(
			() => 'admin-a',
			() => 'activation-1'
		) as unknown as {
			refetchInterval: (query: {
				state: {
					data?: {
						state: string;
						ready_for_confirmation: boolean;
						stale: boolean;
						expired: boolean;
					};
				};
			}) => number | false;
		};
		const preview = {
			state: 'planning',
			ready_for_confirmation: false,
			stale: false,
			expired: false
		};

		expect(options.refetchInterval({ state: {} })).toBe(2000);
		expect(options.refetchInterval({ state: { data: preview } })).toBe(2000);
		expect(
			options.refetchInterval({
				state: { data: { ...preview, state: 'ready', ready_for_confirmation: true } }
			})
		).toBe(false);
		expect(options.refetchInterval({ state: { data: { ...preview, state: 'failed' } } })).toBe(
			false
		);
		expect(options.refetchInterval({ state: { data: { ...preview, state: 'stopped' } } })).toBe(
			false
		);
	});

	it('polls ordinary previews and operations until they are ready or terminal', () => {
		const queries = [
			getLibraryManagementPreviewQuery(
				() => 'admin-a',
				() => 'preview-1'
			),
			getLibraryManagementOperationQuery(
				() => 'admin-a',
				() => 'operation-1'
			)
		] as unknown as Array<{
			refetchInterval: (query: {
				state: {
					data?: {
						state: string;
						ready_for_confirmation: boolean;
						stale: boolean;
						expired: boolean;
					};
				};
			}) => number | false;
		}>;
		const planning = {
			state: 'planning',
			ready_for_confirmation: false,
			stale: false,
			expired: false
		};

		for (const query of queries) {
			expect(query.refetchInterval({ state: {} })).toBe(2000);
			expect(query.refetchInterval({ state: { data: planning } })).toBe(2000);
			expect(
				query.refetchInterval({
					state: { data: { ...planning, state: 'ready', ready_for_confirmation: true } }
				})
			).toBe(false);
			expect(query.refetchInterval({ state: { data: { ...planning, state: 'succeeded' } } })).toBe(
				false
			);
		}
	});

	it('uses every detail endpoint through encoded API builders', async () => {
		const signal = new AbortController().signal;
		const queries = [
			getLibraryManagementTagEditorQuery(
				() => 'admin-a',
				() => 'track/1'
			),
			getLibraryManagementProfileQuery(
				() => 'admin-a',
				() => 'profile/1'
			),
			getLibraryManagementPresetDiffQuery(
				() => 'admin-a',
				() => 'profile/1'
			),
			getLibraryManagementActivationPreviewQuery(
				() => 'admin-a',
				() => 'activation/1'
			),
			getLibraryManagementPreviewQuery(
				() => 'admin-a',
				() => 'preview/1'
			),
			getLibraryManagementOperationQuery(
				() => 'admin-a',
				() => 'operation/1'
			),
			getLibraryManagementRecoveryQuery(() => 'admin-a'),
			getLibraryManagementActivationHealthQuery(() => 'admin-a')
		];
		for (const query of queries) await callQueryFn(query, { signal });

		expect(mockGet.mock.calls.map(([url]) => url)).toEqual([
			'/api/v1/library/management/tracks/track%2F1/tag-editor',
			'/api/v1/settings/library-management/profiles/profile%2F1',
			'/api/v1/settings/library-management/profiles/profile%2F1/preset-diff',
			'/api/v1/settings/library-management/activation-previews/activation%2F1',
			'/api/v1/library/management/previews/preview%2F1',
			'/api/v1/library/management/operations/operation%2F1',
			'/api/v1/library/management/recovery/diagnostics',
			'/api/v1/settings/library-management/activation-health'
		]);
		expect(mockGet.mock.calls.every((call) => call[1]?.signal === signal)).toBe(true);
	});

	it('forwards preview filters and numeric page cursors', async () => {
		const signal = new AbortController().signal;
		const query = getLibraryManagementPlanItemsQuery(
			() => 'admin-a',
			() => 'job/1',
			() => ({
				eligibility: 'warning',
				reasonCode: 'DEFERRED',
				artistId: 'artist/1',
				albumId: 'album/1',
				collisionClass: 'normalized_path_collision',
				hasPreservedValue: true,
				hasRepresentationLoss: true,
				limit: 25
			})
		);
		await callQueryFn(query, { signal, pageParam: 9 });
		expect(mockGet).toHaveBeenCalledWith(
			'/api/v1/library/management/previews/job%2F1/items?after_ordinal=9&limit=25&eligibility=warning&reason_code=DEFERRED&artist_id=artist%2F1&album_id=album%2F1&collision_class=normalized_path_collision&has_preserved_value=true&has_representation_loss=true',
			{ signal }
		);
	});

	it('forwards history filters and opaque cursors', async () => {
		const signal = new AbortController().signal;
		const query = getLibraryManagementOperationsQuery(
			() => 'admin-a',
			() => ({ profileId: 'profile/1', rootId: 'root/1', state: 'succeeded', limit: 20 })
		);
		await callQueryFn(query, { signal, pageParam: 'opaque cursor' });
		expect(mockGet).toHaveBeenCalledWith(
			'/api/v1/library/management/operations?limit=20&cursor=opaque+cursor&profile_id=profile%2F1&root_id=root%2F1&state=succeeded',
			{ signal }
		);
	});

	it('forwards result cursors and limits', async () => {
		const signal = new AbortController().signal;
		const query = getLibraryManagementOperationResultsQuery(
			() => 'admin-a',
			() => 'job/1',
			() => 40
		);
		await callQueryFn(query, { signal, pageParam: 7 });
		expect(mockGet).toHaveBeenCalledWith(
			'/api/v1/library/management/operations/job%2F1/results?after_ordinal=7&limit=40',
			{ signal }
		);
	});

	it('polls operation results until terminal work is visible', () => {
		let operationState = 'running';
		const query = getLibraryManagementOperationResultsQuery(
			() => 'admin-a',
			() => 'job-1',
			() => 40,
			() => operationState
		) as unknown as {
			refetchInterval: (query: {
				state: { data?: { pages: LibraryManagementResultPageResponse[] } };
			}) => number | false;
		};
		const page = (workState: string): LibraryManagementResultPageResponse => ({
			items: [
				{
					plan: {} as LibraryManagementResultPageResponse['items'][number]['plan'],
					work_state: workState,
					failure_code: null,
					result: {},
					journal_states: []
				}
			],
			next_after_ordinal: null,
			has_more: false
		});

		expect(query.refetchInterval({ state: {} })).toBe(2000);
		expect(query.refetchInterval({ state: { data: { pages: [page('pending')] } } })).toBe(2000);
		operationState = 'succeeded';
		expect(query.refetchInterval({ state: { data: { pages: [page('not_scheduled')] } } })).toBe(
			false
		);
		operationState = 'stopped';
		expect(query.refetchInterval({ state: { data: { pages: [page('pending')] } } })).toBe(false);
		operationState = 'succeeded';
		expect(query.refetchInterval({ state: { data: { pages: [page('pending')] } } })).toBe(2000);
		expect(query.refetchInterval({ state: { data: { pages: [page('skipped')] } } })).toBe(false);
	});
});
