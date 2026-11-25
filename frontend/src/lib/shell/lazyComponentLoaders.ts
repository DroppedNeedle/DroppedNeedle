import type { Component, Snippet } from 'svelte';

import type { PlaylistModalHandle } from '$lib/features/playlists/state/playlistModal.svelte';
import type WhatsNewModal from '$lib/shell/WhatsNewModal.svelte';

export async function loadAuthenticatedAppShell(): Promise<Component<{ children: Snippet }>> {
	return (await import('$lib/shell/AuthenticatedAppShell.svelte')).default;
}

export async function loadPlaylistModal(): Promise<
	Component<Record<string, never>, PlaylistModalHandle>
> {
	return (await import('$lib/features/playlists/components/AddToPlaylistModal.svelte')).default;
}

// What's New brings the markdown renderer and sanitizer, so it loads only
// when there are notes to show.
export async function loadWhatsNewModal(): Promise<typeof WhatsNewModal> {
	return (await import('$lib/shell/WhatsNewModal.svelte')).default;
}

export async function loadDiscographyModal(): Promise<Component> {
	return (await import('$lib/features/downloads/components/DiscographyDownloadModal.svelte'))
		.default;
}
