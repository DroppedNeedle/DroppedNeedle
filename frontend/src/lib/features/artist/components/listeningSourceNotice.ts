import type { components } from '$lib/api/v3/openapi';

type ListenSource = components['schemas']['ListenSource'];

/** What an artist section says about where its rows came from. */
export type SectionSources = Pick<
	components['schemas']['TopSongsResponse'],
	'source' | 'asked_source' | 'unavailable_sources' | 'configured'
>;

const LABEL: Record<ListenSource, string> = { listenbrainz: 'ListenBrainz', lastfm: 'Last.fm' };

/**
 * One line saying why the popular lists do not come from the chosen
 * service as asked: it is off or unavailable (switched off, not set up or
 * failing) or has nothing, and the other one stood in; or no list could be
 * read. `null` when the chosen service answered, or when nothing is set up
 * (the lists then say so themselves).
 */
export function listeningSourceNotice(
	answers: (SectionSources | null | undefined)[]
): string | null {
	const known = answers.filter((answer): answer is SectionSources => Boolean(answer));
	if (!known.some((answer) => answer.configured)) return null;
	const asked = known[0].asked_source;
	const down = new Set(known.flatMap((answer) => answer.unavailable_sources));
	if (down.has('listenbrainz') && down.has('lastfm')) {
		return 'ListenBrainz and Last.fm are off or unavailable.';
	}
	const standIn = known.find((answer) => answer.source !== answer.asked_source);
	if (standIn) {
		const why = down.has(asked) ? 'is off or unavailable' : 'has nothing here';
		return `${LABEL[asked]} ${why}, so these lists come from ${LABEL[standIn.source]}.`;
	}
	if (down.has(asked)) {
		return `${LABEL[asked]} is off or unavailable. These lists may be out of date.`;
	}
	return null;
}
