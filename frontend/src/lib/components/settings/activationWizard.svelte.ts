/**
 * The write-access wizard on the Automation tab: one dry run per affected
 * root, each accepted in turn, then the typed confirmation that saves the
 * settings the dry runs covered. Its state is kept in session storage per
 * user, so a reload resumes it. No DOM here: the tab starts it and the
 * activation dialog shows it.
 *
 * Construct it during component setup: it creates the dry-run query and the
 * activation mutations.
 */
import {
	forgetLibraryManagementActivationSession,
	readLibraryManagementActivationSession,
	rememberLibraryManagementActivationSession,
	rememberLibraryManagementPreviewToken
} from '$lib/queries/library-management/LibraryManagementPreviewTokens';
import { getLibraryManagementActivationPreviewQuery } from '$lib/queries/library-management/LibraryManagementQueries.svelte';
import {
	confirmLibraryManagementActivationMutation,
	controlLibraryManagementOperationMutation,
	createLibraryManagementActivationPreviewMutation
} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/queries/library-management/LibraryManagementConfirmation';
import { authStore } from '$lib/stores/authStore.svelte';
import type {
	LibraryManagementActivationProof,
	LibraryManagementProfile,
	LibraryManagementSettings
} from '$lib/queries/library-management/types';
import { createUuid } from '$lib/utils/uuid';

import type { ManagementSettingsDraft } from './managementSettingsDraft.svelte';

export class ActivationWizard {
	/** The settings every dry run covers and the confirmation saves. */
	draft = $state<LibraryManagementSettings | null>(null);
	rootIds = $state<string[]>([]);
	index = $state(0);
	jobId = $state<string | null>(null);
	token = $state('');
	proofs = $state<LibraryManagementActivationProof[]>([]);
	phrase = $state('');
	error = $state('');
	accepting = $state(false);
	/** The user whose session storage this state was read from. */
	sessionUser = $state<string | null>(null);

	readonly preview = getLibraryManagementActivationPreviewQuery(
		() => authStore.user?.id,
		() => this.jobId
	);
	readonly #create = createLibraryManagementActivationPreviewMutation();
	readonly #confirm = confirmLibraryManagementActivationMutation();
	readonly #stop = controlLibraryManagementOperationMutation('stop');
	readonly #settings: ManagementSettingsDraft;
	readonly #policyRevision: () => string;

	readonly active = $derived(Boolean(this.draft && this.rootIds.length > 0));
	readonly currentRootId = $derived(this.rootIds[this.index] ?? null);
	readonly ready = $derived(
		Boolean(
			this.preview.data?.ready_for_confirmation &&
			!this.preview.data.expired &&
			!this.preview.data.stale
		)
	);
	readonly stalled = $derived(Boolean(this.preview.data?.worker_stalled));
	/** False once the draft was edited after the dry runs started. */
	readonly coversDraft = $derived.by(() => {
		const current = this.#settings.draft;
		if (!this.draft || !current) return true;
		return JSON.stringify($state.snapshot(this.draft)) === JSON.stringify($state.snapshot(current));
	});
	readonly starting = $derived(this.#create.isPending);
	readonly confirming = $derived(this.#confirm.isPending);
	readonly stopping = $derived(this.#stop.isPending);
	readonly pending = $derived(
		this.#create.isPending || this.#confirm.isPending || this.#stop.isPending || this.accepting
	);
	readonly canStop = $derived(
		Boolean(
			this.preview.data &&
			['queued', 'running', 'paused'].includes(this.preview.data.state) &&
			this.preview.data.control_request !== 'stop'
		)
	);

	constructor(settings: ManagementSettingsDraft, policyRevision: () => string) {
		this.#settings = settings;
		this.#policyRevision = policyRevision;
	}

	profileFor(rootId: string): LibraryManagementProfile | null {
		if (!this.draft) return null;
		const assignment = this.draft.root_assignments.find((value) => value.root_id === rootId);
		const profileId = assignment?.profile_id ?? this.draft.default_profile_id;
		return this.draft.profiles.find((profile) => profile.id === profileId) ?? null;
	}

	/** Starts the wizard for proposed settings, or carries on when it already covers them. */
	begin(proposed: LibraryManagementSettings, rootIds: string[]): void {
		const resumes = Boolean(
			this.draft &&
			JSON.stringify($state.snapshot(this.draft)) === JSON.stringify(proposed) &&
			JSON.stringify($state.snapshot(this.rootIds)) === JSON.stringify(rootIds)
		);
		if (!resumes) this.#start(proposed, rootIds);
		this.phrase = '';
		this.error = '';
	}

	/** Starts over for saved settings whose dry runs went stale. */
	beginFresh(saved: LibraryManagementSettings, rootIds: string[]): void {
		this.#start(saved, rootIds);
		this.phrase = '';
		this.error = '';
	}

	#start(settings: LibraryManagementSettings, rootIds: string[]): void {
		this.draft = structuredClone(settings);
		this.rootIds = rootIds;
		this.index = 0;
		this.jobId = null;
		this.token = '';
		this.proofs = [];
	}

	async run(): Promise<void> {
		const rootId = this.currentRootId;
		if (!this.draft || !rootId) return;
		this.error = '';
		try {
			const handle = await this.#create.mutateAsync({
				root_id: rootId,
				settings: this.draft,
				expected_settings_revision: this.#settings.sourceRevision,
				expected_policy_revision: this.#policyRevision(),
				idempotency_key: createUuid()
			});
			this.jobId = handle.job_id;
			this.token = handle.preview_token;
			rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
		} catch (error) {
			this.error =
				error instanceof Error ? error.message : 'Could not start the activation dry run.';
		}
	}

