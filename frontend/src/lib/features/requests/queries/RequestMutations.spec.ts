import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { POST: vi.fn(), DELETE: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { authStore } from '$lib/stores/authStore.svelte';
import type { Effect } from '$lib/queries/effects';
import { createCancelRequestMutation } from './RequestMutations.svelte';

type MutationResult<Vars> = {
	mutationFn: (vars: Vars) => Promise<unknown>;
	onMutate?: (vars: Vars) => { userId: string | undefined };
	effects: (data: unknown, vars: Vars, context: { userId: string | undefined }) => Effect[];
	onError?: (err: unknown, vars: Vars) => unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('createCancelRequestMutation', () => {
	it('declares nothing changed when the user changed mid-flight', () => {
		const mutation = createCancelRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		expect(
			mutation.effects({ success: true }, { mbid: 'mbid-1', kind: 'album' }, { userId: 'userB' })
		).toEqual([]);
	});
});
