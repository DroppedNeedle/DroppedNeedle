import type {
	DuplicateCollisionKind,
	LibraryManagementPlanItem
} from '$lib/queries/library-management/types';

export interface ManagementFieldDiff {
	name: string;
	operation: string;
	before: unknown;
	after: unknown;
	representationLoss: string | null;
}

export interface ManagementCollision {
	classification: string;
	requestKind: DuplicateCollisionKind | null;
	existingRootId: string | null;
	existingRelativePath: string | null;
	existingLocalTrackId: string | null;
}

export interface ManagementRestorationArtwork {
	imageType: string;
	mimeType: string | null;
	description: string;
	width: number | null;
	height: number | null;
	byteSize: number;
	sha256: string;
}

export interface ManagementRestoration {
	scope: string;
	nativeTags: {
		changed: boolean;
		currentPrimaryEntries: number;
		restoredPrimaryEntries: number;
		currentAuxiliaryEntries: number;
		restoredAuxiliaryEntries: number;
		currentEncodedPrimary: boolean;
		restoredEncodedPrimary: boolean;
		currentFingerprint: string | null;
		restoredFingerprint: string | null;
		changedRawKeys: string[];
	};
	artwork: {
		changed: boolean;
		current: ManagementRestorationArtwork[];
		restored: ManagementRestorationArtwork[];
	};
	fileAttributes: {
		changed: boolean;
		currentMtimeNs: string | null;
		restoredMtimeNs: string | null;
		currentPermissionBits: number | null;
		restoredPermissionBits: number | null;
	};
}

export interface ManagementLyricsProjection {
	status: 'available' | 'not_found' | 'deferred' | 'mismatch';
	providerId: number | null;
	providerRevision: string | null;
	reason: string | null;
	plainAvailable: boolean;
	syncedAvailable: boolean;
	plainSelected: boolean;
	syncedSelected: boolean;
	syncedSupported: boolean;
	preserveExisting: boolean;
}

export interface ManagementScrubbedRawTag {
	key: string;
	valueKind: string;
	values: string[];
	valueCount: number;
	truncated: boolean;
	sha256: string | null;
}

export type ManagementAuditChangeKind = 'tags' | 'artwork' | 'path' | 'sidecars';

const MANAGEMENT_ARTWORK_PREVIEW_MIME_TYPES = new Set([
	'image/gif',
	'image/jpeg',
	'image/png',
	'image/webp'
]);

