import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createInfiniteQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((options: Record<string, unknown>) => options)
}));

vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: vi.fn(),
			post: vi.fn(),
			put: vi.fn()
		}
	}
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

const { clearSearch } = vi.hoisted(() => ({ clearSearch: vi.fn() }));
vi.mock('$lib/stores/search', () => ({ searchStore: { clear: clearSearch } }));

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { getLibraryActivityQueryOptions } from './LibraryActivityQueries.svelte';
import { getCurrentLibraryRunsQueryOptions } from './LibraryOperationQueries.svelte';
import { getLibraryReviewsQuery } from './LibraryReviewQueries.svelte';
import { getLibraryPolicyTreeQuery } from './LibraryPolicyQueries.svelte';
import {
	getLibraryRepairEstimateQuery,
	getLibraryRepairFindingsQuery
} from './LibraryRepairQueries.svelte';
import {
	getLibraryIdentityPreparationEstimateQuery,
	getLibraryIdentityPreparationFindingsQuery,
	getLibraryIdentityPreparationsQuery
} from './LibraryIdentityPreparationQueries.svelte';
import { requestLibraryRun } from './LibraryOperationMutations.svelte';
import { applyArtistMerge } from './LibraryCatalogMutations.svelte';
import { actOnLibraryReview } from './LibraryReviewMutations.svelte';
import { applyLibraryRepair } from './LibraryRepairMutations.svelte';
import {
	applyLibraryIdentityPreparation,
	createLibraryIdentityPreparation
} from './LibraryIdentityPreparationMutations.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

const mockGet = vi.mocked(api.global.get);
const mockPost = vi.mocked(api.global.post);

function queryFn(options: unknown) {
	return (options as { queryFn: (context: { signal: AbortSignal }) => Promise<unknown> }).queryFn;
}

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({});
	mockPost.mockResolvedValue({});
});

