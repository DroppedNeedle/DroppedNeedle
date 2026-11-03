import type {
	BecauseYouListenTo,
	ChartSection,
	PageSection,
	SectionKey,
	SectionZone,
	TopPicksSection,
	WeeklyExploration
} from './types';

type Sections = PageSection[] | null | undefined;
type Content = PageSection['content'];
type Shaped<S extends Content['shape']> = Extract<Content, { shape: S }>;

function contentOf<S extends Content['shape']>(
	sections: Sections,
	key: SectionKey,
	shape: S
): Shaped<S> | null {
	const content = sections?.find((section) => section.key === key)?.content;
	return content?.shape === shape ? (content as Shaped<S>) : null;
}

/** The sections of one zone, in page order. */
export function zoneSections(sections: Sections, zone: SectionZone): PageSection[] {
	return (sections ?? []).filter((section) => section.zone === zone);
}

/** A one-shelf section's shelf. */
export function shelfOf(sections: Sections, key: SectionKey): ChartSection | null {
	return contentOf(sections, key, 'shelf')?.shelf ?? null;
}

/** A section of several shelves (Daily Mixes, radio). */
export function shelvesOf(sections: Sections, key: SectionKey): ChartSection[] {
	return contentOf(sections, key, 'shelves')?.shelves ?? [];
}

/** The "because you listen to" shelves. */
export function becauseOf(sections: Sections): BecauseYouListenTo[] {
	return contentOf(sections, 'because_you_listen_to', 'because')?.entries ?? [];
}

/** Top Picks. */
export function picksOf(sections: Sections): TopPicksSection | null {
	return contentOf(sections, 'top_picks', 'picks')?.picks ?? null;
}

/** The weekly exploration playlist. */
export function weeklyOf(sections: Sections): WeeklyExploration | null {
	return contentOf(sections, 'weekly_exploration', 'weekly')?.weekly ?? null;
}
