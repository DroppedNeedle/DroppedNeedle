/*
	One settings form: a draft seeded from a query, compared with the saved
	copy, saved through a mutation and reported on `SaveBar`.

	const section = createSettingsSection(getThingQuery(), createSaveThingMutation());

	<QueryBoundary query={section} what="the thing settings">
		{#snippet children(draft)}
			<TextField label="Name" bind:value={draft.name} />
			<SaveBar {...section.bar} />
		{/snippet}
	</QueryBoundary>

	A refetch refills the draft only while it has no unsaved edits. A secret
	field keeps the server's mask in the draft, so an untouched secret is sent
	back as the mask and the server keeps the stored value. A 4xx answer shows
	the server's message; anything else a fixed line. While a section has
	unsaved edits, `confirmLeave` and `UnsavedChangesGuard` ask before the tab
	or page is left.
*/
import { untrack } from 'svelte';
import { SvelteSet } from 'svelte/reactivity';
import { api, ApiError } from '$lib/api/client';
import { HOME_ENDPOINTS } from '$lib/queries/charts/endpoints';
import { integrationStore } from '$lib/stores/integration';

interface QueryLike<T> {
	data: T | undefined;
	isPending: boolean;
	isError: boolean;
	error: unknown;
	refetch: () => unknown;
}

interface MutationLike<T> {
	mutateAsync: (body: T) => Promise<T>;
	isPending: boolean;
}

export interface SettingsStatus {
	tone: 'success' | 'error' | 'info';
	message: string;
}

/** What a connection test found. */
export interface TestOutcome {
	passed: boolean;
	message: string;
}

export interface SettingsSectionOptions<T> {
	/**
	 * Runs the Test button. It gets the live draft and may fill fields in it,
	 * such as a user picked from what the test found.
	 */
	test?: (draft: T) => Promise<TestOutcome>;
	/**
	 * Tidies a draft before it is compared or saved, e.g. trims a list and
	 * drops its blank rows.
	 */
	normalize?: (draft: T) => T;
	/** Replaces the message shown for a refused save. */
	explain?: (error: ApiError) => string | undefined;
	/** Re-reads which integrations are connected after a save (the sidebar dots). */
	refreshIntegration?: boolean;
	/** Replaces "Saved." after a save. */
	savedMessage?: string;
}

const SAVED_CLEARS_AFTER_MS = 5000;

interface Guarded {
	readonly dirty: boolean;
	discard(): void;
}

const mounted = new SvelteSet<Guarded>();
let pendingLeave = $state<(() => void) | null>(null);

/** True while any settings section on screen has unsaved edits. */
export function hasUnsavedSettings(): boolean {
	return [...mounted].some((section) => section.dirty);
}

/**
 * Runs `proceed` now when nothing is unsaved; otherwise asks first, through
 * the `UnsavedChangesGuard` on the page.
 */
export function confirmLeave(proceed: () => void): void {
	if (hasUnsavedSettings()) pendingLeave = proceed;
	else proceed();
}

/** The leave the guard is asking about, if any. */
export const unsavedLeave = {
	get pending() {
		return pendingLeave !== null;
	},
	/** Drops every unsaved draft and carries on. */
	confirm() {
		const proceed = pendingLeave;
		pendingLeave = null;
		for (const section of mounted) if (section.dirty) section.discard();
		proceed?.();
	},
	cancel() {
		pendingLeave = null;
	}
};

function same(a: unknown, b: unknown): boolean {
	if (Object.is(a, b)) return true;
	if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
	if (Array.isArray(a) !== Array.isArray(b)) return false;
	const keysA = Object.keys(a);
	const keysB = Object.keys(b);
	if (keysA.length !== keysB.length) return false;
	return keysA.every(
		(key) =>
			Object.hasOwn(b, key) &&
			same((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key])
	);
}

function failureMessage(
	error: unknown,
	fallback: string,
	explain?: (error: ApiError) => string | undefined
): string {
	if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
		return explain?.(error) ?? error.message;
	}
	return fallback;
}

