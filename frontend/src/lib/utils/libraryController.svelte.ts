import { isAbortError } from '$lib/utils/errorHandling';
import { ApiError } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { playerStore } from '$lib/stores/player.svelte';
import type { QueueItem } from '$lib/player/types';
import type { MenuItem } from '$lib/components/ContextMenu.svelte';
import { ListPlus, ListStart, ListMusic, Download } from 'lucide-svelte';
import { downloadAlbumArchive } from '$lib/utils/downloadActions';
import { LocalDownloadsApi } from '$lib/queries/local/LocalDownloadsApi';
import type { LocalAlbumSummary } from '$lib/types';

const PAGE_SIZE = 48;

export interface SidebarData<TAlbum> {
	recentAlbums: TAlbum[];
	favoriteAlbums: TAlbum[];
	genres: string[];
	moods: string[];
	stats: Record<string, unknown> | null;
}

export interface LibraryAdapter<TAlbum> {
	sourceType: 'jellyfin' | 'navidrome' | 'local' | 'plex';

	getAlbumId(album: TAlbum): string | number;
	getAlbumName(album: TAlbum): string;
	getArtistName(album: TAlbum): string;
	getAlbumMbid(album: TAlbum): string | undefined;
	getAlbumImageUrl(album: TAlbum): string | null;
	getAlbumYear(album: TAlbum): number | null | undefined;

	fetchAlbums(params: {
		limit: number;
		offset: number;
		sortBy: string;
		sortOrder: string;
		genre?: string;
		mood?: string;
		decade?: string;
		tag?: string;
		search?: string;
		signal: AbortSignal;
	}): Promise<{ items: TAlbum[]; total: number }>;

	fetchSidebarData(
		signal: AbortSignal,
		current: SidebarData<TAlbum>
	): Promise<{ data: SidebarData<TAlbum>; hasFreshData: boolean }>;

	fetchAlbumQueueItems(album: TAlbum): Promise<QueueItem[]>;
	launchPlayback(album: TAlbum, shuffle: boolean): Promise<void>;

	getAlbumsListCached(
		key: string
	): { data: { items: TAlbum[]; total: number }; timestamp: number } | null;
	setAlbumsListCached(key: string, data: { items: TAlbum[]; total: number }): void;
	isAlbumsListCacheStale(timestamp: number): boolean;
	getSidebarCached(): { data: SidebarData<TAlbum>; timestamp: number } | null;
	setSidebarCached(data: SidebarData<TAlbum>): void;
	isSidebarCacheStale(timestamp: number): boolean;

	sortOptions: { value: string; label: string }[];
	defaultSortBy: string;
	ascValue: string;
	descValue: string;
	getDefaultSortOrder(field: string): string;
	supportsGenres: boolean;
	supportsMoods: boolean;
	supportsDecades: boolean;
	supportsTags: boolean;
	supportsFavorites: boolean;
	supportsShuffle: boolean;
	errorMessage: string;
}

