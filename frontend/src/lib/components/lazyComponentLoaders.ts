import type { Component, Snippet } from 'svelte';

import type { PlaylistModalHandle } from '$lib/stores/playlistModal.svelte';

export async function loadAuthenticatedAppShell(): Promise<Component<{ children: Snippet }>> {
	return (await import('$lib/components/AuthenticatedAppShell.svelte')).default;
}

export async function loadPlaylistModal(): Promise<
	Component<Record<string, never>, PlaylistModalHandle>
> {
	return (await import('$lib/components/AddToPlaylistModal.svelte')).default;
}

export async function loadDiscographyModal(): Promise<Component> {
	return (await import('$lib/features/downloads/components/DiscographyDownloadModal.svelte'))
		.default;
}
