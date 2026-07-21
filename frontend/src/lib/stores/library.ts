import { get, writable } from 'svelte/store';

interface LibraryState {
	mbidSet: Set<string>;
	requestedSet: Set<string>;
	loading: boolean;
	lastUpdated: number | null;
	initialized: boolean;
}

const initialState: LibraryState = {
	mbidSet: new Set(),
	requestedSet: new Set(),
	loading: false,
	lastUpdated: null,
	initialized: false
};

function createLibraryStore() {
	const { subscribe, update } = writable<LibraryState>(initialState);
	let sessionUserId: string | null = null;

	function setSession(userId: string | null) {
		if (sessionUserId === userId) return;
		sessionUserId = userId;
		update(() => initialState);
	}

	function isInLibrary(mbid: string | null | undefined): boolean {
		if (!mbid) return false;
		return get({ subscribe }).mbidSet.has(mbid.toLowerCase());
	}

	function addMbid(mbid: string) {
		update((state) => {
			const normalized = mbid.toLowerCase();
			const mbidSet = new Set(state.mbidSet);
			const requestedSet = new Set(state.requestedSet);
			mbidSet.add(normalized);
			requestedSet.delete(normalized);
			return { ...state, mbidSet, requestedSet };
		});
	}

	function removeMbid(mbid: string) {
		update((state) => {
			const normalized = mbid.toLowerCase();
			const mbidSet = new Set(state.mbidSet);
			const requestedSet = new Set(state.requestedSet);
			mbidSet.delete(normalized);
			requestedSet.delete(normalized);
			return { ...state, mbidSet, requestedSet };
		});
	}

	function addRequested(mbid: string) {
		update((state) => {
			const normalized = mbid.toLowerCase();
			if (state.mbidSet.has(normalized)) return state;
			const requestedSet = new Set(state.requestedSet);
			requestedSet.add(normalized);
			return { ...state, requestedSet };
		});
	}

	function isRequested(mbid: string | null | undefined): boolean {
		if (!mbid) return false;
		const normalized = mbid.toLowerCase();
		const state = get({ subscribe });
		return state.requestedSet.has(normalized) && !state.mbidSet.has(normalized);
	}

	return {
		subscribe,
		setSession,
		isInLibrary,
		addMbid,
		removeMbid,
		isRequested,
		addRequested
	};
}

export const libraryStore = createLibraryStore();
