import { beforeEach, describe, expect, it, vi } from 'vitest';

const prefetch = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

vi.mock('$lib/queries/QueryClient', () => ({
	queryClient: { prefetchQuery: (...args: unknown[]) => prefetch(...args) }
}));

import { load } from './+page';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';

// The discover load warms the v3 home read the page mounts, asserted
// through the same factory so the key stays byte-identical.
describe('discover route load prefetch', () => {
	beforeEach(() => {
		prefetch.mockClear();
	});

	it('prefetches the v3 home query and returns an empty payload', async () => {
		const result = await load({
			params: {},
			route: { id: '/discover' },
			url: new URL('http://localhost/discover')
		} as never);

		expect(result).toEqual({});
		expect(prefetch).toHaveBeenCalledTimes(1);
		expect(prefetch.mock.calls[0][0].queryKey).toEqual(
			DiscoverQueryKeyFactory.v3.home(undefined)
		);
	});
});
