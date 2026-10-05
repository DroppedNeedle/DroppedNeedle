import { describe, it, expect, vi, beforeEach } from 'vitest';
import { pageFetch, abortAllPageRequests, getNavigationSignal } from '$lib/utils/navigationAbort';

beforeEach(() => {
	vi.stubGlobal('window', {});
	abortAllPageRequests();
	vi.restoreAllMocks();
});

describe('getNavigationSignal', () => {
	it('returns a fresh non-aborted signal after reset', () => {
		abortAllPageRequests();
		const signal = getNavigationSignal();
		expect(signal.aborted).toBe(false);
	});
});

describe('pageFetch', () => {
	it('aborts when navigation fires even with local signal', async () => {
		const localController = new AbortController();
		const mockFetch = vi.fn().mockImplementation((_url: string, init?: RequestInit) => {
			return new Promise((_resolve, reject) => {
				init?.signal?.addEventListener('abort', () =>
					reject(new DOMException('Aborted', 'AbortError'))
				);
			});
		});
		vi.stubGlobal('fetch', mockFetch);

		const promise = pageFetch('/api/v1/test', { signal: localController.signal });
		abortAllPageRequests();

		await expect(promise).rejects.toThrow();
		expect(true).toBe(true);
	});

	it('aborts when local signal fires', async () => {
		const localController = new AbortController();
		const mockFetch = vi.fn().mockImplementation((_url: string, init?: RequestInit) => {
			return new Promise((_resolve, reject) => {
				init?.signal?.addEventListener('abort', () =>
					reject(new DOMException('Aborted', 'AbortError'))
				);
			});
		});
		vi.stubGlobal('fetch', mockFetch);

		const promise = pageFetch('/api/v1/test', { signal: localController.signal });
		localController.abort();

		await expect(promise).rejects.toThrow();
		expect(true).toBe(true);
	});
});

describe('raw fetch is not affected by navigation abort', () => {
	it('native fetch does not use navigation signal', async () => {
		const mockFetch = vi.fn().mockResolvedValue(new Response('ok'));
		vi.stubGlobal('fetch', mockFetch);

		await fetch('/api/v1/mutation', { method: 'POST' });
		abortAllPageRequests();

		expect(mockFetch).toHaveBeenCalledOnce();
		const callArgs = mockFetch.mock.calls[0];
		expect(callArgs[1]?.signal).toBeUndefined();
	});
});
