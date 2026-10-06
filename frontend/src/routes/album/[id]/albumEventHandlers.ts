import { goto } from '$app/navigation';
import { artistHref } from '$lib/utils/entityRoutes';
import type { AlbumBasicInfo, YouTubeTrackLink, YouTubeLink, YouTubeQuotaStatus } from '$lib/types';
import { compareDiscTrack, getDiscTrackKey } from '$lib/player/queueHelpers';
import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';

export interface EventHandlerDeps {
	getAlbum: () => AlbumBasicInfo | null;
	setAlbum: (a: AlbumBasicInfo | null) => void;
	getAlbumId: () => string;
	/** The edition the page shows, which a request fetches. */
	getSelectedRelease?: () => string | null;
	albumBasicCacheSet: (data: AlbumBasicInfo, key: string) => void;
	setTrackLinks: (tl: YouTubeTrackLink[]) => void;
	getTrackLinks: () => YouTubeTrackLink[];
	setAlbumLink: (l: YouTubeLink) => void;
	setQuota: (q: YouTubeQuotaStatus) => void;
	setRequesting: (v: boolean) => void;
	getRequesting: () => boolean;
	setShowDeleteModal: (v: boolean) => void;
	setToast: (msg: string, type: 'success' | 'error' | 'info' | 'warning') => void;
	setShowToast: (v: boolean) => void;
	onRequestSuccess?: () => void;
}

export function createEventHandlers(deps: EventHandlerDeps) {
	// createEventHandlers runs inside the album page's component initialization, so
	// the TanStack mutation hook resolves its QueryClient context here.
	const pageRequest = requestAlbum();

	function handleTrackGenerated(link: YouTubeTrackLink): void {
		const linkKey = getDiscTrackKey(link);
		deps.setTrackLinks(
			[...deps.getTrackLinks().filter((tl) => getDiscTrackKey(tl) !== linkKey), link].sort(
				compareDiscTrack
			)
		);
	}

	function handleTrackLinksUpdate(links: YouTubeTrackLink[]): void {
		deps.setTrackLinks([...links].sort(compareDiscTrack));
	}

	function handleAlbumLinkUpdate(link: YouTubeLink): void {
		deps.setAlbumLink(link);
	}

	function handleQuotaUpdate(q: YouTubeQuotaStatus): void {
		deps.setQuota(q);
	}

	async function handleRequest(): Promise<void> {
		const album = deps.getAlbum();
		if (!album || deps.getRequesting()) return;
		deps.setRequesting(true);
		try {
			const result = await pageRequest
				.mutateAsync({
					release_group_mbid: album.musicbrainz_id,
					artist_name: album.artist_name ?? undefined,
					album_title: album.title,
					year: album.year ?? undefined,
					artist_mbid: album.artist_id,
					release_mbid: deps.getSelectedRelease?.() ?? null
				})
				.catch(() => null);
			const current = deps.getAlbum();
			if (result?.success && current) {
				current.requested = true;
				deps.setAlbum(current);
				deps.albumBasicCacheSet(current, deps.getAlbumId());
				deps.setToast('Added to Library', 'success');
				deps.setShowToast(true);
				deps.onRequestSuccess?.();
			}
		} finally {
			deps.setRequesting(false);
		}
	}

	function handleDeleteClick(): void {
		deps.setShowDeleteModal(true);
	}

	function goToArtist(): void {
		const album = deps.getAlbum();
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- artistHref uses resolve() internally
		if (album?.artist_id) goto(artistHref(album.artist_id));
	}

	return {
		handleTrackGenerated,
		handleTrackLinksUpdate,
		handleAlbumLinkUpdate,
		handleQuotaUpdate,
		handleRequest,
		handleDeleteClick,
		goToArtist
	};
}
