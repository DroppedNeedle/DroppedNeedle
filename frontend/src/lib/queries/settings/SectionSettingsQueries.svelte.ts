import { v3 } from '$lib/api/v3/endpoint';
// Read and save hooks for the settings sections that are one form each.
// Every save answers with the section as stored, secrets masked, and that
// answer replaces the cached copy.
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { SettingsQueryKeyFactory } from './SettingsQueryKeyFactory';
import {
	anyArtistChanged,
	concertsChanged,
	createAppMutation,
	type Effect
} from '$lib/queries/effects';

type Schemas = components['schemas'];
export type AdvancedSettings = Schemas['AdvancedSettings'];
export type EditionPreferences = Schemas['EditionPreferences'];
export type EventsSettings = Schemas['EventsSettings'];
export type GetItSettings = Schemas['GetIt'];
export type LastFmSettings = Schemas['LastFmSettings'];
export type LibraryWatcherSettings = Schemas['FilesystemWatcher'];
export type NavidromeSettings = Schemas['NavidromeConnection'];
export type OidcSettings = Schemas['OidcConnection'];
export type PlexSettings = Schemas['PlexConnection'];
export type PlexVerifyResult = Schemas['PlexVerifyResponse'];
export type ReleasePreferences = Schemas['UserPreferences'];
export type SecuritySettings = Schemas['SecuritySettings'];
export type SpotifySettings = Schemas['SpotifySettings'];
export type VerifyResult = Schemas['VerifyConnectionResponse'];
export type WrappedSettings = Schemas['WrappedSettings'];
export type YouTubeSettings = Schemas['YouTubeConnection'];

function section<T>(
	name: string,
	read: (signal: AbortSignal) => Promise<T>,
	write: (body: T) => Promise<T>,
	effects: () => Effect[] = () => []
) {
	const keys = {
		saved: (userId: string | null | undefined) => SettingsQueryKeyFactory.section(userId, name)
	};
	return {
		keys,
		query: (enabled: () => boolean = () => true) =>
			createQuery(() => ({
				queryKey: keys.saved(authStore.user?.id),
				queryFn: ({ signal }) => read(signal),
				enabled: enabled()
			})),
		save: () =>
			createAppMutation(() => ({
				mutationFn: write,
				onSuccess: (saved: T) => setQueryDataWithPersister(keys.saved(authStore.user?.id), saved),
				effects
			}))
	};
}

