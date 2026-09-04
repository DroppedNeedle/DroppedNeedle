import type { ManagementAuditChangeKind } from './LibraryManagementDisplay';

export type ManagementAuditTone = 'success' | 'warning' | 'error' | 'neutral';

export interface ManagementAuditEntry {
	ordinal: number;
	bundleOrdinal: number;
	trackLabel: string;
	title: string;
	artist: string | null;
	albumTitle: string | null;
	albumArtist: string | null;
	albumId: string | null;
	albumMbid: string | null;
	albumArtworkVersion: number | null;
	format: string;
	status: string;
	statusTone: ManagementAuditTone;
	reason: string | null;
	reasonCode: string | null;
	changes: ManagementAuditChangeKind[];
	exceptional: boolean;
	sourceRoot: string;
	sourcePath: string | null;
	destinationRoot: string;
	destinationPath: string | null;
	artworkUrl: string | null;
}

export interface ManagementAuditDossier {
	bundleOrdinal: number;
	title: string;
	artist: string;
	artworkUrl: string | null;
	albumId: string | null;
	albumMbid: string | null;
	albumArtworkVersion: number | null;
	entries: ManagementAuditEntry[];
}

export function groupManagementAuditEntries(
	entries: ManagementAuditEntry[]
): ManagementAuditDossier[] {
	const groups = new Map<number, ManagementAuditEntry[]>();
	for (const entry of entries) {
		const existing = groups.get(entry.bundleOrdinal);
		if (existing) existing.push(entry);
		else groups.set(entry.bundleOrdinal, [entry]);
	}
	return Array.from(groups.entries())
		.sort(([left], [right]) => left - right)
		.map(([bundleOrdinal, group]) => ({
			bundleOrdinal,
			title: group.find((entry) => entry.albumTitle)?.albumTitle ?? `Release ${bundleOrdinal + 1}`,
			artist: group.find((entry) => entry.albumArtist)?.albumArtist ?? 'Unknown artist',
			artworkUrl: group.find((entry) => entry.artworkUrl)?.artworkUrl ?? null,
			albumId: group.find((entry) => entry.albumId)?.albumId ?? null,
			albumMbid: group.find((entry) => entry.albumMbid)?.albumMbid ?? null,
			albumArtworkVersion:
				group.find((entry) => entry.albumArtworkVersion !== null)?.albumArtworkVersion ?? null,
			entries: group.sort((left, right) => left.ordinal - right.ordinal)
		}));
}