	/** Rechecks the current dry run and, when it still holds, moves to the next root. */
	async accept(): Promise<void> {
		const rootId = this.currentRootId;
		if (!this.ready || !rootId || !this.jobId) return;
		this.error = '';
		this.accepting = true;
		try {
			const refreshed = await this.preview.refetch();
			if (refreshed.error) throw refreshed.error;
			const preview = refreshed.data;
			if (!preview || !preview.ready_for_confirmation || preview.stale || preview.expired) {
				this.error =
					'This dry run is no longer current. Start a fresh dry run before enabling automatic writes.';
				return;
			}
		} catch (error) {
			this.error = error instanceof Error ? error.message : 'Could not recheck this dry run.';
			return;
		} finally {
			this.accepting = false;
		}
		this.proofs = [
			...this.proofs,
			{ root_id: rootId, job_id: this.jobId, preview_token: this.token }
		];
		this.index += 1;
		this.jobId = null;
		this.token = '';
	}

	discard(): void {
		if (this.pending) return;
		this.jobId = null;
		this.token = '';
		this.error = '';
	}

	async stopRun(): Promise<void> {
		const preview = this.preview.data;
		if (!this.jobId || !preview || !this.canStop) return;
		this.error = '';
		try {
			await this.#stop.mutateAsync({
				jobId: this.jobId,
				expectedRevision: preview.operation_row_revision
			});
			await this.preview.refetch();
		} catch (error) {
			this.error = error instanceof Error ? error.message : 'Could not stop this dry run.';
		}
	}

	/** Saves the covered settings with every dry run as proof. True when it saved. */
	async enable(): Promise<boolean> {
		if (
			!this.draft ||
			this.proofs.length !== this.rootIds.length ||
			this.phrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
			!this.coversDraft
		)
			return false;
		this.error = '';
		try {
			const saved = await this.#confirm.mutateAsync({
				settings: this.draft,
				proofs: this.proofs,
				expected_settings_revision: this.#settings.sourceRevision,
				confirmation: true
			});
			this.#settings.accept(saved);
			this.reset();
			return true;
		} catch (error) {
			this.error = error instanceof Error ? error.message : 'Could not enable file organization.';
			return false;
		}
	}

	/** Ends the wizard and forgets its saved session. */
	reset(): void {
		const userId = authStore.user?.id;
		if (userId) forgetLibraryManagementActivationSession(userId);
		this.clear();
	}

	clear(): void {
		this.draft = null;
		this.rootIds = [];
		this.index = 0;
		this.jobId = null;
		this.token = '';
		this.proofs = [];
		this.phrase = '';
		this.error = '';
	}

	/**
	 * Picks up this user's saved wizard when it was started from the same
	 * settings and policy revisions, and forgets it otherwise.
	 */
	restore(userId: string, settingsRevision: string): void {
		const session = readLibraryManagementActivationSession(userId);
		this.sessionUser = userId;
		if (
			session &&
			session.sourceRevision === settingsRevision &&
			session.policyRevision === this.#policyRevision()
		) {
			this.#settings.draft = structuredClone(session.draft);
			this.draft = structuredClone(session.activationDraft);
			this.rootIds = [...session.rootIds];
			this.index = session.rootIndex;
			this.jobId = session.jobId;
			this.token = session.previewToken;
			this.proofs = structuredClone(session.proofs);
		} else {
			forgetLibraryManagementActivationSession(userId);
		}
	}

	/** Writes the wizard to session storage, or forgets it once it has ended. */
	persist(userId: string | null | undefined): void {
		if (!userId || this.sessionUser !== userId) return;
		const current = this.#settings.draft;
		if (!current || !this.draft || this.rootIds.length === 0) {
			forgetLibraryManagementActivationSession(userId);
			return;
		}
		rememberLibraryManagementActivationSession(userId, {
			sourceRevision: this.#settings.sourceRevision,
			policyRevision: this.#policyRevision(),
			draft: $state.snapshot(current),
			activationDraft: $state.snapshot(this.draft),
			rootIds: $state.snapshot(this.rootIds),
			rootIndex: this.index,
			jobId: this.jobId,
			previewToken: this.token,
			proofs: $state.snapshot(this.proofs)
		});
	}
}