export function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function stringOrNull(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function numberOrNull(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function restorationArtwork(value: unknown): ManagementRestorationArtwork[] {
	if (!Array.isArray(value)) return [];
	return value.flatMap((item) => {
		if (!isRecord(item) || typeof item.sha256 !== 'string') return [];
		return [
			{
				imageType: stringOrNull(item.image_type) ?? 'other',
				mimeType: stringOrNull(item.mime_type),
				description: stringOrNull(item.description) ?? '',
				width: numberOrNull(item.width),
				height: numberOrNull(item.height),
				byteSize: numberOrNull(item.byte_size) ?? 0,
				sha256: item.sha256
			}
		];
	});
}

export function managementRestoration(
	item: LibraryManagementPlanItem
): ManagementRestoration | null {
	const value = item.diff.restoration;
	if (!isRecord(value) || typeof value.scope !== 'string') return null;
	const nativeTags = isRecord(value.native_tags) ? value.native_tags : {};
	const artwork = isRecord(value.artwork) ? value.artwork : {};
	const fileAttributes = isRecord(value.file_attributes) ? value.file_attributes : {};
	return {
		scope: value.scope,
		nativeTags: {
			changed: nativeTags.changed === true,
			currentPrimaryEntries: numberOrNull(nativeTags.current_primary_entries) ?? 0,
			restoredPrimaryEntries: numberOrNull(nativeTags.restored_primary_entries) ?? 0,
			currentAuxiliaryEntries: numberOrNull(nativeTags.current_auxiliary_entries) ?? 0,
			restoredAuxiliaryEntries: numberOrNull(nativeTags.restored_auxiliary_entries) ?? 0,
			currentEncodedPrimary: nativeTags.current_encoded_primary === true,
			restoredEncodedPrimary: nativeTags.restored_encoded_primary === true,
			currentFingerprint: stringOrNull(nativeTags.current_fingerprint),
			restoredFingerprint: stringOrNull(nativeTags.restored_fingerprint),
			changedRawKeys: managementStringList(nativeTags.changed_raw_keys)
		},
		artwork: {
			changed: artwork.changed === true,
			current: restorationArtwork(artwork.current),
			restored: restorationArtwork(artwork.restored)
		},
		fileAttributes: {
			changed: fileAttributes.changed === true,
			currentMtimeNs: stringOrNull(fileAttributes.current_mtime_ns),
			restoredMtimeNs: stringOrNull(fileAttributes.restored_mtime_ns),
			currentPermissionBits: numberOrNull(fileAttributes.current_permission_bits),
			restoredPermissionBits: numberOrNull(fileAttributes.restored_permission_bits)
		}
	};
}

export function managementFieldDiffs(item: LibraryManagementPlanItem): ManagementFieldDiff[] {
	const raw = item.diff.field_mutations;
	if (!Array.isArray(raw)) return [];
	return raw.flatMap((value) => {
		if (!isRecord(value) || typeof value.name !== 'string' || typeof value.operation !== 'string') {
			return [];
		}
		return [
			{
				name: value.name,
				operation: value.operation,
				before: value.before,
				after: value.after,
				representationLoss: stringOrNull(value.representation_loss)
			}
		];
	});
}

export function managementCustomTagDiffs(item: LibraryManagementPlanItem): ManagementFieldDiff[] {
	const raw = item.diff.custom_tag_mutations;
	if (!Array.isArray(raw)) return [];
	return raw.flatMap((value) => {
		if (!isRecord(value) || typeof value.name !== 'string' || typeof value.operation !== 'string') {
			return [];
		}
		return [
			{
				name: `Custom: ${value.name}`,
				operation: value.operation,
				before: value.before,
				after: value.after,
				representationLoss: null
			}
		];
	});
}

export function managementScrubbedRawTags(
	item: LibraryManagementPlanItem
): ManagementScrubbedRawTag[] {
	const raw = item.diff.scrubbed_raw_tags;
	if (!Array.isArray(raw)) return [];
	return raw.flatMap((value) => {
		if (!isRecord(value) || typeof value.key !== 'string') return [];
		return [
			{
				key: value.key,
				valueKind: stringOrNull(value.value_kind) ?? 'text',
				values: managementStringList(value.values),
				valueCount: numberOrNull(value.value_count) ?? 0,
				truncated: value.truncated === true,
				sha256: stringOrNull(value.sha256)
			}
		];
	});
}

export function managementLyricsProjection(
	item: LibraryManagementPlanItem
): ManagementLyricsProjection | null {
	const diff = isRecord(item.diff) ? item.diff : {};
	const value = diff.lyrics_projection;
	if (!isRecord(value)) return null;
	const status = stringOrNull(value.status);
	if (!status || !['available', 'not_found', 'deferred', 'mismatch'].includes(status)) {
		return null;
	}
	return {
		status: status as ManagementLyricsProjection['status'],
		providerId: numberOrNull(value.provider_id),
		providerRevision: stringOrNull(value.provider_revision),
		reason: stringOrNull(value.reason),
		plainAvailable: value.plain_available === true,
		syncedAvailable: value.synced_available === true,
		plainSelected: value.plain_selected === true,
		syncedSelected: value.synced_selected === true,
		syncedSupported: value.synced_supported !== false,
		preserveExisting: value.preserve_existing === true
	};
}

export function managementStringList(value: unknown): string[] {
	return Array.isArray(value)
		? value.filter((item): item is string => typeof item === 'string')
		: [];
}

function desiredFieldValue(item: LibraryManagementPlanItem, name: string): unknown {
	const fields = isRecord(item.desired_document) ? item.desired_document.fields : null;
	if (!Array.isArray(fields)) return null;
	for (const value of fields) {
		if (isRecord(value) && value.name === name) return value.value;
	}
	return null;
}

export function managementDesiredField(
	item: LibraryManagementPlanItem,
	name: string
): string | null {
	const value = desiredFieldValue(item, name);
	return value === null || value === undefined || value === ''
		? null
		: formatManagementValue(value);
}

export function managementPlanTitle(item: LibraryManagementPlanItem): string {
	return (
		managementDesiredField(item, 'title') ??
		stringOrNull(item.capability.catalog_track_title) ??
		item.destination_relative_path?.split('/').at(-1) ??
		item.source_relative_path?.split('/').at(-1) ??
		`Item ${item.ordinal + 1}`
	);
}

export function managementPlanArtist(item: LibraryManagementPlanItem): string | null {
	return (
		managementDesiredField(item, 'artist') ?? stringOrNull(item.capability.catalog_artist_name)
	);
}

export function managementPlanAlbumArtist(item: LibraryManagementPlanItem): string | null {
	return (
		managementDesiredField(item, 'album_artist') ??
		stringOrNull(item.capability.catalog_album_artist_name) ??
		managementPlanArtist(item)
	);
}

export function managementPlanAlbum(item: LibraryManagementPlanItem): string | null {
	return managementDesiredField(item, 'album') ?? stringOrNull(item.capability.catalog_album_title);
}

export function managementPlanTrackLabel(item: LibraryManagementPlanItem): string {
	const catalogTrackNumber = numberOrNull(item.capability.catalog_track_number);
	const catalogDiscNumber = numberOrNull(item.capability.catalog_disc_number);
	const trackNumber =
		managementDesiredField(item, 'track_number') ??
		(catalogTrackNumber !== null && catalogTrackNumber > 0 ? String(catalogTrackNumber) : null);
	const discNumber =
		managementDesiredField(item, 'disc_number') ??
		(catalogDiscNumber !== null && catalogDiscNumber > 0 ? String(catalogDiscNumber) : null);
	if (!trackNumber) return String(item.ordinal + 1);
	return discNumber && discNumber !== '1' ? `${discNumber}.${trackNumber}` : trackNumber;
}

export function managementPlanChanges(
	item: LibraryManagementPlanItem
): ManagementAuditChangeKind[] {
	const diff = isRecord(item.diff) ? item.diff : {};
	const changes: ManagementAuditChangeKind[] = [];
	if (
		diff.tags_changed === true ||
		(Array.isArray(diff.field_mutations) &&
			diff.field_mutations.some(
				(value) =>
					isRecord(value) &&
					typeof value.operation === 'string' &&
					['set', 'clear', 'merge'].includes(value.operation)
			)) ||
		(Array.isArray(diff.custom_tag_mutations) &&
			diff.custom_tag_mutations.some(
				(value) =>
					isRecord(value) &&
					typeof value.operation === 'string' &&
					['set', 'append', 'delete'].includes(value.operation)
			))
	) {
		changes.push('tags');
	}
	if (
		diff.artwork_changed === true ||
		(Array.isArray(item.artwork_choices) && item.artwork_choices.length)
	) {
		changes.push('artwork');
	}
	if (
		diff.path_changed === true ||
		(Boolean(item.destination_relative_path) &&
			(item.destination_relative_path !== item.source_relative_path ||
				item.destination_root_id !== item.source_root_id))
	) {
		changes.push('path');
	}
	if (Array.isArray(diff.sidecars) && diff.sidecars.length > 0) changes.push('sidecars');
	return changes;
}

export function managementPlanIsExceptional(item: LibraryManagementPlanItem): boolean {
	const capability = isRecord(item.capability) ? item.capability : {};
	return Boolean(
		item.eligibility !== 'eligible' ||
		item.reason_code ||
		managementStringList(capability.warnings).length ||
		managementStringList(capability.blockers).length ||
		managementStringList(capability.representation_losses).length ||
		(Array.isArray(item.collisions) && item.collisions.length)
	);
}

export function managementArtworkPreviewHash(choice: Record<string, unknown>): string | null {
	const sha256 = stringOrNull(choice.blob_sha256);
	const mimeType = stringOrNull(choice.mime_type);
	if (!sha256?.match(/^[0-9a-f]{64}$/)) return null;
	return mimeType === null || MANAGEMENT_ARTWORK_PREVIEW_MIME_TYPES.has(mimeType) ? sha256 : null;
}

export function managementAudioFormat(item: LibraryManagementPlanItem): string {
	return stringOrNull(item.capability.audio_format) ?? 'unknown';
}

export function managementAlbumArtworkVersion(item: LibraryManagementPlanItem): number | null {
	return numberOrNull(item.capability.album_artwork_version);
}

export function managementAdapter(item: LibraryManagementPlanItem): string | null {
	return stringOrNull(item.capability.adapter);
}

export function managementSidecars(
	item: LibraryManagementPlanItem
): Array<Record<string, unknown>> {
	const value = item.diff.sidecars;
	return Array.isArray(value) ? value.filter(isRecord) : [];
}

function collisionRequestKind(classification: string): DuplicateCollisionKind | null {
	const exact: DuplicateCollisionKind[] = [
		'same_path_same_content',
		'same_path_different_content',
		'same_release_position_different_content',
		'normalized_path_collision',
		'sidecar_collision',
		'destination_created_after_preview'
	];
	if (exact.includes(classification as DuplicateCollisionKind)) {
		return classification as DuplicateCollisionKind;
	}
	if (classification === 'normalized_catalog_path_collision') return 'normalized_path_collision';
	if (classification === 'sidecar_path_collision') return 'sidecar_collision';
	return null;
}

export function managementCollisions(item: LibraryManagementPlanItem): ManagementCollision[] {
	return item.collisions.flatMap((value) => {
		if (!isRecord(value) || typeof value.classification !== 'string') return [];
		return [
			{
				classification: value.classification,
				requestKind: collisionRequestKind(value.classification),
				existingRootId:
					stringOrNull(value.existing_root_id) ??
					stringOrNull(value.destination_root_id) ??
					item.destination_root_id,
				existingRelativePath:
					stringOrNull(value.existing_relative_path) ??
					stringOrNull(value.destination_relative_path) ??
					item.destination_relative_path,
				existingLocalTrackId:
					stringOrNull(value.existing_local_track_id) ?? stringOrNull(value.catalog_track_id)
			}
		];
	});
}

export function formatManagementValue(value: unknown): string {
	if (value === null || value === undefined || value === '') return 'Empty';
	if (Array.isArray(value))
		return value.length ? value.map(formatManagementValue).join(' · ') : 'Empty';
	if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
		return String(value);
	}
	if (isRecord(value)) {
		return Object.entries(value)
			.map(([key, item]) => `${titleManagementValue(key)}: ${formatManagementValue(item)}`)
			.join(' · ');
	}
	return 'Unavailable';
}

export function titleManagementValue(value: string): string {
	return value.replaceAll('_', ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
}

/**
 * Human copy for every stable management reason code. Wording stays neutral
 * about loss: warnings-only rows reuse these labels while the inspector's
 * Warnings section carries the detail.
 */
export const MANAGEMENT_REASON_LABELS: Record<string, string> = {
	MANAGEMENT_DISABLED: 'File organization is turned off',
	IDENTITY_NOT_ACCEPTED: 'Identity needs review before organizing',
	RELEASE_NOT_SELECTED: 'Exact MusicBrainz edition not chosen',
	TRACK_NOT_MAPPED: 'Exact edition selected; track map missing',
	METADATA_UNAVAILABLE: 'Required metadata unavailable',
	OPTIONAL_ENRICHMENT_DEFERRED: 'Optional enrichment deferred',
	FORMAT_UNSUPPORTED: 'File format cannot be organized',
	FIELD_UNSUPPORTED_BY_FORMAT: 'Some fields cannot be stored in this format',
	FILE_UNREADABLE: 'File metadata could not be read',
	FILE_CHANGED: 'File changed after this preview was planned',
	PROFILE_CHANGED: 'Profile changed after this preview was planned',
	POLICY_CHANGED: 'Library policy changed after this preview was planned',
	OVERRIDE_CHANGED: 'Root override changed after this preview was planned',
	ROOT_UNAVAILABLE: 'Source root unavailable',
	ROOT_READ_ONLY: 'Destination root is read-only',
	OUT_OF_ROOT: 'File sits outside the configured roots',
	SYMLINK_UNSUPPORTED: 'Symbolic links cannot be organized',
	PATH_COLLISION_IDENTICAL: 'Identical file already at the destination',
	PATH_COLLISION_DIFFERENT: 'Different file already at the destination',
	POSITION_COLLISION: 'Two files claim the same release position',
	SIDECAR_COLLISION: 'Sidecar file already at the destination',
	PATH_TOO_LONG: 'Planned path exceeds the configured length limit',
	SCRIPT_VALIDATION_FAILED: 'Profile script could not safely process this file',
	INSUFFICIENT_SPACE: 'Not enough temporary disk space',
	BASELINE_UNAVAILABLE: 'Original baseline unavailable',
	BASELINE_SNAPSHOT_MISSING: 'Before-state snapshot missing',
	BASELINE_SNAPSHOT_CORRUPT: 'Before-state snapshot unreadable',
	UNDO_EXPIRED: 'Undo snapshot expired',
	RECOVERY_NEEDS_ATTENTION: 'Recovery needs attention before retrying',
	BUNDLE_BLOCKED: 'Another file in this release blocks the bundle',
	BUNDLE_TOO_LARGE: 'Release bundle exceeds the file limit',
	RECYCLE_UNAVAILABLE: 'Recycle bin unavailable for the displaced file',
	DUPLICATE_CHANGED: 'Duplicate changed after this preview was planned',
	EXTERNAL_REFRESH_PROTOCOL_UNAVAILABLE: 'Media-server refresh protocol unavailable',
	EXTERNAL_REFRESH_NOT_CONFIGURED: 'Media-server refresh not configured',
	EXTERNAL_REFRESH_AUTH_FAILED: 'Media-server refresh sign-in failed',
	EXTERNAL_REFRESH_FAILED: 'Media-server refresh failed',
	EXTERNAL_REFRESH_INTERRUPTED: 'Media-server refresh interrupted'
};

export function managementReasonLabel(value: string): string {
	return MANAGEMENT_REASON_LABELS[value] ?? titleManagementValue(value);
}
