type ListenSource = 'listenbrainz' | 'lastfm';

/** What an artist section answered: the service that answered, the one asked
 * for, and the services that failed while it was built. */
export interface SectionSources {
	source: ListenSource;
	asked_source: ListenSource;
	unavailable_sources: ListenSource[];
}

const LABEL: Record<ListenSource, string> = { listenbrainz: 'ListenBrainz', lastfm: 'Last.fm' };

/**
 * One line saying why the popular lists do not come from the service the
 * switcher names: it is not available (failing or switched off), or it has
 * nothing for this artist, and the other one stood in; or neither is. `null` when the
 * lists come from the service asked for and nothing failed.
 */
export function listeningSourceNotice(
	answers: (SectionSources | null | undefined)[]
): string | null {
	const known = answers.filter((answer): answer is SectionSources => Boolean(answer));
	if (known.length === 0) return null;
	const asked = known[0].asked_source;
	const down = new Set(known.flatMap((answer) => answer.unavailable_sources));
	if (down.has('listenbrainz') && down.has('lastfm')) {
		return 'Neither ListenBrainz nor Last.fm is available right now, so these lists may be empty or out of date.';
	}
	const standIn = known.find((answer) => answer.source !== answer.asked_source);
	if (standIn) {
		const why = down.has(asked) ? 'is not available right now' : 'has nothing for this artist';
		return `${LABEL[asked]} ${why}, so these lists come from ${LABEL[standIn.source]}.`;
	}
	if (down.has(asked)) {
		return `${LABEL[asked]} is not available right now, so these lists may be out of date.`;
	}
	return null;
}
