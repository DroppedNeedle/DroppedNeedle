import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { POST: vi.fn() } } }
}));

import { api } from '$lib/api/client';
import { PLEX_ENDPOINTS } from './endpoints';
import { pollPlexFlow, startPlexFlow } from './PlexFlowApi';

const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

beforeEach(() => {
	vi.clearAllMocks();
});

describe('startPlexFlow', () => {
	it('mints a pin for one purpose with no body', async () => {
		mockPost.mockResolvedValue({ pin_id: 7, authorize_url: 'https://plex.tv/link' });
		const started = await startPlexFlow('login');

		expect(mockPost).toHaveBeenCalledWith(PLEX_ENDPOINTS.start('login'));
		expect(started).toEqual({ pin_id: 7, authorize_url: 'https://plex.tv/link' });
	});
});

describe('pollPlexFlow', () => {
	it('polls the purpose route with the pin id', async () => {
		mockPost.mockResolvedValue({ completed: false });
		const result = await pollPlexFlow('link', 7);

		expect(mockPost).toHaveBeenCalledWith(PLEX_ENDPOINTS.poll('link'), { pin_id: 7 });
		expect(result).toEqual({ completed: false });
	});
});
