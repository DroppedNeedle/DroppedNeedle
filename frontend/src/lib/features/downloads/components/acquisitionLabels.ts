/**
 * Pure copy/label mappings for the acquisition-quality surfaces (Acquisition plan).
 * Every string contract lives here so server-project specs can pin exact wording,
 * browser components stay dumb, and sibling slices (request mutations) share one
 * source of truth.
 *
 * Copy rules from the frozen API contract:
 * - Policy summary sentences are displayed verbatim from the backend.
 * - Lossy wording never says "MP3" ("Lossy 320 kbps" etc.).
 * - Lossless says bit depth / sample rate words, never "lossless bitrate".
 */

import type { Reason } from '$lib/types';

/** Shown when a zero-byte wait has no deadline and nothing further can follow. */
export const NO_FALLBACK_COPY = 'No fallback accepted';

// ---------------------------------------------------------------------------
// Request-copy lines (spec: four request-status strings)
// ---------------------------------------------------------------------------

export type RequestStatusToken = 'dispatched' | 'submitted' | 'duplicate_active' | 'in_library';

/** Verbatim previous wording for an already-owned album (backend request_service). */
export const ALREADY_IN_LIBRARY_COPY = 'Album is already in the library';

/** A request accepted while no download source is set up: it is held, not started. */
export const WAITING_FOR_SOURCE_COPY = 'Requested - waiting for a download source.';

const SUBMITTED_FOR_APPROVAL_COPY =
	'Submitted for approval. The current server policy will apply when approved.';

const REQUEST_STATUS_TOKENS: Record<string, RequestStatusToken> = {
	dispatched: 'dispatched',
	searching_now: 'dispatched',
	accepted_auto: 'dispatched',
	auto_dispatched: 'dispatched',
	submitted: 'submitted',
	pending_approval: 'submitted',
	duplicate_active: 'duplicate_active',
	already_queued: 'duplicate_active',
	in_library: 'in_library',
	already_in_library: 'in_library'
};

/**
 * Success copy keyed to the normalized request outcome (callers map backend
 * payloads onto these tokens - "pending" alone is ambiguous).
 * - dispatched      -> "Requested - searching now using <summary>." / "Requested - searching now."
 * - submitted       -> SUBMITTED_FOR_APPROVAL_COPY
 * - duplicate_active-> "Already being acquired using <summary>." / "Already being acquired."
 * - in_library      -> ALREADY_IN_LIBRARY_COPY (verbatim previous wording)
 */
export function requestStatusCopy(
	status: string | null | undefined,
	summary?: string | null
): string {
	const token = status == null ? null : REQUEST_STATUS_TOKENS[status.toLowerCase().trim()];
	switch (token) {
		case 'dispatched': {
			const detail = summary?.trim() || null;
			return detail ? `Requested - searching now using ${detail}.` : 'Requested - searching now.';
		}
		case 'duplicate_active': {
			const detail = summary?.trim() || null;
			return detail ? `Already being acquired using ${detail}.` : 'Already being acquired.';
		}
		case 'in_library':
			return ALREADY_IN_LIBRARY_COPY;
		default:
			return SUBMITTED_FOR_APPROVAL_COPY;
	}
}

// ---------------------------------------------------------------------------
// Batch request formatter
// ---------------------------------------------------------------------------

/**
 * Batch outcome toast copy - counts verbatim, never claims anything was queued.
 * Base: "<requested> requested, <skipped> skipped"; overflow appends
 * ", <n> were over the batch request limit" (singular "was").
 */
export function batchRequestCopy(requested: number, skipped: number, overflow: number): string {
	const base = `${Math.max(0, requested)} requested, ${Math.max(0, skipped)} skipped`;
	if (!Number.isFinite(overflow) || overflow <= 0) return base;
	const verb = overflow === 1 ? 'was' : 'were';
	return `${base}, ${overflow} ${verb} over the batch request limit`;
}

// ---------------------------------------------------------------------------
// Failure / empty-state classification (purely from DownloadTask fields)
// ---------------------------------------------------------------------------

export type AcquisitionEmptyState =
	| 'probe-mismatch'
	| 'nothing-found'
	| 'outside-settings'
	| 'held-outside-settings'
	| 'source-failure';

export interface EmptyStateSource {
	status?: string;
	outcome_code?: Reason | null;
}

export const EMPTY_STATE_COPY: Record<AcquisitionEmptyState, { title: string; detail: string }> = {
	'nothing-found': {
		title: 'Nothing found yet',
		detail: 'The search finished without matching copies. Try again later.'
	},
	'probe-mismatch': {
		title: 'Quality did not match what was promised',
		detail: 'The downloaded file failed verification against the advertised quality.'
	},
	'outside-settings': {
		title: 'No copy your settings allow',
		detail: 'Copies exist, but none your download settings allow. Pick a source to take one.'
	},
	'held-outside-settings': {
		title: 'Held: not in your quality settings',
		detail: 'The files wait on the Held list for you to import or discard.'
	},
	'source-failure': {
		title: 'The source gave up',
		detail: 'Search sources errored before usable copies appeared.'
	}
};

/** Outcomes that mean no source had the release or the track. */
const NOTHING_FOUND: ReadonlySet<Reason> = new Set<Reason>([
	'no_source',
	'track_not_found',
	'no_album_for_recording',
	'no_album_folder',
	'no_album_release',
	'album_releases_exhausted'
]);

/**
 * Classifies a non-productive task purely from persisted fields. Precedence:
 * a quality refusal -> probe mismatch; only refused copies -> outside-settings; unlisted
 * files held -> held-outside-settings; no source had it -> nothing-found; any other
 * outcome -> source-failure; otherwise null (healthy/active task, no extra UI).
 */
export function classifyEmptyState(task: EmptyStateSource): AcquisitionEmptyState | null {
	const outcome = task.outcome_code ?? null;
	if (outcome === 'quality_rejected') return 'probe-mismatch';
	if (outcome === null) return null;
	if (outcome === 'nothing_allowed') return 'outside-settings';
	if (outcome === 'format_not_listed') return 'held-outside-settings';
	if (NOTHING_FOUND.has(outcome)) return 'nothing-found';
	return 'source-failure';
}
