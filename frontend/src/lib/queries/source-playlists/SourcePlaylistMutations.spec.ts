import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { put: vi.fn(), v3: { POST: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('../QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { createSourcePlaylistImportMutation } from '../source-playlists/SourcePlaylistMutations.svelte';

const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

type MutationResult<Vars> = {
	mutationFn: (vars: Vars) => Promise<unknown>;
	onMutate?: (vars: Vars) => { userId: string | null | undefined };
	onSuccess?: (
		data: unknown,
		vars: Vars,
		context: { userId: string | null | undefined }
	) => unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('createSourcePlaylistImportMutation', () => {
	it('skips invalidation when the user changed mid-flight', async () => {
		const mutation = createSourcePlaylistImportMutation(
			() => 'plex'
		) as unknown as MutationResult<string>;
		await mutation.onSuccess?.({}, 'pl1', { userId: 'userB' });
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});
