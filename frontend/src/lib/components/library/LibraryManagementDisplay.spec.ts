import { describe, expect, it } from 'vitest';

import {
	MANAGEMENT_REASON_LABELS,
	managementReasonLabel,
	titleManagementValue
} from './LibraryManagementDisplay';

const ALL_STABLE_CODES = [
	'MANAGEMENT_DISABLED',
	'IDENTITY_NOT_ACCEPTED',
	'RELEASE_NOT_SELECTED',
	'TRACK_NOT_MAPPED',
	'METADATA_UNAVAILABLE',
	'OPTIONAL_ENRICHMENT_DEFERRED',
	'FORMAT_UNSUPPORTED',
	'FIELD_UNSUPPORTED_BY_FORMAT',
	'FILE_UNREADABLE',
	'FILE_CHANGED',
	'PROFILE_CHANGED',
	'POLICY_CHANGED',
	'OVERRIDE_CHANGED',
	'ROOT_UNAVAILABLE',
	'ROOT_READ_ONLY',
	'OUT_OF_ROOT',
	'SYMLINK_UNSUPPORTED',
	'PATH_COLLISION_IDENTICAL',
	'PATH_COLLISION_DIFFERENT',
	'POSITION_COLLISION',
	'SIDECAR_COLLISION',
	'PATH_TOO_LONG',
	'SCRIPT_VALIDATION_FAILED',
	'INSUFFICIENT_SPACE',
	'BASELINE_UNAVAILABLE',
	'BASELINE_SNAPSHOT_MISSING',
	'BASELINE_SNAPSHOT_CORRUPT',
	'UNDO_EXPIRED',
	'RECOVERY_NEEDS_ATTENTION',
	'BUNDLE_BLOCKED',
	'BUNDLE_TOO_LARGE',
	'RECYCLE_UNAVAILABLE',
	'DUPLICATE_CHANGED',
	'EXTERNAL_REFRESH_PROTOCOL_UNAVAILABLE',
	'EXTERNAL_REFRESH_NOT_CONFIGURED',
	'EXTERNAL_REFRESH_AUTH_FAILED',
	'EXTERNAL_REFRESH_FAILED',
	'EXTERNAL_REFRESH_INTERRUPTED'
];

describe('managementReasonLabel', () => {
	it('covers every stable reason code with human copy', () => {
		expect(Object.keys(MANAGEMENT_REASON_LABELS).sort()).toEqual([...ALL_STABLE_CODES].sort());
		for (const code of ALL_STABLE_CODES) {
			const label = managementReasonLabel(code);
			expect(label.length).toBeGreaterThan(0);
			expect(label).not.toBe(code);
			expect(label).not.toContain('_');
		}
	});

	it('keeps the warnings-only field code free of loss claims', () => {
		expect(managementReasonLabel('FIELD_UNSUPPORTED_BY_FORMAT')).toBe(
			'Some fields cannot be stored in this format'
		);
		expect(managementReasonLabel('FIELD_UNSUPPORTED_BY_FORMAT').toLowerCase()).not.toContain(
			'loss'
		);
		expect(managementReasonLabel('FIELD_UNSUPPORTED_BY_FORMAT').toLowerCase()).not.toContain(
			'lost'
		);
	});

	it('falls back to title case for unknown codes', () => {
		expect(managementReasonLabel('SOME_FUTURE_CODE')).toBe(
			titleManagementValue('SOME_FUTURE_CODE')
		);
	});
});
