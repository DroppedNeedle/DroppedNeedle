import { toastStore } from '$lib/stores/toast';
import { downloadBlob } from '$lib/utils/blobDownload';

/**
 * Shared behaviour for every album/track download button and menu item: fetch
 * through the api client so failures surface as toasts instead of failing
 * silently like the old anchor-click helper did.
 */
export async function downloadAlbumArchive(url: string): Promise<void> {
	try {
		await downloadBlob(url);
		toastStore.show({ message: 'Album download started', type: 'success' });
	} catch {
		toastStore.show({
			message: 'Could not download this album. It may have no local files left to archive.',
			type: 'error'
		});
	}
}

export async function downloadTrackFile(url: string, title: string): Promise<void> {
	try {
		await downloadBlob(url);
		toastStore.show({ message: `Download started: ${title}`, type: 'success' });
	} catch {
		toastStore.show({ message: `Could not download ${title}`, type: 'error' });
	}
}
