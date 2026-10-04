import { describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

// The real api client rides along (queryFns never run here) and pulls
// userSessionCleanup -> the real QueryClient module, which needs the
// QueryClient class the plain-object tanstack mock cannot re-export.
// Stub the module: nothing under test touches the cache.
vi.mock('../QueryClient', () => ({
	queryClient: {
		getQueryData: vi.fn(),
		setQueryData: vi.fn(),
		removeQueries: vi.fn(),
		invalidateQueries: vi.fn(),
		cancelQueries: vi.fn(),
		clear: vi.fn(),
		ensureQueryData: vi.fn()
	},
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import {
	getAlbumDownloadsQuery,
	getDownloadActivitySummaryQueryOptions,
	getDownloadsQueryOptions
} from './DownloadQueries.svelte';
import { getHeldImportsQuery } from './HeldQueries.svelte';

// vi.mock swaps createQuery for an evaluator at runtime, but svelte-check still sees the
// real TanStack result types - cast the evaluated option objects explicitly.
const asOpts = (value: unknown): Record<string, unknown> => value as Record<string, unknown>;

// B6 policy pin: held/downloads projections are event-driven (mutation invalidations +
// SSE reconciliation), so focus-'always' refetches are gone. This spec fails if anyone
// reintroduces per-focus churn or drops the stale windows.
describe('downloads staleness/focus policy (B6)', () => {
	it('held imports: 30 s stale window, no focus-always refetch', () => {
		const opts = asOpts(
			getHeldImportsQuery(
				() => undefined,
				() => true
			)
		);
		expect(opts.staleTime).toBe(30_000);
		expect(opts.refetchOnWindowFocus).toBeUndefined();
	});

	it('activity summary: staleTime 0 (poll owner), reconnect-always kept, focus-always dropped', () => {
		const opts = asOpts(getDownloadActivitySummaryQueryOptions());
		expect(opts.staleTime).toBe(0);
		expect(opts.refetchOnReconnect).toBe('always');
		expect(opts.refetchOnWindowFocus).toBeUndefined();
		expect(typeof opts.refetchInterval).toBe('function');
	});

	it('downloads list: 30 s stale window, neither always-flag remains', () => {
		const opts = asOpts(getDownloadsQueryOptions());
		expect(opts.staleTime).toBe(30_000);
		expect(opts.refetchOnReconnect).toBeUndefined();
		expect(opts.refetchOnWindowFocus).toBeUndefined();
	});

	it('album-scoped downloads query is untouched by B6 (self-stopping poll, no focus flags)', () => {
		const opts = asOpts(
			getAlbumDownloadsQuery(
				() => 'rg-1',
				() => true
			)
		);
		expect(opts.staleTime).toBe(0);
		expect(opts.refetchOnWindowFocus).toBeUndefined();
		expect(opts.refetchOnReconnect).toBeUndefined();
		expect(typeof opts.refetchInterval).toBe('function');
	});
});
