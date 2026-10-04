import { beforeEach, describe, expect, it, vi } from 'vitest';

const prefetch = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

vi.mock('$lib/queries/QueryClient', () => ({
	queryClient: { prefetchQuery: (...args: unknown[]) => prefetch(...args) }
}));

import { load } from './+page';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';

// ST7 W1: the album load warms detail + copies + status in one fire-and-forget
// wave; keys must be byte-identical to what the branch components mount. The
// detail gate reads v3 (asserted through the same factory the component
// uses); copies + status stay v1 until their consumers migrate.
describe('album route load prefetch', () => {
	beforeEach(() => {
		prefetch.mockClear();
	});

	it('fires exactly the three library-native reads and returns params untouched', async () => {
		const result = await load({
			params: { id: 'alb-1' },
			route: { id: '/album/[id]' },
			url: new URL('http://localhost/album/alb-1')
		} as never);

		expect(result).toEqual({ albumId: 'alb-1' });
		expect(prefetch).toHaveBeenCalledTimes(3);
		const keys = prefetch.mock.calls.map((call) => call[0].queryKey);
		expect(keys).toContainEqual(LibraryQueryKeyFactory.v3.albumDetail(undefined, 'alb-1'));
		expect(keys).toContainEqual(['library', 'album-copies', 'alb-1']);
		expect(keys).toContainEqual(['library', 'album', 'alb-1']);
	});
});
