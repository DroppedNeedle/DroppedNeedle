export function getSourceLabel(sourceType: string): string {
	if (sourceType === 'local') return 'Local';
	if (sourceType === 'jellyfin') return 'Jellyfin';
	if (sourceType === 'navidrome') return 'Navidrome';
	if (sourceType === 'plex') return 'Plex';
	if (sourceType === 'youtube') return 'YouTube';
	if (sourceType === 'spotify') return 'Spotify';
	return 'Unknown';
}

export function getSourceColor(sourceType: string): string {
	if (sourceType === 'jellyfin') return 'rgb(var(--brand-jellyfin))';
	if (sourceType === 'navidrome') return 'rgb(var(--brand-navidrome))';
	if (sourceType === 'plex') return 'rgb(var(--brand-plex))';
	if (sourceType === 'local') return 'rgb(var(--brand-localfiles))';
	if (sourceType === 'youtube') return 'var(--color-youtube)';
	if (sourceType === 'spotify') return 'rgb(var(--brand-spotify))';
	return 'currentColor';
}