export function createLibraryController<TAlbum>(
	adapter: LibraryAdapter<TAlbum>,
	options?: { initialSortBy?: string; initialSortOrder?: string }
) {
	let albums = $state<TAlbum[]>([]);
	let recentAlbums = $state<TAlbum[]>([]);
	let favoriteAlbums = $state<TAlbum[]>([]);
	let genres = $state<string[]>([]);
	let moods = $state<string[]>([]);
	let stats = $state<Record<string, unknown> | null>(null);
	let total = $state(0);
	let loading = $state(true);
	let loadingMore = $state(false);
	let fetchError = $state('');
	let fetchErrorCode = $state('');

	let sortBy = $state(options?.initialSortBy ?? adapter.defaultSortBy);
	let sortOrder = $state(
		options?.initialSortOrder ??
			(options?.initialSortBy
				? adapter.getDefaultSortOrder(options.initialSortBy)
				: adapter.ascValue)
	);
	let selectedGenre = $state('');
	let selectedMood = $state('');
	let selectedDecade = $state('');
	let selectedTag = $state('');
	let searchQuery = $state('');
	let searchTimeout: ReturnType<typeof setTimeout> | null = null;
	let fetchId = 0;
	let albumsAbortController: AbortController | null = null;
	let sidebarAbortController: AbortController | null = null;
	let playingAlbumId = $state<string | number | null>(null);

	let detailModalOpen = $state(false);
	let selectedAlbum = $state<TAlbum | null>(null);
	let menuLoadingAlbumId = $state<string | number | null>(null);
	let playlistModalRef = $state<{ open: (tracks: QueueItem[]) => void } | null>(null);

	function getCacheKey(offset: number): string {
		const search = searchQuery.trim() || '';
		const genre = selectedGenre || '';
		const mood = selectedMood || '';
		const decade = selectedDecade || '';
		const tag = selectedTag || '';
		return `${sortBy}:${sortOrder}:${genre}:${mood}:${decade}:${tag}:${search}:${PAGE_SIZE}:${offset}`;
	}

	async function fetchAlbums(reset = false): Promise<void> {
		const id = ++fetchId;
		fetchError = '';
		fetchErrorCode = '';

		if (albumsAbortController) albumsAbortController.abort();
		albumsAbortController = new AbortController();
		const signal = albumsAbortController.signal;

		if (reset) {
			loading = true;
			albums = [];
		} else {
			loadingMore = true;
		}

		try {
			const offset = reset ? 0 : albums.length;
			const cacheKey = getCacheKey(offset);
			const cached = adapter.getAlbumsListCached(cacheKey);
			const albumsBeforeCache = [...albums];
			if (cached) {
				albums = reset ? cached.data.items : [...albums, ...cached.data.items];
				total = cached.data.total;
				if (!adapter.isAlbumsListCacheStale(cached.timestamp)) {
					loading = false;
					loadingMore = false;
					return;
				}
				loading = false;
			}

			const data = await adapter.fetchAlbums({
				limit: PAGE_SIZE,
				offset,
				sortBy,
				sortOrder,
				genre: selectedGenre || undefined,
				mood: selectedMood || undefined,
				decade: selectedDecade || undefined,
				tag: selectedTag || undefined,
				search: searchQuery.trim() || undefined,
				signal
			});
			if (id !== fetchId) return;

			albums = reset ? data.items : [...albumsBeforeCache, ...data.items];
			total = data.total;
			adapter.setAlbumsListCached(cacheKey, { items: data.items, total: data.total });
		} catch (e) {
			if (isAbortError(e)) return;
			if (id === fetchId) {
				fetchError = e instanceof ApiError ? e.message : adapter.errorMessage;
				fetchErrorCode = e instanceof ApiError ? e.code : '';
			}
		} finally {
			if (id === fetchId) {
				loading = false;
				loadingMore = false;
			}
		}
	}

	async function fetchSidebar(forceRefresh = false): Promise<void> {
		const cached = adapter.getSidebarCached();
		if (cached && !forceRefresh) {
			recentAlbums = cached.data.recentAlbums;
			favoriteAlbums = cached.data.favoriteAlbums;
			genres = cached.data.genres;
			moods = cached.data.moods ?? [];
			stats = cached.data.stats;
			if (!adapter.isSidebarCacheStale(cached.timestamp)) return;
		}

		if (sidebarAbortController) sidebarAbortController.abort();
		sidebarAbortController = new AbortController();

		try {
			const { data: result, hasFreshData } = await adapter.fetchSidebarData(
				sidebarAbortController.signal,
				{ recentAlbums, favoriteAlbums, genres, moods, stats }
			);
			recentAlbums = result.recentAlbums;
			favoriteAlbums = result.favoriteAlbums;
			genres = result.genres;
			moods = result.moods ?? [];
			stats = result.stats;
			if (hasFreshData) adapter.setSidebarCached(result);
		} catch (e) {
			if (isAbortError(e)) return;
		}
	}

	function openDetail(album: TAlbum): void {
		selectedAlbum = album;
		detailModalOpen = true;
	}

	function handleDetailClose(): void {
		selectedAlbum = null;
	}

	function handleSortChange(value: string): void {
		if (value !== sortBy) {
			sortBy = value;
			sortOrder = adapter.getDefaultSortOrder(value);
		}
		fetchAlbums(true);
	}

	function toggleSortOrder(): void {
		sortOrder = sortOrder === adapter.ascValue ? adapter.descValue : adapter.ascValue;
		fetchAlbums(true);
	}

	function handleGenreChange(value: string): void {
		selectedGenre = value;
		fetchAlbums(true);
	}

	function handleMoodChange(value: string): void {
		selectedMood = value;
		fetchAlbums(true);
	}

	function handleDecadeChange(value: string): void {
		selectedDecade = value;
		fetchAlbums(true);
	}

	function handleTagChange(value: string): void {
		selectedTag = value;
		fetchAlbums(true);
	}

	function handleSearch(): void {
		if (searchTimeout) clearTimeout(searchTimeout);
		searchTimeout = setTimeout(() => fetchAlbums(true), 300);
	}

	function loadMore(): void {
		if (!loadingMore && albums.length < total) fetchAlbums(false);
	}

	async function quickPlay(album: TAlbum, e: Event): Promise<void> {
		e.stopPropagation();
		playingAlbumId = adapter.getAlbumId(album);
		try {
			await adapter.launchPlayback(album, false);
		} catch (e) {
			if (!isAbortError(e)) toastStore.show({ message: 'Playback failed', type: 'error' });
		} finally {
			playingAlbumId = null;
		}
	}

	async function quickShuffle(album: TAlbum, e: Event): Promise<void> {
		e.stopPropagation();
		playingAlbumId = adapter.getAlbumId(album);
		try {
			await adapter.launchPlayback(album, true);
		} catch (e) {
			if (!isAbortError(e)) toastStore.show({ message: 'Playback failed', type: 'error' });
		} finally {
			playingAlbumId = null;
		}
	}

	async function addAlbumToQueue(album: TAlbum): Promise<void> {
		menuLoadingAlbumId = adapter.getAlbumId(album);
		try {
			const items = await adapter.fetchAlbumQueueItems(album);
			if (items.length === 0) {
				toastStore.show({ message: 'No tracks found for this album', type: 'info' });
				return;
			}
			playerStore.addMultipleToQueue(items);
		} catch (e) {
			if (!isAbortError(e))
				toastStore.show({ message: 'Failed to load album tracks', type: 'error' });
		} finally {
			menuLoadingAlbumId = null;
		}
	}

	async function playAlbumNext(album: TAlbum): Promise<void> {
		menuLoadingAlbumId = adapter.getAlbumId(album);
		try {
			const items = await adapter.fetchAlbumQueueItems(album);
			if (items.length === 0) {
				toastStore.show({ message: 'No tracks found for this album', type: 'info' });
				return;
			}
			playerStore.playMultipleNext(items);
		} catch (e) {
			if (!isAbortError(e))
				toastStore.show({ message: 'Failed to load album tracks', type: 'error' });
		} finally {
			menuLoadingAlbumId = null;
		}
	}

	async function addAlbumToPlaylist(album: TAlbum): Promise<void> {
		menuLoadingAlbumId = adapter.getAlbumId(album);
		try {
			const items = await adapter.fetchAlbumQueueItems(album);
			if (items.length === 0) {
				toastStore.show({ message: 'No tracks found for this album', type: 'info' });
				return;
			}
			playlistModalRef?.open(items);
		} catch (e) {
			if (!isAbortError(e))
				toastStore.show({ message: 'Failed to load album tracks', type: 'error' });
		} finally {
			menuLoadingAlbumId = null;
		}
	}

	function getAlbumMenuItems(album: TAlbum): MenuItem[] {
		const isLoading = menuLoadingAlbumId === adapter.getAlbumId(album);
		const items: MenuItem[] = [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => void addAlbumToQueue(album),
				disabled: isLoading
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => void playAlbumNext(album),
				disabled: isLoading
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: () => void addAlbumToPlaylist(album),
				disabled: isLoading
			}
		];
		if (adapter.sourceType === 'local') {
			const localAlbum = album as LocalAlbumSummary;
			if (localAlbum.download_allowed !== false) {
				items.push({
					label: 'Download Album',
					icon: Download,
					onclick: () =>
						void downloadAlbumArchive(LocalDownloadsApi.albumByMbid(localAlbum.musicbrainz_id))
				});
			}
		}
		return items;
	}

	function init(): void {
		fetchAlbums(true);
		fetchSidebar();
	}

	function cleanup(): void {
		if (searchTimeout) clearTimeout(searchTimeout);
		if (albumsAbortController) {
			albumsAbortController.abort();
			albumsAbortController = null;
		}
		if (sidebarAbortController) {
			sidebarAbortController.abort();
			sidebarAbortController = null;
		}
	}

	return {
		get albums() {
			return albums;
		},
		get recentAlbums() {
			return recentAlbums;
		},
		get favoriteAlbums() {
			return favoriteAlbums;
		},
		get genres() {
			return genres;
		},
		get moods() {
			return moods;
		},
		get stats() {
			return stats;
		},
		get total() {
			return total;
		},
		get loading() {
			return loading;
		},
		get loadingMore() {
			return loadingMore;
		},
		get fetchError() {
			return fetchError;
		},
		get fetchErrorCode() {
			return fetchErrorCode;
		},
		get sortBy() {
			return sortBy;
		},
		get sortOrder() {
			return sortOrder;
		},
		get selectedGenre() {
			return selectedGenre;
		},
		get selectedMood() {
			return selectedMood;
		},
		get selectedDecade() {
			return selectedDecade;
		},
		get selectedTag() {
			return selectedTag;
		},
		get searchQuery() {
			return searchQuery;
		},
		set searchQuery(v: string) {
			searchQuery = v;
		},
		get playingAlbumId() {
			return playingAlbumId;
		},
		get detailModalOpen() {
			return detailModalOpen;
		},
		set detailModalOpen(v: boolean) {
			detailModalOpen = v;
		},
		get selectedAlbum() {
			return selectedAlbum;
		},
		get playlistModalRef() {
			return playlistModalRef;
		},
		set playlistModalRef(v) {
			playlistModalRef = v;
		},

		adapter,

		fetchAlbums,
		fetchSidebar,
		openDetail,
		handleDetailClose,
		handleSortChange,
		toggleSortOrder,
		handleGenreChange,
		handleMoodChange,
		handleDecadeChange,
		handleTagChange,
		handleSearch,
		loadMore,
		quickPlay,
		quickShuffle,
		addAlbumToQueue,
		playAlbumNext,
		addAlbumToPlaylist,
		getAlbumMenuItems,
		init,
		cleanup
	};
}

export type LibraryController<TAlbum> = ReturnType<typeof createLibraryController<TAlbum>>;
