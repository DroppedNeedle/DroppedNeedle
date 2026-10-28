// The review list's filters, read from and written to the page address so a
// link or a reload reopens the same view.
import type {
	LibraryReviewParams,
	LibraryReviewSort,
	LibraryReviewState
} from '$lib/queries/library/LibraryQueryKeyFactory';

type Policy = NonNullable<LibraryReviewParams['policy']>;
type JobState = NonNullable<LibraryReviewParams['jobState']>;

/** The five views, as the stored states they show. */
export const REVIEW_VIEWS: { state: LibraryReviewState; label: string }[] = [
	{ state: 'pending', label: 'Needs a decision' },
	{ state: 'rejected', label: 'Kept as tagged' },
	{ state: 'excluded', label: 'Excluded' },
	{ state: 'resolved', label: 'Resolved' },
	{ state: 'all', label: 'All' }
];

export const REVIEW_SORTS: { value: LibraryReviewSort; label: string }[] = [
	{ value: 'newest', label: 'Newest first' },
	{ value: 'oldest', label: 'Oldest first' },
	{ value: 'title', label: 'Title' },
	{ value: 'artist', label: 'Artist' },
	{ value: 'root', label: 'Folder' },
	{ value: 'reason', label: 'Grouped by reason' },
	{ value: 'tracks', label: 'Most tracks first' }
];

const POLICIES: Policy[] = ['automatic', 'local_metadata', 'excluded'];
const JOB_STATES: JobState[] = [
	'queued',
	'running',
	'deferred',
	'failed',
	'attention',
	'succeeded'
];

function oneOf<T extends string>(value: string | null, allowed: readonly T[]): T | '' {
	return value && (allowed as readonly string[]).includes(value) ? (value as T) : '';
}

function dayStart(value: string): number | undefined {
	const time = value ? new Date(`${value}T00:00:00`).getTime() : NaN;
	return Number.isFinite(time) ? time : undefined;
}

/** Address keys, so a search param never collides with the page's own. */
const KEYS = [
	'state',
	'reason',
	'q',
	'root',
	'policy',
	'job',
	'hide_active',
	'candidates',
	'from',
	'to',
	'sort'
] as const;

export class ReviewFilters {
	state = $state<LibraryReviewState>('pending');
	reasonCode = $state('');
	search = $state('');
	rootId = $state('');
	policy = $state<Policy | ''>('');
	jobState = $state<JobState | ''>('');
	hideActiveJobs = $state(false);
	withCandidates = $state(false);
	filedFrom = $state('');
	filedTo = $state('');
	sort = $state<LibraryReviewSort>('newest');

	constructor(params: URLSearchParams) {
		const state = params.get('state');
		// `needs_review` is the old name for pending.
		this.state =
			oneOf(
				state,
				REVIEW_VIEWS.map((view) => view.state)
			) || 'pending';
		this.reasonCode = params.get('reason') ?? '';
		this.search = params.get('q') ?? '';
		this.rootId = params.get('root') ?? '';
		this.policy = oneOf(params.get('policy'), POLICIES);
		this.jobState = oneOf(params.get('job'), JOB_STATES);
		this.hideActiveJobs = params.get('hide_active') === '1';
		this.withCandidates = params.get('candidates') === '1';
		this.filedFrom = params.get('from') ?? '';
		this.filedTo = params.get('to') ?? '';
		this.sort =
			oneOf(
				params.get('sort'),
				REVIEW_SORTS.map((sort) => sort.value)
			) || 'newest';
	}

	/** True when any address key of the review list is set. */
	static inAddress(params: URLSearchParams): boolean {
		return KEYS.some((key) => params.has(key)) || params.has('review');
	}

	/** The server's filter (paging aside). */
	get params(): Omit<LibraryReviewParams, 'limit'> {
		return {
			state: this.state,
			search: this.search || undefined,
			reasonCode: this.reasonCode || undefined,
			rootId: this.rootId || undefined,
			policy: this.policy || undefined,
			jobState: this.jobState || undefined,
			hideActiveJobs: this.hideActiveJobs,
			withCandidates: this.withCandidates,
			createdFrom: dayStart(this.filedFrom),
			createdTo: this.filedTo ? (dayStart(this.filedTo) ?? 0) + 86_399_999 : undefined,
			sort: this.sort
		};
	}

	/** Write the filters into `url`, leaving defaults out. */
	writeTo(url: URL): void {
		const values: Record<(typeof KEYS)[number], string> = {
			state: this.state,
			reason: this.reasonCode,
			q: this.search,
			root: this.rootId,
			policy: this.policy,
			job: this.jobState,
			hide_active: this.hideActiveJobs ? '1' : '',
			candidates: this.withCandidates ? '1' : '',
			from: this.filedFrom,
			to: this.filedTo,
			sort: this.sort === 'newest' ? '' : this.sort
		};
		for (const key of KEYS) {
			if (values[key]) url.searchParams.set(key, values[key]);
			else url.searchParams.delete(key);
		}
	}

	/** Remove every review key from `url`. */
	static clearFrom(url: URL): void {
		for (const key of KEYS) url.searchParams.delete(key);
		url.searchParams.delete('review');
	}
}
