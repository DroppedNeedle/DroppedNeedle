import type { Component, Snippet } from 'svelte';

import type { PlaylistModalHandle } from '$lib/features/playlists/state/playlistModal.svelte';

export async function loadAuthenticatedAppShell(): Promise<Component<{ children: Snippet }>> {
	return (await import('$lib/shell/AuthenticatedAppShell.svelte')).default;
}

export async function loadPlaylistModal(): Promise<
	Component<Record<string, never>, PlaylistModalHandle>
> {
	return (await import('$lib/features/playlists/components/AddToPlaylistModal.svelte')).default;
}

export async function loadDiscographyModal(): Promise<Component> {
	return (await import('$lib/features/downloads/components/DiscographyDownloadModal.svelte'))
		.default;
}
