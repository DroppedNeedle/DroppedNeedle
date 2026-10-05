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

// ---------------------------------------------------------------------------
// Preference-step labels
// ---------------------------------------------------------------------------

export type QualityStepEvidence = {
	/** Index of this candidate/task inside the accepted preference order. */
	preference_step?: number | null;
	/** Length of the accepted preference order the step indexes into. */
	preference_steps_total?: number | null;
	certainty?: string | null;
};

/**
 * Step-index label used on task cards and review candidates.
 * - step 0 -> "Preferred"
 * - step n -> "Fallback n"
 * - step null/negative/out of [0, total) -> "Outside policy"
 */
export function labelForQualityStep(
	step: number | null | undefined,
	total: number | null | undefined
): string {
	if (typeof step !== 'number' || !Number.isFinite(step) || step < 0 || !Number.isInteger(step)) {
		return 'Outside policy';
	}
	if (typeof total === 'number' && Number.isFinite(total) && total > 0 && step >= total) {
		return 'Outside policy';
	}
	return step === 0 ? 'Preferred' : `Fallback ${step}`;
}

/**
 * Quality text label for a review candidate. Legacy candidate blobs written
 * before snapshots carry no preference_step (`undefined`): fall back to a
 * conservative "Within policy" ("Unknown" only when certainty says so).
 * `null` means the backend computed the step and found no accepted match.
 */
export function candidateQualityLabel(evidence?: QualityStepEvidence | null): string {
	const provided = evidence !== null && evidence !== undefined;
	if (!provided || evidence.preference_step === undefined) {
		if (provided && evidence.certainty === 'unknown') return 'Unknown';
		return 'Within policy';
	}
	if (evidence.certainty === 'unknown') return 'Unknown';
	return labelForQualityStep(
		evidence.preference_step ?? null,
		evidence.preference_steps_total ?? null
	);
}

// ---------------------------------------------------------------------------
// Fallback countdown copy
// ---------------------------------------------------------------------------

/** Shown when a zero-byte wait has no deadline and nothing further can follow. */
export const NO_FALLBACK_COPY = 'No fallback accepted';

const LOSSLESS_EXTENSIONS: Record<string, true> = {
	flac: true,
	alac: true,
	wav: true,
	ape: true,
	wv: true,
	aiff: true,
	aif: true,
	dsf: true,
	dff: true
};
const LOSSY_EXTENSIONS: Record<string, true> = {
	mp3: true,
	ogg: true,
	oga: true,
	opus: true,
	aac: true,
	m4a: true,
	wma: true,
	mp2: true
};

function khzLabel(sampleRateHz: number): string {
	const khz = sampleRateHz / 1000;
	return Number.isInteger(khz) ? khz.toFixed(0) : khz.toFixed(1);
}

export interface NextPreferenceSource {
	format?: string | null;
	bitrate?: number | null;
	bit_depth?: number | null;
	sample_rate?: number | null;
}

/**
 * Names the preference currently being awaited, from advertised evidence.
 * Lossless requires depth AND rate known; lossy requires a positive bitrate;
 * anything unnameable returns null so callers use "Next accepted preference".
 * Exact outputs: "24-bit/96 kHz lossless", "Lossy 320".
 */
export function nextPreferenceLabel(source?: NextPreferenceSource | null): string | null {
	if (!source) return null;
	const ext = source.format?.toLowerCase().trim() ?? '';
	const depth =
		typeof source.bit_depth === 'number' && source.bit_depth > 0 ? source.bit_depth : null;
	const rate =
		typeof source.sample_rate === 'number' && source.sample_rate > 0 ? source.sample_rate : null;
	if (LOSSLESS_EXTENSIONS[ext] && depth !== null && rate !== null) {
		return `${depth}-bit/${khzLabel(rate)} kHz lossless`;
	}
	const bitrate = typeof source.bitrate === 'number' && source.bitrate > 0 ? source.bitrate : null;
	if (LOSSY_EXTENSIONS[ext] && bitrate !== null) {
		return `Lossy ${Math.round(bitrate)}`;
	}
	return null;
}

/**
 * Countdown line while queued on a zero-byte source.
 * Exact outputs: "Lossy 320 fallback in 8m", "Next accepted preference fallback in 8m"
 * and (for the no-deadline/no-next-source slot) NO_FALLBACK_COPY.
 */
export function fallbackCopy(
	nextPreferenceName: string | null | undefined,
	minutes: number | null | undefined
): string {
	if (typeof minutes !== 'number' || !Number.isFinite(minutes)) return NO_FALLBACK_COPY;
	const whole = Math.max(0, Math.ceil(minutes));
	const label = nextPreferenceName || 'Next accepted preference';
	return `${label} fallback in ${whole}m`;
}

// ---------------------------------------------------------------------------
// Request-copy lines (spec: four request-status strings)
// ---------------------------------------------------------------------------

export type RequestStatusToken = 'dispatched' | 'submitted' | 'duplicate_active' | 'in_library';

/** Verbatim previous wording for an already-owned album (backend request_service). */
export const ALREADY_IN_LIBRARY_COPY = 'Album is already in the library';

export const SUBMITTED_FOR_APPROVAL_COPY =
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
	'probe-mismatch' | 'unknown-only' | 'all-outside-policy' | 'nothing-found' | 'source-failure';

export interface EmptyStateSource {
	status?: string;
	error_message?: string | null;
	manual_quality_override?: boolean;
	quality_certainty?: string | null;
	quality_snapshot_summary?: string | null;
}

export const EMPTY_STATE_COPY: Record<AcquisitionEmptyState, { title: string; detail: string }> = {
	'nothing-found': {
		title: 'Nothing found yet',
		detail: 'The search finished without matching copies. Try again later.'
	},
	'all-outside-policy': {
		title: 'Copies were outside your quality policy',
		detail: 'Matches existed, but none met the accepted quality range.'
	},
	'unknown-only': {
		title: 'Only unknown-resolution copies were found',
		detail: 'Everything available hid its real quality, so we stopped instead of guessing.'
	},
	'probe-mismatch': {
		title: 'Quality did not match what was promised',
		detail: 'The downloaded file failed verification against the advertised quality.'
	},
	'source-failure': {
		title: 'The source gave up',
		detail: 'Search sources errored before usable copies appeared.'
	}
};

/**
 * Classifies a non-productive task purely from persisted fields. Precedence:
 * manual override / any "quality" failure -> probe mismatch; family-unknown
 * evidence -> unknown-only; explicit outside-policy wording -> all-outside-policy;
 * a message naming no results -> nothing-found; any other message ->
 * source-failure; otherwise null (healthy/active task, no extra UI).
 */
export function classifyEmptyState(task: EmptyStateSource): AcquisitionEmptyState | null {
	const message = task.error_message ?? '';
	if (task.manual_quality_override || /quality/i.test(message)) return 'probe-mismatch';
	if (
		task.quality_certainty === 'unknown' ||
		/unknown-resolution/i.test(task.quality_snapshot_summary ?? '')
	) {
		return 'unknown-only';
	}
	const lower = message.toLowerCase();
	if (/outside polic|outside the accepted/.test(lower)) return 'all-outside-policy';
	if (
		/\bno (usable|acceptable|matching|suitable)\b|\bno \w*(results|candidates|copies)\b|not found/.test(
			lower
		)
	) {
		return 'nothing-found';
	}
	if (lower.trim()) return 'source-failure';
	return null;
}

/** Reason copy attached to hard-blocked (unimportable) review candidates. */
export const BLOCKED_PICK_REASON = 'Blocked: outside the accepted quality policy.';
