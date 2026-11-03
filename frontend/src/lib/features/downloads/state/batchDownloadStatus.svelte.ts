/**
 * Tracks in-flight batch download progress.
 * Fed by polling the library store after a batch request is submitted.
 */

type BatchDownloadJob = {
	artistName: string;
	artistId: string;
	total: number;
	musicbrainzIds: string[];
	startedAt: number;
};

type BatchState = {
	jobs: BatchDownloadJob[];
	minimized: boolean;
};

const state = $state<BatchState>({ jobs: [], minimized: false });

export const batchDownloadStore = {
	get jobs() {
		return state.jobs;
	},
	get hasActive() {
		return state.jobs.length > 0;
	},
	get minimized() {
		return state.minimized;
	},

	addJob(artistName: string, artistId: string, musicbrainzIds: string[]) {
		const existing = state.jobs.find((j) => j.artistId === artistId);
		if (existing) {
			const merged = new Set([...existing.musicbrainzIds, ...musicbrainzIds]); // eslint-disable-line svelte/prefer-svelte-reactivity
			existing.musicbrainzIds = [...merged];
			existing.total = merged.size;
			state.jobs = [...state.jobs];
			return;
		}
		state.jobs = [
			...state.jobs,
			{
				artistName,
				artistId,
				total: musicbrainzIds.length,
				musicbrainzIds,
				startedAt: Date.now()
			}
		];
	},

	removeJob(artistId: string) {
		state.jobs = state.jobs.filter((j) => j.artistId !== artistId);
	},

	toggleMinimized() {
		state.minimized = !state.minimized;
	},

	clear() {
		state.jobs = [];
	}
};
