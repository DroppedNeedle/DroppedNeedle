import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

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

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import { PlaylistQueryKeyFactory } from '../playlists/PlaylistQueryKeyFactory';
import { REMOTE_ENDPOINTS } from './endpoints';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import {
	createImportRemotePlaylistMutation,
	createSaveRemoteFoldersMutation
} from './RemoteMutations.svelte';

const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPut = vi.mocked(api.global.put);
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

function invalidatedPrefixes(): unknown[][] {
	return mockInvalidate.mock.calls.map((call) => [
		...((call[0] as { queryKey: readonly unknown[] }).queryKey ?? [])
	]);
}

describe('createImportRemotePlaylistMutation', () => {
	it('imports behind the source segment, then sweeps remotes plus local playlists', async () => {
		mockPost.mockResolvedValue({
			local_playlist_id: 'local-1',
			tracks_imported: 12,
			tracks_failed: 0,
			already_imported: false
		});
		const mutation = createImportRemotePlaylistMutation() as unknown as MutationResult<{
			source: 'plex';
			id: string;
		}>;
		const vars = { source: 'plex' as const, id: 'pl1' };

		await mutation.mutationFn(vars);
		expect(mockPost).toHaveBeenCalledWith(REMOTE_ENDPOINTS.importPlaylist('plex', 'pl1'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({}, vars, context);
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RemoteQueryKeyFactory.source('userA', 'plex')]);
		expect(swept).toContainEqual([...PlaylistQueryKeyFactory.v3.root('userA')]);
	});

	it('skips invalidation when the user changed mid-flight', async () => {
		const mutation = createImportRemotePlaylistMutation() as unknown as MutationResult<{
			source: 'plex';
			id: string;
		}>;
		await mutation.onSuccess?.({}, { source: 'plex', id: 'pl1' }, { userId: 'userB' });
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});

describe('createSaveRemoteFoldersMutation', () => {
	it('saves the navidrome scope and sweeps the adapter prefix plus the catalog', async () => {
		mockPut.mockResolvedValue({ mode: 'selected', folder_ids: ['7'] });
		const mutation = createSaveRemoteFoldersMutation() as unknown as MutationResult<{
			mode: string;
			selected_folder_ids: string[];
		}>;
		const vars = { mode: 'selected', selected_folder_ids: ['7'] };

		await mutation.mutationFn(vars);
		expect(mockPut).toHaveBeenCalledWith(REMOTE_ENDPOINTS.folders(), vars);

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({}, vars, context);
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RemoteQueryKeyFactory.source('userA', 'navidrome')]);
		expect(swept).toContainEqual([...LibraryQueryKeyFactory.all]);
	});
});
