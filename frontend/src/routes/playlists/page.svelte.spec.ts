import { page } from 'vitest/browser';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';
import type { PlaylistListItemV3 } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';

// The list page consumes the user-scoped TanStack query + create mutation; stub both so
// the page renders without a QueryClientProvider and tests drive data directly.
const listQuery = {
	data: [] as PlaylistListItemV3[],
	isLoading: false,
	isError: false,
	error: null as Error | null,
	refetch: vi.fn()
};
const mockMutateAsync = vi.fn();

vi.mock('$lib/features/playlists/queries/PlaylistV3Queries.svelte', () => ({
	getPlaylistListV3Query: () => listQuery
}));
vi.mock('$lib/features/playlists/queries/PlaylistV3Mutations.svelte', () => ({
	createPlaylistV3Mutation: () => ({ mutateAsync: mockMutateAsync, isPending: false }),
	createDeletePlaylistV3Mutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/features/profile/queries/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({ data: undefined, isPending: false })
}));

// PlaylistCard pulls this in; the list tests never trigger it.
vi.mock('$lib/api/playlists', () => ({
	isRedactedPlaylist: (p: { is_redacted?: boolean } | null | undefined) => p?.is_redacted === true,
	fetchPlaylist: vi.fn()
}));

const mockToastShow = vi.fn();
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => mockToastShow(...args) }
}));

const mockGoto = vi.fn();
vi.mock('$app/navigation', () => ({
	goto: (...args: unknown[]) => mockGoto(...args)
}));

import PlaylistsPage from './+page.svelte';

function makePlaylist(overrides: Record<string, unknown> = {}): PlaylistListItemV3 {
	return {
		id: 'pl-1',
		name: 'Test Playlist',
		track_count: 5,
		total_duration: 900,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: 1767225600,
		updated_at: 1767312000,
		is_public: false,
		is_owner: true,
		can_delete: false,
		owner_name: null,
		is_redacted: false,
		...overrides
	};
}

function makeRedacted(overrides: Record<string, unknown> = {}): PlaylistListItemV3 {
	return {
		id: 'pl-x',
		track_count: 7,
		owner_name: 'Cara',
		can_delete: false,
		is_redacted: true,
		...overrides
	};
}

describe('Playlists list page', () => {
	beforeEach(() => {
		listQuery.data = [];
		listQuery.isLoading = false;
		listQuery.isError = false;
		listQuery.error = null;
		listQuery.refetch.mockReset();
		mockMutateAsync.mockReset();
		mockToastShow.mockReset();
		mockGoto.mockReset();
	});

	it('offers delete exactly where the server says so, on redacted and public cards alike', async () => {
		{
			// Renders a redacted private card with no delete.
			listQuery.data = [makeRedacted({ id: 'priv', track_count: 4, owner_name: 'Cara' })];
			await render(PlaylistsPage);

			await expect
				.element(page.getByRole('heading', { name: 'Private', exact: true }))
				.toBeVisible();
			await expect.element(page.getByText('Private playlist')).toBeVisible();
			await expect.element(page.getByText(/owned by Cara/)).toBeVisible();
			expect(page.getByRole('button', { name: /Delete/ }).elements()).toHaveLength(0);
		}

		{
			// Marks a private playlist with no owner and offers delete where the server says so.
			cleanup();
			listQuery.data = [makeRedacted({ id: 'stray', owner_name: null, can_delete: true })];
			await render(PlaylistsPage);

			await expect.element(page.getByText(/No owner/)).toBeVisible();
			await expect
				.element(page.getByRole('button', { name: 'Delete private playlist with no owner' }))
				.toBeVisible();
		}

		{
			// Shows delete on a public playlist with no owner where the server says so.
			cleanup();
			listQuery.data = [
				makePlaylist({
					id: 'stray',
					name: 'Old Mix',
					is_owner: false,
					is_public: true,
					owner_name: null,
					can_delete: true
				})
			];
			await render(PlaylistsPage);

			await expect.element(page.getByText('No owner')).toBeVisible();
			await expect.element(page.getByRole('button', { name: 'Delete Old Mix' })).toBeVisible();
		}

		{
			// Hides the delete button on a shared (non-owned) card.
			cleanup();
			listQuery.data = [
				makePlaylist({
					id: 'theirs',
					name: 'Ann Mix',
					is_owner: false,
					is_public: true,
					owner_name: 'Ann'
				})
			];
			await render(PlaylistsPage);

			await expect.element(page.getByText('Ann Mix')).toBeVisible();
			expect(page.getByRole('button', { name: /Delete Ann Mix/ }).elements()).toHaveLength(0);
		}
	});
});
