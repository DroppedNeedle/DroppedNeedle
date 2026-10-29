// Read and save hooks for the settings sections that are one form each.
// Every save answers with the section as stored, secrets masked, and that
// answer replaces the cached copy.
import { createMutation, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
import { PLEX_ENDPOINTS } from '$lib/queries/plex/endpoints';
import { SPOTIFY_ENDPOINTS } from '$lib/queries/spotify/endpoints';
import { authStore } from '$lib/stores/authStore.svelte';
import { userIdSegment } from '../userKeySegment';
import { SETTINGS_ENDPOINTS } from './endpoints';

type Schemas = components['schemas'];
export type AdvancedSettings = Schemas['AdvancedSettings'];
export type EditionPreferences = Schemas['EditionPreferences'];
export type EventsSettings = Schemas['EventsSettings'];
export type GetItSettings = Schemas['GetIt'];
export type LastFmSettings = Schemas['LastFmSettings'];
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
	afterSave?: () => Promise<unknown>
) {
	const keys = {
		prefix: ['settings', name] as const,
		saved: (userId: string | null | undefined) => ['settings', name, userIdSegment(userId)] as const
	};
	return {
		keys,
		query: () =>
			createQuery(() => ({
				queryKey: keys.saved(authStore.user?.id),
				queryFn: ({ signal }) => read(signal)
			})),
		save: () =>
			createMutation(() => ({
				mutationFn: write,
				onSuccess: async (saved: T) => {
					await setQueryDataWithPersister(keys.saved(authStore.user?.id), saved);
					await afterSave?.();
				}
			}))
	};
}

const advanced = section<AdvancedSettings>(
	'advanced',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.advanced(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.advanced(), body)
);
export const AdvancedSettingsQueryKeyFactory = advanced.keys;
export const getAdvancedSettingsQuery = advanced.query;
export const createSaveAdvancedSettingsMutation = advanced.save;

const editions = section<EditionPreferences>(
	'edition-preferences',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.editionPreferences(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.editionPreferences(), body)
);
export const EditionPreferencesQueryKeyFactory = editions.keys;
export const getEditionPreferencesQuery = editions.query;
export const createSaveEditionPreferencesMutation = editions.save;

// A save starts a fresh sweep and may change whether events are configured,
// so the Following page's concert lists are read again.
const events = section<EventsSettings>(
	'events',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.events(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.events(), body),
	async () => {
		const userId = authStore.user?.id;
		await invalidateQueriesWithPersister({ queryKey: FollowQueryKeyFactory.concerts(userId) });
		await invalidateQueriesWithPersister({
			queryKey: FollowQueryKeyFactory.concertsUnseen(userId)
		});
	}
);
export const EventsSettingsQueryKeyFactory = events.keys;
export const getEventsSettingsQuery = events.query;
export const createSaveEventsSettingsMutation = events.save;

const getIt = section<GetItSettings>(
	'get-it',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.getIt(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.getIt(), body)
);
export const GetItSettingsQueryKeyFactory = getIt.keys;
export const getGetItSettingsQuery = getIt.query;
export const createSaveGetItSettingsMutation = getIt.save;

const lastfm = section<LastFmSettings>(
	'lastfm',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.lastfm(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.lastfm(), body)
);
export const LastFmSettingsQueryKeyFactory = lastfm.keys;
export const getLastFmSettingsQuery = lastfm.query;
export const createSaveLastFmSettingsMutation = lastfm.save;

const navidrome = section<NavidromeSettings>(
	'navidrome',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.navidrome(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.navidrome(), body)
);
export const NavidromeSettingsQueryKeyFactory = navidrome.keys;
export const getNavidromeSettingsQuery = navidrome.query;
export const createSaveNavidromeSettingsMutation = navidrome.save;

// Which release types artist pages and search show.
const releasePreferences = section<ReleasePreferences>(
	'preferences',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.preferences(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.preferences(), body)
);
export const getReleasePreferencesQuery = releasePreferences.query;
export const createSaveReleasePreferencesMutation = releasePreferences.save;

