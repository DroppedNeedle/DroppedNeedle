import { getContext, setContext } from 'svelte';
import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
import { getDownloadAccessQuery } from '$lib/features/album/queries/DownloadAccessQuery.svelte';
import { authStore } from '$lib/stores/authStore.svelte';

/** What an album tile offers: its Play and Shuffle buttons, and the menu's Download entry. */
export interface AlbumCardAccess {
	readonly canPlay: boolean;
	readonly canDownload: boolean;
}

const ACCESS = Symbol('albumCardAccess');
const NOTHING_SET_UP: AlbumCardAccess = { canPlay: false, canDownload: false };

/**
 * Called by the app shell, next to the integration status query it holds. Every
 * album tile under the shell reads these answers instead of holding its own.
 */
export function provideAlbumCardAccess(): void {
	const integrations = useIntegrationStatus();
	const downloadAccess = getDownloadAccessQuery(() => authStore.user?.id);
	const canPlay = $derived(
		integrations.current.localfiles ||
			integrations.current.navidrome ||
			integrations.current.jellyfin
	);
	// Download offers wait for the admin's "Library downloads" answer.
	const canDownload = $derived(
		integrations.current.localfiles && downloadAccess.data?.allowed === true
	);
	setContext<AlbumCardAccess>(ACCESS, {
		get canPlay() {
			return canPlay;
		},
		get canDownload() {
			return canDownload;
		}
	});
}

/** The access the app shell provides. Outside the shell nothing is set up. */
export function getAlbumCardAccess(): AlbumCardAccess {
	return getContext<AlbumCardAccess | undefined>(ACCESS) ?? NOTHING_SET_UP;
}
