import type { MusicBrainzSeed } from '$lib/types';

export const MUSICBRAINZ_RELEASE_EDITOR = 'https://musicbrainz.org/release/add';

const RELEASE_MBID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function parseMusicBrainzReleaseId(value: string): string | null {
	let candidate = value.trim();
	if (candidate.includes('://')) {
		try {
			const url = new URL(candidate);
			const match = /^\/release\/([0-9a-f-]{36})\/?$/i.exec(url.pathname);
			if (
				url.protocol !== 'https:' ||
				url.hostname !== 'musicbrainz.org' ||
				url.port !== '' ||
				url.username !== '' ||
				url.password !== '' ||
				url.search !== '' ||
				url.hash !== '' ||
				!match
			) {
				return null;
			}
			candidate = match[1];
		} catch {
			return null;
		}
	}
	return RELEASE_MBID.test(candidate) ? candidate.toLowerCase() : null;
}

export function postMusicBrainzSeed(seed: MusicBrainzSeed, target: string): void {
	if (seed.action_url !== MUSICBRAINZ_RELEASE_EDITOR || seed.method !== 'POST') {
		throw new Error('Unexpected MusicBrainz editor target');
	}

	const form = document.createElement('form');
	form.action = MUSICBRAINZ_RELEASE_EDITOR;
	form.method = 'POST';
	form.target = target;
	form.hidden = true;

	for (const field of seed.fields) {
		const input = document.createElement('input');
		input.type = 'hidden';
		input.name = field.name;
		input.value = field.value;
		form.append(input);
	}

	document.body.append(form);
	try {
		form.submit();
	} finally {
		form.remove();
	}
}
