import type { AlbumView } from '$lib/types';
import type { components } from '$lib/api/v3/openapi';

/** A release group's MusicBrainz types, as `/artists/{mbid}/owned-releases` gives them. */
export type ReleaseTypes = Pick<
	components['schemas']['OwnedRelease'],
	'id' | 'primary_type' | 'secondary_types'
>;

/** An owned album and the type shown on its card, when it is not a plain album. */
export interface FiledAlbum {
	album: AlbumView;
	label: string | null;
}

/** The artist's own albums by kind: every one lands in exactly one list. */
export interface LibrarySections {
	albums: FiledAlbum[];
	epsAndSingles: FiledAlbum[];
	other: FiledAlbum[];
}

/**
 * File the albums an artist leads by their release group's types. A secondary
 * type (Compilation, Live, Soundtrack, ...) or an unusual primary type files
 * under `other`, labelled with it. Albums MusicBrainz could not type file by
 * their compilation flag, else as albums: nothing the library holds drops out.
 */
export function fileOwnedAlbums(albums: AlbumView[], types: ReleaseTypes[]): LibrarySections {
	const byId = new Map(types.map((release) => [release.id.toLowerCase(), release]));
	const sections: LibrarySections = { albums: [], epsAndSingles: [], other: [] };
	for (const album of albums) {
		const release = album.release_group_mbid
			? byId.get(album.release_group_mbid.toLowerCase())
			: undefined;
		if (!release) {
			if (album.is_compilation) sections.other.push({ album, label: 'Compilation' });
			else sections.albums.push({ album, label: null });
			continue;
		}
		const primary = release.primary_type ?? null;
		if (release.secondary_types.length > 0) {
			sections.other.push({ album, label: release.secondary_types.join(', ') });
			continue;
		}
		switch (primary?.toLowerCase()) {
			case 'album':
			case undefined:
				sections.albums.push({ album, label: null });
				break;
			case 'ep':
			case 'single':
				sections.epsAndSingles.push({ album, label: primary });
				break;
			default:
				sections.other.push({ album, label: primary });
		}
	}
	for (const list of [sections.albums, sections.epsAndSingles, sections.other]) {
		list.sort((a, b) => (b.album.year ?? -Infinity) - (a.album.year ?? -Infinity));
	}
	return sections;
}
