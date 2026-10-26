import { afterEach, describe, expect, it, vi } from 'vitest';
import { coverAnsweredMissing } from './coverWarmCoordinator';

const URL_A = '/api/v3/library/albums/a/artwork?size=250';

function answered(status: number) {
	vi.spyOn(performance, 'getEntriesByName').mockReturnValue([
		{ responseStatus: status } as unknown as PerformanceEntry
	]);
}

describe('coverAnsweredMissing', () => {
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('takes a 204 or a 404 as the server saying there is no art', () => {
		for (const status of [204, 404]) {
			answered(status);
			expect(coverAnsweredMissing(URL_A), String(status)).toBe(true);
		}
	});

	it('keeps polling on a warming answer or a server error', () => {
		for (const status of [202, 500]) {
			answered(status);
			expect(coverAnsweredMissing(URL_A), String(status)).toBe(false);
		}
	});

	it('keeps polling when the browser recorded no status', () => {
		vi.spyOn(performance, 'getEntriesByName').mockReturnValue([]);
		expect(coverAnsweredMissing(URL_A)).toBe(false);
	});
});
