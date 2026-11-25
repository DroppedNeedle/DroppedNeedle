import type {
	ChartSection,
	NewReleaseSheet,
	PageSection,
	SectionKey,
	SectionState,
	SectionZone,
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

/** New from your artists: the newest of the person's new releases. */
export function newReleasesOf(sections: Sections): NewReleaseSheet | null {
	return contentOf(sections, 'new_from_your_artists', 'new_releases')?.new_releases ?? null;
}

/** The weekly exploration playlist. */
export function weeklyOf(sections: Sections): WeeklyExploration | null {
	return contentOf(sections, 'weekly_exploration', 'weekly')?.weekly ?? null;
}

/** How section `key` stands, when it is not plainly built. */
export function stateOf(
	states: SectionState[] | null | undefined,
	key: SectionKey
): SectionState | null {
	return states?.find((state) => state.key === key) ?? null;
}
