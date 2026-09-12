import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { createRawSnippet } from 'svelte';
import AlbumSourceBar from './AlbumSourceBar.svelte';

const iconSnippet = createRawSnippet(() => ({
	render: () => '<span data-testid="source-icon">⚡</span>'
}));

function makeProps(overrides: Record<string, unknown> = {}) {
	return {
		sourceLabel: 'Jellyfin',
		sourceColor: '#00a4dc',
		trackCount: 10,
		totalTracks: 12,
		onPlayAll: vi.fn(),
		onShuffle: vi.fn(),
		icon: iconSnippet,
		...overrides
	};
}

async function renderBar(overrides: Record<string, unknown> = {}) {
	return await render(AlbumSourceBar, {
		props: makeProps(overrides)
	} as unknown as Parameters<typeof render<typeof AlbumSourceBar>>[1]);
}

describe('AlbumSourceBar.svelte', () => {
	it('renders source label and track count', async () => {
		await renderBar();
		await expect.element(page.getByText('Jellyfin')).toBeVisible();
		await expect.element(page.getByText('10/12')).toBeVisible();
	});

	it('renders Play All and Shuffle buttons', async () => {
		await renderBar();
		await expect.element(page.getByText('Play All')).toBeVisible();
		await expect.element(page.getByText('Shuffle')).toBeVisible();
	});

	it('fires onPlayAll callback when Play All is clicked', async () => {
		const props = makeProps();
		await render(AlbumSourceBar, { props } as unknown as Parameters<
			typeof render<typeof AlbumSourceBar>
		>[1]);
		await page.getByText('Play All').click();
		expect(props.onPlayAll).toHaveBeenCalledOnce();
	});

	it('fires onShuffle callback when Shuffle is clicked', async () => {
		const props = makeProps();
		await render(AlbumSourceBar, { props } as unknown as Parameters<
			typeof render<typeof AlbumSourceBar>
		>[1]);
		await page.getByText('Shuffle').click();
		expect(props.onShuffle).toHaveBeenCalledOnce();
	});

	it('shows context menu with "Add All to Playlist" when onAddAllToPlaylist is provided', async () => {
		const onAddAllToPlaylist = vi.fn();
		await renderBar({ onAddAllToPlaylist });
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		await expect.element(page.getByText('Add All to Playlist')).toBeVisible();
	});

	it('fires onAddAllToPlaylist callback when "Add All to Playlist" is clicked', async () => {
		const onAddAllToPlaylist = vi.fn();
		await renderBar({ onAddAllToPlaylist });
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		await page.getByText('Add All to Playlist').click();
		expect(onAddAllToPlaylist).toHaveBeenCalledOnce();
	});

	it('does not show context menu when no optional callbacks are provided', async () => {
		await renderBar();
		const triggers = page.getByLabelText('More actions');
		await expect.element(triggers).not.toBeInTheDocument();
	});

	it('shows "Add All to Queue" and "Play All Next" in context menu when callbacks are provided', async () => {
		const onAddAllToQueue = vi.fn();
		const onPlayAllNext = vi.fn();
		await renderBar({ onAddAllToQueue, onPlayAllNext });
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		await expect.element(page.getByText('Add All to Queue')).toBeVisible();
		await expect.element(page.getByText('Play All Next')).toBeVisible();
	});

	it('hides buttons when trackCount is 0', async () => {
		await renderBar({ trackCount: 0 });
		const playAll = page.getByText('Play All');
		await expect.element(playAll).not.toBeInTheDocument();
	});
});