const advanced = section<AdvancedSettings>(
	'advanced',
	(signal) => api.v3.GET(v3('/api/v3/settings/advanced'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/advanced'), body)
);
export const AdvancedSettingsQueryKeyFactory = advanced.keys;
export const getAdvancedSettingsQuery = advanced.query;
export const createSaveAdvancedSettingsMutation = advanced.save;

const editions = section<EditionPreferences>(
	'edition-preferences',
	(signal) => api.v3.GET(v3('/api/v3/settings/edition-preferences'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/edition-preferences'), body)
);
export const getEditionPreferencesQuery = editions.query;
export const createSaveEditionPreferencesMutation = editions.save;

// A save starts a fresh sweep and may change whether events are configured,
// so the Following page's concert lists are read again.
const events = section<EventsSettings>(
	'events',
	(signal) => api.v3.GET(v3('/api/v3/settings/events'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/events'), body),
	() => [concertsChanged(authStore.user?.id)]
);
export const getEventsSettingsQuery = events.query;
export const createSaveEventsSettingsMutation = events.save;

const getIt = section<GetItSettings>(
	'get-it',
	(signal) => api.v3.GET(v3('/api/v3/settings/get-it'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/get-it'), body)
);
export const getGetItSettingsQuery = getIt.query;
export const createSaveGetItSettingsMutation = getIt.save;

const lastfm = section<LastFmSettings>(
	'lastfm',
	(signal) => api.v3.GET(v3('/api/v3/settings/lastfm'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/lastfm'), body)
);
export const getLastFmSettingsQuery = lastfm.query;
export const createSaveLastFmSettingsMutation = lastfm.save;

// Read on the Library tab, which non-admins also open: pass `enabled`.
const libraryWatcher = section<LibraryWatcherSettings>(
	'library-watcher',
	(signal) => api.v3.GET(v3('/api/v3/settings/library/watcher'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/library/watcher'), body)
);
export const getLibraryWatcherSettingsQuery = libraryWatcher.query;
export const createSaveLibraryWatcherSettingsMutation = libraryWatcher.save;

const navidrome = section<NavidromeSettings>(
	'navidrome',
	(signal) => api.v3.GET(v3('/api/v3/settings/navidrome'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/navidrome'), body)
);
export const getNavidromeSettingsQuery = navidrome.query;
export const createSaveNavidromeSettingsMutation = navidrome.save;

// Which release types artist pages and search show.
const releasePreferences = section<ReleasePreferences>(
	'preferences',
	(signal) => api.v3.GET(v3('/api/v3/settings/preferences'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/preferences'), body),
	() => [anyArtistChanged()]
);
export const getReleasePreferencesQuery = releasePreferences.query;
export const createSaveReleasePreferencesMutation = releasePreferences.save;

const oidc = section<OidcSettings>(
	'oidc',
	(signal) => api.v3.GET(v3('/api/v3/settings/oidc'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/oidc'), body)
);
export const getOidcSettingsQuery = oidc.query;
export const createSaveOidcSettingsMutation = oidc.save;

const plex = section<PlexSettings>(
	'plex',
	(signal) => api.v3.GET(v3('/api/v3/settings/plex'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/plex'), body)
);
export const getPlexSettingsQuery = plex.query;
export const createSavePlexSettingsMutation = plex.save;

const security = section<SecuritySettings>(
	'security',
	(signal) => api.v3.GET(v3('/api/v3/settings/security'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/security'), body)
);
export const getSecuritySettingsQuery = security.query;
export const createSaveSecuritySettingsMutation = security.save;

const spotify = section<SpotifySettings>(
	'spotify',
	(signal) => api.v3.GET(v3('/api/v3/acquire/spotify/settings'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/acquire/spotify/settings'), body)
);
export const getSpotifySettingsQuery = spotify.query;
export const createSaveSpotifySettingsMutation = spotify.save;

const wrapped = section<WrappedSettings>(
	'wrapped',
	(signal) => api.v3.GET(v3('/api/v3/settings/wrapped'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/wrapped'), body)
);
export const getWrappedSettingsQuery = wrapped.query;
export const createSaveWrappedSettingsMutation = wrapped.save;

const youtube = section<YouTubeSettings>(
	'youtube',
	(signal) => api.v3.GET(v3('/api/v3/settings/youtube'), { signal }),
	(body) => api.v3.PUT(v3('/api/v3/settings/youtube'), body)
);
export const getYouTubeSettingsQuery = youtube.query;
export const createSaveYouTubeSettingsMutation = youtube.save;

/** Checks the entered connection without saving it. */
export const createVerifyNavidromeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: NavidromeSettings) =>
			api.v3.POST(v3('/api/v3/settings/navidrome/verify'), body)
	}));

/** Checks the entered provider without saving it. */
export const createVerifyOidcMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: OidcSettings) => api.v3.POST(v3('/api/v3/settings/oidc/verify'), body)
	}));

/** Checks the entered connection and lists its libraries, without saving. */
export const createVerifyPlexMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: PlexSettings) => api.v3.POST(v3('/api/v3/settings/plex/verify'), body)
	}));

/** Checks the entered Last.fm key and shared secret without saving them. */
export const createVerifyLastFmMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: LastFmSettings) => api.v3.POST(v3('/api/v3/settings/lastfm/verify'), body)
	}));

/** Checks the entered API key without saving it. */
export const createVerifyYouTubeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: YouTubeSettings) => api.v3.POST(v3('/api/v3/settings/youtube/verify'), body)
	}));

/** Checks the breached-password file without saving the section. */
export const createVerifyHibpMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: SecuritySettings) =>
			api.v3.POST(v3('/api/v3/settings/security/verify-hibp'), body)
	}));

/** Tries one events source's key without saving it. */
export const createTestEventsSourceMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: ({
			provider,
			body
		}: {
			provider: 'ticketmaster' | 'skiddle';
			body: EventsSettings;
		}) => {
			const endpoint =
				provider === 'ticketmaster'
					? v3('/api/v3/settings/events/test-ticketmaster')
					: v3('/api/v3/settings/events/test-skiddle');
			return api.v3.POST(endpoint, body);
		}
	}));

/** Writes the playlists to Navidrome, from the saved settings. */
export const createNavidromePlaylistSyncMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: () => api.v3.POST(v3('/api/v3/settings/navidrome/playlist-sync'))
	}));
