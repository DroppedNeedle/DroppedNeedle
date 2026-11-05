import {
	discographyDownloadStore,
	type DiscographyRelease
} from '$lib/features/downloads/state/discographyDownload.svelte';
import { fetchArtistDiscography } from '$lib/features/artist/queries/ArtistQueries.svelte';
import { toastStore } from '$lib/stores/toast';

/** Loads an artist's albums, EPs and singles and opens the discography dialog. */
export async function openDiscography(artistName: string, artistMbid: string): Promise<void> {
	try {
		const data = await fetchArtistDiscography(artistMbid);
		const releases: DiscographyRelease[] = [
			...data.albums.map((r) => ({ ...r, type: r.type ?? 'Album' })),
			...data.eps.map((r) => ({ ...r, type: r.type ?? 'EP' })),
			...data.singles.map((r) => ({ ...r, type: r.type ?? 'Single' }))
		];
		if (releases.length === 0) {
			toastStore.show({ message: 'No releases found for this artist', type: 'info' });
			return;
		}
		discographyDownloadStore.show(artistName, artistMbid, releases);
	} catch {
		toastStore.show({ message: 'Failed to load releases', type: 'error' });
	}
}
