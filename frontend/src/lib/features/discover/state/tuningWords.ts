import type { components } from '$lib/api/v3/openapi';

type TuningKind = components['schemas']['TuningKind'];
type TuningEntry = components['schemas']['DiscoverTuningEntry'];

/** The statements a person makes about one of their artists or a group. */
export type ArtistTuning = Extract<TuningKind, 'more_from_seed' | 'less_from_seed' | 'phase'>;

/** Each statement in the words the page and the list use. */
export const TUNING_WORDS: Record<
	ArtistTuning,
	{ button: string; short: string; head: string; word: string; state: string }
> = {
	more_from_seed: {
		button: 'More',
		short: 'More',
		head: 'More from this',
		word: 'more',
		state: 'More picks from it'
	},
	less_from_seed: {
		button: 'Less',
		short: 'Less',
		head: 'Less from this',
		word: 'less',
		state: 'Fewer picks from it'
	},
	phase: {
		button: 'That was a phase',
		short: 'A phase',
		head: 'That was a phase',
		word: 'a phase',
		state: 'Its plays no longer count'
	}
};

/** The statements, in the order their buttons stand. */
export const ARTIST_TUNINGS: readonly ArtistTuning[] = [
	'more_from_seed',
	'less_from_seed',
	'phase'
];

export const isArtistTuning = (kind: TuningKind): kind is ArtistTuning => kind in TUNING_WORDS;

/** What an Undo button for one statement is called. */
export const undoLabel = (kind: ArtistTuning, name: string) =>
	kind === 'phase' ? `Undo the phase for ${name}` : `Undo ${TUNING_WORDS[kind].word} from ${name}`;

/** A group's name as the page shows it. */
export const aroundName = (label: string) => `Around ${label}`;

/**
 * One line of what a person said: one artist, or a whole group's rows
 * gathered under the group's name. `target` is the row an undo names.
 */
export interface TuningLine {
	key: string;
	kind: ArtistTuning;
	name: string;
	/** The artists a group's statement covers; empty for one artist. */
	artists: string[];
	target: string;
	group: boolean;
	created_at: number;
}

/** `items` as lines, newest first: each group's rows gathered into one. */
export function tuningLines(items: TuningEntry[]): TuningLine[] {
	const lines: TuningLine[] = [];
	const groups = new Map<string, TuningLine>();
	for (const item of items) {
		if (!isArtistTuning(item.kind)) continue;
		const name = item.name || item.target;
		if (!item.group_anchor_mbid) {
			lines.push({
				key: `${item.kind}:${item.target}`,
				kind: item.kind,
				name,
				artists: [],
				target: item.target,
				group: false,
				created_at: item.created_at
			});
			continue;
		}
		const key = `${item.kind}:group:${item.group_anchor_mbid}`;
		const line = groups.get(key);
		if (line) {
			line.artists.push(name);
			line.created_at = Math.max(line.created_at, item.created_at);
			continue;
		}
		const gathered: TuningLine = {
			key,
			kind: item.kind,
			name: aroundName(item.group_label ?? name),
			artists: [name],
			target: item.target,
			group: true,
			created_at: item.created_at
		};
		groups.set(key, gathered);
		lines.push(gathered);
	}
	return lines.sort((a, b) => b.created_at - a.created_at);
}

/** A source a person may turn off with "Not this source", as the page names it. */
export interface SourceSwitch {
	source: 'lastfm' | 'listenbrainz';
	name: string;
	about: string;
}

/** The sources a person may turn off, in the order the page lists them. */
export const SOURCE_SWITCHES: readonly SourceSwitch[] = [
	{ source: 'lastfm', name: "Last.fm's picks", about: 'Similar artists from Last.fm.' },
	{
		source: 'listenbrainz',
		name: 'ListenBrainz weekly picks',
		about: "ListenBrainz's own picks and new releases for you."
	}
];
