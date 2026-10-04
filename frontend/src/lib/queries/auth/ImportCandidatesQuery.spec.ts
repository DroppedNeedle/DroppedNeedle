import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn() } }
}));

import { api } from '$lib/api/client';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';
import { getImportCandidatesQuery } from './ImportCandidatesQuery.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: unknown;
};

beforeEach(() => {
	vi.clearAllMocks();
});

describe('getImportCandidatesQuery', () => {
	it('lists Plex import candidates while the picker is open', async () => {
		mockGet.mockResolvedValue({ items: [] });
		const query = getImportCandidatesQuery(
			() => 'plex',
			() => true,
			() => 'admin-1'
		) as QueryResult;
		const controller = new AbortController();

		await query.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(AUTH_ENDPOINTS.adminImportPlex, {
			signal: controller.signal
		});
		expect(query.queryKey).toEqual(AuthQueryKeyFactory.importCandidates('plex', 'admin-1'));
	});

	it('switches to the Jellyfin enumeration on the Jellyfin tab', async () => {
		mockGet.mockResolvedValue({ items: [] });
		const query = getImportCandidatesQuery(
			() => 'jellyfin',
			() => true,
			() => 'admin-1'
		) as QueryResult;

		await query.queryFn?.({ signal: new AbortController().signal });

		expect(mockGet).toHaveBeenCalledWith(
			AUTH_ENDPOINTS.adminImportJellyfin,
			expect.objectContaining({})
		);
		expect(query.queryKey).toEqual(AuthQueryKeyFactory.importCandidates('jellyfin', 'admin-1'));
	});

	it('stays quiet until the picker opens', () => {
		const query = getImportCandidatesQuery(
			() => 'plex',
			() => false,
			() => 'admin-1'
		) as QueryResult;

		expect(query.enabled).toBe(false);
		expect(mockGet).not.toHaveBeenCalled();
	});
});
