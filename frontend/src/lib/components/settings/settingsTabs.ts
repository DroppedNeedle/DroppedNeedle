import type { Component } from 'svelte';

type SettingsModule = { default: Component };
type SettingsLoader = () => Promise<SettingsModule>;

const adminTabs = new Set([
	'free-music',
	'download-client',
	'indexers',
	'lidarr-import',
	'lastfm',
	'spotify',
	'events',
	'get-it',
	'plugins',
	'security',
	'diagnostics',
	'wrapped',
	'users',
	'musicbrainz'
]);

export function isSettingsTabVisible(id: string, isAdmin: boolean): boolean {
	return isAdmin || !adminTabs.has(id);
}

const settingsTabLoaders: Record<string, SettingsLoader> = {
	library: () => import('./SettingsLibrary.svelte'),
	'free-music': () => import('./SettingsFreeMusic.svelte'),
	'download-client': () => import('./SettingsDownloadClients.svelte'),
	indexers: () => import('./SettingsIndexers.svelte'),
	'lidarr-import': () => import('./SettingsLidarrImport.svelte'),
	'connect-apps': () => import('./SettingsConnectApps.svelte'),
	jellyfin: () => import('./SettingsJellyfin.svelte'),
	navidrome: () => import('./SettingsNavidrome.svelte'),
	plex: () => import('./SettingsPlex.svelte'),
	youtube: () => import('./SettingsYouTube.svelte'),
	lastfm: () => import('./SettingsLastFmApp.svelte'),
	spotify: () => import('./SettingsSpotify.svelte'),
	events: () => import('./SettingsEvents.svelte'),
	'get-it': () => import('./SettingsGetIt.svelte'),
	settings: () => import('./SettingsPreferences.svelte'),
	home: () => import('./SettingsHome.svelte'),
	discover: () => import('./SettingsDiscover.svelte'),
	sidebar: () => import('./SettingsSidebar.svelte'),
	'music-source': () => import('./SettingsMusicSource.svelte'),
	cache: () => import('./SettingsCache.svelte'),
	musicbrainz: () => import('./SettingsMusicBrainz.svelte'),
	diagnostics: () => import('./SettingsDiagnostics.svelte'),
	users: () => import('./SettingsUsers.svelte'),
	security: () => import('./SettingsSecurity.svelte'),
	plugins: () => import('./SettingsPlugins.svelte'),
	wrapped: () => import('./SettingsWrapped.svelte'),
	advanced: () => import('./SettingsAdvanced.svelte'),
	about: () => import('./SettingsAbout.svelte')
};
export async function loadSettingsTab(id: string, isAdmin: boolean): Promise<Component | null> {
	if (!isSettingsTabVisible(id, isAdmin)) return null;
	const loader = settingsTabLoaders[id];
	return loader ? (await loader()).default : null;
}