describe('Feedback Fixes library query contracts', () => {
	it('segments activity keys by user and forwards the abort signal', async () => {
		const first = getLibraryActivityQueryOptions('user-a');
		const second = getLibraryActivityQueryOptions('user-b');
		expect(first.queryKey).not.toEqual(second.queryKey);
		expect(first.queryKey).toEqual(['library', 'activity', 'user-a']);
		const signal = new AbortController().signal;
		await queryFn(first)({ signal });
		expect(mockGet).toHaveBeenCalledWith('/api/v1/library/activity', { signal });
	});

	it('uses target current-run and policy endpoints with abort signals', async () => {
		const signal = new AbortController().signal;
		await queryFn(getCurrentLibraryRunsQueryOptions())({ signal });
		const policy = getLibraryPolicyTreeQuery() as unknown;
		await queryFn(policy)({ signal });
		expect(mockGet).toHaveBeenNthCalledWith(1, '/api/v1/library/scan-runs/current', {
			signal
		});
		expect(mockGet).toHaveBeenNthCalledWith(2, '/api/v1/settings/library/policy-tree', {
			signal
		});
	});

	it('keeps review filters and cursor in server-side pagination', async () => {
		const options = getLibraryReviewsQuery(() => ({
			state: 'needs_review',
			reasonCode: 'AMBIGUOUS',
			search: 'blue',
			sort: 'oldest'
		})) as unknown as {
			queryFn: (context: { pageParam: string; signal: AbortSignal }) => Promise<unknown>;
			getNextPageParam: (page: { next_cursor: string | null }) => string | undefined;
		};
		const signal = new AbortController().signal;
		await options.queryFn({ pageParam: 'cursor-2', signal });
		const url = mockGet.mock.calls[0][0] as string;
		expect(url).toContain('cursor=cursor-2');
		expect(url).toContain('state=needs_review');
		expect(url).toContain('reason_code=AMBIGUOUS');
		expect(url).toContain('search=blue');
		expect(options.getNextPageParam({ next_cursor: 'next' })).toBe('next');
		expect(options.getNextPageParam({ next_cursor: null })).toBeUndefined();
		expect(mockGet.mock.calls[0][1]).toEqual({ signal });
	});

	it('invalidates activity and operation prefixes after starting work', async () => {
		const mutation = requestLibraryRun() as unknown as {
			mutationFn: (input: Record<string, unknown>) => Promise<unknown>;
			onSuccess: () => Promise<void>;
		};
		await mutation.mutationFn({
			kind: 'incremental',
			scope_ids: [],
			expected_policy_revision: 'policy-1'
		});
		expect(mockPost.mock.calls[0][0]).toContain('/api/v1/library/scan-runs');
		await mutation.onSuccess();
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.activityPrefix()
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.operationsPrefix()
		});
	});

	it('keeps repair scope and finding category in stable keys and forwards pagination', async () => {
		const estimate = getLibraryRepairEstimateQuery(
			() => ['root-b', 'root-a'],
			() => true
		) as unknown;
		const signal = new AbortController().signal;
		await queryFn(estimate)({ signal });
		expect(mockGet).toHaveBeenNthCalledWith(
			1,
			'/api/v1/library/identity-repairs/estimate?root_id=root-b&root_id=root-a',
			{ signal }
		);

		const findings = getLibraryRepairFindingsQuery(
			() => 'repair-1',
			() => 'needs_review'
		) as unknown as {
			queryKey: readonly unknown[];
			queryFn: (context: { pageParam: string; signal: AbortSignal }) => Promise<unknown>;
			getNextPageParam: (page: { next_cursor: string | null }) => string | undefined;
		};
		expect(findings.queryKey).toContain('needs_review');
		await findings.queryFn({ pageParam: 'finding-20', signal });
		const url = mockGet.mock.calls[1][0] as string;
		expect(url).toContain('cursor=finding-20');
		expect(url).toContain('finding_category=needs_review');
		expect(findings.getNextPageParam({ next_cursor: 'finding-40' })).toBe('finding-40');
		expect(findings.getNextPageParam({ next_cursor: null })).toBeUndefined();
		expect(mockGet.mock.calls[1][1]).toEqual({ signal });
	});

	it('clears persistent search results after catalog-changing mutations', async () => {
		const catalog = applyArtistMerge() as unknown as { onSuccess: () => Promise<void> };
		await catalog.onSuccess();

		const review = actOnLibraryReview('keep_tagged') as unknown as {
			onSuccess: (result: unknown, input: { reviewId: string }) => Promise<void>;
		};
		await review.onSuccess({}, { reviewId: 'review-1' });

		const repair = applyLibraryRepair() as unknown as { onSuccess: () => Promise<void> };
		await repair.onSuccess();

		expect(clearSearch).toHaveBeenCalledTimes(3);
	});

	it('segments identity preparation by administrator and forwards scope, pagination, and signals', async () => {
		const first = getLibraryIdentityPreparationsQuery(
			() => 'admin-a',
			() => true
		) as unknown as {
			queryKey: readonly unknown[];
			queryFn: (context: { pageParam: string; signal: AbortSignal }) => Promise<unknown>;
		};
		const second = getLibraryIdentityPreparationsQuery(
			() => 'admin-b',
			() => true
		) as unknown as { queryKey: readonly unknown[] };
		expect(first.queryKey).not.toEqual(second.queryKey);
		expect(first.queryKey).toContain('admin-a');
		const signal = new AbortController().signal;
		await first.queryFn({ pageParam: '10:job', signal });
		expect(mockGet).toHaveBeenCalledWith(
			'/api/v1/library/management/identity-preparations?limit=5&cursor=10%3Ajob',
			{ signal }
		);

		const estimate = getLibraryIdentityPreparationEstimateQuery(
			() => 'admin-a',
			() => ['root-b', 'root-a'],
			() => true
		) as unknown;
		await queryFn(estimate)({ signal });
		expect(mockGet).toHaveBeenCalledWith(
			'/api/v1/library/management/identity-preparations/estimate?root_id=root-b&root_id=root-a',
			{ signal }
		);

		const findings = getLibraryIdentityPreparationFindingsQuery(
			() => 'admin-a',
			() => 'job-1',
			() => 'mapping_ready'
		) as unknown as {
			queryKey: readonly unknown[];
			queryFn: (context: { pageParam: string; signal: AbortSignal }) => Promise<unknown>;
		};
		expect(findings.queryKey).toContain('mapping_ready');
		await findings.queryFn({ pageParam: 'finding-2', signal });
		expect(mockGet).toHaveBeenCalledWith(
			'/api/v1/library/management/identity-preparations/job-1/findings?limit=100&cursor=finding-2&finding_category=mapping_ready',
			{ signal }
		);
	});

	it('uses explicit preparation and catalog-only apply mutations', async () => {
		const create = createLibraryIdentityPreparation(() => 'admin-a') as unknown as {
			mutationFn: (rootIds: string[]) => Promise<unknown>;
		};
		await create.mutationFn(['root-1']);
		expect(mockPost).toHaveBeenCalledWith(
			'/api/v1/library/management/identity-preparations',
			expect.objectContaining({ root_ids: ['root-1'] })
		);

		const apply = applyLibraryIdentityPreparation(() => 'admin-a') as unknown as {
			mutationFn: (input: { jobId: string; expectedRevision: number }) => Promise<unknown>;
			onSuccess: () => Promise<void>;
		};
		await apply.mutationFn({ jobId: 'job/1', expectedRevision: 7 });
		expect(mockPost).toHaveBeenCalledWith(
			'/api/v1/library/management/identity-preparations/job%2F1/apply',
			{ expected_row_revision: 7, confirmation: true }
		);
		await apply.onSuccess();
		expect(clearSearch).toHaveBeenCalledOnce();
	});
});
