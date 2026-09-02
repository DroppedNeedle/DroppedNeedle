import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { FreeMusicTasks } from './types';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => unknown) => factory())
}));

const mockGet = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: (...args: unknown[]) => mockGet(...args)
		}
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { getFreeMusicTasksQuery } from './FreeMusicQueries.svelte';

describe('Free Music task query', () => {
	beforeEach(() => mockGet.mockReset());

	it('preserves the pinned quality summary and hash from the task response', async () => {
		const response: FreeMusicTasks = {
			tasks: [
				{
					id: 'task-1',
					user_id: 'user-1',
					kind: 'album',
					mbid: 'release-1',
					artist: 'Brad Sucks',
					title: "Guess Who's a Mess",
					status: 'completed',
					created_at: 0,
					updated_at: 0,
					identifier: 'jamendo-117853',
					licence_url: '',
					format: 'mp3',
					files_total: 10,
					files_completed: 10,
					bytes_total: 1000,
					bytes_downloaded: 1000,
					error: null,
					quality_snapshot_hash: 'snapshot-hash',
					quality_snapshot_summary: 'Lossless preferred; MP3 320 fallback.'
				}
			]
		};
		mockGet.mockResolvedValueOnce(response);
		const signal = new AbortController().signal;
		const query = getFreeMusicTasksQuery(
			() => true,
			() => false
		) as unknown as {
			queryFn: (context: { signal: AbortSignal }) => Promise<FreeMusicTasks>;
		};

		await expect(query.queryFn({ signal })).resolves.toBe(response);
		expect(mockGet).toHaveBeenCalledWith('/api/v1/free-music/tasks', { signal });
		expect(response.tasks[0]).toMatchObject({
			quality_snapshot_hash: 'snapshot-hash',
			quality_snapshot_summary: 'Lossless preferred; MP3 320 fallback.'
		});
	});
});