/** Call while a component initialises: it registers effects. */
export function createSettingsSection<T extends object>(
	query: QueryLike<T>,
	mutation: MutationLike<T>,
	options: SettingsSectionOptions<T> = {}
) {
	const tidy = (value: T): T => (options.normalize ? options.normalize(value) : value);

	// The server's copy: the query's data, or a save's answer when that is newer.
	let saved = $state.raw<T | undefined>(undefined);
	let draft = $state<T | undefined>(undefined);
	let status = $state<SettingsStatus | null>(null);
	let testing = $state(false);
	let testPassed = $state(false);
	let clearTimer: ReturnType<typeof setTimeout> | undefined;

	const dirty = $derived(
		draft !== undefined &&
			saved !== undefined &&
			!same(tidy($state.snapshot(draft) as T), tidy(saved))
	);

	function setStatus(next: SettingsStatus | null, clears = false) {
		clearTimeout(clearTimer);
		status = next;
		if (clears) clearTimer = setTimeout(() => (status = null), SAVED_CLEARS_AFTER_MS);
	}

	function adopt(latest: T) {
		const untouched =
			draft === undefined ||
			saved === undefined ||
			same(tidy($state.snapshot(draft) as T), tidy(saved));
		saved = latest;
		if (untouched) draft = structuredClone(latest);
	}

	// Runs before the markup reads the draft, so the form never renders empty.
	$effect.pre(() => {
		const latest = query.data;
		if (latest !== undefined) untrack(() => adopt(latest));
	});

	const entry: Guarded = {
		get dirty() {
			return dirty;
		},
		discard
	};
	$effect(() => {
		mounted.add(entry);
		return () => {
			mounted.delete(entry);
			clearTimeout(clearTimer);
		};
	});

	function discard() {
		if (saved !== undefined) draft = structuredClone(saved);
		setStatus(null);
	}

	async function save() {
		if (draft === undefined) return;
		setStatus(null);
		try {
			const stored = await mutation.mutateAsync(tidy($state.snapshot(draft) as T));
			saved = stored;
			draft = structuredClone(stored);
			setStatus({ tone: 'success', message: options.savedMessage ?? 'Saved.' }, true);
			if (options.refreshIntegration) await refreshIntegration();
		} catch (error) {
			setStatus({
				tone: 'error',
				message: failureMessage(error, 'Could not save. Try again.', options.explain)
			});
		}
	}

	async function test() {
		if (draft === undefined || !options.test) return;
		testing = true;
		setStatus(null);
		try {
			const outcome = await options.test(draft);
			testPassed = outcome.passed;
			setStatus({ tone: outcome.passed ? 'success' : 'error', message: outcome.message });
		} catch (error) {
			testPassed = false;
			setStatus({ tone: 'error', message: failureMessage(error, 'Could not run the test.') });
		} finally {
			testing = false;
		}
	}

	return {
		/** The editable copy, undefined until the query first loads. Same as `data`. */
		get draft() {
			return draft;
		},
		/** The server's copy. */
		get saved() {
			return saved;
		},
		get dirty() {
			return dirty;
		},
		get saving() {
			return mutation.isPending;
		},
		get testing() {
			return testing;
		},
		/** The last test passed. */
		get testPassed() {
			return testPassed;
		},
		get status() {
			return status;
		},
		// The query's shape, so the section can be handed to QueryBoundary.
		get data() {
			return draft;
		},
		get isPending() {
			return query.isPending || (query.data !== undefined && draft === undefined);
		},
		get isError() {
			return query.isError;
		},
		get error() {
			return query.error;
		},
		refetch: () => query.refetch(),
		/** SaveBar's props: `<SaveBar {...section.bar} />`. */
		get bar() {
			return {
				onsave: save,
				ondiscard: discard,
				dirty,
				saving: mutation.isPending,
				status,
				...(options.test ? { ontest: test, testing } : {})
			};
		},
		save,
		discard,
		test
	};
}

async function refreshIntegration() {
	try {
		const current = await api.global.v3.GET(HOME_ENDPOINTS.integrationStatus());
		if (current) integrationStore.setStatus(current);
	} catch {
		// The dots catch up on the next page load.
	}
}