const oidc = section<OidcSettings>(
	'oidc',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.oidc(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.oidc(), body)
);
export const OidcSettingsQueryKeyFactory = oidc.keys;
export const getOidcSettingsQuery = oidc.query;
export const createSaveOidcSettingsMutation = oidc.save;

const plex = section<PlexSettings>(
	'plex',
	(signal) => api.v3.GET(PLEX_ENDPOINTS.settings(), { signal }),
	(body) => api.v3.PUT(PLEX_ENDPOINTS.settings(), body)
);
export const PlexSettingsQueryKeyFactory = plex.keys;
export const getPlexSettingsQuery = plex.query;
export const createSavePlexSettingsMutation = plex.save;

const security = section<SecuritySettings>(
	'security',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.security(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.security(), body)
);
export const SecuritySettingsQueryKeyFactory = security.keys;
export const getSecuritySettingsQuery = security.query;
export const createSaveSecuritySettingsMutation = security.save;

const spotify = section<SpotifySettings>(
	'spotify',
	(signal) => api.v3.GET(SPOTIFY_ENDPOINTS.settings(), { signal }),
	(body) => api.v3.PUT(SPOTIFY_ENDPOINTS.settings(), body)
);
export const SpotifySettingsQueryKeyFactory = spotify.keys;
export const getSpotifySettingsQuery = spotify.query;
export const createSaveSpotifySettingsMutation = spotify.save;

const wrapped = section<WrappedSettings>(
	'wrapped',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.wrapped(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.wrapped(), body)
);
export const WrappedSettingsQueryKeyFactory = wrapped.keys;
export const getWrappedSettingsQuery = wrapped.query;
export const createSaveWrappedSettingsMutation = wrapped.save;

const youtube = section<YouTubeSettings>(
	'youtube',
	(signal) => api.v3.GET(SETTINGS_ENDPOINTS.youtube(), { signal }),
	(body) => api.v3.PUT(SETTINGS_ENDPOINTS.youtube(), body)
);
export const YouTubeSettingsQueryKeyFactory = youtube.keys;
export const getYouTubeSettingsQuery = youtube.query;
export const createSaveYouTubeSettingsMutation = youtube.save;

/** Checks the entered connection without saving it. */
export const createVerifyNavidromeMutation = () =>
	createMutation(() => ({
		mutationFn: (body: NavidromeSettings) => api.v3.POST(SETTINGS_ENDPOINTS.navidromeVerify(), body)
	}));

/** Checks the entered provider without saving it. */
export const createVerifyOidcMutation = () =>
	createMutation(() => ({
		mutationFn: (body: OidcSettings) => api.v3.POST(SETTINGS_ENDPOINTS.oidcVerify(), body)
	}));

/** Checks the entered connection and lists its libraries, without saving. */
export const createVerifyPlexMutation = () =>
	createMutation(() => ({
		mutationFn: (body: PlexSettings) => api.v3.POST(PLEX_ENDPOINTS.verify(), body)
	}));

/** Checks the entered API key without saving it. */
export const createVerifyYouTubeMutation = () =>
	createMutation(() => ({
		mutationFn: (body: YouTubeSettings) => api.v3.POST(SETTINGS_ENDPOINTS.youtubeVerify(), body)
	}));

/** Checks the breached-password file without saving the section. */
export const createVerifyHibpMutation = () =>
	createMutation(() => ({
		mutationFn: (body: SecuritySettings) =>
			api.v3.POST(SETTINGS_ENDPOINTS.securityVerifyHibp(), body)
	}));

/** Tries one events source's key without saving it. */
export const createTestEventsSourceMutation = () =>
	createMutation(() => ({
		mutationFn: ({
			provider,
			body
		}: {
			provider: 'ticketmaster' | 'skiddle';
			body: EventsSettings;
		}) => {
			const endpoint =
				provider === 'ticketmaster'
					? SETTINGS_ENDPOINTS.eventsTestTicketmaster()
					: SETTINGS_ENDPOINTS.eventsTestSkiddle();
			return api.v3.POST(endpoint, body);
		}
	}));

/** Writes the playlists to Navidrome, from the saved settings. */
export const createNavidromePlaylistSyncMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.v3.POST(SETTINGS_ENDPOINTS.navidromePlaylistSync())
	}));
