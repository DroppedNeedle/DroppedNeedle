import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), DELETE: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

// The real effects module loads the cache helpers; stub them.
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { createRevokeSessionMutation } from '$lib/features/profile/queries/sessions.svelte';

type MutationResult = {
	mutationFn: (vars: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown) => { userId: string | undefined };
	onSuccess: (
		data: unknown,
		vars: unknown,
		context: { userId: string | undefined }
	) => Promise<void>;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('createRevokeSessionMutation', () => {
	it('declares nothing changed when the user changed mid-flight', async () => {
		const mutation = createRevokeSessionMutation() as unknown as MutationResult;
		const vars = { id: 'sess-9', label: 'Office laptop' };

		await mutation.onSuccess(undefined, vars, { userId: 'userB' });
		expect(invalidateQueriesWithPersister).not.toHaveBeenCalled();
	});
});
