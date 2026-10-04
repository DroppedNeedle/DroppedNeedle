import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { POST: vi.fn() } } }
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { PLEX_ENDPOINTS } from './endpoints';
import { createPlexPollMutation, createPlexStartMutation } from './PlexAuthMutations.svelte';
import type { PlexPurpose } from './types';

const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

type MutationResult<Vars> = {
	mutationFn: (vars: Vars) => Promise<unknown>;
	onError?: (err: unknown, vars: Vars) => unknown;
};

beforeEach(() => {
	vi.clearAllMocks();
});

describe('createPlexStartMutation', () => {
	it('starts the requested purpose with no body', async () => {
		mockPost.mockResolvedValue({ pin_id: 9, authorize_url: 'https://plex.tv/link' });
		const mutation = createPlexStartMutation() as unknown as MutationResult<{
			purpose: PlexPurpose;
		}>;

		await mutation.mutationFn({ purpose: 'connect' });
		expect(mockPost).toHaveBeenCalledWith(PLEX_ENDPOINTS.start('connect'));
	});
});

describe('createPlexPollMutation', () => {
	it('polls the bound purpose route with the pin id', async () => {
		mockPost.mockResolvedValue({ completed: true, token: 'tok' });
		const mutation = createPlexPollMutation('connect') as unknown as MutationResult<number>;

		const result = await mutation.mutationFn(9);
		expect(mockPost).toHaveBeenCalledWith(PLEX_ENDPOINTS.poll('connect'), { pin_id: 9 });
		expect(result).toEqual({ completed: true, token: 'tok' });
	});
});
