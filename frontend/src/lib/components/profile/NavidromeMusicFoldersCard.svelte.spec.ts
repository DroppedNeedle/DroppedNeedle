import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { RemoteFolders } from '$lib/queries/remotes/types';

const h = vi.hoisted(() => ({
	preference: undefined as RemoteFolders | undefined,
	isPending: false,
	isError: false,
	isMutationError: false,
	isMutationSuccess: false,
	save: vi.fn().mockResolvedValue({}),
	isSaving: false
}));

vi.mock('$lib/queries/remotes/RemoteQueries.svelte', () => ({
	getRemoteFoldersQuery: () => ({
		get data() {
			return h.preference;
		},
		get isPending() {
			return h.isPending;
		},
		get isError() {
			return h.isError;
		}
	})
}));

vi.mock('$lib/queries/remotes/RemoteMutations.svelte', () => ({
	createSaveRemoteFoldersMutation: () => ({
		mutateAsync: h.save,
		get isPending() {
			return h.isSaving;
		},
		get isError() {
			return h.isMutationError;
		},
		get isSuccess() {
			return h.isMutationSuccess;
		}
	})
}));

import NavidromeMusicFoldersCard from './NavidromeMusicFoldersCard.svelte';

function preference(overrides: Partial<RemoteFolders> = {}): RemoteFolders {
	return {
		mode: 'all',
		folder_ids: [],
		available_folders: [
			{ id: 'folder-a', name: 'Folder A' },
			{ id: 'folder-b', name: 'Folder B' }
		],
		stale_folder_ids: [],
		source_available: true,
		...overrides
	};
}

beforeEach(() => {
	h.preference = preference();
	h.isPending = false;
	h.isError = false;
	h.isMutationError = false;
	h.isMutationSuccess = false;
	h.isSaving = false;
	vi.clearAllMocks();
});

describe('NavidromeMusicFoldersCard', () => {
	it('defaults to All folders and keeps the folder list visible', async () => {
		await render(NavidromeMusicFoldersCard, { userId: 'alice' });
		await expect.element(page.getByRole('radio', { name: /All folders/ })).toBeChecked();
		await page.getByRole('radio', { name: /Selected folders/ }).click();
		await expect.element(page.getByText('Folder A', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('Folder B', { exact: true })).toBeInTheDocument();
	});

	it('saves multiple selected folders in one mutation', async () => {
		await render(NavidromeMusicFoldersCard, { userId: 'alice' });
		await page.getByRole('radio', { name: /Selected folders/ }).click();
		await page.getByRole('checkbox', { name: /Folder A/ }).click();
		await page.getByRole('checkbox', { name: /Folder B/ }).click();
		await page.getByRole('button', { name: 'Save folders' }).click();
		expect(h.save).toHaveBeenCalledWith({
			mode: 'selected',
			selected_folder_ids: ['folder-a', 'folder-b']
		});
	});

	it('shows stale folders without widening to All', async () => {
		h.preference = preference({
			mode: 'selected',
			folder_ids: ['gone'],
			available_folders: [{ id: 'folder-b', name: 'Folder B' }],
			stale_folder_ids: ['gone']
		});
		await render(NavidromeMusicFoldersCard, { userId: 'alice' });
		await expect.element(page.getByRole('radio', { name: /Selected folders/ })).toBeChecked();
		await expect
			.element(page.getByText('Some saved folders are no longer available.'))
			.toBeInTheDocument();
		await expect.element(page.getByText('gone', { exact: true })).toBeInTheDocument();
		await page.getByRole('checkbox', { name: /Unavailable folder/ }).click();
		await page.getByRole('checkbox', { name: /Folder B/ }).click();
		await page.getByRole('button', { name: 'Save folders' }).click();
		expect(h.save).toHaveBeenCalledWith({
			mode: 'selected',
			selected_folder_ids: ['folder-b']
		});
	});

	it('makes the saved choice read-only while Navidrome is unavailable', async () => {
		h.preference = preference({ source_available: false });
		await render(NavidromeMusicFoldersCard, { userId: 'alice' });
		await expect.element(page.getByText(/Navidrome is unavailable/)).toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Save folders' })).toBeDisabled();
	});
});
