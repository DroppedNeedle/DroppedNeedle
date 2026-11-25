import type { SetupServiceKind } from '$lib/features/settings/queries/setupFound.svelte';

/** Each service's name as the guide shows it. */
export const SERVICE_NAMES: Record<SetupServiceKind, string> = {
	slskd: 'slskd',
	sabnzbd: 'SABnzbd',
	nzbget: 'NZBGet',
	prowlarr: 'Prowlarr',
	lidarr: 'Lidarr',
	jellyfin: 'Jellyfin',
	navidrome: 'Navidrome',
	plex: 'Plex',
	deemix: 'Deemix'
};

/** The Settings tab each service is set up in. */
export const SERVICE_TABS: Partial<Record<SetupServiceKind, string>> = {
	lidarr: 'lidarr-import',
	jellyfin: 'jellyfin',
	navidrome: 'navidrome'
};
