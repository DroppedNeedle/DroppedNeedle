import type { ImportFolderStatus } from '$lib/features/settings/queries/SectionSettingsQueries.svelte';

export type ImportFolderTone = 'neutral' | 'info' | 'success' | 'warning';

/** Where the import folder card sits, so a link can land on it. */
export const IMPORT_FOLDER_ANCHOR = 'import-folder';

const OVERLAPS: Record<NonNullable<ImportFolderStatus['overlap']>, string> = {
	library: 'Overlaps a library folder',
	recycle_bin: 'Overlaps the recycle bin',
	downloads: 'Overlaps a download folder',
	app: 'Overlaps an app folder'
};

/** The status chip's words and tone for what the import folder is doing. */
export function describeImportFolder(status: ImportFolderStatus): {
	label: string;
	tone: ImportFolderTone;
} {
	switch (status.state) {
		case 'off':
			return { label: 'Off', tone: 'neutral' };
		case 'watching':
			return { label: 'Watching', tone: 'success' };
		case 'importing':
			return {
				label: status.waiting > 1 ? `Importing · ${status.waiting} albums left` : 'Importing',
				tone: 'info'
			};
		case 'waiting_for_scan':
			return { label: 'Waiting for a scan', tone: 'info' };
		case 'retrying':
			return { label: 'Will try again soon', tone: 'info' };
		case 'library_off':
			return { label: 'Library is off', tone: 'warning' };
		case 'settings_unreadable':
			return { label: "Settings can't be read", tone: 'warning' };
		case 'unreachable':
			return { label: 'Folder not found', tone: 'warning' };
		case 'read_only':
			return { label: 'Folder is read-only', tone: 'warning' };
		case 'overlaps':
			return {
				label: status.overlap ? OVERLAPS[status.overlap] : 'Overlaps another folder',
				tone: 'warning'
			};
	}
}
