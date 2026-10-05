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
import { getLibraryManagementActivationPreviewQuery } from './LibraryManagementQueries.svelte';

const mockGet = vi.mocked(api.global.get);

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
});
