import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { put: vi.fn(), v3: { POST: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { authStore } from '$lib/stores/authStore.svelte';
import type { Effect } from '../effects';
import { createSourcePlaylistImportMutation } from '../source-playlists/SourcePlaylistMutations.svelte';

type MutationResult<Vars> = {
	mutationFn: (vars: Vars) => Promise<unknown>;
	onMutate?: (vars: Vars) => { userId: string | null | undefined };
	effects: (data: unknown, vars: Vars, context: { userId: string | null | undefined }) => Effect[];
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('createSourcePlaylistImportMutation', () => {
	it('declares nothing changed when the user changed mid-flight', () => {
		const mutation = createSourcePlaylistImportMutation(
			() => 'plex'
		) as unknown as MutationResult<string>;
		expect(mutation.effects({}, 'pl1', { userId: 'userB' })).toEqual([]);
		expect(mutation.effects({}, 'pl1', { userId: 'userA' }).length).toBeGreaterThan(0);
	});
});
