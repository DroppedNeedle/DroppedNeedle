import type { components } from '$lib/api/v3/openapi';
import type { AutoDownloadState, FollowStatus, FollowedArtist, NewRelease } from './types';

const AUTO_DOWNLOAD_STATES: readonly AutoDownloadState[] = [
	'none',
	'pending',
	'approved',
	'rejected',
	'revoked'
];

function toAutoDownloadState(state: string): AutoDownloadState {
	return AUTO_DOWNLOAD_STATES.find((known) => known === state) ?? 'none';
}

export function toFollowStatus(
	status: components['schemas']['FollowStatusResponse']
): FollowStatus {
	return {
		followed: status.followed,
		auto_download: status.auto_download,
		auto_download_state: toAutoDownloadState(status.auto_download_state)
	};
}

export function toFollowedArtist(artist: components['schemas']['FollowedArtist']): FollowedArtist {
	return {
		mbid: artist.artist_mbid,
		name: artist.name,
		image_url: null,
		auto_download: artist.auto_download,
		auto_download_state: toAutoDownloadState(artist.auto_download_state),
		followed_at: artist.followed_at
	};
}

export function toNewRelease(release: components['schemas']['NewReleaseItem']): NewRelease {
	return {
		release_group_mbid: release.release_group_mbid,
		title: release.title,
		artist_name: release.artist_name,
		artist_mbid: release.artist_mbid,
		primary_type: release.primary_type ?? null,
		first_release_date: release.first_release_date ?? null
	};
}
