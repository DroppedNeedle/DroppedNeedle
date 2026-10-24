import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mockGet = vi.hoisted(() => vi.fn());

vi.mock('$lib/api/client', () => ({
	api: { global: { get: mockGet } }
}));

import { watchWarmingCover, type CoverWarmUpdate } from './coverWarmCoordinator';

describe('coverWarmCoordinator', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		mockGet.mockReset();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('coalesces subscribers into one request per retry generation and shares late success', async () => {
		mockGet.mockResolvedValueOnce(new Response('', { status: 202 })).mockResolvedValueOnce(
			new Response(new Blob(['cover'], { type: 'image/jpeg' }), {
				status: 200,
				headers: { 'x-cover-source': 'cover-art-archive' }
			})
		);
		const first: CoverWarmUpdate[] = [];
		const second: CoverWarmUpdate[] = [];

		const stopFirst = watchWarmingCover('/api/v1/covers/artist/a?size=250', (value) =>
			first.push(value)
		);
		const stopSecond = watchWarmingCover('/api/v1/covers/artist/a?size=250', (value) =>
			second.push(value)
		);

		expect(first).toEqual([{ status: 'warming' }]);
		expect(second).toEqual([{ status: 'warming' }]);
		await vi.advanceTimersByTimeAsync(1500);
		expect(mockGet).toHaveBeenCalledTimes(1);
		expect(mockGet).toHaveBeenLastCalledWith(
			'/api/v1/covers/artist/a?size=250&_r=1',
			expect.objectContaining({ raw: true, cache: 'no-store', timeoutMs: 10_000 })
		);

		await vi.advanceTimersByTimeAsync(3000);
		expect(mockGet).toHaveBeenCalledTimes(2);
		await vi.waitFor(() => {
			expect(first.at(-1)).toMatchObject({ status: 'ready' });
			expect(second.at(-1)).toEqual(first.at(-1));
		});

		stopFirst();
		stopSecond();
	});

	it('treats a stable placeholder response as terminal', async () => {
		mockGet.mockResolvedValueOnce(
			new Response(new Blob(['placeholder']), {
				status: 200,
				headers: { 'x-cover-source': 'placeholder' }
			})
		);
		const updates: CoverWarmUpdate[] = [];
		const stop = watchWarmingCover('/api/v1/covers/release-group/b?size=250', (value) =>
			updates.push(value)
		);

		await vi.advanceTimersByTimeAsync(1500);

		expect(mockGet).toHaveBeenCalledTimes(1);
		expect(updates).toEqual([{ status: 'warming' }, { status: 'failed' }]);
		stop();
	});
});

describe('stable local artwork provider fallback', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		mockGet.mockReset();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('follows bounded warming responses until provider art is ready', async () => {
		mockGet.mockResolvedValueOnce(new Response('', { status: 202 })).mockResolvedValueOnce(
			new Response(new Blob(['art'], { type: 'image/jpeg' }), {
				status: 200,
				headers: { 'x-cover-source': 'cover-art-archive' }
			})
		);
		const updates: CoverWarmUpdate[] = [];
		const stop = watchWarmingCover('/api/v1/library/albums/local-id/artwork/local', (value) =>
			updates.push(value)
		);
		await vi.advanceTimersByTimeAsync(1500);
		expect(updates.at(-1)).toEqual({ status: 'warming' });
		await vi.advanceTimersByTimeAsync(3000);
		await vi.waitFor(() => expect(updates.at(-1)).toMatchObject({ status: 'ready' }));
		expect(mockGet).toHaveBeenCalledTimes(2);
		stop();
	});

	it('stops after the first 404 rather than consuming the warming retry budget', async () => {
		mockGet.mockResolvedValue(new Response('', { status: 404 }));
		const updates: CoverWarmUpdate[] = [];
		const stop = watchWarmingCover('/api/v1/library/albums/missing/artwork/local', (value) =>
			updates.push(value)
		);
		await vi.advanceTimersByTimeAsync(60000);
		expect(updates).toEqual([{ status: 'warming' }, { status: 'failed' }]);
		expect(mockGet).toHaveBeenCalledTimes(1);
		stop();
	});

	it('eventually stops even when a provider remains warming', async () => {
		mockGet.mockResolvedValue(new Response('', { status: 202 }));
		const updates: CoverWarmUpdate[] = [];
		const stop = watchWarmingCover('/api/v1/library/albums/pending/artwork/local', (value) =>
			updates.push(value)
		);
		await vi.advanceTimersByTimeAsync(60000);
		expect(updates.at(-1)).toEqual({ status: 'failed' });
		expect(mockGet).toHaveBeenCalledTimes(6);
		stop();
	});
});
