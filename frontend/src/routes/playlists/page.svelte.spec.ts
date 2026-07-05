import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { PlaylistListItem, PlaylistSummary, RedactedPlaylist } from '$lib/api/playlists';

// The list page consumes the user-scoped TanStack query + create mutation; stub both so
// the page renders without a QueryClientProvider and tests drive data directly.
const listQuery = {
	data: [] as PlaylistListItem[],
	isLoading: false,
	isError: false,
	error: null as Error | null,
	refetch: vi.fn()
};
const mockMutateAsync = vi.fn();

vi.mock('$lib/queries/playlists/PlaylistQuery.svelte', () => ({
	getPlaylistListQuery: () => listQuery
}));
vi.mock('$lib/queries/playlists/PlaylistMutations.svelte', () => ({
	createCreatePlaylistMutation: () => ({ mutateAsync: mockMutateAsync, isPending: false })
}));
vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({ data: undefined, isPending: false })
}));

// PlaylistCard pulls these in; the list tests never trigger them.
vi.mock('$lib/api/playlists', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/api/playlists')>()),
	fetchPlaylist: vi.fn(),
	deletePlaylist: vi.fn()
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

function makePlaylist(overrides: Partial<PlaylistSummary> = {}): PlaylistSummary {
	return {
		id: 'pl-1',
		name: 'Test Playlist',
		track_count: 5,
		total_duration: 900,
		cover_urls: [],
		custom_cover_url: null,
		source_ref: null,
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-02T00:00:00Z',
		is_public: false,
		is_owner: true,
		owner_name: null,
		is_redacted: false,
		...overrides
	};
}

function makeRedacted(overrides: Partial<RedactedPlaylist> = {}): RedactedPlaylist {
	return { id: 'pl-x', track_count: 7, owner_name: 'Cara', is_redacted: true, ...overrides };
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

	it('renders playlist cards with correct data', async () => {
		listQuery.data = [
			makePlaylist({ id: 'pl-1', name: 'Rock Mix', track_count: 10 }),
			makePlaylist({ id: 'pl-2', name: 'Chill Vibes', track_count: 3 })
		];
		render(PlaylistsPage);

		await expect.element(page.getByText('Rock Mix')).toBeVisible();
		await expect.element(page.getByText('Chill Vibes')).toBeVisible();
		await expect.element(page.getByText(/10 tracks/)).toBeVisible();
	});

	it('renders empty state when no playlists exist', async () => {
		listQuery.data = [];
		render(PlaylistsPage);

		await expect.element(page.getByText('No playlists yet')).toBeVisible();
		await expect.element(page.getByText('Create your first playlist')).toBeVisible();
	});

	it('renders error state when fetch fails', async () => {
		listQuery.isError = true;
		listQuery.error = new Error('Server error');
		render(PlaylistsPage);

		await expect.element(page.getByText('Server error')).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Retry/ })).toBeVisible();
	});

	it('shows new playlist input when clicking New Playlist', async () => {
		listQuery.data = [];
		render(PlaylistsPage);

		await expect.element(page.getByText('No playlists yet')).toBeVisible();
		const newBtn = page.getByRole('button', { name: /New Playlist/ }).first();
		await newBtn.click();

		await expect.element(page.getByPlaceholder('Playlist name...')).toBeVisible();
	});

	it('page heading is visible', async () => {
		listQuery.data = [];
		render(PlaylistsPage);
		await expect
			.element(page.getByRole('heading', { name: 'Playlists', exact: true }))
			.toBeVisible();
	});

	it('groups owned and shared playlists into labelled sections', async () => {
		listQuery.data = [
			makePlaylist({ id: 'mine', name: 'My Mix', is_owner: true }),
			makePlaylist({
				id: 'theirs',
				name: 'Ann Mix',
				is_owner: false,
				is_public: true,
				owner_name: 'Ann Smith'
			})
		];
		render(PlaylistsPage);

		await expect.element(page.getByRole('heading', { name: 'My Playlists' })).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Shared with you' })).toBeVisible();
		await expect.element(page.getByText(/Shared by Ann Smith/)).toBeVisible();
	});

	it('renders an admin redacted private card', async () => {
		listQuery.data = [makeRedacted({ id: 'priv', track_count: 4, owner_name: 'Cara' })];
		render(PlaylistsPage);

		await expect.element(page.getByText('Private playlist')).toBeVisible();
		await expect.element(page.getByText(/owned by Cara/)).toBeVisible();
	});

	it('hides the delete button on a shared (non-owned) card', async () => {
		listQuery.data = [
			makePlaylist({
				id: 'theirs',
				name: 'Ann Mix',
				is_owner: false,
				is_public: true,
				owner_name: 'Ann'
			})
		];
		render(PlaylistsPage);

		await expect.element(page.getByText('Ann Mix')).toBeVisible();
		expect(page.getByRole('button', { name: /Delete Ann Mix/ }).elements()).toHaveLength(0);
	});
});
