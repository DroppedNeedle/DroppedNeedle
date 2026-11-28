/**
 * Where a playlist came from, as its tab and fact line name it: the
 * Weekly Mix, or an imported playlist's source. Null for one made by a
 * person, a list on rotation or a list Discover keeps for apps.
 */
export function playlistSourceType(playlist: {
	kind: 'own' | 'imported' | 'weekly_mix' | 'rotation' | 'discover';
	source_ref: string | null;
}): string | null {
	if (playlist.kind === 'weekly_mix') return 'weekly_mix';
	if (playlist.kind !== 'imported') return null;
	return playlist.source_ref?.split(':')[0] ?? null;
}

export function getSourceLabel(sourceType: string): string {
	if (sourceType === 'weekly_mix') return 'Weekly Mix';
	if (sourceType === 'local') return 'Local';
	if (sourceType === 'jellyfin') return 'Jellyfin';
	if (sourceType === 'navidrome') return 'Navidrome';
	if (sourceType === 'plex') return 'Plex';
	if (sourceType === 'youtube') return 'YouTube';
	if (sourceType === 'spotify') return 'Spotify';
	return 'Unknown';
}

export function getSourceColor(sourceType: string): string {
	if (sourceType === 'weekly_mix') return 'var(--dn-title)';
	if (sourceType === 'jellyfin') return 'rgb(var(--brand-jellyfin))';
	if (sourceType === 'navidrome') return 'rgb(var(--brand-navidrome))';
	if (sourceType === 'plex') return 'rgb(var(--brand-plex))';
	if (sourceType === 'local') return 'rgb(var(--brand-localfiles))';
	if (sourceType === 'youtube') return 'var(--color-youtube)';
	if (sourceType === 'spotify') return 'var(--color-spotify)';
	return 'currentColor';
}
