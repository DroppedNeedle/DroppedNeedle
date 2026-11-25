import type { components } from '$lib/api/v3/openapi';

export type RotationIdea = components['schemas']['RotationIdea'];
export type RotationInfo = components['schemas']['RotationInfo'];

/** Each idea's name while it waits, and the one line saying how it is made. */
const IDEAS: Record<RotationIdea, { name: string; rule: string }> = {
	added_recently: { name: 'Added recently', rule: 'The newest 50 songs in the library.' },
	on_repeat: { name: 'On repeat', rule: 'What you played most in the last 30 days.' },
	forgotten_favourites: {
		name: 'Forgotten favourites',
		rule: 'Rated 4 stars or more, or hearted, and not played in 6 months.'
	},
	deep_cuts: { name: 'Deep cuts', rule: 'Songs you never played from the albums you play most.' },
	five_stars: { name: 'Five stars', rule: 'Songs you rated 5 stars.' },
	never_played: { name: 'Never played', rule: 'Songs you have never played.' },
	followed_artists: {
		name: 'Artists you follow',
		rule: 'Artists you follow, not played in the last 30 days.'
	},
	lossless: { name: 'Lossless', rule: 'FLAC, WAV and ALAC files.' },
	a_year_ago: { name: 'A year ago', rule: 'What you played this time a year ago.' },
	decade: { name: 'A decade', rule: 'Songs from one decade.' },
	long_ones: { name: 'Long ones', rule: 'Songs of 7 minutes or more.' }
};

/** The ideas whose 50 songs are drawn again each Monday. */
const SHUFFLED = new Set<RotationIdea>([
	'forgotten_favourites',
	'deep_cuts',
	'five_stars',
	'never_played',
	'followed_artists',
	'lossless',
	'decade',
	'long_ones'
]);

export function ideaName(idea: RotationIdea): string {
	return IDEAS[idea].name;
}

/** How a list is made, in plain words; the decade names its years. */
export function ideaRule(rotation: Pick<RotationInfo, 'idea' | 'decade'>): string {
	const { idea, decade } = rotation;
	const rule =
		idea === 'decade' && decade != null
			? `Songs from ${decade} to ${decade + 9}.`
			: IDEAS[idea].rule;
	return SHUFFLED.has(idea) ? `${rule.slice(0, -1)}, a new 50 each week.` : rule;
}

/** "Mon 30 Nov" for an epoch-seconds time, in UTC, as the lists change. */
export function changeDay(epochSec: number): string {
	return new Date(epochSec * 1000).toLocaleDateString('en-GB', {
		weekday: 'short',
		day: 'numeric',
		month: 'short',
		timeZone: 'UTC'
	});
}
