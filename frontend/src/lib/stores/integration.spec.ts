import { describe, expect, it, beforeEach, vi } from 'vitest';
import { get } from 'svelte/store';

const apiGet = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: { global: { get: (...args: unknown[]) => apiGet(...args) } }
}));

type IntegrationStore = (typeof import('$lib/stores/integration'))['integrationStore'];

describe('integrationStore', () => {
	let integrationStore: IntegrationStore;

	beforeEach(async () => {
		vi.clearAllMocks();
		vi.resetModules();
		({ integrationStore } = await import('$lib/stores/integration'));
	});

	it('leaves loaded false on failure so a later ensureLoaded retries (#155)', async () => {
		apiGet.mockRejectedValueOnce(new Error('unauthenticated'));
		await integrationStore.ensureLoaded();
		expect(get(integrationStore).loaded).toBe(false);

		apiGet.mockResolvedValueOnce({ jellyfin: true });
		await integrationStore.ensureLoaded();
		const state = get(integrationStore);
		expect(state.loaded).toBe(true);
		expect(state.jellyfin).toBe(true);
		expect(apiGet).toHaveBeenCalledTimes(2);
	});

	it('coalesces concurrent loads into one request', async () => {
		let resolveGet!: (value: object) => void;
		apiGet.mockImplementationOnce(() => new Promise((resolve) => (resolveGet = resolve)));
		const first = integrationStore.ensureLoaded();
		const second = integrationStore.ensureLoaded();
		resolveGet({ navidrome: true });
		await Promise.all([first, second]);
		expect(apiGet).toHaveBeenCalledTimes(1);
		expect(get(integrationStore).navidrome).toBe(true);
	});

	it('reset clears flags and allows a reload', async () => {
		apiGet.mockResolvedValue({ jellyfin: true });
		await integrationStore.ensureLoaded();

		integrationStore.reset();
		const state = get(integrationStore);
		expect(state.loaded).toBe(false);
		expect(state.jellyfin).toBe(false);

		await integrationStore.ensureLoaded();
		expect(apiGet).toHaveBeenCalledTimes(2);
		expect(get(integrationStore).jellyfin).toBe(true);
	});
});
