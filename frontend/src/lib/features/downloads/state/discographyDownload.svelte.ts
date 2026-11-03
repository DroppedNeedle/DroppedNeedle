/**
 * Global state for the Discography Download modal.
 * Opened from artist cards, hero section, or ReleaseList headers.
 */
export type DiscographyRelease = {
	id: string;
	title: string;
	type?: string | null;
	year?: number | null;
	in_library?: boolean;
	requested?: boolean;
};

type DiscographyDownloadState = {
	open: boolean;
	artistName: string;
	artistId: string;
	releases: DiscographyRelease[];
};

const initial: DiscographyDownloadState = {
	open: false,
	artistName: '',
	artistId: '',
	releases: []
};

let state = $state<DiscographyDownloadState>({ ...initial });

export const discographyDownloadStore = {
	get open() {
		return state.open;
	},
	get artistName() {
		return state.artistName;
	},
	get artistId() {
		return state.artistId;
	},
	get releases() {
		return state.releases;
	},

	show(artistName: string, artistId: string, releases: DiscographyRelease[]) {
		state = { open: true, artistName, artistId, releases };
	},

	close() {
		state = { ...initial };
	}
};
