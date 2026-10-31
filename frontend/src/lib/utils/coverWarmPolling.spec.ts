import { afterEach, describe, expect, it, vi } from 'vitest';

const raw = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', () => ({ api: { raw } }));

describe('watchWarmingCover', () => {
	afterEach(() => {
		vi.useRealTimers();
		raw.mockReset();
	});

	it('keeps asking through a cover archive outage of several minutes', async () => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:cover');
		const { watchWarmingCover } = await import('./coverWarmCoordinator');
		const outage = new Response(null, { status: 500 });
		raw.mockImplementation(async () => outage.clone());
		const updates: string[] = [];
		const stop = watchWarmingCover('/api/v3/covers/release-group/x?size=250', (update) =>
			updates.push(update.status)
		);

		// Three minutes of 500s: the old schedule gave up after under a minute.
		await vi.advanceTimersByTimeAsync(180_000);
		expect(updates).not.toContain('failed');

		raw.mockImplementation(
			async () =>
				new Response(new Blob(['png']), { status: 200, headers: { 'content-type': 'image/png' } })
		);
		await vi.advanceTimersByTimeAsync(300_000);
		// Reading the image body settles outside the faked timers.
		for (let tick = 0; tick < 20 && updates.at(-1) !== 'ready'; tick++) {
			await new Promise((resolve) => setImmediate(resolve));
		}
		expect(updates.at(-1)).toBe('ready');
		stop();
	});
});
